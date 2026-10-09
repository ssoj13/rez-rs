//! Platform detection and system identification.
//!
//! Ported from Python rez platform bindings and system utilities.

use std::collections::HashMap;
use std::env;
use std::fmt;
use std::sync::LazyLock;

// ---------------------------------------------------------------------------
// Platform enum
// ---------------------------------------------------------------------------

/// Operating system platform
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    Windows,
    Linux,
    MacOS,
}

impl Platform {
    /// Detect current platform at compile time
    pub fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOS
        } else {
            Self::Linux
        }
    }

    /// Rez-compatible platform name string
    pub fn name(&self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Linux => "linux",
            Self::MacOS => "osx",
        }
    }

    /// Whether filesystem is case-sensitive
    pub fn has_case_sensitive_fs(&self) -> bool {
        !matches!(self, Self::Windows)
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Rename a filesystem entry with an explicit destination replacement policy.
///
/// No-replace publication is atomic: a concurrently created destination is
/// preserved, including an empty directory. No existence-check/rename race.
pub fn rename(
    source: &std::path::Path,
    destination: &std::path::Path,
    replace: bool,
) -> std::io::Result<()> {
    if replace {
        return std::fs::rename(source, destination);
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::ffi::OsStrExt;
        let source = std::ffi::CString::new(source.as_os_str().as_bytes())
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
        let destination = std::ffi::CString::new(destination.as_os_str().as_bytes())
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
        #[cfg(target_os = "linux")]
        // SAFETY: both pointers refer to live nul-terminated path strings. The
        // syscall avoids requiring a newer glibc renameat2 wrapper.
        let status = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                destination.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(target_os = "macos")]
        // SAFETY: both pointers refer to live nul-terminated path strings.
        let status =
            unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) };
        if status == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};
        let mut source: Vec<u16> = source.as_os_str().encode_wide().collect();
        let mut destination: Vec<u16> = destination.as_os_str().encode_wide().collect();
        if source.contains(&0) || destination.contains(&0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "rename path contains a nul character",
            ));
        }
        source.push(0);
        destination.push(0);
        // SAFETY: both buffers are live nul-terminated Windows path strings.
        // Omitting MOVEFILE_REPLACE_EXISTING prevents overwriting a destination.
        if unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "atomic no-replace rename is unsupported on this platform",
        ))
    }
}

/// Return the device identity of an existing file or directory, following links.
///
/// Cache policy compares actual filesystem devices, including mounted volumes
/// and Windows junctions; drive letters are not reliable device identities.
pub fn filesystem_device(path: &std::path::Path) -> std::io::Result<u64> {
    filesystem_identity(path).map(|(device, _)| device)
}

/// Return device and file identity, following links and preserving 128-bit Windows IDs.
pub fn filesystem_identity(path: &std::path::Path) -> std::io::Result<(u64, u128)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(path)?;
        Ok((metadata.dev(), u128::from(metadata.ino())))
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
            BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_INFO,
            FILE_READ_ATTRIBUTES,
        };

        let file = std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)?;
        let handle = file.as_raw_handle();
        let mut id = std::mem::MaybeUninit::<FILE_ID_INFO>::uninit();
        // SAFETY: the live File owns the handle; the output buffer has the exact
        // FILE_ID_INFO size and is read only when Windows reports success.
        if unsafe {
            GetFileInformationByHandleEx(
                handle,
                FileIdInfo,
                id.as_mut_ptr().cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        } != 0
        {
            // SAFETY: the successful call initialized FILE_ID_INFO.
            let id = unsafe { id.assume_init() };
            return Ok((
                id.VolumeSerialNumber,
                u128::from_le_bytes(id.FileId.Identifier),
            ));
        }
        // Older filesystems may not provide FILE_ID_INFO; use their legacy volume ID.
        let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
        // SAFETY: the handle is live and the output buffer is correctly sized.
        if unsafe { GetFileInformationByHandle(handle, info.as_mut_ptr()) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: GetFileInformationByHandle reported success.
        let info = unsafe { info.assume_init() };
        Ok((
            u64::from(info.dwVolumeSerialNumber),
            (u128::from(info.nFileIndexHigh) << 32) | u128::from(info.nFileIndexLow),
        ))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "filesystem device identity is unsupported on this platform",
        ))
    }
}

