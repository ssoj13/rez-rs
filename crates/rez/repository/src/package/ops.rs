// SPDX-License-Identifier: Apache-2.0

//! Package operations - copy, move, remove.
//!
//! Combines Python rez package_copy/move/remove modules.

use std::fs;
use std::path::{Path, PathBuf};

use crate::errors::{Result, RezError};

// ============================================================================
// Result types
// ============================================================================

/// Outcome of a package copy operation.
#[derive(Debug, Clone)]
pub struct CopyResult {
    /// Source package root path
    pub src_path: PathBuf,
    /// Destination package root path
    pub dest_path: PathBuf,
    /// Files/dirs that were skipped (already existed, overwrite=false)
    pub skipped: Vec<String>,
    /// Files/dirs that were copied
    pub copied: Vec<String>,
    /// Selected source indices mapped to actual destination variant indices.
    pub variant_indices: Vec<(usize, Option<usize>)>,
    /// Exact destination resource handles, including selections skipped as already installed.
    pub handles: Vec<(usize, crate::provider::ResourceHandle)>,
    pub installed_indices: Vec<usize>,
    pub skipped_indices: Vec<usize>,
}

/// Outcome of a single variant copy.
#[derive(Debug, Clone)]
pub struct CopyVariantResult {
    /// Variant index within the package
    pub variant_index: usize,
    /// Source variant root directory
    pub src_root: PathBuf,
    /// Destination variant root directory
    pub dest_root: PathBuf,
    /// Top-level entries copied, relative to the variant root.
    pub copied: Vec<PathBuf>,
}

// ============================================================================
// Copy options
// ============================================================================

/// Configuration for package copy operations.
#[derive(Debug, Clone, Default)]
pub struct CopyOptions {
    /// Overwrite existing files at destination
    pub overwrite: bool,
    /// Follow symlinks instead of copying the links themselves
    pub follow_symlinks: bool,
    /// Preserve the original package repository timestamp; payload stats are always copied.
    pub keep_timestamp: bool,
    /// Bypass the package's relocatability policy.
    pub force: bool,
    /// None selects every source variant; indices are validated before writing.
    pub variants: Option<Vec<usize>>,
    pub destination_name: Option<String>,
    pub destination_version: Option<version::Version>,
    /// Optional isolated configuration; package-level overrides still apply.
    pub config: Option<crate::config::RezConfig>,
    /// Require a new destination package, checked under its publication lock.
    pub require_new: bool,
}

// ============================================================================
// Helper: recursive directory copy
// ============================================================================

pub(crate) use foundation::filesystem::{copy_dir_contents, remove_path, safe_remove_dir};

/// Remove a directory only if it's empty.
///
/// Returns true if the directory was removed, false if not empty or missing.
fn remove_if_empty(path: &Path) -> Result<bool> {
    if !path.exists() || !path.is_dir() {
        return Ok(false);
    }

    // Check if directory is empty
    let mut entries = fs::read_dir(path)?;
    if entries.next().is_some() {
        return Ok(false);
    }

    fs::remove_dir(path)?;
    Ok(true)
}

// ============================================================================
// Package definition filenames
// ============================================================================

/// Get the configured package definition file names for payload filtering.
fn pkg_definition_names() -> Result<Vec<String>> {
    crate::serialise::package_definition_file_names(crate::serialise::PACKAGE_DEFINITION_EXTENSIONS)
}

// ============================================================================
// copy_package
// ============================================================================

