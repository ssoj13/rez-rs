//! Local package cache for fast variant payload access.
//!
//! Ported from Python rez package_cache.py.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::errors::{Result, RezError};

// ---------------------------------------------------------------------------
// Cache status
// ---------------------------------------------------------------------------

/// Status of a variant in the package cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheStatus {
    /// Variant found, payload at path.
    Found(PathBuf),
    /// Variant payload is still being copied.
    Copying(PathBuf),
    /// Variant copy has stalled (no mtime update for too long).
    CopyStalled(PathBuf),
    /// Variant is pending caching.
    Pending,
    /// Variant not in cache.
    NotFound,
    /// Variant was removed.
    Removed,
    /// Variant skipped due to cache size limit.
    Skipped,
}

// ---------------------------------------------------------------------------
// VariantHandle - uniquely identifies a variant for caching
// ---------------------------------------------------------------------------

/// Handle identifying a specific package variant for cache operations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VariantHandle {
    pub name: String,
    pub version: String,
    pub index: Option<usize>,
    pub repository: String,
    /// Exact filesystem source, including combined-definition extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<crate::provider::ResourceHandle>,
}

impl VariantHandle {
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        index: Option<usize>,
        repository: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            index,
            repository: repository.into(),
            resource: None,
        }
    }

    /// Derive cache identity from the same typed variant used by the resolver.
    pub fn from_variant(variant: &crate::provider::PackageVariant) -> Result<Self> {
        let package = &variant.variant.parent;
        crate::serialise::validate_rez_package_path(
            &package.name,
            (!package.version.is_empty())
                .then_some(package.version.to_string())
                .as_deref(),
        )?;
        match variant.variant.index {
            Some(index) if index >= package.variants.len() => {
                return Err(RezError::PackageCache(format!(
                    "Invalid variant index {index}"
                )));
            }
            None if package.has_variants() => {
                return Err(RezError::PackageCache(
                    "Indexed package requires a variant index".into(),
                ));
            }
            _ => {}
        }
        let provenance = variant
            .provenance
            .as_ref()
            .ok_or_else(|| RezError::PackageCache("Variant has no repository provenance".into()))?;
        if provenance.location.is_empty() {
            return Err(RezError::PackageCache(
                "Variant repository location is empty".into(),
            ));
        }
        let mut handle = Self::new(
            &package.name,
            package.version.to_string(),
            variant.variant.index,
            format!("{}@{}", provenance.repository_type, provenance.location),
        );
        handle.resource =
            provenance.resource_handle(&package.name, &package.version, variant.variant.index);
        if let Some(resource) = &handle.resource {
            resource.validate()?;
        }
        Ok(handle)
    }

    /// Serialise handle to JSON value for storage.
    pub fn to_dict(&self) -> serde_json::Value {
        let mut data = serde_json::json!({
            "name": self.name,
            "version": self.version,
            "index": self.index,
            "repository": self.repository,
        });
        if let Some(resource) = &self.resource {
            data["resource"] = serde_json::to_value(resource)
                .expect("resource handles contain only JSON-compatible fields");
        }
        data
    }

    /// Produce a hashable string representation (mirrors Python _hashable_repr).
    pub fn hashable_repr(&self) -> String {
        // Deterministic field order; optional exact source is part of identity.
        let dict = self.to_dict();
        serde_json::to_string(&dict).unwrap_or_default()
    }

    /// First four hexadecimal characters of the native stable identity hash.
    pub fn cache_key(&self) -> String {
        let h = crate::util::stable_hash(self.hashable_repr().as_bytes());
        format!("{:016x}", h)[..4].to_string()
    }

    /// Qualified name: "name-version"
    pub fn qualified_name(&self) -> String {
        if self.version.is_empty() {
            self.name.clone()
        } else {
            format!("{}-{}", self.name, self.version)
        }
    }
}

// ---------------------------------------------------------------------------
// CachedVariantInfo
// ---------------------------------------------------------------------------

/// Info about a cached variant on disk.
#[derive(Debug, Clone)]
pub struct CachedVariantInfo {
    pub handle: VariantHandle,
    pub cache_path: PathBuf,
    pub status: CacheStatus,
}

// ---------------------------------------------------------------------------
// base26 helpers
// ---------------------------------------------------------------------------