/// Return total, used, and caller-available bytes for an existing directory.
/// Windows totals and usage respect caller quotas. Unix usage is filesystem-wide,
/// while available bytes exclude space reserved from the calling user.
pub fn filesystem_usage(path: &std::path::Path) -> std::io::Result<(u64, u64, u64)> {
    let path = std::fs::canonicalize(path)?;
    if !path.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "filesystem usage requires a directory",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
        let mut data = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: path is live and nul-terminated; the output has statvfs's exact size.
        if unsafe { libc::statvfs(path.as_ptr(), data.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: statvfs reported success and initialized the output.
        let data = unsafe { data.assume_init() };
        let unit = u128::from(data.f_frsize);
        let used = data.f_blocks.checked_sub(data.f_bfree).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid filesystem block counts",
            )
        })?;
        let bytes = |blocks: u128| {
            u64::try_from(blocks * unit).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "filesystem size exceeds u64",
                )
            })
        };
        Ok((
            bytes(u128::from(data.f_blocks))?,
            bytes(u128::from(used))?,
            bytes(u128::from(data.f_bavail))?,
        ))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let mut path: Vec<u16> = path.as_os_str().encode_wide().collect();
        if path.last() != Some(&u16::from(b'\\')) {
            path.push(u16::from(b'\\'));
        }
        path.push(0);
        let (mut available, mut total) = (0, 0);
        // SAFETY: the path is a live nul-terminated directory string, and all
        // output pointers refer to writable u64 values.
        if unsafe {
            GetDiskFreeSpaceExW(
                path.as_ptr(),
                &mut available,
                &mut total,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        // Both values are caller-relative. Mixing quota-limited total bytes
        // with volume-wide free bytes can underflow on a quota-enabled volume.
        let used = total.checked_sub(available).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid filesystem byte counts",
            )
        })?;
        Ok((total, used, available))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "filesystem usage is unsupported on this platform",
        ))
    }
}

// ---------------------------------------------------------------------------
// Architecture
// ---------------------------------------------------------------------------

/// CPU architecture
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Arch {
    name: String,
}

impl Arch {
    /// Detect current architecture
    pub fn current() -> Self {
        Self {
            name: if cfg!(windows) {
                env::var("PROCESSOR_ARCHITEW6432")
                    .ok()
                    .filter(|value| !value.is_empty())
                    .or_else(|| {
                        env::var("PROCESSOR_ARCHITECTURE")
                            .ok()
                            .filter(|value| !value.is_empty())
                    })
                    .unwrap_or_else(|| {
                        match std::env::consts::ARCH {
                            "x86_64" => "AMD64",
                            "x86" => "x86",
                            "aarch64" => "ARM64",
                            arch => arch,
                        }
                        .to_owned()
                    })
            } else if cfg!(target_os = "macos") && std::env::consts::ARCH == "aarch64" {
                "arm64".to_owned()
            } else {
                Self::rez_name(std::env::consts::ARCH).to_owned()
            },
        }
    }

    /// Rez-compatible arch name (maps Rust arch to Python platform.machine() style)
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Map Rust std::env::consts::ARCH to rez-compatible name
    fn rez_name(arch: &str) -> &str {
        match arch {
            "x86_64" => "x86_64",
            "x86" => "i386",
            "aarch64" => "aarch64",
            "arm" => "arm",
            "powerpc64" => "ppc64",
            "powerpc" => "ppc",
            "s390x" => "s390x",
            other => other,
        }
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

// ---------------------------------------------------------------------------
// OS version string
// ---------------------------------------------------------------------------

/// Operating system with version info
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OsVersion {
    raw: String,
}

impl OsVersion {
    /// Detect OS version string for current platform
    pub fn current() -> Self {
        let raw = detect_os_version();
        Self {
            raw: make_safe_version_string(&raw),
        }
    }

    /// Raw OS version string (e.g., "ubuntu-22.04", "windows-10.0", "osx-14.2")
    pub fn name(&self) -> &str {
        &self.raw
    }
}

impl fmt::Display for OsVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

// ---------------------------------------------------------------------------
// System info aggregate (singleton)
// ---------------------------------------------------------------------------

/// Ordered Python regular-expression substitutions for one system attribute.
/// A map is retained as ordered entries because Rez stops after the first substitution.
#[derive(Debug, Clone, Default)]
pub struct PlatformRules(pub Vec<(String, String)>);

impl serde::Serialize for PlatformRules {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (pattern, replacement) in &self.0 {
            map.serialize_entry(pattern, replacement)?;
        }
        map.end()
    }
}

impl<'de> serde::Deserialize<'de> for PlatformRules {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RulesVisitor;
        impl<'de> serde::de::Visitor<'de> for RulesVisitor {
            type Value = PlatformRules;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an ordered mapping of regex patterns to replacement strings")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut rules = Vec::new();
                while let Some(entry) = map.next_entry::<String, String>()? {
                    rules.push(entry);
                }
                Ok(PlatformRules(rules))
            }
        }
        deserializer.deserialize_map(RulesVisitor)
    }
}

/// Aggregated system information (cached singleton)
#[derive(Debug, Clone)]
pub struct SystemInfo {
    pub platform: Platform,
    pub arch: Arch,
    pub os: OsVersion,
}

/// Global cached system info
#[doc(hidden)]
pub static RAW_SYSTEM: LazyLock<SystemInfo> = LazyLock::new(|| SystemInfo {
    platform: Platform::current(),
    arch: Arch::current(),
    // Mapping is applied to the unsanitized OS name before system normalization.
    os: OsVersion {
        raw: detect_os_version(),
    },
});

/// Mapped runtime identity. Configuration prepares this before publication, so
/// consumers (including Python callbacks) never enqueue recursive VM work.
pub static SYSTEM: LazyLock<SystemInfo> = LazyLock::new(|| {
    crate::config::CONFIG
        .system_info
        .clone()
        .unwrap_or_else(|| RAW_SYSTEM.clone())
});

