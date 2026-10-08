// SPDX-License-Identifier: Apache-2.0

//! Python executable identity for applications that register the Rez CLI entry point.
//!
//! Distribution contains one executable. A private, content-addressed runtime
//! snapshot gives Python an unambiguous interpreter name without inherited flags.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use sha2::Sha256;

use foundation::errors::{Result, RezError};

const PYTHON_NAME: &str = if cfg!(windows) {
    "rez-python.exe"
} else {
    "rez-python"
};
static CLI_EXECUTABLE: OnceLock<PathBuf> = OnceLock::new();
static PYTHON_EXECUTABLE: OnceLock<PathBuf> = OnceLock::new();

/// Register that this application supports the Rez/Python executable entry point.
///
/// Embedding hosts must opt in explicitly; their executable is otherwise left
/// unchanged. Registration itself does not create a runtime snapshot.
pub fn register_cli() -> Result<()> {
    let executable = std::env::current_exe()
        .map_err(|error| RezError::Python(format!("locate CLI executable: {error}")))?;
    let _ = CLI_EXECUTABLE.set(executable);
    Ok(())
}

/// Recognize the dedicated Python executable name before parsing any options.
pub fn is_python_executable(executable: &OsStr) -> bool {
    Path::new(executable).file_name() == Some(OsStr::new(PYTHON_NAME))
}

/// Return the registered CLI's stable Python identity, or None for an embedding host.
pub fn executable() -> Result<Option<PathBuf>> {
    let Some(source) = CLI_EXECUTABLE.get() else {
        return Ok(None);
    };
    if is_python_executable(source.as_os_str()) {
        // A snapshot already running as Python must never create another snapshot.
        return Ok(Some(source.clone()));
    }
    if let Some(path) = PYTHON_EXECUTABLE.get() {
        return Ok(Some(path.clone()));
    }
    let home = foundation::home().ok_or_else(|| {
        RezError::Python("Cannot locate user home for Python runtime snapshot".into())
    })?;
    let home = std::path::absolute(&home)
        .map_err(|error| RezError::Python(format!("resolve Python runtime home: {error}")))?;
    if !home.is_dir() {
        return Err(RezError::Python(format!(
            "Python runtime home is not a directory: {}",
            home.display()
        )));
    }
    let root = home.join(".rez").join("python");
    let path = materialize(source, &root).map_err(|error| {
        RezError::Python(format!(
            "prepare Python executable in {}: {error}",
            root.display()
        ))
    })?;
    let _ = PYTHON_EXECUTABLE.set(path.clone());
    Ok(Some(path))
}

/// Publish an independent snapshot; never hardlink the mutable source executable.
fn materialize(source: &Path, root: &Path) -> io::Result<PathBuf> {
    let mut original = File::open(source)?;
    let expected = foundation::util::hash_reader_hex::<Sha256>(&mut original)?;
    original.seek(SeekFrom::Start(0))?;

    // Check every directory created within our cache boundary. Existing .rez may
    // contain other caches, so preserve its permissions while rejecting unsafe roots.
    if let Some(parent) = root.parent() {
        directory(parent, false)?;
    }
    directory(root, true)?;
    let directory_path = root.join(&expected);
    directory(&directory_path, true)?;
    let destination = directory_path.join(PYTHON_NAME);
    match fs::symlink_metadata(&destination) {
        Ok(_) => {
            validate_snapshot(&destination, &expected)?;
            return Ok(destination);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let mut staged = tempfile::NamedTempFile::new_in(&directory_path)?;
    io::copy(&mut original, staged.as_file_mut())?;
    staged.as_file_mut().seek(SeekFrom::Start(0))?;
    if foundation::util::hash_reader_hex::<Sha256>(staged.as_file_mut())? != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source executable changed during snapshot",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        staged
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o700))?;
    }
    staged.as_file().sync_all()?;
    match staged.persist_noclobber(&destination) {
        Ok(file) => drop(file),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
            // Another process won. The unpublished NamedTempFile is dropped here.
        }
        Err(error) => return Err(error.error),
    }
    validate_snapshot(&destination, &expected)?;
    Ok(destination)
}

fn directory(path: &Path, private: bool) -> io::Result<()> {
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let mut builder = builder;
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || is_link(&metadata) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Python runtime cache directory is not a regular directory",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mask = if private { 0o077 } else { 0o022 };
        if metadata.permissions().mode() & mask != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Python runtime cache permissions are not private",
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = private; // Windows directories inherit the user-profile ACL.
    Ok(())
}

fn validate_snapshot(path: &Path, expected: &str) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || is_link(&metadata) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Python runtime snapshot is not a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        if mode & 0o077 != 0 || mode & 0o100 == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Python runtime snapshot permissions are not private and executable",
            ));
        }
    }
    let actual = foundation::util::hash_reader_hex::<Sha256>(&mut File::open(path)?)?;
    if actual != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Python runtime snapshot checksum mismatch",
        ));
    }
    Ok(())
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Reject junctions and all other reparse points as well as symbolic links.
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    metadata.file_type().is_symlink()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_is_independent_and_versions_follow_content() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("rez");
        let cache = temp.path().join("cache");
        fs::write(&source, b"first executable").unwrap();
        let first = materialize(&source, &cache).unwrap();
        assert_eq!(first, materialize(&source, &cache).unwrap());
        fs::write(&source, b"new executable").unwrap();
        let second = materialize(&source, &cache).unwrap();
        assert_ne!(first, second);
        assert_eq!(fs::read(first).unwrap(), b"first executable");
        assert_eq!(fs::read(second).unwrap(), b"new executable");
    }

    #[test]
    fn concurrent_snapshot_publishers_share_complete_content() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("rez");
        let cache = temp.path().join("cache");
        let bytes = vec![0xab; 64 * 1024];
        fs::write(&source, &bytes).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let source = source.clone();
                let cache = cache.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    materialize(&source, &cache).unwrap()
                })
            })
            .collect();
        let paths: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        for path in &paths {
            assert_eq!(path, &paths[0]);
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
        assert_eq!(fs::read_dir(paths[0].parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn corrupt_snapshot_and_non_directory_cache_fail_without_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("rez");
        let cache = temp.path().join("cache");
        fs::write(&source, b"executable").unwrap();
        let snapshot = materialize(&source, &cache).unwrap();
        fs::write(&snapshot, b"corrupt").unwrap();
        assert_eq!(
            materialize(&source, &cache).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(fs::read(&snapshot).unwrap(), b"corrupt");
        fs::write(temp.path().join("blocked"), b"file").unwrap();
        assert!(materialize(&source, &temp.path().join("blocked").join("cache")).is_err());
    }

    #[test]
    fn python_identity_requires_exact_executable_name() {
        assert!(is_python_executable(OsStr::new(PYTHON_NAME)));
        assert!(is_python_executable(
            Path::new("directory").join(PYTHON_NAME).as_os_str()
        ));
        assert!(!is_python_executable(OsStr::new("rez")));
        assert!(!is_python_executable(OsStr::new("rez.exe")));
        assert!(!is_python_executable(OsStr::new("not-rez-python.exe")));
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_rejects_symlink_cache_and_public_permissions() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("rez");
        fs::write(&source, b"executable").unwrap();
        let actual = temp.path().join("actual");
        fs::create_dir(&actual).unwrap();
        symlink(&actual, temp.path().join("cache")).unwrap();
        assert!(materialize(&source, &temp.path().join("cache")).is_err());
        let public = temp.path().join("public");
        fs::create_dir(&public).unwrap();
        fs::set_permissions(&public, fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(
            materialize(&source, &public).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }
}
