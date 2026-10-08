// SPDX-License-Identifier: Apache-2.0

//! Filesystem copy primitives with declared destination authority.

use crate::errors::{Result, RezError};
use std::fs;
use std::path::{Path, PathBuf};

struct CopySettings {
    follow_symlinks: bool,
    keep_timestamp: bool,
}

/// Recursively copy contents of `src` into `dest`.
///
/// Returns a list of relative paths that were copied.
/// Creates `dest` if it does not exist. A supplied `root` is the configured
/// authority; generated destination components beneath it cannot redirect.
/// `None` declares `dest` itself as an explicit configured authority. Source
/// link following is independent. `exclude` contains exact source-relative paths.
/// File replacement is atomic and does not write through old destination links.
pub fn copy_dir_contents(
    src: &Path,
    dest: &Path,
    follow_symlinks: bool,
    keep_timestamp: bool,
    root: Option<&Path>,
    exclude: Option<&[PathBuf]>,
) -> Result<Vec<PathBuf>> {
    if !src.is_dir() {
        return Err(RezError::PackageCopy(format!(
            "Source is not a directory: {}",
            src.display()
        )));
    }

    let destination = if let Some(root) = root {
        let relative = crate::util::relative_to_authority(root, dest)?.ok_or_else(|| {
            RezError::PackageCopy("Destination is outside its declared copy root".into())
        })?;
        crate::util::directory(root, &relative, true)?
    } else {
        // None declares the explicit destination itself as the configured authority.
        crate::util::directory(dest, Path::new(""), true)?
    };
    let options = CopySettings {
        follow_symlinks,
        keep_timestamp,
    };
    let mut copied = Vec::new();

    copy_dir_recursive(
        src,
        &destination,
        src,
        &options,
        &mut copied,
        &mut std::collections::HashSet::new(),
        exclude,
    )?;

    // Preserve directory timestamps if requested
    if keep_timestamp {
        copy_timestamps(src, &destination)?;
    }

    Ok(copied)
}

/// Internal recursive walker for copy_dir_contents.
fn copy_dir_recursive(
    base_src: &Path,
    base_dest: &Path,
    current_src: &Path,
    options: &CopySettings,
    copied: &mut Vec<PathBuf>,
    ancestors: &mut std::collections::HashSet<PathBuf>,
    exclude: Option<&[PathBuf]>,
) -> Result<()> {
    let actual = fs::canonicalize(current_src)?;
    if !ancestors.insert(actual.clone()) {
        return Err(RezError::PackageCopy(format!(
            "Payload directory symlink cycle at {}",
            current_src.display()
        )));
    }
    let entries = fs::read_dir(current_src).map_err(|e| {
        RezError::PackageCopy(format!(
            "Failed to read directory {}: {}",
            current_src.display(),
            e
        ))
    })?;

    for entry in entries {
        let entry = entry?;
        let src_path = entry.path();
        let rel = src_path
            .strip_prefix(base_src)
            .map_err(|e| RezError::PackageCopy(e.to_string()))?;
        if exclude.is_some_and(|paths| paths.iter().any(|path| path == rel)) {
            continue;
        }
        let dest_path = base_dest.join(rel);

        let file_type = if options.follow_symlinks {
            // Follow symlinks: use metadata (follows links)
            fs::metadata(&src_path)
        } else {
            // Don't follow: use symlink_metadata
            fs::symlink_metadata(&src_path)
        };

        let meta = file_type.map_err(|e| {
            RezError::PackageCopy(format!("Failed to stat {}: {}", src_path.display(), e))
        })?;

        crate::util::directory(
            base_dest,
            rel.parent().unwrap_or_else(|| Path::new("")),
            true,
        )?;
        if meta.is_dir() {
            crate::util::directory(base_dest, rel, true)?;
            copy_dir_recursive(
                base_src, base_dest, &src_path, options, copied, ancestors, exclude,
            )?;
            if options.keep_timestamp {
                copy_timestamps(&src_path, &dest_path)?;
            }
        } else if meta.file_type().is_symlink() {
            copy_symlink(&src_path, &dest_path)?;
        } else if meta.is_file() {
            copy_file(&src_path, &dest_path, options.keep_timestamp)?;
        } else {
            return Err(RezError::PackageCopy(format!(
                "Unsupported payload object {}",
                src_path.display()
            )));
        }

        copied.push(rel.to_path_buf());
    }
    ancestors.remove(&actual);
    Ok(())
}

/// Copy a regular file with atomic leaf replacement and optional filesystem stats.
/// The caller validates destination-parent authority before invoking this primitive.
#[doc(hidden)]
pub fn copy_file(src: &Path, dest: &Path, keep_timestamp: bool) -> Result<()> {
    let parent = dest
        .parent()
        .ok_or_else(|| RezError::PackageCopy("Copied file has no destination parent".into()))?;
    let temporary = tempfile::NamedTempFile::new_in(parent)?.into_temp_path();
    fs::copy(src, &temporary)?;
    if keep_timestamp {
        copy_timestamps(src, &temporary)?;
    }
    temporary
        .persist(dest)
        .map_err(|error| RezError::Io(error.error))?;
    Ok(())
}