impl SystemInfo {
    /// Apply Rez platform_map using Python's own regex replacement semantics.
    #[doc(hidden)]
    pub fn mapped(&self, rules: &HashMap<String, PlatformRules>) -> crate::errors::Result<Self> {
        let inject = HashMap::from([
            (
                "_rules".into(),
                serde_json::to_value(rules).map_err(|error| {
                    crate::errors::RezError::Config(format!("platform_map: {error}"))
                })?,
            ),
            (
                "_values".into(),
                serde_json::json!({"arch": self.arch.name(), "os": self.os.name()}),
            ),
        ]);
        let values = if rules.get("arch").is_some_and(|rules| !rules.0.is_empty())
            || rules.get("os").is_some_and(|rules| !rules.0.is_empty())
        {
            python_runtime::exec_py_globals(
            "import re\nfor _axis, _value in _values.items():\n    for _pattern, _replacement in _rules.get(_axis, {}).items():\n        try:\n            _value, _changes = re.subn(_pattern, _replacement, _value)\n        except re.error as _error:\n            raise ValueError(f'platform_map.{_axis}[{_pattern!r}]: {_error}') from _error\n        if _changes:\n            break\n    _values[_axis] = _value\nresult = _values\n",
            "<platform_map>", Some(&inject), &["_rules", "_values", "_axis", "_value", "_pattern", "_replacement", "_changes"],
        ).map_err(|error| crate::errors::RezError::Config(format!("platform_map: {error}")))?
        } else {
            HashMap::from([("result".into(), inject["_values"].clone())])
        };
        let result = values.get("result").ok_or_else(|| {
            crate::errors::RezError::Config("platform_map: missing mapped system".into())
        })?;
        let value = |axis: &str| -> crate::errors::Result<String> {
            result
                .get(axis)
                .and_then(serde_json::Value::as_str)
                .map(make_safe_version_string)
                .ok_or_else(|| {
                    crate::errors::RezError::Config(format!(
                        "platform_map.{axis}: mapped value must be a string"
                    ))
                })
        };
        Ok(Self {
            platform: self.platform,
            arch: Arch {
                name: value("arch")?,
            },
            os: OsVersion { raw: value("os")? },
        })
    }

    /// Rez "implicit packages" variant list: ["platform-X", "arch-Y", "os-Z"]
    pub fn variant(&self) -> Vec<String> {
        vec![
            format!("platform-{}", self.platform),
            format!("arch-{}", self.arch),
            format!("os-{}", self.os),
        ]
    }

    /// Current username
    pub fn user() -> String {
        env::var("USER")
            .or_else(|_| env::var("USERNAME"))
            .unwrap_or_else(|_| "unknown".to_string())
    }

    /// Home directory, honoring explicit overrides before native account discovery.
    pub fn home() -> Option<String> {
        foundation::home()
    }

    /// Hostname
    pub fn hostname() -> String {
        hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "unknown".to_string())
    }

    /// Invoking system shell, independent of a configured execution shell.
    pub fn shell() -> &'static str {
        if Platform::current() == Platform::Windows {
            "powershell"
        } else {
            default_shell()
        }
    }

    /// Whether the invoking process is a Rez self-test subprocess.
    pub fn selftest_is_running() -> bool {
        env::var("__REZ_SELFTEST_RUNNING").is_ok_and(|value| value == "1")
    }

    /// Locate a production CLI directory without materializing a Python snapshot.
    ///
    /// Python Rez recognizes its production marker. Native deployments instead
    /// prove ownership with their schema-1 primary executable hash. A loose
    /// release or development executable is not a production installation.
    pub fn rez_bin_path(
        executable: &std::path::Path,
    ) -> crate::errors::Result<Option<std::path::PathBuf>> {
        use crate::errors::RezError;
        use std::fs::{self, File};
        use std::io::ErrorKind;

        let Some(directory) = executable.parent() else {
            return Ok(None);
        };
        let error = |message: String| {
            RezError::System(format!(
                "Inspect Rez production directory {}: {message}",
                directory.display()
            ))
        };
        if directory.join(".rez_production_install").exists() {
            return directory
                .canonicalize()
                .map(Some)
                .map_err(|cause| error(cause.to_string()));
        }

        let manifest = directory.join(".rez-rs-cli.json");
        let bytes = match fs::read(&manifest) {
            Ok(bytes) => bytes,
            Err(cause) if cause.kind() == ErrorKind::NotFound => return Ok(None),
            Err(cause) => return Err(error(format!("read {}: {cause}", manifest.display()))),
        };
        #[derive(serde::Deserialize)]
        struct Ownership {
            schema: u32,
            hashes: HashMap<String, String>,
        }
        let ownership: Ownership = serde_json::from_slice(&bytes)
            .map_err(|cause| error(format!("invalid {}: {cause}", manifest.display())))?;
        if ownership.schema != 1 {
            return Err(error(format!(
                "unsupported ownership schema {}",
                ownership.schema
            )));
        }
        let primary = if cfg!(windows) { "rez.exe" } else { "rez" };
        let Some(expected) = ownership.hashes.get(primary) else {
            return Ok(None);
        };
        if expected.len() != 64
            || !expected
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(error(format!("invalid primary hash for {primary}")));
        }
        let mut incoming = File::open(executable)
            .map_err(|cause| error(format!("open {}: {cause}", executable.display())))?;
        let actual = crate::util::hash_reader_hex::<sha2::Sha256>(&mut incoming)
            .map_err(|cause| error(format!("hash {}: {cause}", executable.display())))?;
        if &actual != expected {
            return Err(error(format!(
                "primary ownership hash does not match {}",
                executable.display()
            )));
        }
        directory
            .canonicalize()
            .map(Some)
            .map_err(|cause| error(cause.to_string()))
    }

    /// Shared package and bundle Rex system snapshot.
    ///
    /// Identity uses the mapped current system, like Python Rez's global System.
    /// DNS properties are evaluated lazily by the Python binding.
    #[doc(hidden)]
    pub fn rex_data(
        &self,
        parent: &HashMap<String, String>,
    ) -> crate::errors::Result<serde_json::Value> {
        let executable = env::current_exe().map_err(|cause| {
            crate::errors::RezError::System(format!("Locate Rez executable: {cause}"))
        })?;
        let rez_bin_path = Self::rez_bin_path(&executable)?;
        Ok(serde_json::json!({
            "rez_version": env!("CARGO_PKG_VERSION"),
            "platform": self.platform.name(),
            "arch": self.arch.name(),
            "os": self.os.name(),
            "variant": self.variant(),
            "shell": Self::shell(),
            "user": Self::user(),
            "home": Self::home().unwrap_or_else(|| "~".into()),
            "hostname": Self::hostname(),
            "rez_bin_path": rez_bin_path.map(|path| path.to_string_lossy().into_owned()),
            "selftest_is_running": Self::selftest_is_running(),
            "paths": crate::environment::system_paths(self.platform, parent),
            "environ": crate::environment::platform_environ(self.platform, parent),
        }))
    }
}

