// SPDX-License-Identifier: Apache-2.0

//! Shared utility functions.

/// Normalize lexical path-prefix aliases for identity comparisons and staging coordinates.
/// Filesystem links and native path components remain unresolved and unchanged.
#[doc(hidden)]
pub fn path_key(path: &std::path::Path) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        let mut normalized = std::path::PathBuf::new();
        for component in path.components() {
            match component {
                Component::Prefix(prefix) => match prefix.kind() {
                    Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                        normalized.push(format!("{}:", char::from(letter.to_ascii_uppercase())));
                    }
                    Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                        let mut unc = std::ffi::OsString::from("\\\\");
                        unc.push(server);
                        unc.push("\\");
                        unc.push(share);
                        normalized.push(unc);
                    }
                    _ => normalized.push(component.as_os_str()),
                },
                _ => normalized.push(component.as_os_str()),
            }
        }
        normalized
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

/// Obtain relative coordinates beneath a configured directory authority.
///
/// Lexical prefix aliases are compared without filesystem access first, allowing
/// a configured root that has not been created yet. Otherwise only the existing
/// configured root is canonicalized; Windows 8.3 spellings are expanded only in
/// the destination's authority prefix. Generated descendants are never followed.
///
/// Returns None for an outside path or a suffix containing parent/root/prefix
/// components. Descendant names and filesystem objects are not validated here:
/// write callers must pass these coordinates to directory() before touching them.
#[doc(hidden)]
pub fn relative_to_authority(
    root: &std::path::Path,
    destination: &std::path::Path,
) -> std::io::Result<Option<std::path::PathBuf>> {
    use std::path::{Component, Path, PathBuf};
    let current = std::env::current_dir()?;
    let root = path_key(&if root.is_absolute() {
        root.to_path_buf()
    } else {
        current.join(root)
    });
    let destination = path_key(&if destination.is_absolute() {
        destination.to_path_buf()
    } else {
        current.join(destination)
    });
    let relative = if let Ok(relative) = destination.strip_prefix(&root) {
        Some(relative.to_path_buf())
    } else {
        let canonical_root = path_key(&directory(&root, Path::new(""), false)?);
        let relative = destination
            .strip_prefix(&canonical_root)
            .ok()
            .map(Path::to_path_buf);
        #[cfg(windows)]
        let relative =
            relative.or_else(|| windows_authority_relative(&destination, &canonical_root));
        relative
    };
    Ok(relative.filter(|relative: &PathBuf| {
        relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    }))
}

// Expand 8.3 spellings only in the existing authority prefix. GetLongPathNameW
// preserves junction names; canonicalizing a generated destination would follow
// redirects and could incorrectly accept an escape from the declared authority.
#[cfg(windows)]
fn windows_authority_relative(
    destination: &std::path::Path,
    authority: &std::path::Path,
) -> Option<std::path::PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Component, PathBuf, Prefix};
    use windows_sys::Win32::Storage::FileSystem::GetLongPathNameW;

    let depth = authority.components().count();
    let prefix: PathBuf = destination.components().take(depth).collect();
    let units: Vec<u16> = prefix.as_os_str().encode_wide().collect();
    let mut wide: Vec<u16> = match prefix.components().next()? {
        Component::Prefix(value) => match value.kind() {
            Prefix::Disk(_) => r"\\?\".encode_utf16().chain(units).collect(),
            Prefix::UNC(_, _) => r"\\?\UNC\"
                .encode_utf16()
                .chain(units.into_iter().skip(2))
                .collect(),
            _ => return None,
        },
        _ => return None,
    };
    if wide.contains(&0) {
        return None;
    }
    wide.push(0);
    let mut buffer = vec![0u16; 32768];
    // Both buffers are valid for the call, the input is NUL-terminated, and the
    // output capacity is supplied in UTF-16 code units.
    let length =
        unsafe { GetLongPathNameW(wide.as_ptr(), buffer.as_mut_ptr(), buffer.len() as u32) }
            as usize;
    if length == 0 || length >= buffer.len() {
        return None;
    }
    let long_prefix = PathBuf::from(OsString::from_wide(&buffer[..length]));
    (path_key(&long_prefix) == authority).then(|| destination.components().skip(depth).collect())
}