/// Copy selected variants from an exact repository candidate through the locked publisher.
///
/// Relocatability uses repository provenance and package configuration. Payload and
/// include assets are staged before publication; destination handles use the actual
/// under-lock metadata indices, including selections skipped as already installed.
pub fn copy_package(
    candidate: &crate::provider::PackageCandidate,
    destination_repository: &Path,
    opts: &CopyOptions,
) -> Result<CopyResult> {
    use crate::provider::PackageProvenance;
    use crate::repository::{FsRepo, PackageRepository, PublicationPayload};
    let package = &candidate.package;
    let provenance = candidate.provenance.as_ref().ok_or_else(|| {
        RezError::PackageCopy("Source package has no repository provenance".into())
    })?;
    let source_root = package
        .base
        .as_ref()
        .ok_or_else(|| RezError::PackageCopy("Source package has no payload base".into()))?;
    if provenance.repository_type != "filesystem" || !source_root.is_dir() {
        return Err(RezError::PackageCopy(format!(
            "Source package has no filesystem payload root: {}",
            source_root.display()
        )));
    }
    let config = package.config(opts.config.as_ref())?;
    if !opts.force && !package.is_relocatable(Some(&provenance.location), Some(&config)) {
        return Err(RezError::PackageCopy(format!(
            "Cannot copy non-relocatable package {}-{}",
            package.name, package.version
        )));
    }
    let name = opts.destination_name.as_deref().unwrap_or(&package.name);
    let version = opts
        .destination_version
        .as_ref()
        .unwrap_or(&package.version);
    let version_text = version.to_string();
    crate::serialise::validate_rez_package_path(
        name,
        (!version_text.is_empty()).then_some(version_text.as_str()),
    )?;
    let destination = if version.is_empty() {
        destination_repository.join(name)
    } else {
        destination_repository.join(name).join(&version_text)
    };
    if destination.exists() && fs::canonicalize(&destination)? == fs::canonicalize(source_root)? {
        return Err(RezError::PackageCopy(
            "Cannot copy package over itself".into(),
        ));
    }
    let indices = opts
        .variants
        .clone()
        .unwrap_or_else(|| (0..package.variants.len().max(1)).collect());
    let mut seen = std::collections::HashSet::new();
    for &index in &indices {
        if index >= package.variants.len().max(1) || !seen.insert(index) {
            return Err(RezError::PackageCopy(format!(
                "Invalid or duplicate variant selection {index}"
            )));
        }
    }
    let mut result = CopyResult {
        src_path: source_root.clone(),
        dest_path: destination.clone(),
        copied: Vec::new(),
        skipped: Vec::new(),
        variant_indices: Vec::new(),
        handles: Vec::new(),
        installed_indices: Vec::new(),
        skipped_indices: Vec::new(),
    };
    if indices.is_empty() {
        return Ok(result);
    }
    let stage = tempfile::tempdir()?;
    let roots = (0..package.variants.len().max(1))
        .map(|index| {
            candidate
                .clone()
                .into_variant((!package.variants.is_empty()).then_some(index), false)
                .and_then(|variant| {
                    variant.variant.root.ok_or_else(|| {
                        RezError::PackageCopy("Source variant has no payload root".into())
                    })
                })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut entries = Vec::new();
    let mut copied_roots = std::collections::HashMap::<PathBuf, Vec<PathBuf>>::new();
    for &index in &indices {
        let source = &roots[index];
        let relative = package
            .variants
            .get(index)
            .and_then(|variant| {
                model::package::Variant::compute_subpath(variant, package.hashed_variants)
            })
            .map(PathBuf::from)
            .unwrap_or_default();
        let excluded = roots
            .iter()
            .filter_map(|other| {
                other
                    .strip_prefix(source)
                    .ok()
                    .filter(|relative| !relative.as_os_str().is_empty())
                    .and_then(|relative| {
                        relative
                            .components()
                            .next()
                            .map(|part| PathBuf::from(part.as_os_str()))
                    })
            })
            .collect::<Vec<_>>();
        let copied = if let Some(copied) = copied_roots.get(source) {
            copied.clone()
        } else {
            let copied = copy_variant(
                index,
                source,
                &stage.path().join(&relative),
                opts,
                Some(&excluded),
                Some(stage.path()),
            )?
            .copied;
            copied_roots.insert(source.clone(), copied.clone());
            copied
        };
        for entry in copied {
            entries.push((Some(index), relative.join(entry)));
        }
    }
    let includes = source_root.join(".rez/include");
    if includes.is_dir() && !stage.path().join(".rez/include").exists() {
        copy_dir_contents(
            &includes,
            &stage.path().join(".rez/include"),
            opts.follow_symlinks,
            true,
            Some(stage.path()),
            None,
        )?;
        entries.push((None, PathBuf::from(".rez/include")));
    }
    let mut data = package.to_data()?;
    data.insert("name".into(), serde_json::json!(name));
    if version.is_empty() {
        data.remove("version");
    } else {
        data.insert("version".into(), serde_json::json!(version_text));
    }
    if !opts.keep_timestamp {
        data.insert(
            "timestamp".into(),
            serde_json::json!(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| RezError::PackageCopy(error.to_string()))?
                .as_secs()),
        );
    }
    let destination_package = model::package::Package::from_data(data)?;
    let payload_entries = entries.clone();
    let published = crate::repository::publish_package(
        &destination_package,
        &destination,
        &indices,
        Some(PublicationPayload::Owned {
            root: stage,
            replace: opts.overwrite,
            paths: Some(entries),
            require_new: opts.require_new,
        }),
        None,
    )?;
    let repo = FsRepo::open(destination_repository, false)?
        .ok_or_else(|| RezError::PackageCopy("Published repository is missing".into()))?;
    let (info, source) = repo
        .get_package_with_source(name, version, None)?
        .ok_or_else(|| RezError::PackageCopy("Published destination cannot be reloaded".into()))?;
    let destination_provenance = PackageProvenance {
        repository_type: "filesystem".into(),
        location: destination_repository.to_string_lossy().into_owned(),
        source,
    };
    for &(source_index, destination_index) in &published.variant_indices {
        let handle = destination_provenance
            .resource_handle(name, version, destination_index)
            .ok_or_else(|| RezError::PackageCopy("Destination has no filesystem handle".into()))?;
        crate::provider::PackageCandidate {
            package: info.to_package()?,
            provenance: Some(destination_provenance.clone()),
        }
        .into_variant(destination_index, false)?;
        result.handles.push((source_index, handle));
    }
    if !published.installed_indices.is_empty() {
        let mut copied = std::collections::BTreeSet::new();
        for (owner, path) in payload_entries {
            if owner.is_none_or(|index| published.installed_indices.contains(&index)) {
                copied.insert(path.to_string_lossy().into_owned());
            }
        }
        if let Some(source) = &destination_provenance.source {
            if let Ok(path) = source.path.strip_prefix(&destination) {
                copied.insert(path.to_string_lossy().into_owned());
            }
        }
        result.copied = copied.into_iter().collect();
    }
    result.skipped = published
        .skipped_indices
        .iter()
        .map(|index| format!("variant {index}"))
        .collect();
    result.variant_indices = published.variant_indices;
    result.installed_indices = published.installed_indices;
    result.skipped_indices = published.skipped_indices;
    Ok(result)
}

// ============================================================================
// copy_variant
// ============================================================================

/// Copy a single variant's payload from source to destination.
///
/// # Arguments
/// * `variant_index` - Variant index for the result
/// * `src_root` - Source variant root directory
/// * `dest_root` - Destination variant root directory
/// * `opts` - Copy options
///
/// # Returns
/// `CopyVariantResult` on success.
pub fn copy_variant(
    variant_index: usize,
    src_root: &Path,
    dest_root: &Path,
    opts: &CopyOptions,
    exclude: Option<&[PathBuf]>,
    root: Option<&Path>,
) -> Result<CopyVariantResult> {
    let mut excluded = pkg_definition_names()?
        .into_iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if let Some(names) = exclude {
        excluded.extend_from_slice(names);
    }
    let copied = copy_dir_contents(
        src_root,
        dest_root,
        opts.follow_symlinks,
        true,
        root,
        Some(&excluded),
    )?
    .into_iter()
    .filter_map(|entry| {
        entry
            .components()
            .next()
            .map(|component| PathBuf::from(component.as_os_str()))
    })
    .collect::<std::collections::BTreeSet<_>>()
    .into_iter()
    .collect();
    Ok(CopyVariantResult {
        variant_index,
        src_root: src_root.to_path_buf(),
        dest_root: dest_root.to_path_buf(),
        copied,
    })
}

// ============================================================================
// move_package
// ============================================================================

/// Publish a complete copy, then hide the source while retaining its payload.
///
/// Destination metadata must not already exist; the publisher checks this under
/// its version lock. The source is ignored only after successful publication and
/// exact destination reload. Force and timestamp options use the same copy path.
pub fn move_package(
    candidate: &crate::provider::PackageCandidate,
    destination_repository: &Path,
    opts: &CopyOptions,
) -> Result<CopyResult> {
    let mut options = opts.clone();
    options.require_new = true;
    options.variants = None;
    let result = copy_package(candidate, destination_repository, &options)?;
    let source = candidate.provenance.as_ref().ok_or_else(|| {
        RezError::PackageMove("Source package has no repository provenance".into())
    })?;
    crate::repository::ignore_package(
        Path::new(&source.location),
        &candidate.package.name,
        &candidate.package.version.to_string(),
    )?;
    Ok(result)
}

// ============================================================================
// remove_package
// ============================================================================

/// Remove a package version from a repository.
///
/// Deletes the version directory and cleans up empty parent dirs (family dir).
///
/// # Arguments
/// * `name` - Package name
/// * `version` - Package version string
/// * `path` - Repository root path (e.g., /packages)
///
/// The version directory is expected at: `path / name / version`
///
/// # Returns
/// `Ok(true)` if removed, `Ok(false)` if not found.
pub fn remove_package(name: &str, version: &str, path: &Path) -> Result<bool> {
    crate::serialise::validate_rez_package_path(name, Some(version))?;
    let version_dir = path.join(name).join(version);

    if !version_dir.exists() {
        return Ok(false);
    }

    safe_remove_dir(&version_dir)?;

    // Clean up empty family directory
    let family_dir = path.join(name);
    let _ = remove_if_empty(&family_dir);

    Ok(true)
}

// ============================================================================
// remove_package_family
// ============================================================================

/// Remove a package family through the repository's shared force-aware path.
pub fn remove_package_family(name: &str, path: &Path, force: bool) -> Result<Option<usize>> {
    crate::repository::remove_package_family(path, name, force)
}

// ============================================================================
// Internal helpers
// ============================================================================

/// Walk up from the given path removing empty parent directories.
/// Stops at the first non-empty parent or when reaching the filesystem root.
#[cfg(test)]
fn cleanup_empty_parents(path: &Path) -> Result<()> {
    let mut current = path.to_path_buf();
    while let Some(parent) = current.parent() {
        if !remove_if_empty(parent)? {
            break;
        }
        current = parent.to_path_buf();
    }
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Create a unique temp directory for test isolation.
    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("rez_rs_test_package_ops")
            .join(name)
            .join(format!("{}", std::process::id()));
        if dir.exists() {
            fs::remove_dir_all(&dir).ok();
        }
        fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    /// Clean up test directory.
    fn cleanup(dir: &Path) {
        fs::remove_dir_all(dir).ok();
    }

    fn candidate(path: &Path, name: &str, version: &str) -> crate::provider::PackageCandidate {
        let data = if path.join("package.yaml").is_file() {
            crate::serialise::load_from_file(
                &path.join("package.yaml"),
                crate::serialise::FileFormat::Yaml,
                crate::serialise::PackageDataCache::Disabled,
            )
            .unwrap()
        } else {
            serde_json::from_value(serde_json::json!({"name": name, "version": version})).unwrap()
        };
        let mut package = model::package::Package::from_data(data).unwrap();
        package.base = Some(path.to_path_buf());
        let repository = path.parent().and_then(Path::parent).unwrap_or(path);
        crate::provider::PackageCandidate {
            package,
            provenance: Some(crate::provider::PackageProvenance {
                repository_type: "filesystem".into(),
                location: repository.to_string_lossy().into_owned(),
                source: Some(crate::repository::PackageSource {
                    kind: crate::repository::PackageSourceKind::Directory,
                    path: path.join("package.yaml"),
                }),
            }),
        }
    }

    /// Create a minimal package directory structure for testing.
    fn make_pkg(root: &Path, name: &str, version: &str) -> PathBuf {
        let ver_dir = root.join(name).join(version);
        fs::create_dir_all(&ver_dir).expect("create version dir");
        // Write a package definition file
        fs::write(
            ver_dir.join("package.yaml"),
            format!("name: {}\nversion: '{}'\n", name, version),
        )
        .expect("write package.yaml");
        // Write a payload file
        fs::write(ver_dir.join("payload.txt"), "test payload data").expect("write payload");
        ver_dir
    }

    /// Create a package with variant subdirectories.
    fn make_pkg_with_variants(root: &Path, name: &str, version: &str) -> PathBuf {
        let ver_dir = root.join(name).join(version);
        fs::create_dir_all(&ver_dir).expect("create version dir");
        fs::write(
            ver_dir.join("package.yaml"),
            format!("name: {}\nversion: '{}'\nhashed_variants: false\nvariants: [[platform-linux], [platform-windows]]\n", name, version),
        )
        .expect("write package.yaml");

        // Variant 0: platform-linux
        let v0 = ver_dir.join("platform-linux");
        fs::create_dir_all(&v0).expect("create variant 0 dir");
        fs::write(v0.join("bin.sh"), "#!/bin/bash\necho hello").expect("write bin.sh");

        // Variant 1: platform-windows
        let v1 = ver_dir.join("platform-windows");
        fs::create_dir_all(&v1).expect("create variant 1 dir");
        fs::write(v1.join("bin.bat"), "@echo hello").expect("write bin.bat");

        ver_dir
    }

    // -----------------------------------------------------------------------
    // copy_dir_contents tests
    // -----------------------------------------------------------------------

    #[test]
    fn selected_copy_preserves_includes_metadata_and_returns_exact_renamed_handles() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source/original/1");
        fs::create_dir_all(source.join(".rez/include")).unwrap();
        fs::write(source.join(".rez/include/helper.py"), "VALUE=42").unwrap();
        fs::write(source.join(".rez/include/helper.sha1"), "receipt").unwrap();
        fs::write(source.join("package.yaml"), "name: original\nversion: '1'\nvariants: [[a], [b], [c]]\nhashed_variants: false\ncustom: {marker: retained}\ncommands: |\n  env.MARKER.set('retained')\n").unwrap();
        for root in ["a", "b", "c"] {
            fs::create_dir_all(source.join(root)).unwrap();
            fs::write(source.join(root).join("payload"), root).unwrap();
        }
        let destination = temp.path().join("destination");
        let options = CopyOptions {
            variants: Some(vec![2]),
            destination_name: Some("renamed".into()),
            destination_version: Some(version::Version::new("2").unwrap()),
            config: Some(crate::config::RezConfig::default()),
            ..Default::default()
        };
        let package = candidate(&source, "original", "1");
        let result = copy_package(&package, &destination, &options).unwrap();
        assert_eq!(result.variant_indices, vec![(2, Some(0))]);
        assert_eq!(result.handles[0].1.variables.index, Some(0));
        assert_eq!(result.handles[0].1.variables.name, "renamed");
        assert_eq!(result.handles[0].1.variables.version.as_deref(), Some("2"));
        let root = destination.join("renamed/2");
        assert!(root.join("c/payload").is_file());
        assert!(!root.join("a").exists());
        assert!(!root.join("b").exists());
        assert_eq!(
            fs::read_to_string(root.join(".rez/include/helper.py")).unwrap(),
            "VALUE=42"
        );
        assert_eq!(
            fs::read_to_string(root.join(".rez/include/helper.sha1")).unwrap(),
            "receipt"
        );
        let reloaded = candidate(&root, "renamed", "2");
        assert_eq!(reloaded.package.attributes["custom"]["marker"], "retained");
        assert_eq!(reloaded.package.variants.len(), 1);
        let skipped = copy_package(&package, &destination, &options).unwrap();
        assert_eq!(skipped.skipped_indices, vec![2]);
        assert_eq!(skipped.handles, result.handles);
    }

    #[test]
    fn selected_copy_and_move_apply_policy_before_destination_writes_and_honor_force() {
        let temp = tempfile::tempdir().unwrap();
        let source = make_pkg(temp.path(), "fixed", "1");
        let mut package = candidate(&source, "fixed", "1");
        package
            .package
            .attributes
            .insert("relocatable".into(), serde_json::json!(false));
        package.package.relocatable = Some(false);
        let destination = temp.path().join("destination");
        let options = CopyOptions {
            config: Some(crate::config::RezConfig::default()),
            ..Default::default()
        };
        assert!(copy_package(&package, &destination, &options).is_err());
        assert!(move_package(&package, &destination, &options).is_err());
        assert!(!destination.exists());
        let options = CopyOptions {
            force: true,
            ..options
        };
        let moved = move_package(&package, &destination, &options).unwrap();
        assert_eq!(moved.variant_indices, vec![(0, None)]);
        assert!(source.join("payload.txt").is_file());
        assert!(crate::repository::is_package_ignored(
            temp.path(),
            "fixed",
            "1"
        ));
        assert!(move_package(&package, &destination, &options).is_err());
    }

    #[test]
    fn selected_copy_does_not_copy_unselected_nested_roots() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source/nested/1");
        fs::create_dir_all(source.join("a/b")).unwrap();
        fs::write(
            source.join("package.yaml"),
            "name: nested\nversion: '1'\nvariants: [[a], [a,b]]\nhashed_variants: false\n",
        )
        .unwrap();
        fs::write(source.join("a/parent"), "parent").unwrap();
        fs::write(source.join("a/b/child"), "child").unwrap();
        let destination = temp.path().join("destination");
        let options = CopyOptions {
            variants: Some(vec![0]),
            config: Some(crate::config::RezConfig::default()),
            ..Default::default()
        };
        let package = candidate(&source, "nested", "1");
        copy_package(&package, &destination, &options).unwrap();
        let root = destination.join("nested/1");
        assert!(root.join("a/parent").is_file());
        assert!(!root.join("a/b").exists());
        let child_options = CopyOptions {
            variants: Some(vec![1]),
            ..options.clone()
        };
        copy_package(&package, &destination, &child_options).unwrap();
        fs::write(root.join("a/unrelated"), "keep").unwrap();
        fs::write(source.join("a/parent"), "updated").unwrap();
        copy_package(
            &package,
            &destination,
            &CopyOptions {
                overwrite: true,
                ..options
            },
        )
        .unwrap();
        assert_eq!(fs::read_to_string(root.join("a/b/child")).unwrap(), "child");
        assert_eq!(
            fs::read_to_string(root.join("a/unrelated")).unwrap(),
            "keep"
        );
        assert_eq!(
            fs::read_to_string(root.join("a/parent")).unwrap(),
            "updated"
        );
        assert_eq!(candidate(&root, "nested", "1").package.variants.len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn copy_preserves_dangling_symlinks_and_follow_mode_materializes_files() {
        let temp = tempfile::tempdir().unwrap();
        let source = make_pkg(temp.path(), "links", "1");
        std::os::unix::fs::symlink("missing", source.join("dangling")).unwrap();
        std::os::unix::fs::symlink("payload.txt", source.join("linked")).unwrap();
        let destination = temp.path().join("destination");
        copy_package(
            &candidate(&source, "links", "1"),
            &destination,
            &CopyOptions::default(),
        )
        .unwrap();
        let root = destination.join("links/1");
        assert_eq!(
            fs::read_link(root.join("dangling")).unwrap(),
            PathBuf::from("missing")
        );
        assert_eq!(
            fs::read_link(root.join("linked")).unwrap(),
            PathBuf::from("payload.txt")
        );
        fs::remove_file(source.join("dangling")).unwrap();
        copy_package(
            &candidate(&source, "links", "1"),
            &destination,
            &CopyOptions {
                overwrite: true,
                follow_symlinks: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!fs::symlink_metadata(root.join("linked"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_to_string(root.join("linked")).unwrap(),
            "test payload data"
        );
    }

    #[test]
    fn test_copy_dir_contents_basic() {
        let root = test_dir("copy_dir_basic");
        let src = root.join("src");
        let dest = root.join("dest");

        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("a.txt"), "aaa").unwrap();
        fs::write(src.join("b.txt"), "bbb").unwrap();

        let copied = copy_dir_contents(&src, &dest, false, false, None, None).unwrap();
        assert_eq!(copied.len(), 2);
        assert!(dest.join("a.txt").exists());
        assert!(dest.join("b.txt").exists());
        assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "aaa");

        cleanup(&root);
    }

    #[test]
    fn test_copy_dir_contents_nested() {
        let root = test_dir("copy_dir_nested");
        let src = root.join("src");
        let dest = root.join("dest");

        let sub = src.join("sub").join("deep");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("file.txt"), "deep content").unwrap();
        fs::write(src.join("top.txt"), "top content").unwrap();

        let copied = copy_dir_contents(&src, &dest, false, false, None, None).unwrap();
        assert!(copied.len() >= 3); // sub, sub/deep, sub/deep/file.txt, top.txt
        assert!(dest.join("sub").join("deep").join("file.txt").exists());
        assert_eq!(
            fs::read_to_string(dest.join("sub").join("deep").join("file.txt")).unwrap(),
            "deep content"
        );

        cleanup(&root);
    }

    #[test]
    fn test_copy_dir_contents_not_a_dir() {
        let root = test_dir("copy_dir_not_dir");
        let file = root.join("file.txt");
        fs::write(&file, "x").unwrap();
        let dest = root.join("dest");

        let result = copy_dir_contents(&file, &dest, false, false, None, None);
        assert!(result.is_err());

        cleanup(&root);
    }

    #[test]
    fn test_copy_dir_preserve_timestamps() {
        let root = test_dir("copy_dir_timestamps");
        let src = root.join("src");
        let dest = root.join("dest");

        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("data.txt"), "hello").unwrap();

        // Copy with timestamp preservation
        let _ = copy_dir_contents(&src, &dest, false, true, None, None).unwrap();
        assert!(dest.join("data.txt").exists());

        cleanup(&root);
    }

    // -----------------------------------------------------------------------
    // safe_remove_dir tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_safe_remove_dir_exists() {
        let root = test_dir("safe_remove");
        let dir = root.join("target");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("f.txt"), "x").unwrap();

        safe_remove_dir(&dir).unwrap();
        assert!(!dir.exists());

        cleanup(&root);
    }

    #[test]
    fn test_safe_remove_dir_missing() {
        let root = test_dir("safe_remove_missing");
        let dir = root.join("nonexistent");
        // Should not error
        safe_remove_dir(&dir).unwrap();
        cleanup(&root);
    }

    // -----------------------------------------------------------------------
    // copy_package tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_copy_package_basic() {
        let root = test_dir("copy_pkg_basic");
        let src_repo = root.join("src_repo");
        let dest_repo = root.join("dest_repo");

        let src = make_pkg(&src_repo, "foo", "1.0.0");
        let dest = dest_repo.join("foo").join("1.0.0");

        let opts = CopyOptions::default();
        let result = copy_package(&candidate(&src, "foo", "1.0.0"), &dest_repo, &opts).unwrap();

        assert!(dest.join("package.yaml").exists());
        assert!(dest.join("payload.txt").exists());
        assert_eq!(
            fs::read_to_string(dest.join("payload.txt")).unwrap(),
            "test payload data"
        );
        assert!(!result.copied.is_empty());
        assert!(result.skipped.is_empty());

        cleanup(&root);
    }

    #[test]
    fn test_copy_package_skip_existing() {
        let root = test_dir("copy_pkg_skip");
        let src_repo = root.join("src_repo");
        let dest_repo = root.join("dest_repo");

        let src = make_pkg(&src_repo, "foo", "1.0.0");
        let dest = dest_repo.join("foo").join("1.0.0");
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("existing.txt"), "pre-existing").unwrap();
        fs::write(dest.join("package.yaml"), "name: foo\nversion: 1.0.0\n").unwrap();

        let opts = CopyOptions {
            overwrite: false,
            ..Default::default()
        };
        let result = copy_package(&candidate(&src, "foo", "1.0.0"), &dest_repo, &opts).unwrap();

        // Should skip because dest exists
        assert!(!result.skipped.is_empty());
        assert!(result.copied.is_empty());
        // Original file should still be there
        assert!(dest.join("existing.txt").exists());

        cleanup(&root);
    }

    #[test]
    fn test_copy_package_overwrite() {
        let root = test_dir("copy_pkg_overwrite");
        let src_repo = root.join("src_repo");
        let dest_repo = root.join("dest_repo");

        let src = make_pkg(&src_repo, "foo", "1.0.0");
        let dest = dest_repo.join("foo").join("1.0.0");
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("old.txt"), "old data").unwrap();

        let opts = CopyOptions {
            overwrite: true,
            ..Default::default()
        };
        let result = copy_package(&candidate(&src, "foo", "1.0.0"), &dest_repo, &opts).unwrap();

        // Reference copy replaces declared entries while retaining unrelated top-level payload.
        assert!(dest.join("old.txt").exists());
        // New files should be present
        assert!(dest.join("package.yaml").exists());
        assert!(!result.copied.is_empty());

        cleanup(&root);
    }

    #[test]
    fn test_copy_package_self() {
        let root = test_dir("copy_pkg_self");
        let src = make_pkg(&root, "foo", "1.0.0");

        let opts = CopyOptions::default();
        let result = copy_package(&candidate(&src, "foo", "1.0.0"), &root, &opts);
        assert!(result.is_err());

        cleanup(&root);
    }

    #[test]
    fn test_copy_package_missing_source() {
        let root = test_dir("copy_pkg_missing");
        let src = root.join("nonexistent");
        let dest_repo = root.join("dest");

        let opts = CopyOptions::default();
        let result = copy_package(&candidate(&src, "foo", "1.0.0"), &dest_repo, &opts);
        assert!(result.is_err());

        cleanup(&root);
    }

    #[test]
    fn test_copy_package_with_variants() {
        let root = test_dir("copy_pkg_variants");
        let src_repo = root.join("src_repo");
        let dest_repo = root.join("dest_repo");

        let src = make_pkg_with_variants(&src_repo, "bar", "2.0.0");
        let dest = dest_repo.join("bar").join("2.0.0");

        let opts = CopyOptions::default();
        let result = copy_package(&candidate(&src, "bar", "2.0.0"), &dest_repo, &opts).unwrap();

        assert!(dest.join("package.yaml").exists());
        assert!(dest.join("platform-linux").join("bin.sh").exists());
        assert!(dest.join("platform-windows").join("bin.bat").exists());
        assert_eq!(result.installed_indices, vec![0, 1]);

        cleanup(&root);
    }

    // -----------------------------------------------------------------------
    // copy_variant tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_copy_variant_basic() {
        let root = test_dir("copy_var_basic");
        let src = root.join("src_var");
        let dest = root.join("dest_var");

        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("lib.so"), "binary data").unwrap();
        fs::write(src.join("package.yaml"), "name: test").unwrap(); // should be skipped

        let opts = CopyOptions::default();
        let result = copy_variant(0, &src, &dest, &opts, None, None).unwrap();

        assert_eq!(result.variant_index, 0);
        assert!(dest.join("lib.so").exists());
        // package.yaml should NOT be copied (it's a definition file)
        assert!(!dest.join("package.yaml").exists());

        cleanup(&root);
    }

    #[test]
    fn test_copy_variant_missing_src() {
        let root = test_dir("copy_var_missing");
        let src = root.join("nope");
        let dest = root.join("dest");

        let opts = CopyOptions::default();
        let result = copy_variant(0, &src, &dest, &opts, None, None);
        assert!(result.is_err());

        cleanup(&root);
    }

    // -----------------------------------------------------------------------
    // move_package tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_move_package_same_fs() {
        let root = test_dir("move_pkg_same");
        let src_repo = root.join("src_repo");
        let dest_repo = root.join("dest_repo");

        let src = make_pkg(&src_repo, "foo", "1.0.0");
        let dest = dest_repo.join("foo").join("1.0.0");

        let result = move_package(
            &candidate(&src, "foo", "1.0.0"),
            &dest_repo,
            &CopyOptions::default(),
        )
        .unwrap();

        // Reference move retains the payload and hides the source.
        assert!(src.exists());
        assert!(crate::repository::is_package_ignored(
            &src_repo, "foo", "1.0.0"
        ));
        // Dest should have files
        assert!(dest.join("package.yaml").exists());
        assert!(dest.join("payload.txt").exists());
        assert!(!result.copied.is_empty());

        cleanup(&root);
    }

    #[test]
    fn test_move_package_dest_exists() {
        let root = test_dir("move_pkg_exists");
        let src_repo = root.join("src_repo");
        let dest_repo = root.join("dest_repo");

        let src = make_pkg(&src_repo, "foo", "1.0.0");
        let dest = dest_repo.join("foo").join("1.0.0");
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("package.yaml"), "name: foo\nversion: 1.0.0\n").unwrap();

        let result = move_package(
            &candidate(&src, "foo", "1.0.0"),
            &dest_repo,
            &CopyOptions::default(),
        );
        assert!(result.is_err());

        cleanup(&root);
    }

    #[test]
    fn test_move_package_missing_source() {
        let root = test_dir("move_pkg_missing");
        let src = root.join("nonexistent");
        let dest_repo = root.join("dest");

        let result = move_package(
            &candidate(&src, "foo", "1.0.0"),
            &dest_repo,
            &CopyOptions::default(),
        );
        assert!(result.is_err());

        cleanup(&root);
    }

    // -----------------------------------------------------------------------
    // remove_package tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_remove_rejects_path_traversal_and_keeps_sibling_data() {
        let root = test_dir("remove_path_traversal");
        let repo = root.join("repo");
        let sibling = root.join("1.0");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&sibling).unwrap();
        let marker = sibling.join("keep.txt");
        fs::write(&marker, "keep").unwrap();

        assert!(remove_package("..", "1.0", &repo).is_err());
        assert!(remove_package("safe", "1/0", &repo).is_err());
        assert!(remove_package_family("..", &repo, true).is_err());
        assert_eq!(fs::read_to_string(&marker).unwrap(), "keep");

        cleanup(&root);
    }

    #[test]
    fn test_remove_package_exists() {
        let root = test_dir("remove_pkg");
        let _ver_dir = make_pkg(&root, "foo", "1.0.0");

        let removed = remove_package("foo", "1.0.0", &root).unwrap();
        assert!(removed);

        // Version dir should be gone
        assert!(!root.join("foo").join("1.0.0").exists());
        // Family dir should also be cleaned up (was empty)
        assert!(!root.join("foo").exists());
    }

    #[test]
    fn test_remove_package_not_found() {
        let root = test_dir("remove_pkg_missing");
        fs::create_dir_all(&root).unwrap();

        let removed = remove_package("foo", "1.0.0", &root).unwrap();
        assert!(!removed);

        cleanup(&root);
    }

    #[test]
    fn test_remove_package_keeps_other_versions() {
        let root = test_dir("remove_pkg_keeps");
        make_pkg(&root, "foo", "1.0.0");
        make_pkg(&root, "foo", "2.0.0");

        let removed = remove_package("foo", "1.0.0", &root).unwrap();
        assert!(removed);
        assert!(!root.join("foo").join("1.0.0").exists());
        // Family dir should remain (has 2.0.0)
        assert!(root.join("foo").exists());
        assert!(root.join("foo").join("2.0.0").exists());

        cleanup(&root);
    }

    // -----------------------------------------------------------------------
    // remove_package_family tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_remove_family() {
        let root = test_dir("remove_family");
        make_pkg(&root, "foo", "1.0.0");
        make_pkg(&root, "foo", "2.0.0");
        make_pkg(&root, "foo", "3.0.0");

        let count = remove_package_family("foo", &root, true).unwrap();
        assert_eq!(count, Some(3));
        assert!(!root.join("foo").exists());

        cleanup(&root);
    }

    #[test]
    fn test_remove_family_requires_force_for_recognized_packages() {
        let root = test_dir("remove_family_requires_force");
        make_pkg(&root, "foo", "1.0.0");

        assert!(remove_package_family("foo", &root, false).is_err());
        assert!(root.join("foo").join("1.0.0").exists());
        assert_eq!(remove_package_family("foo", &root, true).unwrap(), Some(1));

        cleanup(&root);
    }

    #[test]
    fn test_remove_combined_family_removes_file_source() {
        let root = test_dir("remove_combined_family");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("foo.yaml"), "name: foo\nversions: ['1.0']\n").unwrap();

        assert!(remove_package_family("foo", &root, false).is_err());
        assert!(root.join("foo.yaml").exists());
        assert_eq!(remove_package_family("foo", &root, true).unwrap(), Some(1));
        assert!(!root.join("foo.yaml").exists());

        cleanup(&root);
    }

    #[test]
    fn test_remove_family_not_found() {
        let root = test_dir("remove_family_missing");
        fs::create_dir_all(&root).unwrap();

        let count = remove_package_family("nonexistent", &root, true).unwrap();
        assert_eq!(count, None);

        cleanup(&root);
    }

    #[test]
    fn test_remove_family_preserves_others() {
        let root = test_dir("remove_family_others");
        make_pkg(&root, "foo", "1.0.0");
        make_pkg(&root, "bar", "1.0.0");

        let count = remove_package_family("foo", &root, true).unwrap();
        assert_eq!(count, Some(1));
        assert!(!root.join("foo").exists());
        assert!(root.join("bar").exists());

        cleanup(&root);
    }

    // -----------------------------------------------------------------------
    // remove_if_empty tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_remove_if_empty_empty() {
        let root = test_dir("remove_empty");
        let dir = root.join("empty_dir");
        fs::create_dir_all(&dir).unwrap();

        assert!(remove_if_empty(&dir).unwrap());
        assert!(!dir.exists());

        cleanup(&root);
    }

    #[test]
    fn test_remove_if_empty_not_empty() {
        let root = test_dir("remove_not_empty");
        let dir = root.join("has_file");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("x.txt"), "x").unwrap();

        assert!(!remove_if_empty(&dir).unwrap());
        assert!(dir.exists());

        cleanup(&root);
    }

    // -----------------------------------------------------------------------
    // Edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_copy_package_empty_dir() {
        let root = test_dir("copy_pkg_empty");
        let src = root.join("src").join("empty").join("1.0.0");
        let dest_repo = root.join("dest");
        let dest = dest_repo.join("empty").join("1.0.0");
        fs::create_dir_all(&src).unwrap();

        let opts = CopyOptions::default();
        let result = copy_package(&candidate(&src, "empty", "1.0.0"), &dest_repo, &opts).unwrap();

        assert!(dest.join("package.yaml").exists());
        assert_eq!(result.installed_indices, vec![0]);

        cleanup(&root);
    }

    #[test]
    fn test_pkg_definition_names() {
        let names = pkg_definition_names().unwrap();
        assert!(names.contains(&"package.yaml".to_string()));
        assert!(names.contains(&"package.toml".to_string()));
        assert!(names.contains(&"package.py".to_string()));
    }

    #[test]
    fn test_cleanup_empty_parents() {
        let root = test_dir("cleanup_parents");
        let deep = root.join("a").join("b").join("c");
        fs::create_dir_all(&deep).unwrap();

        // Remove 'c' first so 'b' becomes empty
        fs::remove_dir(&deep).unwrap();
        cleanup_empty_parents(&deep).unwrap();

        // 'b' should be gone (was empty after 'c' removal)
        assert!(!root.join("a").join("b").exists());
        // 'a' should also be gone
        assert!(!root.join("a").exists());

        cleanup(&root);
    }

    #[cfg(unix)]
    #[test]
    fn copy_does_not_write_through_destination_links() {
        let owned = tempfile::tempdir().unwrap();
        let source = owned.path().join("source");
        let destination = owned.path().join("destination");
        let foreign = owned.path().join("foreign");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::create_dir(&foreign).unwrap();
        fs::write(source.join("file"), b"new").unwrap();
        fs::write(foreign.join("file"), b"keep").unwrap();
        std::os::unix::fs::symlink(foreign.join("file"), destination.join("file")).unwrap();
        copy_dir_contents(
            &source,
            &destination,
            false,
            false,
            Some(&destination),
            None,
        )
        .unwrap();
        assert_eq!(fs::read(destination.join("file")).unwrap(), b"new");
        assert!(!fs::symlink_metadata(destination.join("file"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(foreign.join("file")).unwrap(), b"keep");
        std::os::unix::fs::symlink(&foreign, destination.join("link")).unwrap();
        assert!(copy_dir_contents(
            &source,
            &destination.join("link/inner"),
            false,
            false,
            Some(&destination),
            None
        )
        .is_err());
        assert!(!foreign.join("inner").exists());
        fs::create_dir(source.join("link")).unwrap();
        fs::write(source.join("link/file"), b"wrong").unwrap();
        assert!(copy_dir_contents(
            &source,
            &destination,
            false,
            false,
            Some(&destination),
            None
        )
        .is_err());
        assert_eq!(fs::read(foreign.join("file")).unwrap(), b"keep");
    }

    #[test]
    fn copy_excludes_exact_nested_subtree_and_preserves_siblings() {
        let owned = tempfile::tempdir().unwrap();
        let source = owned.path().join("source");
        let destination = owned.path().join("destination");
        for relative in [
            "build/variant",
            "build/sibling",
            "excluded",
            "source/variant",
        ] {
            fs::create_dir_all(source.join(relative)).unwrap();
            fs::write(source.join(relative).join("payload"), relative).unwrap();
        }
        copy_dir_contents(
            &source,
            &destination,
            false,
            true,
            Some(owned.path()),
            Some(&[PathBuf::from("build/variant"), PathBuf::from("excluded")]),
        )
        .unwrap();
        assert!(!destination.join("build/variant").exists());
        assert!(!destination.join("excluded").exists());
        assert_eq!(
            fs::read_to_string(destination.join("build/sibling/payload")).unwrap(),
            "build/sibling"
        );
        assert_eq!(
            fs::read_to_string(destination.join("source/variant/payload")).unwrap(),
            "source/variant"
        );
    }

    #[test]
    fn copy_replacement_does_not_mutate_foreign_hardlinks() {
        let owned = tempfile::tempdir().unwrap();
        let source = owned.path().join("source");
        let destination = owned.path().join("destination");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        let foreign = owned.path().join("foreign");
        fs::write(&foreign, b"keep").unwrap();
        fs::write(source.join("file"), b"new").unwrap();
        fs::hard_link(&foreign, destination.join("file")).unwrap();
        copy_dir_contents(
            &source,
            &destination,
            false,
            false,
            Some(&destination),
            None,
        )
        .unwrap();
        assert_eq!(fs::read(destination.join("file")).unwrap(), b"new");
        assert_eq!(fs::read(foreign).unwrap(), b"keep");
    }

    #[cfg(unix)]
    #[test]
    fn canonical_copy_keeps_native_non_utf8_payload_paths() {
        use std::os::unix::ffi::OsStringExt;
        let owned = tempfile::tempdir().unwrap();
        let source = owned.path().join("source");
        let destination = owned.path().join("destination");
        fs::create_dir(&source).unwrap();
        let name = std::ffi::OsString::from_vec(vec![b'd', 0xff]);
        fs::create_dir(source.join(&name)).unwrap();
        let file = std::ffi::OsString::from_vec(vec![b'f', 0xfe]);
        fs::write(source.join(&name).join(&file), b"data").unwrap();
        let result = copy_variant(
            0,
            &source,
            &destination,
            &CopyOptions::default(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(result.copied, vec![PathBuf::from(&name)]);
        assert_eq!(
            fs::read(destination.join(&name).join(&file)).unwrap(),
            b"data"
        );
    }

    #[test]
    fn package_timestamp_policy_does_not_change_payload_filesystem_stats() {
        let owner = tempfile::tempdir().unwrap();
        let source_repo = owner.path().join("source");
        let source = make_pkg(&source_repo, "time_probe", "1.0");
        fs::write(
            source.join("package.yaml"),
            "name: time_probe\nversion: '1.0'\ntimestamp: 1\n",
        )
        .unwrap();
        let original = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(3600);
        fs::OpenOptions::new()
            .write(true)
            .open(source.join("payload.txt"))
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(original))
            .unwrap();
        for keep_timestamp in [false, true] {
            let destination = owner
                .path()
                .join(if keep_timestamp { "keep" } else { "reset" });
            copy_package(
                &candidate(&source, "time_probe", "1.0"),
                &destination,
                &CopyOptions {
                    keep_timestamp,
                    ..CopyOptions::default()
                },
            )
            .unwrap();
            let installed = destination.join("time_probe/1.0");
            assert_eq!(
                fs::metadata(installed.join("payload.txt"))
                    .unwrap()
                    .modified()
                    .unwrap(),
                original
            );
            let data =
                crate::repository::load_package_data(&installed.join("package.yaml")).unwrap();
            let timestamp = data
                .get("timestamp")
                .and_then(serde_json::Value::as_u64)
                .unwrap();
            if keep_timestamp {
                assert_eq!(timestamp, 1);
            } else {
                assert!(timestamp > 1);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn filesystem_stat_copy_preserves_directory_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let owner = tempfile::tempdir().unwrap();
        let source = owner.path().join("source");
        let destination = owner.path().join("destination");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(source.join("nested/file"), b"payload").unwrap();
        fs::set_permissions(source.join("nested"), fs::Permissions::from_mode(0o750)).unwrap();
        copy_dir_contents(&source, &destination, false, true, Some(owner.path()), None).unwrap();
        assert_eq!(
            fs::metadata(destination.join("nested"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o750
        );
    }
    #[cfg(windows)]
    #[test]
    fn copy_accepts_both_mixed_windows_authority_prefix_representations() {
        let owned = tempfile::tempdir().unwrap();
        let source = owned.path().join("source");
        let authority = owned.path().join("authority");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&authority).unwrap();
        fs::write(source.join("file"), b"data").unwrap();
        let canonical_authority = fs::canonicalize(&authority).unwrap();
        let canonical_destination =
            crate::util::directory(&authority, Path::new("canonical"), true).unwrap();
        copy_dir_contents(
            &source,
            &canonical_destination,
            false,
            true,
            Some(&authority),
            None,
        )
        .unwrap();
        assert_eq!(
            fs::read(canonical_destination.join("file")).unwrap(),
            b"data"
        );
        let raw_destination = authority.join("raw");
        copy_dir_contents(
            &source,
            &raw_destination,
            false,
            true,
            Some(&canonical_authority),
            None,
        )
        .unwrap();
        assert_eq!(fs::read(raw_destination.join("file")).unwrap(), b"data");
    }

    #[cfg(unix)]
    #[test]
    fn canonical_authority_does_not_follow_generated_destination_redirects() {
        let owned = tempfile::tempdir().unwrap();
        let authority = owned.path().join("authority");
        let source = owned.path().join("source");
        let foreign = owned.path().join("foreign");
        for directory in [&authority, &source, &foreign] {
            fs::create_dir(directory).unwrap();
        }
        fs::write(source.join("file"), b"incoming").unwrap();
        fs::write(foreign.join("file"), b"valuable").unwrap();
        std::os::unix::fs::symlink(&foreign, authority.join("redirect")).unwrap();
        let canonical_authority = fs::canonicalize(&authority).unwrap();
        assert!(copy_dir_contents(
            &source,
            &authority.join("redirect/nested"),
            false,
            true,
            Some(&canonical_authority),
            None
        )
        .is_err());
        assert_eq!(fs::read(foreign.join("file")).unwrap(), b"valuable");
        assert!(!foreign.join("nested").exists());
    }
}