// ---------------------------------------------------------------------------
// OS version detection (platform-specific)
// ---------------------------------------------------------------------------

/// Detect OS version string for rez
fn detect_os_version() -> String {
    #[cfg(target_os = "windows")]
    {
        detect_os_windows()
    }

    #[cfg(target_os = "linux")]
    {
        detect_os_linux()
    }

    #[cfg(target_os = "macos")]
    {
        detect_os_macos()
    }
}

#[cfg(target_os = "windows")]
fn detect_os_windows() -> String {
    // Use ver command or registry for Windows version
    use std::process::Command;
    let command = env::var_os("SystemRoot")
        .map(|root| {
            std::path::PathBuf::from(root)
                .join("System32")
                .join("cmd.exe")
        })
        .unwrap_or_else(|| "cmd.exe".into());
    if let Ok(output) = Command::new(command).args(["/D", "/C", "ver"]).output() {
        let ver = String::from_utf8_lossy(&output.stdout);
        // Parse "Microsoft Windows [Version 10.0.22631.4890]"
        // Match the numeric version independently of localized "Version" text.
        if let Some(start) = ver.find(|c: char| c.is_ascii_digit()) {
            let rest = &ver[start..];
            if let Some(end) = rest.find(|c: char| !c.is_ascii_digit() && c != '.') {
                let version = &rest[..end];
                // Python platform.win32_ver reports major.minor.build. Retain
                // the build number: configured Windows 10/11 mappings depend on it.
                let parts: Vec<&str> = version.split('.').take(3).collect();
                if parts.len() >= 2
                    && parts
                        .iter()
                        .all(|part| part.chars().all(|c| c.is_ascii_digit()))
                {
                    return format!("windows-{}", parts.join("."));
                }
            }
        }
    }
    "windows".to_string()
}

#[cfg(target_os = "linux")]
fn detect_os_linux() -> String {
    // Try /etc/os-release first (freedesktop standard)
    if let Ok(content) = std::fs::read_to_string("/etc/os-release") {
        let mut id = None;
        let mut version_id = None;
        for line in content.lines() {
            if let Some(val) = line.strip_prefix("ID=") {
                id = Some(val.trim_matches('"').to_lowercase());
            }
            if let Some(val) = line.strip_prefix("VERSION_ID=") {
                version_id = Some(val.trim_matches('"').to_string());
            }
        }
        match (id, version_id) {
            (Some(id), Some(ver)) => return format!("{}-{}", id, ver),
            (Some(id), None) => return id,
            (None, _) => {}
        }
    }

    // Fallback: /etc/lsb-release (Ubuntu/Debian)
    if let Ok(content) = std::fs::read_to_string("/etc/lsb-release") {
        let mut distrib = None;
        let mut release = None;
        for line in content.lines() {
            if let Some(val) = line.strip_prefix("DISTRIB_ID=") {
                distrib = Some(val.trim_matches('"').to_lowercase());
            }
            if let Some(val) = line.strip_prefix("DISTRIB_RELEASE=") {
                release = Some(val.trim_matches('"').to_string());
            }
        }
        if let (Some(d), Some(r)) = (distrib, release) {
            return format!("{}-{}", d, r);
        }
    }

    "linux".to_string()
}

#[cfg(target_os = "macos")]
fn detect_os_macos() -> String {
    use std::process::Command;
    if let Ok(output) = Command::new("sw_vers").arg("-productVersion").output() {
        let ver = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !ver.is_empty() {
            return format!("osx-{}", ver);
        }
    }
    "osx".to_string()
}