/// Copy a symbolic link as an opaque filesystem object, including dangling links.
#[doc(hidden)]
pub fn copy_symlink(src: &Path, dest: &Path) -> Result<()> {
    let target = fs::read_link(src)?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, dest)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt;
        if fs::symlink_metadata(src)?.file_type().is_symlink_dir() {
            std::os::windows::fs::symlink_dir(target, dest)?;
        } else {
            std::os::windows::fs::symlink_file(target, dest)?;
        }
    }
    Ok(())
}

/// Remove one payload object without following symbolic links.
#[doc(hidden)]
pub fn remove_path(path: &Path) -> std::io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt;
        if metadata.file_type().is_symlink_dir() {
            return fs::remove_dir(path);
        }
    }
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// Copy modified/accessed timestamps and permission bits after copying an object.
#[doc(hidden)]
pub fn copy_timestamps(src: &Path, dest: &Path) -> Result<()> {
    let metadata = fs::metadata(src)?;
    let times = fs::FileTimes::new()
        .set_accessed(metadata.accessed()?)
        .set_modified(metadata.modified()?);
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_WRITE_ATTRIBUTES,
        };
        options
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    }
    let file = options.open(dest)?;
    file.set_times(times)?;
    file.set_permissions(metadata.permissions())?;
    Ok(())
}

// ============================================================================
// Helper: safe directory removal
// ============================================================================

/// Remove a directory and all its contents.
///
/// If the directory doesn't exist, this is a no-op (not an error).
pub fn safe_remove_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    fs::remove_dir_all(path).map_err(|e| {
        RezError::PackageCopy(format!(
            "Failed to remove directory {}: {}",
            path.display(),
            e
        ))
    })
}
#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    fn short_path(path: &Path) -> PathBuf {
        let input: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut output = vec![0u16; 32768];
        // The input is NUL-terminated and output has the declared capacity.
        let length = unsafe {
            windows_sys::Win32::Storage::FileSystem::GetShortPathNameW(
                input.as_ptr(),
                output.as_mut_ptr(),
                output.len() as u32,
            )
        } as usize;
        assert!(length > 0 && length < output.len());
        PathBuf::from(OsString::from_wide(&output[..length]))
    }

    fn junction(link: &Path, target: &Path) {
        let output = std::process::Command::new("cmd")
            .args(["/D", "/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    #[test]
    fn copy_accepts_short_authority_spellings_and_rejects_foreign_roots() {
        let owned = tempfile::Builder::new()
            .prefix("rez-copy-authority-long-")
            .tempdir()
            .unwrap();
        let source = owned.path().join("source");
        let authority = owned.path().join("authority-directory-long");
        let foreign = owned.path().join("foreign-directory-long");
        for path in [&source, &authority, &foreign] {
            fs::create_dir(path).unwrap();
        }
        fs::write(source.join("file"), b"payload").unwrap();
        let canonical = fs::canonicalize(&authority).unwrap();
        let short = short_path(&authority);
        for (root, destination) in [
            (&canonical, short.join("short-destination")),
            (&short, canonical.join("long-destination")),
        ] {
            copy_dir_contents(&source, &destination, false, false, Some(root), None).unwrap();
            assert_eq!(fs::read(destination.join("file")).unwrap(), b"payload");
        }
        let outside = short_path(&foreign).join("absent");
        assert!(
            copy_dir_contents(&source, &outside, false, false, Some(&canonical), None).is_err()
        );
        assert!(!foreign.join("absent").exists());
    }

    #[test]
    fn short_authority_preserves_configured_junctions_and_rejects_generated_junctions() {
        let owned = tempfile::Builder::new()
            .prefix("rez-copy-junction-long-")
            .tempdir()
            .unwrap();
        let source = owned.path().join("source");
        let authority = owned.path().join("authority-directory-long");
        let foreign = owned.path().join("foreign");
        for path in [&source, &authority, &foreign] {
            fs::create_dir(path).unwrap();
        }
        fs::write(source.join("file"), b"payload").unwrap();
        fs::write(foreign.join("file"), b"valuable").unwrap();
        let configured = owned.path().join("configured");
        junction(&configured, &authority);
        let short = short_path(&authority);
        copy_dir_contents(
            &source,
            &short.join("accepted"),
            false,
            false,
            Some(&configured),
            None,
        )
        .unwrap();
        assert_eq!(
            fs::read(authority.join("accepted/file")).unwrap(),
            b"payload"
        );

        let redirect = authority.join("redirect");
        junction(&redirect, &foreign);
        let canonical = fs::canonicalize(&authority).unwrap();
        assert!(copy_dir_contents(
            &source,
            &short.join("redirect/nested"),
            false,
            false,
            Some(&canonical),
            None,
        )
        .is_err());
        assert_eq!(fs::read(foreign.join("file")).unwrap(), b"valuable");
        assert!(!foreign.join("nested").exists());
        // Remove the junction objects before the scratch directory is dropped.
        fs::remove_dir(redirect).unwrap();
        fs::remove_dir(configured).unwrap();
    }
}