/// Classify redirects uniformly for generated directories and regular files.
#[doc(hidden)]
pub fn is_redirect(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

/// Resolve generated directories beneath an explicitly configured root.
///
/// A configured root may intentionally be a junction or symbolic link. Its
/// canonical target becomes the authority; each generated component is checked
/// independently and must be a real directory rather than a redirect.
#[doc(hidden)]
pub fn directory(
    root: &std::path::Path,
    relative: &std::path::Path,
    create: bool,
) -> std::io::Result<std::path::PathBuf> {
    use std::io::{Error, ErrorKind};
    let components = relative
        .components()
        .map(|component| {
            let std::path::Component::Normal(name) = component else {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Generated directory must be relative",
                ));
            };
            // Validation checks unsafe ASCII separators/reserved characters;
            // keep the original OsStr for identity, including native non-UTF8 names.
            let display = name.to_string_lossy();
            if !crate::path::is_safe_rez_path_component(&display, false) || display.contains('\0') {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Unsafe generated directory component",
                ));
            }
            Ok(name)
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    if create {
        std::fs::create_dir_all(root)?;
    }
    let mut path = root.canonicalize()?;
    if !path.is_dir() {
        return Err(Error::new(
            ErrorKind::NotADirectory,
            "Configured root is not a directory",
        ));
    }
    for name in components {
        path.push(name);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound && create => {
                match std::fs::create_dir(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
                std::fs::symlink_metadata(&path)?
            }
            Err(error) => return Err(error),
        };
        if is_redirect(&metadata) || !metadata.is_dir() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "Generated directory is not a regular directory: {}",
                    path.display()
                ),
            ));
        }
    }
    Ok(path)
}

/// Open a regular file without following a final-component symbolic link.
///
/// Callers remain responsible for validating generated parent directories beneath
/// their configured root. Flags are applied before opening, avoiding a check/open
/// race at the file itself; opened metadata also rejects devices and reparse links.
#[doc(hidden)]
pub fn open_file(
    path: &std::path::Path,
    options: &std::fs::OpenOptions,
) -> std::io::Result<std::fs::File> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if is_redirect(&metadata) || !metadata.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "Expected a regular file without a symbolic link: {}",
                    path.display()
                ),
            ));
        }
    }
    let mut options = options.clone();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if is_redirect(&metadata) || !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "Expected a regular file without a symbolic link: {}",
                path.display()
            ),
        ));
    }
    Ok(file)
}

/// Persistent OS lock shared by repository and cache writers.
///
/// The file is retained so waiting handles and subsequent writers always lock
/// the same filesystem object. Age alone cannot establish a stale owner.
#[doc(hidden)]
pub struct FileLock {
    path: std::path::PathBuf,
    file: Option<std::fs::File>,
}