// ---------------------------------------------------------------------------
// Safe version string
// ---------------------------------------------------------------------------

/// Normalize system identity like Rez System._make_safe_version_string.
/// Retain the first separator between non-empty tokens; invalid token characters
/// become underscores. Rez trims dots before dashes, rather than trimming both together.
fn make_safe_version_string(s: &str) -> String {
    let s = s.trim_matches('.').trim_matches('-');
    let mut result = String::with_capacity(s.len());
    let mut token = false;
    for ch in s.chars() {
        if ch == '.' || ch == '-' {
            if token {
                result.push(ch);
            }
            token = false;
        } else {
            result.push(if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            });
            token = true;
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Shell detection
// ---------------------------------------------------------------------------

/// Default shell for current platform
pub fn default_shell() -> &'static str {
    match Platform::current() {
        Platform::Windows => "cmd",
        Platform::Linux => detect_shell_unix(),
        Platform::MacOS => detect_shell_unix(),
    }
}

fn detect_shell_unix() -> &'static str {
    static SHELL: LazyLock<String> = LazyLock::new(|| {
        env::var("SHELL")
            .ok()
            .and_then(|s| s.rsplit('/').next().map(String::from))
            .unwrap_or_else(|| "bash".to_string())
    });
    &SHELL
}

// ---------------------------------------------------------------------------
// Tool detection
// ---------------------------------------------------------------------------

/// Check if a command exists in system PATH
fn cmd_exists(name: &str) -> bool {
    env::var_os("PATH")
        .and_then(|paths| {
            env::split_paths(&paths)
                .flat_map(|p| {
                    let mut candidates = vec![p.join(name)];
                    if cfg!(target_os = "windows") {
                        candidates.push(p.join(format!("{}.exe", name)));
                        candidates.push(p.join(format!("{}.cmd", name)));
                        candidates.push(p.join(format!("{}.bat", name)));
                    }
                    candidates
                })
                .find(|p| p.exists())
        })
        .is_some()
}

/// Detect system text editor
pub fn detect_editor() -> Option<String> {
    // Check VISUAL first, then EDITOR
    if let Ok(ed) = env::var("VISUAL") {
        if !ed.is_empty() {
            return Some(ed);
        }
    }
    if let Ok(ed) = env::var("EDITOR") {
        if !ed.is_empty() {
            return Some(ed);
        }
    }

    // Try common editors
    let editors = if cfg!(target_os = "windows") {
        vec!["code", "notepad", "vim", "nano"]
    } else if cfg!(target_os = "macos") {
        vec!["open", "code", "vim", "nano", "vi"]
    } else {
        vec!["code", "vim", "nano", "vi"]
    };

    editors
        .iter()
        .find(|&&ed| cmd_exists(ed))
        .map(|&ed| ed.to_string())
}

/// Detect diff tool
pub fn detect_difftool() -> Option<String> {
    // Check DIFF_TOOL env var
    if let Ok(tool) = env::var("DIFF_TOOL") {
        if !tool.is_empty() {
            return Some(tool);
        }
    }

    // Try common diff tools
    let tools = if cfg!(target_os = "windows") {
        vec![
            "meld", "WinMerge", "kdiff3", "diffuse", "vimdiff", "diff", "fc",
        ]
    } else if cfg!(target_os = "macos") {
        vec!["meld", "kdiff3", "diffuse", "vimdiff", "diff"]
    } else {
        vec!["kdiff3", "meld", "diffuse", "vimdiff", "diff"]
    };

    tools
        .iter()
        .find(|&&t| cmd_exists(t))
        .map(|&t| t.to_string())
}

/// Detect image viewer
pub fn detect_image_viewer() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        Some("open".to_string())
    }

    #[cfg(target_os = "windows")]
    {
        // Windows uses default association via empty string or explorer
        if cmd_exists("explorer") {
            Some("explorer".to_string())
        } else {
            Some(String::new()) // Empty string means use default association
        }
    }

    #[cfg(target_os = "linux")]
    {
        let viewers = ["xdg-open", "eog", "kview", "feh", "display"];
        viewers
            .iter()
            .find(|&&v| cmd_exists(v))
            .map(|&v| v.to_string())
    }
}

/// Detect terminal emulator
pub fn detect_terminal() -> Option<String> {
    // Check TERMINAL env var
    if let Ok(term) = env::var("TERMINAL") {
        if !term.is_empty() {
            return Some(term);
        }
    }

    #[cfg(target_os = "macos")]
    {
        Some("Terminal.app".to_string())
    }

    #[cfg(target_os = "windows")]
    {
        let terminals = ["wt", "cmd"];
        terminals
            .iter()
            .find(|&&t| cmd_exists(t))
            .map(|&t| t.to_string())
    }

    #[cfg(target_os = "linux")]
    {
        let terminals = ["x-terminal-emulator", "gnome-terminal", "konsole", "xterm"];
        terminals
            .iter()
            .find(|&&t| cmd_exists(t))
            .map(|&t| t.to_string())
    }
}

// ---------------------------------------------------------------------------
// CPU info
// ---------------------------------------------------------------------------