/// Get next base26 letter-ID: None->"a", "a"->"b", "z"->"aa", "az"->"ba".
pub fn next_base26(prev: Option<&str>) -> String {
    match prev {
        None => "a".to_string(),
        Some("") => "a".to_string(),
        Some(s) => {
            let bytes = s.as_bytes();
            let last = *bytes.last().unwrap();
            if last < b'z' {
                let mut result = s[..s.len() - 1].to_string();
                result.push((last + 1) as char);
                result
            } else {
                // Last char is 'z', carry over
                let prefix = &s[..s.len() - 1];
                let next_prefix = if prefix.is_empty() {
                    next_base26(None)
                } else {
                    next_base26(Some(prefix))
                };
                format!("{next_prefix}a")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Shared persistent OS lock
// ---------------------------------------------------------------------------

const LOCK_TIMEOUT: Duration = Duration::from_secs(10);
const COPYING_TIME_INC: Duration = Duration::from_millis(200);
const COPYING_TIME_MAX: Duration = Duration::from_secs(5);

use crate::util::FileLock as LockFile;

/// Refresh a copying marker so long, active copies are not mistaken for stalled work.
struct CopyingHeartbeat {
    stop: mpsc::Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl CopyingHeartbeat {
    fn start(path: &Path) -> io::Result<Self> {
        fs::OpenOptions::new()
            .write(true)
            .open(path)?
            .set_modified(SystemTime::now())?;

        let (stop, stopped) = mpsc::channel();
        let path = path.to_path_buf();
        let worker = std::thread::Builder::new()
            .name("rez-package-cache-heartbeat".into())
            .spawn(move || {
                while stopped.recv_timeout(COPYING_TIME_INC).is_err() {
                    let result = crate::util::open_file(&path, fs::OpenOptions::new().write(true))
                        .and_then(|file| file.set_modified(SystemTime::now()));
                    if result.is_err() {
                        break;
                    }
                }
            })?;

        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
}

impl Drop for CopyingHeartbeat {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

// ---------------------------------------------------------------------------
// PackageCache
// ---------------------------------------------------------------------------

/// Local package cache storing variant payload copies for fast access.
///
/// Avoids fetching package files over shared storage at runtime.
/// Not a package repository - only stores payload copies.
pub struct PackageCache {
    /// Cache root directory.
    pub path: PathBuf,
}

impl PackageCache {
    /// Create a new package cache at the given path. Creates internal dirs.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if !path.is_dir() {
            return Err(RezError::PackageCache(format!(
                "Not a directory: {}",
                path.display()
            )));
        }
        let cache = Self {
            path: path.canonicalize()?,
        };
        for relative in [
            ".sys",
            ".sys/log",
            ".sys/pending",
            ".sys/to_delete",
            ".sys/claims",
        ] {
            crate::util::directory(&cache.path, Path::new(relative), true)?;
        }
        Ok(cache)
    }

    pub fn initialize_worker(path: &Path, request: &Path) -> Result<()> {
        let cache = Self::new(path)?;
        let pending =
            crate::util::directory(&cache.path, std::path::Path::new(".sys/pending"), false)?;
        if request
            .parent()
            .map(fs::canonicalize)
            .transpose()?
            .as_deref()
            != Some(pending.as_path())
        {
            return Err(RezError::PackageCache(
                "Worker request must belong to the configured pending directory".into(),
            ));
        }
        let file = crate::util::open_file(request, std::fs::OpenOptions::new().read(true))?;
        let value: serde_json::Value = serde_json::from_reader(file)?;
        let handle = crate::provider::ResourceHandle::from_json(
            &value["handle"],
            Some("package cache request"),
        )?;
        use sha2::Digest;
        let identity = crate::util::hex_encode(sha2::Sha256::digest(serde_json::to_vec(&handle)?));
        if request.file_name().and_then(|name| name.to_str())
            != Some(format!("request-{identity}.json").as_str())
        {
            return Err(RezError::PackageCache(
                "Cache request filename does not match its exact handle".into(),
            ));
        }
        crate::config::initialize(serde_json::from_value(value["config"].clone())?)
    }

    /// Create cache from config (uses cache_packages_path).
    pub fn from_config() -> Result<Self> {
        let cfg = &*crate::config::CONFIG;
        let path = cfg
            .cache_packages_path
            .as_deref()
            .ok_or_else(|| RezError::PackageCache("cache_packages_path not set".into()))?;
        Self::new(crate::config::RezConfig::expand_path(path).to_os())
    }

    /// Schedule exact variants through the same synchronous worker or a detached native process.
    pub fn add_variants(
        &self,
        variants: &[crate::provider::PackageVariant],
        asynchronous: bool,
        config: Option<&crate::config::RezConfig>,
    ) -> Result<()> {
        let config = config.unwrap_or(&crate::config::CONFIG);
        crate::util::directory(&self.path, Path::new(".sys/pending"), false)?;
        let mut lock = LockFile::new(self.sys_dir().join(".lock"));
        lock.acquire(LOCK_TIMEOUT)?;
        for variant in variants {
            let package = &variant.variant.parent;
            let effective = package.config(Some(config))?;
            let provenance = variant.provenance.as_ref().ok_or_else(|| {
                RezError::PackageCache("Variant has no repository provenance".into())
            })?;
            if !package.is_cachable(Some(&provenance.location), Some(&effective)) {
                continue;
            }
            let handle = VariantHandle::from_variant(variant)?;
            let status = self.get_cached_root(&handle).0;
            if !matches!(status, CacheStatus::NotFound | CacheStatus::Pending)
                && (asynchronous
                    || !matches!(
                        status,
                        CacheStatus::Copying(_) | CacheStatus::CopyStalled(_)
                    ))
            {
                continue;
            }
            let resource = handle.resource.ok_or_else(|| {
                RezError::PackageCache(
                    "Asynchronous cache requests require an exact resource handle".into(),
                )
            })?;
            let path = self.request_path(&resource)?;
            if path.try_exists()? {
                continue;
            }
            let request = serde_json::json!({"handle": resource, "config": config});
            let mut pending = tempfile::NamedTempFile::new_in(self.pending_dir())?;
            serde_json::to_writer(pending.as_file_mut(), &request)?;
            pending.as_file().sync_all()?;
            match pending.persist_noclobber(path) {
                Ok(_) => {}
                Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.error.into()),
            }
        }
        lock.release()?;
        if asynchronous {
            let executable = std::env::current_exe()?;
            let Some(directory) = crate::platform::SystemInfo::rez_bin_path(&executable)? else {
                eprintln!(
                    "Automatic package caching requires a deployed Rez executable; requests remain pending"
                );
                return Ok(());
            };
            let primary = directory.join(if cfg!(windows) { "rez.exe" } else { "rez" });
            let mut command = Command::new(primary);
            command
                .arg("pkg-cache")
                .arg("--dir")
                .arg(&self.path)
                .arg("worker")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x08000000 | 0x00000200);
            }
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                command.process_group(0);
            }
            if let Err(error) = command.spawn() {
                eprintln!(
                    "Package caching daemon could not start; requests remain pending: {error}"
                );
            }
        } else {
            self.run_pending(true, None)?;
        }
        Ok(())
    }

    /// Drain durable requests. Per-request OS locks recover automatically if a worker dies.
    pub fn run_pending(&self, wait: bool, request: Option<&Path>) -> Result<usize> {
        let request = request
            .map(|request| -> Result<PathBuf> {
                let request = std::path::absolute(request)?;
                let parent = request.parent().ok_or_else(|| {
                    RezError::PackageCache("Worker request has no parent directory".into())
                })?;
                let parent = fs::canonicalize(parent)?;
                let pending = crate::util::directory(&self.path, Path::new(".sys/pending"), false)?;
                if parent != pending {
                    return Err(RezError::PackageCache(
                        "Worker request must belong to the configured pending directory".into(),
                    ));
                }
                let filename = request.file_name().ok_or_else(|| {
                    RezError::PackageCache("Worker request has no filename".into())
                })?;
                Ok(parent.join(filename))
            })
            .transpose()?;
        let provider = crate::provider::FilesystemPackageProvider::from_paths(&[])?;
        for relative in [".sys/pending", ".sys/claims", ".sys/log"] {
            crate::util::directory(&self.path, Path::new(relative), false)?;
        }
        let mut completed = 0;
        for entry in fs::read_dir(self.pending_dir())? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(identity) = name
                .strip_prefix("request-")
                .and_then(|name| name.strip_suffix(".json"))
            else {
                continue;
            };
            if identity.len() != 64 || !identity.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                continue;
            }
            if let Some(request) = &request {
                if request != &path {
                    continue;
                }
            } else if let Some(directory) =
                crate::platform::SystemInfo::rez_bin_path(&std::env::current_exe()?)?
            {
                let primary = directory.join(if cfg!(windows) { "rez.exe" } else { "rez" });
                let mut worker = Command::new(primary);
                worker
                    .arg("pkg-cache")
                    .arg("--dir")
                    .arg(&self.path)
                    .arg("worker")
                    .arg("--request")
                    .arg(&path);
                if wait {
                    worker.arg("--wait");
                }
                worker
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    worker.creation_flags(0x08000000);
                }
                let result = worker.status()?;
                if !result.success() && path.try_exists()? {
                    eprintln!("Package cache worker failed for {name}; request remains pending");
                }
                if !path.try_exists()? {
                    completed += 1;
                }
                continue;
            }
            if request.is_none() {
                let file = crate::util::open_file(&path, fs::OpenOptions::new().read(true))?;
                let value: serde_json::Value = serde_json::from_reader(file)?;
                let snapshot: crate::config::RezConfig =
                    serde_json::from_value(value["config"].clone())?;
                if serde_json::to_value(&snapshot)?
                    != serde_json::to_value(&*crate::config::CONFIG)?
                {
                    return Err(RezError::PackageCache("Request configuration requires an isolated deployed worker; request remains pending".into()));
                }
            }
            let mut claim = LockFile::new(
                self.sys_dir()
                    .join("claims")
                    .join(format!("request-{identity}.lock")),
            );
            let claimed = loop {
                match claim.acquire(Duration::ZERO) {
                    Ok(()) => break true,
                    Err(error) if error.kind() == io::ErrorKind::TimedOut && wait => {
                        if !path.try_exists()? {
                            break false;
                        }
                        std::thread::sleep(COPYING_TIME_INC);
                    }
                    Err(error) if error.kind() == io::ErrorKind::TimedOut => break false,
                    Err(error) => return Err(error.into()),
                }
            };
            if !claimed {
                continue;
            }
            if !path.try_exists()? {
                continue;
            }
            let operation = (|| -> Result<(CacheStatus, crate::config::RezConfig)> {
                let file = crate::util::open_file(&path, fs::OpenOptions::new().read(true))?;
                let request: serde_json::Value = serde_json::from_reader(file)?;
                let handle = crate::provider::ResourceHandle::from_json(
                    &request["handle"],
                    Some("package cache request"),
                )?;
                if self.request_path(&handle)? != path {
                    return Err(RezError::PackageCache(
                        "Cache request filename does not match its exact handle".into(),
                    ));
                }
                let config: crate::config::RezConfig =
                    serde_json::from_value(request["config"].clone())?;
                if serde_json::to_value(&config)? != serde_json::to_value(&*crate::config::CONFIG)?
                {
                    return Err(RezError::Config(
                        "Request configuration changed after worker startup; request remains pending".into(),
                    ));
                }
                let variant = provider
                    .get_candidate_for_handle(&handle)?
                    .into_variant(handle.variables.index, false)?;
                let status = loop {
                    let (_, status) = self.add_variant(&variant, false, Some(&config))?;
                    if wait && matches!(status, CacheStatus::Copying(_) | CacheStatus::Pending) {
                        std::thread::sleep(COPYING_TIME_INC);
                        continue;
                    }
                    if wait && matches!(status, CacheStatus::CopyStalled(_)) {
                        if matches!(
                            self.remove_variant(&VariantHandle::from_variant(&variant)?)?,
                            CacheStatus::Copying(_)
                        ) {
                            std::thread::sleep(COPYING_TIME_INC);
                        }
                        continue;
                    }
                    break status;
                };
                Ok((status, config))
            })();
            let mut message = match &operation {
                Ok((status, _)) => format!("{}: {status:?}\n", name),
                Err(error) => format!("{}: {error}\n", name),
            };
            message.push_str(&format!(
                "system: platform={} arch={} os={}\n",
                crate::platform::SYSTEM.platform,
                crate::platform::SYSTEM.arch,
                crate::platform::SYSTEM.os
            ));
            crate::util::directory(&self.path, Path::new(".sys/log"), false)?;
            crate::util::directory(&self.path, Path::new(".sys/pending"), false)?;
            let log_path = self.log_dir().join(format!("request-{identity}.log"));
            let mut log = crate::util::open_file(
                &log_path,
                fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(false),
            )?;
            log.set_len(0)?;
            log.write_all(message.as_bytes())?;
            match operation {
                Ok((CacheStatus::Copying(_) | CacheStatus::Pending, _)) => {}
                Ok((_, config)) => {
                    fs::remove_file(&path)?;
                    completed += 1;
                    if config.package_cache_clean_limit > 0.0 {
                        if let Err(error) = self.clean(
                            config.package_cache_max_variant_days,
                            Some(config.package_cache_clean_limit),
                            Some(&config),
                        ) {
                            writeln!(log, "Cache cleanup failed: {error}")?;
                        }
                    }
                }
                Err(RezError::Config(_)) => {}
                Err(_) => {
                    fs::remove_file(&path)?;
                    completed += 1;
                }
            }
        }
        if request.is_none() && crate::config::CONFIG.package_cache_clean_limit > 0.0 {
            if let Err(error) = self.clean(
                crate::config::CONFIG.package_cache_max_variant_days,
                Some(crate::config::CONFIG.package_cache_clean_limit),
                None,
            ) {
                eprintln!("Package cache cleanup failed: {error}");
            }
        }
        Ok(completed)
    }

    fn request_path(&self, handle: &crate::provider::ResourceHandle) -> Result<PathBuf> {
        use sha2::Digest;
        let digest = crate::util::hex_encode(sha2::Sha256::digest(serde_json::to_vec(handle)?));
        Ok(self.pending_dir().join(format!("request-{digest}.json")))
    }

    /// Shared cache admission check, including bounded inode-aware size estimation.
    fn has_space(
        &self,
        source: &Path,
        config: &crate::config::RezConfig,
        usage: Option<(u64, u64, u64)>,
    ) -> Result<bool> {
        let (total, used, available) = match usage {
            Some(usage) => usage,
            None => crate::platform::filesystem_usage(&self.path)?,
        };
        let percentage = if total == 0 {
            0.0
        } else {
            used as f64 / total as f64 * 100.0
        };
        if percentage <= f64::from(config.package_cache_used_threshold) {
            return Ok(true);
        }
        // fs::copy creates a distinct destination file for each logical entry.
        // Count aliases separately; identities guard directory ancestry, not file size.
        let mut ancestry = HashSet::new();
        let mut pending = vec![(source.to_path_buf(), false)];
        let mut size = 0u64;
        while let Some((path, leaving)) = pending.pop() {
            let metadata = fs::metadata(&path)?;
            if metadata.is_dir() {
                let identity = crate::platform::filesystem_identity(&path)?;
                if leaving {
                    ancestry.remove(&identity);
                    continue;
                }
                if !ancestry.insert(identity) {
                    return Err(RezError::PackageCache(format!(
                        "Payload directory cycle: {}",
                        path.display()
                    )));
                }
                pending.push((path.clone(), true));
                for entry in fs::read_dir(path)? {
                    pending.push((entry?.path(), false));
                }
            } else if metadata.is_file() {
                size = size.checked_add(metadata.len()).ok_or_else(|| {
                    RezError::PackageCache("Variant size exceeds integer range".into())
                })?;
            }
            if available.saturating_sub(size) < config.package_cache_space_buffer {
                return Ok(false);
            }
        }
        Ok(true)
    }

    // -- Internal dirs --

    fn sys_dir(&self) -> PathBuf {
        self.path.join(".sys")
    }

    fn log_dir(&self) -> PathBuf {
        self.path.join(".sys").join("log")
    }

    fn pending_dir(&self) -> PathBuf {
        self.path.join(".sys").join("pending")
    }

    fn remove_dir(&self) -> PathBuf {
        self.path.join(".sys").join("to_delete")
    }

    // -- Hash path for a variant --

    fn hash_path(&self, handle: &VariantHandle) -> PathBuf {
        let ver_dir = if handle.version.is_empty() {
            "_NO_VERSION"
        } else {
            &handle.version
        };
        self.path
            .join(&handle.name)
            .join(ver_dir)
            .join(handle.cache_key())
    }

    // -- Core operations --

    /// Check if variant is cached, return status and root path.
    pub fn get_cached_root(&self, handle: &VariantHandle) -> (CacheStatus, Option<PathBuf>) {
        if crate::serialise::validate_rez_package_path(
            &handle.name,
            (!handle.version.is_empty()).then_some(handle.version.as_str()),
        )
        .is_err()
        {
            return (CacheStatus::NotFound, None);
        }
        let hash_dir = self.hash_path(handle);
        if crate::util::directory(
            &self.path,
            hash_dir
                .strip_prefix(&self.path)
                .expect("cache hash is rooted"),
            false,
        )
        .is_err()
        {
            return (CacheStatus::NotFound, None);
        }

        let handle_dict = handle.to_dict();
        let entries = match fs::read_dir(&hash_dir) {
            Ok(e) => e,
            Err(_) => return (CacheStatus::NotFound, None),
        };

        for entry in entries.flatten() {
            let fname = entry.file_name();
            let fname_str = fname.to_string_lossy();
            if !fname_str.ends_with(".json") || fname_str.starts_with('.') {
                continue;
            }

            let slot = fname_str.trim_end_matches(".json").to_string();
            if slot.is_empty() || !slot.bytes().all(|byte| byte.is_ascii_lowercase()) {
                continue;
            }
            let json_path = entry.path();
            let root_path = hash_dir.join(&slot);
            let copying_path = hash_dir.join(format!(".copying-{slot}"));

            // Read and compare handle JSON
            let data: serde_json::Value =
                match crate::util::open_file(&json_path, fs::OpenOptions::new().read(true))
                    .ok()
                    .and_then(|file| serde_json::from_reader(file).ok())
                {
                    Some(v) => v,
                    None => continue,
                };

            if data.get("handle") != Some(&handle_dict) {
                continue;
            }

            // Found matching handle - check if still copying
            if copying_path.exists() {
                if let Ok(meta) = fs::metadata(&copying_path) {
                    if let Some(age) = meta.modified().ok().and_then(|m| m.elapsed().ok()) {
                        if age > COPYING_TIME_MAX {
                            return (CacheStatus::CopyStalled(root_path.clone()), Some(root_path));
                        }
                    }
                }
                return (CacheStatus::Copying(root_path.clone()), Some(root_path));
            }

            if crate::util::directory(
                &self.path,
                root_path
                    .strip_prefix(&self.path)
                    .expect("cache root is rooted"),
                false,
            )
            .is_ok()
            {
                return (CacheStatus::Found(root_path.clone()), Some(root_path));
            }
        }

        (CacheStatus::NotFound, None)
    }

    /// Copy a variant's payload into the cache.
    ///
    /// Returns the cached path and status.
    pub fn add_variant(
        &self,
        variant: &crate::provider::PackageVariant,
        force: bool,
        config: Option<&crate::config::RezConfig>,
    ) -> Result<(PathBuf, CacheStatus)> {
        let config = variant.variant.parent.config(config)?;
        let config = &config;
        let handle = VariantHandle::from_variant(variant)?;
        let source_root = variant
            .variant
            .root()
            .ok_or_else(|| RezError::PackageCache("Variant has no payload root".into()))?;
        let source_root = source_root.as_path();
        if !source_root.is_dir() {
            return Err(RezError::PackageCache(format!(
                "Variant root not found: {}",
                source_root.display()
            )));
        }

        if !force {
            let package = &variant.variant.parent;
            let provenance = variant.provenance.as_ref().expect("validated above");
            if !package.is_cachable(Some(&provenance.location), Some(config)) {
                return Err(RezError::PackageCache(format!(
                    "Package is not cachable: {}",
                    package.qualified_name()
                )));
            }
            let repo = crate::config::RezConfig::expand_path(&provenance.location).to_os();
            let repo = fs::canonicalize(repo)?;
            let local = config.expanded_local_packages_path().to_os();
            let is_local = match fs::canonicalize(local) {
                Ok(local) => local == repo,
                Err(error) if error.kind() == io::ErrorKind::NotFound => false,
                Err(error) => return Err(error.into()),
            };
            if !config.package_cache_local && is_local {
                return Err(RezError::PackageCache(format!(
                    "Package is local: {}",
                    package.qualified_name()
                )));
            }
            if !config.package_cache_same_device
                && crate::platform::filesystem_device(&self.path)?
                    == crate::platform::filesystem_device(source_root)?
            {
                return Err(RezError::PackageCache(format!(
                    "Variant is on the same device as cache: {}",
                    source_root.display()
                )));
            }
            let temp = config
                .tmpdir
                .as_deref()
                .map(|path| crate::config::RezConfig::expand_path(path).to_os())
                .unwrap_or_else(std::env::temp_dir);
            let temp = fs::canonicalize(temp)?;
            if provenance.repository_type == "filesystem" && repo.starts_with(&temp) && repo != temp
            {
                return Err(RezError::PackageCache(format!(
                    "Package is in a temporary repository: {}",
                    repo.display()
                )));
            }
        }
        let handle = &handle;
        // Check if already cached
        let (status, rootpath) = self.get_cached_root(handle);
        match &status {
            CacheStatus::Found(_) | CacheStatus::Copying(_) | CacheStatus::CopyStalled(_) => {
                return Ok((rootpath.unwrap_or_default(), status));
            }
            _ => {}
        }

        if !self.has_space(source_root, config, None)? {
            return Ok((PathBuf::new(), CacheStatus::Skipped));
        }

        let mut owner = self.variant_lock(handle)?;
        match owner.acquire(Duration::ZERO) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                return Ok((PathBuf::new(), CacheStatus::Pending));
            }
            Err(error) => return Err(error.into()),
        }
        // Hold payload ownership until marker removal, independently of its heartbeat.
        let hash_dir = self.hash_path(handle);
        crate::util::directory(
            &self.path,
            hash_dir
                .strip_prefix(&self.path)
                .expect("cache hash is rooted"),
            true,
        )?;

        // Build handle JSON data
        let data = serde_json::json!({ "handle": handle.to_dict() });

        // 2. Acquire lock
        let mut lock = LockFile::new(self.sys_dir().join(".lock"));
        lock.acquire(LOCK_TIMEOUT)?;

        // Re-check under lock
        let (status, rootpath) = self.get_cached_root(handle);
        if matches!(
            status,
            CacheStatus::Found(_) | CacheStatus::Copying(_) | CacheStatus::CopyStalled(_)
        ) {
            lock.release()?;
            return Ok((rootpath.unwrap_or_default(), status));
        }

        // Find next slot name
        let slot = self.next_slot(&hash_dir)?;

        // 3. Create .copying marker
        let copying_path = hash_dir.join(format!(".copying-{slot}"));
        crate::util::open_file(
            &copying_path,
            fs::OpenOptions::new().write(true).create_new(true),
        )?;

        // Publish complete metadata; a failed writer must not strand an occupied slot.
        let json_path = hash_dir.join(format!("{slot}.json"));
        let registration = (|| -> Result<()> {
            let mut metadata = tempfile::NamedTempFile::new_in(&hash_dir)?;
            serde_json::to_writer_pretty(metadata.as_file_mut(), &data)?;
            metadata.as_file().sync_all()?;
            metadata
                .persist_noclobber(&json_path)
                .map_err(|error| error.error)?;
            Ok(())
        })();
        if let Err(error) = registration {
            fs::remove_file(&copying_path)?;
            return Err(error);
        }

        // 5. Release lock
        lock.release()?;

        // 6. Copy payload
        let dest_path = crate::util::directory(
            &self.path,
            hash_dir
                .join(&slot)
                .strip_prefix(&self.path)
                .expect("cache slot is rooted"),
            true,
        )?;
        let copy_result = CopyingHeartbeat::start(&copying_path).and_then(|_heartbeat| {
            crate::package::ops::copy_dir_contents(
                source_root,
                &dest_path,
                true,
                true,
                Some(&self.path),
                None,
            )
            .map(|_| ())
            .map_err(|error| io::Error::other(error.to_string()))
        });

        if let Err(e) = copy_result {
            // Cleanup on failure
            if crate::util::directory(
                &self.path,
                hash_dir
                    .strip_prefix(&self.path)
                    .expect("cache hash is rooted"),
                false,
            )
            .is_ok()
            {
                let _ = fs::remove_file(&copying_path);
                let _ = fs::remove_file(&json_path);
                if crate::util::directory(
                    &self.path,
                    dest_path
                        .strip_prefix(&self.path)
                        .expect("cache slot is rooted"),
                    false,
                )
                .is_ok()
                {
                    let _ = fs::remove_dir_all(&dest_path);
                }
            }
            return Err(RezError::PackageCache(format!(
                "Failed to copy variant payload: {e}"
            )));
        }

        // 7. Remove .copying marker
        let _ = fs::remove_file(&copying_path);

        let status = CacheStatus::Found(dest_path.clone());
        Ok((dest_path, status))
    }

    /// Remove a variant from the cache (moves payload to to_delete dir).
    pub fn remove_variant(&self, handle: &VariantHandle) -> Result<CacheStatus> {
        crate::serialise::validate_rez_package_path(
            &handle.name,
            (!handle.version.is_empty()).then_some(handle.version.as_str()),
        )?;
        let (status, _) = self.get_cached_root(handle);
        match &status {
            CacheStatus::NotFound | CacheStatus::Copying(_) => return Ok(status),
            _ => {}
        }

        let mut owner = self.variant_lock(handle)?;
        match owner.acquire(Duration::ZERO) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                return Ok(CacheStatus::Copying(self.hash_path(handle)));
            }
            Err(error) => return Err(error.into()),
        }
        crate::util::directory(&self.path, Path::new(".sys/to_delete"), false)?;
        let mut lock = LockFile::new(self.sys_dir().join(".lock"));
        lock.acquire(LOCK_TIMEOUT)?;

        // Re-check under the cache lock: the copy marker may have been refreshed
        // after the initial status read.
        let (status, rootpath) = self.get_cached_root(handle);
        match status {
            CacheStatus::NotFound | CacheStatus::Copying(_) => return Ok(status),
            _ => {}
        }
        let rootpath = match rootpath {
            Some(path) => path,
            None => return Ok(CacheStatus::NotFound),
        };

        // Move payload to remove dir
        let dest_name = format!(
            "{}-{:x}",
            handle.qualified_name(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        );
        let dest_path = self.remove_dir().join(&dest_name);

        if rootpath.exists() {
            crate::util::directory(
                &self.path,
                rootpath
                    .strip_prefix(&self.path)
                    .expect("cache slot is rooted"),
                false,
            )?;
            crate::platform::rename(&rootpath, &dest_path, false)
                .map_err(|e| RezError::PackageCache(format!("Failed to move variant: {e}")))?;
        }

        // Delete json file
        let hash_dir = rootpath.parent().unwrap_or(&rootpath);
        let slot = rootpath
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let json_path = hash_dir.join(format!("{slot}.json"));
        let _ = fs::remove_file(&json_path);

        // Delete .copying file
        let copying_path = hash_dir.join(format!(".copying-{slot}"));
        let _ = fs::remove_file(&copying_path);

        // Clean up empty parent dirs (hash -> version -> name)
        let mut dir = hash_dir.to_path_buf();
        for _ in 0..3 {
            if fs::read_dir(&dir).map_or(true, |mut d| d.next().is_none()) {
                let _ = fs::remove_dir(&dir);
            } else {
                break;
            }
            dir = match dir.parent() {
                Some(p) => p.to_path_buf(),
                None => break,
            };
        }

        lock.release()?;
        Ok(CacheStatus::Removed)
    }

    /// Clean the cache: remove old/stalled variants and pending deletions.
    ///
    /// Returns count of deleted items.
    pub fn clean(
        &self,
        max_age_days: u32,
        time_limit: Option<f64>,
        config: Option<&crate::config::RezConfig>,
    ) -> Result<u32> {
        let started = Instant::now();
        let config = config.unwrap_or(&crate::config::CONFIG);
        let mut removed = 0u32;
        let max_age = Duration::from_secs(max_age_days as u64 * 86400);

        // Collect variants to remove
        let variants = self.get_variants()?;
        for info in &variants {
            if time_limit.is_some_and(|limit| started.elapsed().as_secs_f64() >= limit) {
                break;
            }
            match &info.status {
                CacheStatus::Found(path) => {
                    if max_age_days == 0 {
                        continue; // 0 = no age limit
                    }
                    // Check json mtime for last-used time
                    let json_path = PathBuf::from(format!("{}.json", path.display()));
                    if let Ok(meta) = fs::metadata(&json_path) {
                        if let Some(age) = meta.modified().ok().and_then(|m| m.elapsed().ok()) {
                            if age > max_age {
                                if let Ok(CacheStatus::Removed) = self.remove_variant(&info.handle)
                                {
                                    removed += 1;
                                }
                            }
                        }
                    }
                }
                CacheStatus::CopyStalled(_) => {
                    if let Ok(CacheStatus::Removed) = self.remove_variant(&info.handle) {
                        removed += 1;
                    }
                }
                _ => {}
            }
        }

        // Never traverse a redirected generated directory during cleanup.
        let remove_dir = crate::util::directory(&self.path, Path::new(".sys/to_delete"), false)?;
        if let Ok(entries) = fs::read_dir(remove_dir) {
            for entry in entries.flatten() {
                if time_limit.is_some_and(|limit| started.elapsed().as_secs_f64() >= limit) {
                    break;
                }
                let path = entry.path();
                if entry.file_type()?.is_dir() {
                    crate::util::directory(
                        &self.path,
                        path.strip_prefix(&self.path)
                            .expect("cache removal is rooted"),
                        false,
                    )?;
                    if fs::remove_dir_all(&path).is_ok() {
                        removed += 1;
                    }
                } else if fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            }
        }

        let log_dir = crate::util::directory(&self.path, Path::new(".sys/log"), false)?;
        for entry in fs::read_dir(log_dir)? {
            if time_limit.is_some_and(|limit| started.elapsed().as_secs_f64() >= limit) {
                break;
            }
            let entry = entry?;
            if entry.file_type()?.is_file()
                && entry.file_name().to_string_lossy().starts_with("request-")
            {
                let age = entry.metadata()?.modified()?.elapsed().unwrap_or_default();
                if age > Duration::from_secs(u64::from(config.package_cache_log_days) * 86400) {
                    fs::remove_file(entry.path())?;
                }
            }
        }
        Ok(removed)
    }

    /// List all cached variants with their statuses.
    pub fn get_variants(&self) -> Result<Vec<CachedVariantInfo>> {
        let mut results = Vec::new();

        let entries = match fs::read_dir(&self.path) {
            Ok(e) => e,
            Err(_) => return Ok(results),
        };

        for pkg_entry in entries.flatten() {
            let pkg_name = pkg_entry.file_name();
            let pkg_str = pkg_name.to_string_lossy();
            if pkg_str.starts_with('.') {
                continue; // skip .sys etc
            }
            if !pkg_entry.file_type()?.is_dir() {
                continue;
            }
            crate::util::directory(&self.path, Path::new(pkg_name.as_os_str()), false)?;

            let ver_entries = match fs::read_dir(pkg_entry.path()) {
                Ok(e) => e,
                Err(_) => continue,
            };

            for ver_entry in ver_entries.flatten() {
                if !ver_entry.file_type()?.is_dir() {
                    continue;
                }
                crate::util::directory(
                    &self.path,
                    ver_entry
                        .path()
                        .strip_prefix(&self.path)
                        .expect("cache version is rooted"),
                    false,
                )?;

                let hash_entries = match fs::read_dir(ver_entry.path()) {
                    Ok(e) => e,
                    Err(_) => continue,
                };

                for hash_entry in hash_entries.flatten() {
                    if !hash_entry.file_type()?.is_dir() {
                        continue;
                    }
                    crate::util::directory(
                        &self.path,
                        hash_entry
                            .path()
                            .strip_prefix(&self.path)
                            .expect("cache hash is rooted"),
                        false,
                    )?;

                    let slot_entries = match fs::read_dir(hash_entry.path()) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };

                    for slot_entry in slot_entries.flatten() {
                        let name = slot_entry.file_name();
                        let name_str = name.to_string_lossy();
                        if !name_str.ends_with(".json") || name_str.starts_with('.') {
                            continue;
                        }

                        let json_path = slot_entry.path();
                        let data: serde_json::Value = match crate::util::open_file(
                            &json_path,
                            fs::OpenOptions::new().read(true),
                        )
                        .ok()
                        .and_then(|file| serde_json::from_reader(file).ok())
                        {
                            Some(v) => v,
                            None => continue,
                        };

                        let handle: VariantHandle = match data
                            .get("handle")
                            .and_then(|h| serde_json::from_value(h.clone()).ok())
                        {
                            Some(h) => h,
                            None => continue,
                        };

                        let (status, cache_path) = self.get_cached_root(&handle);
                        results.push(CachedVariantInfo {
                            handle,
                            cache_path: cache_path.unwrap_or_default(),
                            status,
                        });
                    }
                }
            }
        }

        Ok(results)
    }

    /// Touch the JSON file to update last-used time, return root if found.
    pub fn touch_cached(&self, handle: &VariantHandle) -> Result<Option<PathBuf>> {
        let (status, rootpath) = self.get_cached_root(handle);
        if !matches!(status, CacheStatus::Found(_)) {
            return Ok(None);
        }
        let Some(rootpath) = rootpath else {
            return Ok(None);
        };
        let json_path = PathBuf::from(format!("{}.json", rootpath.display()));
        match crate::util::open_file(&json_path, fs::OpenOptions::new().write(true))
            .and_then(|file| file.set_modified(SystemTime::now()))
        {
            Ok(()) => Ok(Some(rootpath)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn variant_lock(&self, handle: &VariantHandle) -> Result<LockFile> {
        use sha2::Digest;
        let directory = crate::util::directory(&self.path, Path::new(".sys/claims"), false)?;
        let identity =
            crate::util::hex_encode(sha2::Sha256::digest(handle.hashable_repr().as_bytes()));
        Ok(LockFile::new(
            directory.join(format!("variant-{identity}.lock")),
        ))
    }

    // -- Internal helpers --

    /// Find next available slot name in hash dir.
    fn next_slot(&self, hash_dir: &Path) -> Result<String> {
        let entries = match fs::read_dir(hash_dir) {
            Ok(e) => e,
            Err(_) => return Ok("a".into()),
        };

        let mut max_slot: Option<String> = None;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.ends_with(".json") && !name_str.starts_with('.') {
                let slot = name_str.trim_end_matches(".json").to_string();
                if slot.chars().all(|c| c.is_ascii_lowercase()) {
                    max_slot = Some(match max_slot {
                        Some(prev) if slot > prev => slot,
                        Some(prev) => prev,
                        None => slot,
                    });
                }
            }
        }

        Ok(next_base26(max_slot.as_deref()))
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Create a temp dir for tests.
    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("rez_rs_tests")
            .join("package_cache")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn test_handle() -> VariantHandle {
        VariantHandle::new("foo", "1.2.3", Some(0), "filesystem@/packages")
    }

    fn test_variant(source: &Path, handle: &VariantHandle) -> crate::provider::PackageVariant {
        use crate::provider::{PackageProvenance, PackageVariant};
        use model::package::{Package, Variant};
        let mut package = Package::new(
            &handle.name,
            version::Version::new(&handle.version).unwrap(),
        );
        if let Some(index) = handle.index {
            package.variants = vec![vec![]; index + 1];
        }
        let mut variant = Variant::from_package(package);
        variant.index = handle.index;
        variant.root = Some(source.to_owned());
        let (repository_type, location) = handle.repository.split_once('@').unwrap();
        PackageVariant::new(
            variant,
            false,
            Some(PackageProvenance {
                repository_type: repository_type.into(),
                location: location.into(),
                source: None,
            }),
        )
    }

    fn write_copying_entry(cache: &PackageCache, handle: &VariantHandle) -> (PathBuf, PathBuf) {
        let hash_dir = cache.hash_path(handle);
        fs::create_dir_all(&hash_dir).unwrap();
        let marker = hash_dir.join(".copying-a");
        let metadata = hash_dir.join("a.json");
        fs::write(&marker, "").unwrap();
        fs::write(
            &metadata,
            serde_json::to_vec(&serde_json::json!({ "handle": handle.to_dict() })).unwrap(),
        )
        .unwrap();
        (marker, hash_dir)
    }

    #[test]
    fn active_payload_owner_prevents_stalled_cleanup() {
        let owned = tempfile::tempdir().unwrap();
        let cache = PackageCache::new(owned.path()).unwrap();
        let handle = test_handle();
        let (marker, hash) = write_copying_entry(&cache, &handle);
        fs::OpenOptions::new()
            .write(true)
            .open(&marker)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(20))
            .unwrap();
        fs::create_dir(hash.join("a")).unwrap();
        fs::write(hash.join("a/payload"), "active").unwrap();
        let mut owner = cache.variant_lock(&handle).unwrap();
        owner.acquire(Duration::ZERO).unwrap();
        assert!(matches!(
            cache.remove_variant(&handle).unwrap(),
            CacheStatus::Copying(_)
        ));
        assert_eq!(
            fs::read_to_string(hash.join("a/payload")).unwrap(),
            "active"
        );
        owner.release().unwrap();
        assert_eq!(cache.remove_variant(&handle).unwrap(), CacheStatus::Removed);
    }

    #[test]
    fn admission_counts_hardlink_copies_and_preserves_buffer() {
        let owned = tempfile::tempdir().unwrap();
        let source = owned.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("first"), vec![0u8; 512]).unwrap();
        fs::hard_link(source.join("first"), source.join("second")).unwrap();
        let cache = PackageCache::new(owned.path()).unwrap();
        let config = crate::config::RezConfig {
            package_cache_used_threshold: 0,
            package_cache_space_buffer: 100,
            ..Default::default()
        };
        assert!(!cache
            .has_space(&source, &config, Some((2000, 1000, 1000)))
            .unwrap());
        fs::remove_file(source.join("second")).unwrap();
        assert!(cache
            .has_space(&source, &config, Some((2000, 1000, 1000)))
            .unwrap());
        assert!(!cache
            .has_space(&source, &config, Some((2000, 1000, 600)))
            .unwrap());
    }

    #[test]
    fn generated_cache_directory_redirect_never_mutates_foreign_files() {
        let owned = tempfile::tempdir().unwrap();
        let root = owned.path().join("cache");
        let foreign = owned.path().join("foreign");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&foreign).unwrap();
        fs::write(foreign.join("sentinel"), "preserved").unwrap();
        #[cfg(windows)]
        {
            let shell = std::env::var_os("COMSPEC").expect("Windows command processor");
            assert!(Command::new(shell)
                .args(["/D", "/C", "mklink", "/J"])
                .arg(root.join(".sys"))
                .arg(&foreign)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success());
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&foreign, root.join(".sys")).unwrap();
        assert!(PackageCache::new(&root).is_err());
        assert_eq!(
            fs::read_to_string(foreign.join("sentinel")).unwrap(),
            "preserved"
        );
        assert_eq!(fs::read_dir(&foreign).unwrap().count(), 1);
    }

    #[test]
    fn test_cache_policy_preflight_and_force() {
        let owned = tempfile::tempdir().unwrap();
        let repo = owned.path().join("repo");
        let source = repo.join("foo/1.2.3");
        let cache_path = owned.path().join("cache");
        let temp = owned.path().join("scratch");
        for path in [&source, &cache_path, &temp] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(source.join("payload"), "cached").unwrap();
        let cache = PackageCache::new(&cache_path).unwrap();
        let handle = VariantHandle::new(
            "foo",
            "1.2.3",
            Some(0),
            format!("filesystem@{}", repo.display()),
        );
        let mut variant = test_variant(&source, &handle);
        std::rc::Rc::make_mut(&mut variant.variant.parent).cachable = Some(true);
        let mut config = crate::config::RezConfig {
            package_cache_local: true,
            package_cache_same_device: true,
            tmpdir: Some(temp.to_string_lossy().into_owned()),
            local_packages_path: repo.to_string_lossy().into_owned(),
            ..Default::default()
        };
        for reason in ["cachable", "local", "device", "temporary"] {
            let mut rejected = config.clone();
            let mut candidate = variant.clone();
            match reason {
                "cachable" => {
                    std::rc::Rc::make_mut(&mut candidate.variant.parent).cachable = Some(false)
                }
                "local" => rejected.package_cache_local = false,
                "device" => rejected.package_cache_same_device = false,
                "temporary" => rejected.tmpdir = Some(owned.path().to_string_lossy().into_owned()),
                _ => unreachable!(),
            }
            assert!(
                cache
                    .add_variant(&candidate, false, Some(&rejected))
                    .is_err(),
                "{reason}"
            );
            assert!(
                fs::read_dir(&cache_path)
                    .unwrap()
                    .all(|entry| { entry.unwrap().file_name() == ".sys" }),
                "policy must reject before payload mutation: {reason}"
            );
        }
        config.package_cache_local = false;
        let (cached, status) = cache.add_variant(&variant, true, Some(&config)).unwrap();
        assert!(matches!(status, CacheStatus::Found(_)));
        assert_eq!(
            fs::read_to_string(cached.join("payload")).unwrap(),
            "cached"
        );
        // Force bypasses policy, not missing payload or invalid provenance.
        variant.variant.root = Some(source.join("missing"));
        assert!(cache.add_variant(&variant, true, Some(&config)).is_err());
        variant.variant.root = Some(source.clone());
        variant.provenance = None;
        assert!(cache.add_variant(&variant, true, Some(&config)).is_err());
        assert_eq!(
            crate::platform::filesystem_device(&source).unwrap(),
            crate::platform::filesystem_device(&cache_path).unwrap()
        );
        assert!(crate::platform::filesystem_device(&source.join("missing")).is_err());
    }

    #[test]
    fn test_cache_missing_payload_is_not_a_hit() {
        let owned = tempfile::tempdir().unwrap();
        let source = owned.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("payload"), "recover").unwrap();
        let cache = PackageCache::new(owned.path()).unwrap();
        let handle = test_handle();
        let variant = test_variant(&source, &handle);
        let (cached, _) = cache.add_variant(&variant, true, None).unwrap();
        fs::remove_dir_all(&cached).unwrap();
        assert_eq!(cache.get_cached_root(&handle).0, CacheStatus::NotFound);
        let (refreshed, _) = cache.add_variant(&variant, true, None).unwrap();
        assert_eq!(
            fs::read_to_string(refreshed.join("payload")).unwrap(),
            "recover"
        );
    }

    #[test]
    fn test_cache_exact_combined_sources_have_distinct_identity() {
        use crate::repository::{PackageSource, PackageSourceKind};
        let owned = tempfile::tempdir().unwrap();
        let source = owned.path().join("source");
        let path = owned.path().join("cache");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&path).unwrap();
        fs::write(source.join("payload"), "source").unwrap();
        let cache = PackageCache::new(path).unwrap();
        let legacy = test_handle();
        let mut python = test_variant(&source, &legacy);
        python.provenance.as_mut().unwrap().source = Some(PackageSource {
            kind: PackageSourceKind::Python,
            path: PathBuf::from("/packages/foo.py"),
        });
        let mut yaml = python.clone();
        yaml.provenance.as_mut().unwrap().source = Some(PackageSource {
            kind: PackageSourceKind::Yaml,
            path: PathBuf::from("/packages/foo.yaml"),
        });
        let py_handle = VariantHandle::from_variant(&python).unwrap();
        let yaml_handle = VariantHandle::from_variant(&yaml).unwrap();
        assert_ne!(py_handle.to_dict(), yaml_handle.to_dict());
        let (py_root, _) = cache.add_variant(&python, true, None).unwrap();
        let (yaml_root, _) = cache.add_variant(&yaml, true, None).unwrap();
        assert_ne!(py_root, yaml_root);
        assert!(matches!(
            cache.get_cached_root(&py_handle).0,
            CacheStatus::Found(_)
        ));
        assert!(matches!(
            cache.get_cached_root(&yaml_handle).0,
            CacheStatus::Found(_)
        ));
        assert_eq!(cache.get_variants().unwrap().len(), 2);
    }

    #[test]
    fn test_cache_rejects_unsafe_handles_and_touch_updates_mtime() {
        let owned = tempfile::tempdir().unwrap();
        let cache = PackageCache::new(owned.path()).unwrap();
        let invalid = VariantHandle::new("../outside", "1", None, "filesystem@repo");
        assert_eq!(cache.get_cached_root(&invalid).0, CacheStatus::NotFound);
        assert!(cache.remove_variant(&invalid).is_err());
        let source = owned.path().join("source");
        fs::create_dir_all(&source).unwrap();
        let handle = test_handle();
        let (root, _) = cache
            .add_variant(&test_variant(&source, &handle), true, None)
            .unwrap();
        let metadata = PathBuf::from(format!("{}.json", root.display()));
        let past = SystemTime::now() - Duration::from_secs(600);
        fs::OpenOptions::new()
            .write(true)
            .open(&metadata)
            .unwrap()
            .set_modified(past)
            .unwrap();
        assert_eq!(cache.touch_cached(&handle).unwrap(), Some(root));
        assert!(fs::metadata(metadata).unwrap().modified().unwrap() > past);
    }

    // -- base26 tests --

    #[test]
    fn test_base26_first() {
        assert_eq!(next_base26(None), "a");
        assert_eq!(next_base26(Some("")), "a");
    }

    #[test]
    fn test_base26_increment() {
        assert_eq!(next_base26(Some("a")), "b");
        assert_eq!(next_base26(Some("b")), "c");
        assert_eq!(next_base26(Some("y")), "z");
    }

    #[test]
    fn test_base26_carry() {
        assert_eq!(next_base26(Some("z")), "aa");
        assert_eq!(next_base26(Some("az")), "ba");
        assert_eq!(next_base26(Some("zz")), "aaa");
    }

    #[test]
    fn test_base26_multi() {
        assert_eq!(next_base26(Some("aa")), "ab");
        assert_eq!(next_base26(Some("ab")), "ac");
    }

    // -- VariantHandle tests --

    #[test]
    fn test_handle_to_dict() {
        let h = test_handle();
        let d = h.to_dict();
        assert_eq!(d["name"], "foo");
        assert_eq!(d["version"], "1.2.3");
        assert_eq!(d["index"], 0);
        assert_eq!(d["repository"], "filesystem@/packages");
    }

    #[test]
    fn test_handle_cache_key() {
        let h = test_handle();
        let key = h.cache_key();
        assert_eq!(key.len(), 4);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_handle_cache_key_deterministic() {
        let h1 = test_handle();
        let h2 = test_handle();
        assert_eq!(h1.cache_key(), h2.cache_key());
    }

    #[test]
    fn test_handle_cache_key_differs() {
        let h1 = test_handle();
        let h2 = VariantHandle::new("bar", "1.2.3", Some(0), "filesystem@/packages");
        assert_ne!(h1.cache_key(), h2.cache_key());
    }

    #[test]
    fn test_handle_qualified_name() {
        let h = test_handle();
        assert_eq!(h.qualified_name(), "foo-1.2.3");

        let h2 = VariantHandle::new("bar", "", None, "mem");
        assert_eq!(h2.qualified_name(), "bar");
    }

    // -- PackageCache tests --

    #[test]
    fn test_cache_new() {
        let dir = test_dir("new");
        let cache = PackageCache::new(&dir).unwrap();
        assert!(cache.log_dir().is_dir());
        assert!(cache.pending_dir().is_dir());
        assert!(cache.remove_dir().is_dir());
    }

    #[test]
    fn test_cache_new_nonexistent() {
        let result = PackageCache::new("/nonexistent/path/rez_cache_test_xyz");
        assert!(result.is_err());
    }

    #[test]
    fn test_cache_not_found() {
        let dir = test_dir("not_found");
        let cache = PackageCache::new(&dir).unwrap();
        let h = test_handle();
        let (status, _) = cache.get_cached_root(&h);
        assert_eq!(status, CacheStatus::NotFound);
    }

    #[test]
    fn test_copying_heartbeat_keeps_active_copy_fresh() {
        let dir = test_dir("copying_heartbeat");
        let cache = PackageCache::new(&dir).unwrap();
        let handle = test_handle();
        let (marker, _) = write_copying_entry(&cache, &handle);

        fs::OpenOptions::new()
            .write(true)
            .open(&marker)
            .unwrap()
            .set_modified(SystemTime::now() - COPYING_TIME_MAX - Duration::from_secs(1))
            .unwrap();
        assert!(matches!(
            cache.get_cached_root(&handle).0,
            CacheStatus::CopyStalled(_)
        ));

        let heartbeat = CopyingHeartbeat::start(&marker).unwrap();
        assert!(matches!(
            cache.get_cached_root(&handle).0,
            CacheStatus::Copying(_)
        ));
        std::thread::sleep(COPYING_TIME_INC + Duration::from_millis(50));
        assert!(matches!(
            cache.get_cached_root(&handle).0,
            CacheStatus::Copying(_)
        ));

        drop(heartbeat);
        fs::OpenOptions::new()
            .write(true)
            .open(&marker)
            .unwrap()
            .set_modified(SystemTime::now() - COPYING_TIME_MAX - Duration::from_secs(1))
            .unwrap();
        assert!(matches!(
            cache.get_cached_root(&handle).0,
            CacheStatus::CopyStalled(_)
        ));
    }

    #[test]
    fn test_add_variant_does_not_duplicate_active_copy() {
        let dir = test_dir("add_while_copying");
        let cache = PackageCache::new(&dir).unwrap();
        let source = dir.join("source");
        fs::create_dir_all(&source).unwrap();
        let handle = test_handle();
        let (marker, hash_dir) = write_copying_entry(&cache, &handle);
        let _heartbeat = CopyingHeartbeat::start(&marker).unwrap();

        let (path, status) = cache
            .add_variant(&test_variant(&source, &handle), true, None)
            .unwrap();
        assert!(matches!(status, CacheStatus::Copying(_)));
        assert_eq!(path, hash_dir.join("a"));
        let metadata_entries = fs::read_dir(&hash_dir)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".json"))
            .count();
        assert_eq!(metadata_entries, 1);
    }

    #[test]
    fn test_remove_variant_preserves_active_copy() {
        let dir = test_dir("remove_while_copying");
        let cache = PackageCache::new(&dir).unwrap();
        let handle = test_handle();
        let (marker, hash_dir) = write_copying_entry(&cache, &handle);
        let payload = hash_dir.join("a");
        fs::create_dir_all(&payload).unwrap();
        fs::write(payload.join("partial.bin"), "partial").unwrap();
        let _heartbeat = CopyingHeartbeat::start(&marker).unwrap();

        let status = cache.remove_variant(&handle).unwrap();
        assert!(matches!(status, CacheStatus::Copying(_)));
        assert_eq!(
            fs::read_to_string(payload.join("partial.bin")).unwrap(),
            "partial"
        );
        assert!(marker.exists());
        assert!(hash_dir.join("a.json").exists());
    }
    #[test]
    fn test_cache_add_variant() {
        let dir = test_dir("add_variant");
        let cache = PackageCache::new(&dir).unwrap();

        // Create a source payload
        let src = dir.join("_source");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("package.py"), "name='foo'").unwrap();
        fs::write(src.join("data.txt"), "hello world").unwrap();

        let h = test_handle();
        let (path, status) = cache
            .add_variant(&test_variant(&src, &h), true, None)
            .unwrap();
        assert!(matches!(status, CacheStatus::Found(_)));
        assert!(path.is_dir());
        assert!(path.join("package.py").is_file());
        assert!(path.join("data.txt").is_file());

        // Verify content
        let content = fs::read_to_string(path.join("data.txt")).unwrap();
        assert_eq!(content, "hello world");
    }

    #[test]
    fn test_cache_add_variant_idempotent() {
        let dir = test_dir("add_idempotent");
        let cache = PackageCache::new(&dir).unwrap();

        let src = dir.join("_source");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("file.txt"), "data").unwrap();

        let h = test_handle();

        let (path1, _) = cache
            .add_variant(&test_variant(&src, &h), true, None)
            .unwrap();
        let (path2, status2) = cache
            .add_variant(&test_variant(&src, &h), true, None)
            .unwrap();

        assert_eq!(path1, path2);
        assert!(matches!(status2, CacheStatus::Found(_)));
    }

    #[test]
    fn test_cache_get_cached_root() {
        let dir = test_dir("get_cached");
        let cache = PackageCache::new(&dir).unwrap();

        let src = dir.join("_source");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("file.txt"), "ok").unwrap();

        let h = test_handle();
        cache
            .add_variant(&test_variant(&src, &h), true, None)
            .unwrap();

        let (status, root) = cache.get_cached_root(&h);
        assert!(matches!(status, CacheStatus::Found(_)));
        assert!(root.is_some());
        assert!(root.unwrap().is_dir());
    }

    #[test]
    fn test_cache_remove_variant() {
        let dir = test_dir("remove");
        let cache = PackageCache::new(&dir).unwrap();

        let src = dir.join("_source");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("file.txt"), "bye").unwrap();

        let h = test_handle();
        cache
            .add_variant(&test_variant(&src, &h), true, None)
            .unwrap();

        let status = cache.remove_variant(&h).unwrap();
        assert_eq!(status, CacheStatus::Removed);

        // Should now be not found
        let (status2, _) = cache.get_cached_root(&h);
        assert_eq!(status2, CacheStatus::NotFound);
    }

    #[test]
    fn test_cache_remove_not_found() {
        let dir = test_dir("remove_nf");
        let cache = PackageCache::new(&dir).unwrap();
        let h = test_handle();
        let status = cache.remove_variant(&h).unwrap();
        assert_eq!(status, CacheStatus::NotFound);
    }

    #[test]
    fn test_cache_get_variants() {
        let dir = test_dir("get_variants");
        let cache = PackageCache::new(&dir).unwrap();

        let src = dir.join("_source");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("f.txt"), "x").unwrap();

        let h1 = VariantHandle::new("alpha", "1.0", None, "fs@/a");
        let h2 = VariantHandle::new("beta", "2.0", Some(1), "fs@/b");

        cache
            .add_variant(&test_variant(&src, &h1), true, None)
            .unwrap();
        cache
            .add_variant(&test_variant(&src, &h2), true, None)
            .unwrap();

        let variants = cache.get_variants().unwrap();
        assert_eq!(variants.len(), 2);

        let names: Vec<&str> = variants.iter().map(|v| v.handle.name.as_str()).collect();
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"beta"));
    }

    #[test]
    fn test_cache_clean() {
        let dir = test_dir("clean");
        let cache = PackageCache::new(&dir).unwrap();

        let src = dir.join("_source");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("f.txt"), "data").unwrap();

        let h = test_handle();
        cache
            .add_variant(&test_variant(&src, &h), true, None)
            .unwrap();

        // Clean with very large max_age should remove nothing from cache
        let _removed = cache.clean(9999, None, None).unwrap();
        // Variant is too fresh, no removal expected
        let (status, _) = cache.get_cached_root(&h);
        assert!(matches!(status, CacheStatus::Found(_)));

        // Remove then clean to clear to_delete
        cache.remove_variant(&h).unwrap();
        let removed = cache.clean(0, None, None).unwrap();
        assert!(removed > 0);
    }

    #[test]
    fn test_cache_multiple_slots() {
        let dir = test_dir("multi_slots");
        let cache = PackageCache::new(&dir).unwrap();

        let src = dir.join("_source");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("f.txt"), "1").unwrap();

        // Two handles with same name/version but different repos (may hash to same bucket)
        let h1 = VariantHandle::new("pkg", "1.0", None, "mem@repo_a");
        let h2 = VariantHandle::new("pkg", "1.0", None, "mem@repo_b");

        let (_p1, _) = cache
            .add_variant(&test_variant(&src, &h1), true, None)
            .unwrap();
        let (_p2, _) = cache
            .add_variant(&test_variant(&src, &h2), true, None)
            .unwrap();

        // Both should be found independently
        let (s1, _) = cache.get_cached_root(&h1);
        let (s2, _) = cache.get_cached_root(&h2);
        assert!(matches!(s1, CacheStatus::Found(_)));
        assert!(matches!(s2, CacheStatus::Found(_)));
    }

    #[test]
    fn test_cache_touch() {
        let dir = test_dir("touch");
        let cache = PackageCache::new(&dir).unwrap();

        let src = dir.join("_source");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("f.txt"), "t").unwrap();

        let h = test_handle();
        cache
            .add_variant(&test_variant(&src, &h), true, None)
            .unwrap();

        let root = cache.touch_cached(&h).unwrap();
        assert!(root.is_some());

        // Touch non-existent
        let h2 = VariantHandle::new("noexist", "0.0", None, "x");
        assert!(cache.touch_cached(&h2).unwrap().is_none());
    }

    #[test]
    fn test_cache_dir_structure() {
        let dir = test_dir("structure");
        let cache = PackageCache::new(&dir).unwrap();

        let src = dir.join("_source");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("f.txt"), "s").unwrap();

        let h = test_handle();
        let (path, _) = cache
            .add_variant(&test_variant(&src, &h), true, None)
            .unwrap();

        // Verify structure: <root>/foo/1.2.3/<hash4>/<slot>
        let components: Vec<_> = path
            .strip_prefix(&cache.path)
            .unwrap()
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect();

        assert_eq!(components.len(), 4); // name / version / hash / slot
        assert_eq!(components[0], "foo");
        assert_eq!(components[1], "1.2.3");
        assert_eq!(components[2].len(), 4); // hash4
        assert_eq!(components[3], "a"); // first slot
    }

    #[test]
    fn test_copy_dir_recursive() {
        let dir = test_dir("copy_recursive");
        let src = dir.join("src");
        let dst = dir.join("dst");

        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("a.txt"), "aaa").unwrap();
        fs::write(src.join("sub").join("b.txt"), "bbb").unwrap();

        crate::package::ops::copy_dir_contents(&src, &dst, true, true, Some(&dir), None).unwrap();

        assert_eq!(fs::read_to_string(dst.join("a.txt")).unwrap(), "aaa");
        assert_eq!(
            fs::read_to_string(dst.join("sub").join("b.txt")).unwrap(),
            "bbb"
        );
    }

    #[test]
    fn test_lock_file() {
        let dir = test_dir("lockfile");
        let mut lock = LockFile::new(dir.join(".lock"));
        lock.acquire(LOCK_TIMEOUT).unwrap();
        assert!(dir.join(".lock").exists());
        lock.release().unwrap();
        assert!(dir.join(".lock").exists());
        let mut next = LockFile::new(dir.join(".lock"));
        next.acquire(Duration::ZERO).unwrap();
    }

    #[test]
    fn test_lock_file_drop() {
        let dir = test_dir("lockfile_drop");
        {
            let mut lock = LockFile::new(dir.join(".lock"));
            lock.acquire(LOCK_TIMEOUT).unwrap();
            assert!(dir.join(".lock").exists());
        }
        // Drop releases ownership while retaining the coordination file.
        assert!(dir.join(".lock").exists());
        let mut next = LockFile::new(dir.join(".lock"));
        next.acquire(Duration::ZERO).unwrap();
    }

    #[test]
    fn test_no_version_handle() {
        let dir = test_dir("no_version");
        let cache = PackageCache::new(&dir).unwrap();

        let src = dir.join("_source");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("f.txt"), "nv").unwrap();

        let h = VariantHandle::new("pkg", "", None, "mem@repo");
        let (path, _) = cache
            .add_variant(&test_variant(&src, &h), true, None)
            .unwrap();

        // Should use _NO_VERSION directory
        let rel = path.strip_prefix(&cache.path).unwrap();
        let components: Vec<_> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect();
        assert_eq!(components[1], "_NO_VERSION");
    }
}