impl FileLock {
    #[doc(hidden)]
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            path: path.into(),
            file: None,
        }
    }

    #[doc(hidden)]
    pub fn acquire(&mut self, timeout: std::time::Duration) -> std::io::Result<()> {
        if self.file.is_some() {
            return Ok(());
        }
        let file = open_file(
            &self.path,
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false),
        )?;
        let started = std::time::Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => {
                    self.file = Some(file);
                    return Ok(());
                }
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
            }
            if started.elapsed() >= timeout {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("Lock acquisition timeout for {}", self.path.display()),
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    #[doc(hidden)]
    pub fn release(&mut self) -> std::io::Result<()> {
        if let Some(file) = self.file.take() {
            file.unlock()?;
        }
        Ok(())
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

/// Hash a reader in bounded memory, retaining its raw digest representation.
pub fn hash_reader<D: sha2::Digest>(
    reader: &mut impl std::io::Read,
) -> std::io::Result<sha2::digest::Output<D>> {
    let mut hasher = D::new();
    let mut buffer = [0; 16 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize())
}

/// Hash a reader in bounded memory and return a lowercase hexadecimal digest.
pub fn hash_reader_hex<D: sha2::Digest>(
    reader: &mut impl std::io::Read,
) -> std::io::Result<String> {
    Ok(hex_encode(hash_reader::<D>(reader)?))
}

/// Deterministic FNV-1a hash, stable across process runs.
///
/// Unlike DefaultHasher, this algorithm's offset basis and multiplier are fixed.
pub fn stable_hash(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &byte in data {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Encode bytes as lowercase hexadecimal without allocating an intermediate buffer.
pub fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::{hex_encode, stable_hash};

    #[test]
    fn persistent_os_lock_survives_release_and_drop() {
        let owned = tempfile::tempdir().unwrap();
        let path = owned.path().join(".lock");
        let mut owner = super::FileLock::new(&path);
        owner.acquire(std::time::Duration::ZERO).unwrap();
        let mut waiter = super::FileLock::new(&path);
        assert_eq!(
            waiter
                .acquire(std::time::Duration::ZERO)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::TimedOut
        );
        owner.release().unwrap();
        assert!(path.exists());
        waiter.acquire(std::time::Duration::ZERO).unwrap();
        drop(waiter);
        assert!(path.exists());
        owner.acquire(std::time::Duration::ZERO).unwrap();
    }

    #[test]
    fn stable_hash_uses_fnv1a_64() {
        assert_eq!(stable_hash(b""), 0xcbf29ce484222325);
        assert_eq!(stable_hash(b"a"), 0xaf63dc4c8601ec8c);
    }

    #[test]
    fn hex_encode_is_lowercase_and_preserves_leading_zeroes() {
        assert_eq!(hex_encode([0x00, 0x0f, 0xab, 0xff]), "000fabff");
    }

    #[test]
    fn authority_coordinates_do_not_create_roots_or_accept_parent_traversal() {
        use std::path::{Path, PathBuf};
        let owned = tempfile::tempdir().unwrap();
        let root = owned.path().join("root");
        assert_eq!(
            super::relative_to_authority(&root, &root.join("one/two")).unwrap(),
            Some(PathBuf::from("one/two"))
        );
        assert_eq!(
            super::relative_to_authority(&root, &root).unwrap(),
            Some(PathBuf::new())
        );
        for relative in ["../outside", "one/../../outside"] {
            assert_eq!(
                super::relative_to_authority(&root, &root.join(relative)).unwrap(),
                None
            );
        }
        assert!(!root.exists());
        std::fs::create_dir(&root).unwrap();
        assert_eq!(
            super::relative_to_authority(&root, &owned.path().join("root-sibling/child")).unwrap(),
            None
        );
        let coordinates = super::relative_to_authority(&root, &root.join("one/two"))
            .unwrap()
            .unwrap();
        assert_eq!(coordinates, Path::new("one/two"));
        assert!(!root.join("one").exists());
    }

    #[test]
    fn generated_directories_validate_components_and_creation_policy() {
        let owned = tempfile::tempdir().unwrap();
        let nested = super::directory(owned.path(), std::path::Path::new("one/two"), true).unwrap();
        assert!(nested.is_dir());
        assert!(super::directory(owned.path(), std::path::Path::new("../outside"), true).is_err());
        assert!(super::directory(owned.path(), std::path::Path::new("absent"), false).is_err());
        assert!(!owned.path().join("absent").exists());
        let file = owned.path().join("file");
        std::fs::write(&file, b"data").unwrap();
        assert!(super::directory(owned.path(), std::path::Path::new("file/child"), true).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn lexical_path_keys_unify_prefix_aliases_without_resolving_components() {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        use std::path::Path;
        for (input, expected) in [
            (r"c:\project\child", r"C:\project\child"),
            (r"\\?\c:\project\child", r"C:\project\child"),
            (r"\\?\UNC\server\share\project", r"\\server\share\project"),
            (r"\\server\share\project", r"\\server\share\project"),
        ] {
            assert_eq!(super::path_key(Path::new(input)), Path::new(expected));
        }
        let long = format!(r"\\?\c:\{}", "directory\\".repeat(40));
        let expected = format!(r"C:\{}", "directory\\".repeat(40));
        assert_eq!(super::path_key(Path::new(&long)), Path::new(&expected));
        let native =
            std::ffi::OsString::from_wide(&[b'c' as u16, b':' as u16, b'\\' as u16, 0xD800]);
        let units = super::path_key(Path::new(&native))
            .as_os_str()
            .encode_wide()
            .collect::<Vec<_>>();
        assert_eq!(units, [b'C' as u16, b':' as u16, b'\\' as u16, 0xD800]);
    }

    #[cfg(unix)]
    #[test]
    fn configured_root_links_are_allowed_but_generated_links_are_rejected() {
        let owned = tempfile::tempdir().unwrap();
        let root = owned.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let alias = owned.path().join("configured");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        assert_eq!(
            super::directory(&alias, std::path::Path::new(""), false).unwrap(),
            root
        );
        std::os::unix::fs::symlink(owned.path(), root.join("redirect")).unwrap();
        assert!(super::directory(&root, std::path::Path::new("redirect/outside"), true).is_err());
        assert!(!owned.path().join("outside").exists());
    }

    #[cfg(unix)]
    #[test]
    fn regular_file_open_and_lock_do_not_follow_links() {
        let owned = tempfile::tempdir().unwrap();
        let foreign = owned.path().join("valuable");
        std::fs::write(&foreign, b"keep").unwrap();
        let link = owned.path().join("link");
        std::os::unix::fs::symlink(&foreign, &link).unwrap();
        assert!(super::open_file(&link, std::fs::OpenOptions::new().read(true)).is_err());
        let mut lock = super::FileLock::new(&link);
        assert!(lock.acquire(std::time::Duration::ZERO).is_err());
        assert_eq!(std::fs::read(foreign).unwrap(), b"keep");
    }
}