/// Number of logical CPU cores
pub fn logical_cores() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// Number of physical CPU cores (best-effort)
pub fn physical_cores() -> usize {
    #[cfg(target_os = "linux")]
    {
        linux_physical_cores().unwrap_or_else(logical_cores)
    }

    #[cfg(target_os = "macos")]
    {
        macos_physical_cores().unwrap_or_else(logical_cores)
    }

    #[cfg(target_os = "windows")]
    {
        windows_physical_cores().unwrap_or_else(logical_cores)
    }
}

#[cfg(target_os = "linux")]
fn linux_physical_cores() -> Option<usize> {
    use std::collections::HashSet;
    let content = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    let mut cores = HashSet::new();
    let mut phys_id = None;
    let mut core_id = None;

    for line in content.lines() {
        if let Some(val) = line.strip_prefix("physical id") {
            phys_id = val
                .trim_start_matches([' ', '\t', ':'])
                .trim()
                .parse::<u32>()
                .ok();
        }
        if let Some(val) = line.strip_prefix("core id") {
            core_id = val
                .trim_start_matches([' ', '\t', ':'])
                .trim()
                .parse::<u32>()
                .ok();
        }
        if line.trim().is_empty() {
            if let (Some(p), Some(c)) = (phys_id, core_id) {
                cores.insert((p, c));
            }
            phys_id = None;
            core_id = None;
        }
    }
    // Handle last entry without trailing newline
    if let (Some(p), Some(c)) = (phys_id, core_id) {
        cores.insert((p, c));
    }

    if cores.is_empty() {
        None
    } else {
        Some(cores.len())
    }
}

#[cfg(target_os = "macos")]
fn macos_physical_cores() -> Option<usize> {
    use std::process::Command;
    let output = Command::new("sysctl")
        .args(["-n", "hw.physicalcpu"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

#[cfg(target_os = "windows")]
fn windows_physical_cores() -> Option<usize> {
    use std::process::Command;
    let output = Command::new("wmic")
        .args(["cpu", "get", "NumberOfCores", "/value"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut total = 0usize;
    for line in text.lines() {
        if let Some(val) = line.strip_prefix("NumberOfCores=") {
            if let Ok(n) = val.trim().parse::<usize>() {
                total += n;
            }
        }
    }
    if total > 0 {
        Some(total)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_atomic_no_replace_rename_preserves_foreign_entries() {
        let owned = tempfile::tempdir().unwrap();
        let source = owned.path().join("source");
        let destination = owned.path().join("destination");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("payload"), "owned").unwrap();
        std::fs::create_dir(&destination).unwrap();
        assert!(rename(&source, &destination, false).is_err());
        assert!(source.join("payload").is_file());
        assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
        std::fs::write(destination.join("foreign"), "preserve").unwrap();
        assert!(rename(&source, &destination, false).is_err());
        assert_eq!(
            std::fs::read_to_string(destination.join("foreign")).unwrap(),
            "preserve"
        );

        let source_file = owned.path().join("incoming");
        let destination_file = owned.path().join("existing");
        std::fs::write(&source_file, "incoming").unwrap();
        std::fs::write(&destination_file, "existing").unwrap();
        assert!(rename(&source_file, &destination_file, false).is_err());
        assert_eq!(
            std::fs::read_to_string(&destination_file).unwrap(),
            "existing"
        );
        rename(&source_file, &destination_file, true).unwrap();
        assert_eq!(
            std::fs::read_to_string(&destination_file).unwrap(),
            "incoming"
        );
    }

    #[test]
    fn test_atomic_no_replace_directory_publication_has_one_winner() {
        let owned = tempfile::tempdir().unwrap();
        let destination = owned.path().join("published");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut workers = Vec::new();
        for name in ["a", "b"] {
            let source = owned.path().join(name);
            std::fs::create_dir(&source).unwrap();
            std::fs::write(source.join("identity"), name).unwrap();
            let destination = destination.clone();
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                (
                    name,
                    source.clone(),
                    rename(&source, &destination, false).is_ok(),
                )
            }));
        }
        let results: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|(_, _, success)| *success).count(), 1);
        for (name, source, success) in results {
            if success {
                assert_eq!(
                    std::fs::read_to_string(destination.join("identity")).unwrap(),
                    name
                );
                assert!(!source.exists());
            } else {
                assert_eq!(
                    std::fs::read_to_string(source.join("identity")).unwrap(),
                    name
                );
            }
        }
    }

    #[test]
    fn test_filesystem_identity_tracks_hardlinks_and_device() {
        let owned = tempfile::tempdir().unwrap();
        let original = owned.path().join("original");
        let alias = owned.path().join("alias");
        let separate = owned.path().join("separate");
        std::fs::write(&original, "payload").unwrap();
        std::fs::write(&separate, "payload").unwrap();
        std::fs::hard_link(&original, &alias).unwrap();
        let identity = filesystem_identity(&original).unwrap();
        assert_eq!(identity, filesystem_identity(&alias).unwrap());
        assert_ne!(identity, filesystem_identity(&separate).unwrap());
        assert_eq!(identity.0, filesystem_device(&original).unwrap());
        assert_eq!(identity.0, filesystem_device(owned.path()).unwrap());
        assert!(filesystem_identity(&owned.path().join("missing")).is_err());
    }

    #[test]
    fn test_filesystem_usage_checks_actual_directory() {
        let owned = tempfile::tempdir().unwrap();
        let (total, used, available) = filesystem_usage(owned.path()).unwrap();
        assert!(total > 0);
        assert!(used <= total);
        assert!(available <= total);
        #[cfg(windows)]
        assert!(filesystem_usage(owned.path().ancestors().last().unwrap()).is_ok());
        let file = owned.path().join("file");
        std::fs::write(&file, "payload").unwrap();
        assert_eq!(
            filesystem_usage(&file).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert!(filesystem_usage(&owned.path().join("missing")).is_err());
    }

    #[test]
    fn test_platform_map_order_backreferences_and_first_substitution() {
        let rules: HashMap<String, PlatformRules> = serde_json::from_str(
            r#"{"os":{"windows-10\\.0(\\.2.*)":"windows-11\\1","windows":"ignored"},"arch":{"(?i)^amd64$":"x86_64"}}"#,
        ).unwrap();
        let raw = SystemInfo {
            platform: Platform::Windows,
            arch: Arch {
                name: "AMD64".into(),
            },
            os: OsVersion {
                raw: "windows-10.0.26100".into(),
            },
        };
        let mapped = raw.mapped(&rules).unwrap();
        assert_eq!(mapped.arch.name(), "x86_64");
        assert_eq!(mapped.os.name(), "windows-11.26100");
        let serialized = serde_json::to_string(&rules["os"]).unwrap();
        assert!(serialized.find("windows-10").unwrap() < serialized.find("ignored").unwrap());
        // A replacement identical to its input still counts as a substitution in Rez.
        let rules = serde_json::from_str(r#"{"arch":{"AMD64":"AMD64",".*":"incorrect"}}"#).unwrap();
        assert_eq!(raw.mapped(&rules).unwrap().arch.name(), "AMD64");
    }

    #[test]
    fn test_platform_map_invalid_schema_and_regex_fail() {
        assert!(serde_json::from_str::<PlatformRules>(r#"{"a":12}"#).is_err());
        let rules = serde_json::from_str(r#"{"arch":{"[":"invalid"}}"#).unwrap();
        let error = RAW_SYSTEM.mapped(&rules).unwrap_err().to_string();
        assert!(error.contains("platform_map"), "{error}");
    }

    #[test]
    fn test_platform_map_safe_version_after_mapping() {
        let rules = serde_json::from_str(r#"{"arch":{"^.*$":"CPU Model"}}"#).unwrap();
        assert_eq!(RAW_SYSTEM.mapped(&rules).unwrap().arch.name(), "CPU_Model");
    }

    #[test]
    fn test_platform_current() {
        let p = Platform::current();
        // Should match compile target
        if cfg!(target_os = "windows") {
            assert_eq!(p, Platform::Windows);
            assert_eq!(p.name(), "windows");
        } else if cfg!(target_os = "macos") {
            assert_eq!(p, Platform::MacOS);
            assert_eq!(p.name(), "osx");
        } else {
            assert_eq!(p, Platform::Linux);
            assert_eq!(p.name(), "linux");
        }
    }

    #[test]
    fn test_safe_version_string_reference_boundaries() {
        // Expected values were executed from Rez System._make_safe_version_string.
        // Its initial trims are sequential, so mixed trailing separators can survive.
        let cases = [
            ("", ""),
            (".", ""),
            ("-", ""),
            (".-", ""),
            ("-.", ""),
            ("-.A.-", "A."),
            ("A..B", "A.B"),
            ("A.-B", "A.B"),
            ("A-.B", "A-B"),
            ("A.-.-", "A."),
            ("A---B", "A-B"),
            ("A...B", "A.B"),
            (".foo.", "foo"),
            ("Ubuntu 22.04", "Ubuntu_22.04"),
            ("windows-10.0", "windows-10.0"),
            ("hello world!", "hello_world_"),
            ("é.猫--a💡b", "_._-a_b"),
            ("...---", ""),
            ("_-._", "_-_"),
            (" A / B ", "_A___B_"),
            ("a\n\tb", "a__b"),
        ];
        for (input, expected) in cases {
            assert_eq!(make_safe_version_string(input), expected, "{input:?}");
        }
    }

    #[test]
    fn test_platform_map_precedes_system_sanitization() {
        let raw = SystemInfo {
            platform: Platform::Linux,
            arch: Arch {
                name: "CPU Model".into(),
            },
            os: OsVersion {
                raw: "Scientific Linux-7..9".into(),
            },
        };
        let rules = serde_json::from_str(
            r#"{"arch":{"^CPU Model$":"mapped arch"},"os":{"Scientific Linux-(.*)":"Scientific-\\1"}}"#,
        ).unwrap();
        let mapped = raw.mapped(&rules).unwrap();
        assert_eq!(mapped.arch.name(), "mapped_arch");
        assert_eq!(mapped.os.name(), "Scientific-7.9");
        let normalized = raw.mapped(&HashMap::new()).unwrap();
        assert_eq!(normalized.arch.name(), "CPU_Model");
        assert_eq!(normalized.os.name(), "Scientific_Linux-7.9");
    }

    #[test]
    fn test_arch_current() {
        let a = Arch::current();
        assert!(!a.name().is_empty());
    }

    #[test]
    fn test_os_version() {
        let os = OsVersion::current();
        assert!(!os.name().is_empty());
    }

    #[test]
    fn test_system_variant() {
        let var = SYSTEM.variant();
        assert_eq!(var.len(), 3);
        assert!(var[0].starts_with("platform-"));
        assert!(var[1].starts_with("arch-"));
        assert!(var[2].starts_with("os-"));
    }

    #[test]
    fn test_system_production_installation_identity() {
        use std::fs;
        let directory = tempfile::tempdir().unwrap();
        let primary = if cfg!(windows) { "rez.exe" } else { "rez" };
        let executable = directory.path().join(primary);
        fs::write(&executable, b"owned CLI payload").unwrap();
        assert!(SystemInfo::rez_bin_path(&executable).unwrap().is_none());

        let marker = directory.path().join(".rez_production_install");
        fs::write(&marker, b"").unwrap();
        assert_eq!(
            SystemInfo::rez_bin_path(&executable).unwrap(),
            Some(directory.path().canonicalize().unwrap())
        );
        fs::remove_file(marker).unwrap();

        let expected =
            crate::util::hash_reader_hex::<sha2::Sha256>(&mut fs::File::open(&executable).unwrap())
                .unwrap();
        let manifest = directory.path().join(".rez-rs-cli.json");
        fs::write(
            &manifest,
            serde_json::to_vec(&serde_json::json!({
                "schema": 1,
                "hashes": {primary: expected},
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            SystemInfo::rez_bin_path(&executable).unwrap(),
            Some(directory.path().canonicalize().unwrap())
        );
        fs::write(&executable, b"changed CLI payload").unwrap();
        assert!(SystemInfo::rez_bin_path(&executable)
            .unwrap_err()
            .to_string()
            .contains("hash does not match"));
    }

    #[test]
    fn test_system_invalid_installation_manifest_reports_error() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory
            .path()
            .join(if cfg!(windows) { "rez.exe" } else { "rez" });
        std::fs::write(&executable, b"CLI").unwrap();
        let manifest = directory.path().join(".rez-rs-cli.json");
        for invalid in [
            "{",
            r#"{"schema":2,"hashes":{}}"#,
            r#"{"schema":1,"hashes":{"rez.exe":3,"rez":3}}"#,
            r#"{"schema":1,"hashes":{"rez.exe":"wrong","rez":"wrong"}}"#,
        ] {
            std::fs::write(&manifest, invalid).unwrap();
            let error = SystemInfo::rez_bin_path(&executable)
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("Inspect Rez production directory"),
                "{error}"
            );
        }
    }

    #[test]
    fn test_system_rex_snapshot() {
        let data = SYSTEM.rex_data(&HashMap::new()).unwrap();
        assert_eq!(data["rez_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(data["variant"], serde_json::json!(SYSTEM.variant()));
        assert_eq!(data["shell"], SystemInfo::shell());
        assert_eq!(data["hostname"], SystemInfo::hostname());
        assert_eq!(
            data["selftest_is_running"],
            SystemInfo::selftest_is_running()
        );
        assert!(data["home"].is_string());
        assert!(data["paths"].is_array());
        assert!(data["environ"].is_object());
        if cfg!(windows) {
            assert_eq!(data["shell"], "powershell");
        }
    }

    #[test]
    fn test_safe_version_string() {
        assert_eq!(make_safe_version_string("Ubuntu 22.04"), "Ubuntu_22.04");
        assert_eq!(make_safe_version_string("windows-10.0"), "windows-10.0");
        assert_eq!(make_safe_version_string(".foo."), "foo");
        assert_eq!(make_safe_version_string("hello world!"), "hello_world_");
    }

    #[test]
    fn test_logical_cores() {
        assert!(logical_cores() >= 1);
    }

    #[test]
    fn test_physical_cores() {
        assert!(physical_cores() >= 1);
    }

    #[test]
    fn test_default_shell() {
        let shell = default_shell();
        assert!(!shell.is_empty());
    }

    #[test]
    fn test_user() {
        let user = SystemInfo::user();
        assert!(!user.is_empty());
    }

    #[test]
    fn test_detect_editor() {
        // Should return Some or None without panicking
        let _ = detect_editor();
    }

    #[test]
    fn test_detect_difftool() {
        // Should return Some or None without panicking
        let _ = detect_difftool();
    }

    #[test]
    fn test_detect_image_viewer() {
        // Should return Some or None without panicking
        let viewer = detect_image_viewer();
        // On macOS and Windows should always return Some
        if cfg!(target_os = "macos") {
            assert_eq!(viewer, Some("open".to_string()));
        } else if cfg!(target_os = "windows") {
            assert!(viewer.is_some());
        }
    }

    #[test]
    fn test_detect_terminal() {
        // Should return Some or None without panicking
        let term = detect_terminal();
        // macOS should always return Terminal.app
        if cfg!(target_os = "macos") {
            assert_eq!(term, Some("Terminal.app".to_string()));
        }
    }
}
