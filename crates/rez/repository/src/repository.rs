// SPDX-License-Identifier: Apache-2.0

//! Package repository system — filesystem and in-memory implementations.
//!
//! Core abstraction for rez package storage. Paths use `RezPath` internally; call `.to_os()` at fs boundary.
//!
//! # Main types
//! - `PackageRepository` — trait for get/iter operations
//! - `PackageInfo` — lightweight metadata (name, version, data map); used by discover, search, diff
//! - `FsRepo` — reads package.py/yaml/toml from disk via `serialise::load_from_file`
//! - `MemoryPackageRepository` — in-memory; used in tests and package building
//! - `PackageRepositoryManager` — multi-repo with priority (earlier path wins)
//!
//! # Package loading (single source of truth)
//! - `load_package_data(path)` — loads from file; delegates to `serialise::detect_format` + `serialise::load_from_file`
//! - `FsRepo::load_package_file` — calls `load_package_data` (no duplicate parsing)
//!
//! # Used by
//! - `package/discover` — `build_manager`, `build_repos` for repo iteration
//! - `FsRepo::load_package_file` — internal; loads package files when scanning repos
//! - `resolve` — `FilesystemPackageProvider` wraps repos
//! - `package/search` — iterates families and packages
//
// Example usage:
// ```rust
// use repository::repository::{MemoryPackageRepository, PackageRepository, PackageInfo};
// use std::collections::HashMap;
//
// let mut repo = MemoryPackageRepository::new();
// let mut data = HashMap::new();
// data.insert("name".into(), serde_json::json!("foo"));
// data.insert("version".into(), serde_json::json!("1.0.0"));
// let info = PackageInfo::from_data(data).unwrap();
// repo.add_package(info).unwrap();
//
// let version = "1.0.0".parse().unwrap();
// let pkg = repo.get_package("foo", &version).unwrap();
// assert!(pkg.is_some());
// ```

use crate::config::CONFIG;
use crate::errors::{Result, RezError};
use crate::{log_debug, log_info, log_trace};
use lru::LruCache;
use model::package::Package;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::UNIX_EPOCH;
use version::{Version, VersionRange};

// ============================================================================
// Core package info type used throughout the repository system
// ============================================================================

/// Minimal package info for repository operations.
///
/// This is the canonical PackageInfo type, used by repository, discover,
/// search, and other subsystems for lightweight package metadata.
#[derive(Debug, Clone)]
pub struct PackageInfo {
    pub name: String,
    pub version: Version,
    /// All package data as JSON-compatible map
    pub data: HashMap<String, serde_json::Value>,
}

impl PackageInfo {
    /// Create from raw package data
    pub fn from_data(data: HashMap<String, serde_json::Value>) -> Result<Self> {
        let name = data
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RezError::InvalidPackage("Missing 'name' field".into()))?
            .to_string();

        if !foundation::path::is_valid_rez_package_name(&name) {
            return Err(RezError::InvalidPackage(format!(
                "Not a valid package name: {name:?}"
            )));
        }

        let version_str = data.get("version").and_then(|v| v.as_str()).unwrap_or("");

        let version = if version_str.is_empty() {
            Version::empty() // unversioned package
        } else {
            version_str.parse()?
        };

        Ok(Self {
            name,
            version,
            data,
        })
    }

    /// Get field value from package data
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.data.get(key)
    }

    /// Convert repository metadata using the canonical package parser and validator.
    pub fn to_package(&self) -> Result<model::package::Package> {
        model::package::Package::from_data(self.data.clone())
    }
}

// ============================================================================
// PackageRepository trait
// ============================================================================

/// Base trait for all package repository implementations
pub trait PackageRepository: Send + Sync {
    /// Repository type identifier
    fn name(&self) -> &str;

    /// Repository location (path or URI)
    fn location(&self) -> &str;

    /// Unique identifier for this repository
    fn uid(&self) -> String {
        format!("{}@{}", self.name(), self.location())
    }

    /// Iterate all package family names
    fn iter_family_names(&self) -> Result<Vec<String>>;

    /// Check if package family exists
    fn has_family(&self, name: &str) -> bool {
        self.iter_family_names()
            .map(|names| names.contains(&name.to_string()))
            .unwrap_or(false)
    }

    /// Enumerate package versions in repository order.
    ///
    /// Filesystem repositories preserve source precedence and each source's
    /// native order: directory versions are ascending, while combined-family
    /// files retain their declared version order.
    fn iter_versions(&self, name: &str) -> Result<Vec<Version>>;

    /// Load package data for specific version
    fn get_package(&self, name: &str, version: &Version) -> Result<Option<PackageInfo>>;

    /// Load package data and the filesystem source that supplied it, when available.
    fn get_package_with_source(
        &self,
        name: &str,
        version: &Version,
        source: Option<&PackageSource>,
    ) -> Result<Option<(PackageInfo, Option<PackageSource>)>> {
        if source.is_some() {
            return Err(RezError::PackageRepository(
                "exact filesystem source selection is unsupported by this repository".into(),
            ));
        }
        Ok(self
            .get_package(name, version)?
            .map(|package| (package, None)))
    }

    /// Scan all package families and versions exposed by this repository.
    fn scan_all(&self) -> Result<HashMap<String, Vec<PackageInfo>>> {
        let mut packages = HashMap::new();
        for family in self.iter_family_names()? {
            let versions = self.iter_versions(&family)?;
            let family_packages = versions
                .into_iter()
                .filter_map(|version| match self.get_package(&family, &version) {
                    Ok(Some(package)) => Some(Ok(package)),
                    Ok(None) => None,
                    Err(error) => Some(Err(error)),
                })
                .collect::<Result<Vec<_>>>()?;
            packages.insert(family, family_packages);
        }
        Ok(packages)
    }

    /// Get variant count for a package (default: 0 = no variants)
    fn get_variant_count(&self, _name: &str, _version: &Version) -> Result<usize> {
        Ok(0)
    }

    /// Install a variant (for writable repos only, default: read-only error)
    fn install_variant(&self, _info: &PackageInfo, _variant_index: Option<usize>) -> Result<()> {
        Err(RezError::PackageRepository(
            "Repository is read-only".into(),
        ))
    }
}

// ============================================================================
// Persistent repo directory index (disk cache for family/version listings)
// ============================================================================

/// Timestamp precision returned by filesystem metadata, preserved in the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct DirectoryTimestamp {
    seconds: u64,
    nanoseconds: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct DirectoryEntryFingerprint {
    name: String,
    is_directory: bool,
    is_file: bool,
    is_symlink: bool,
    modified: Option<DirectoryTimestamp>,
    length: u64,
    #[serde(default)]
    package_definition: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct DirectoryFingerprint {
    modified: Option<DirectoryTimestamp>,
    entries: Vec<DirectoryEntryFingerprint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum FamilySourceKind {
    Directory,
    Python,
    Yaml,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PackageSourceKind {
    Directory,
    Python,
    Yaml,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PackageSource {
    pub kind: PackageSourceKind,
    /// Directory package root, or combined package definition file.
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct SourceFingerprint {
    kind: FamilySourceKind,
    path: String,
    modified: Option<DirectoryTimestamp>,
    length: u64,
    content_hash: Option<u64>,
    #[serde(default)]
    directory: Option<DirectoryFingerprint>,
}

#[derive(Debug, Clone)]
struct FamilySource {
    kind: FamilySourceKind,
    path: PathBuf,
}

impl FamilySource {
    fn package_source(&self, version: &Version) -> PackageSource {
        let (kind, path) = match self.kind {
            FamilySourceKind::Directory => (
                PackageSourceKind::Directory,
                self.path.join(version.to_string()),
            ),
            FamilySourceKind::Python => (PackageSourceKind::Python, self.path.clone()),
            FamilySourceKind::Yaml => (PackageSourceKind::Yaml, self.path.clone()),
        };
        PackageSource { kind, path }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SourceSetFingerprint {
    #[serde(default)]
    sources: Vec<SourceFingerprint>,
}

/// Cached family entry: ordered source-set fingerprint + ordered version list.
#[derive(Serialize, Deserialize)]
struct FamilyIndex {
    modified: Option<DirectoryTimestamp>,
    #[serde(default)]
    source: Option<SourceSetFingerprint>,
    versions: Vec<String>,
}

struct CombinedFamilyData {
    data: HashMap<String, serde_json::Value>,
    versions: Vec<Version>,
    has_versions: bool,
    version_overrides: Vec<(VersionRange, serde_json::Map<String, serde_json::Value>)>,
}

/// On-disk index of a single repository directory.
#[derive(Serialize, Deserialize)]
struct RepoIndex {
    repo_path: String,
    #[serde(default)]
    root_fingerprint: Option<DirectoryFingerprint>,
    families: HashMap<String, FamilyIndex>,
}

/// Get the directory modification timestamp without discarding subsecond precision.
fn path_is_directory(path: &Path) -> Result<bool> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(RezError::PackageRepository(format!(
            "Failed to inspect repository path {}: {}",
            path.display(),
            error
        ))),
    }
}

fn dir_mtime(path: &Path) -> Option<DirectoryTimestamp> {
    fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|duration| DirectoryTimestamp {
            seconds: duration.as_secs(),
            nanoseconds: duration.subsec_nanos(),
        })
}

fn metadata_timestamp(path: &Path, metadata: &fs::Metadata) -> Result<Option<DirectoryTimestamp>> {
    let modified = metadata.modified().map_err(|error| {
        RezError::PackageRepository(format!(
            "Failed to read modification time for {}: {}",
            path.display(),
            error
        ))
    })?;
    Ok(modified
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| DirectoryTimestamp {
            seconds: duration.as_secs(),
            nanoseconds: duration.subsec_nanos(),
        }))
}

fn directory_fingerprint(
    path: &Path,
    include_package_definitions: bool,
) -> Result<DirectoryFingerprint> {
    let metadata = fs::metadata(path).map_err(|error| {
        RezError::PackageRepository(format!(
            "Failed to inspect repository directory {}: {}",
            path.display(),
            error
        ))
    })?;
    if !metadata.is_dir() {
        return Err(RezError::PackageRepository(format!(
            "Expected a repository directory: {}",
            path.display()
        )));
    }

    let entries = fs::read_dir(path).map_err(|error| {
        RezError::PackageRepository(format!(
            "Failed to read repository directory {}: {}",
            path.display(),
            error
        ))
    })?;
    let mut fingerprints = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            RezError::PackageRepository(format!(
                "Failed to read an entry in repository directory {}: {}",
                path.display(),
                error
            ))
        })?;
        let entry_path = entry.path();
        let (entry_metadata, is_symlink) = match fs::symlink_metadata(&entry_path) {
            Ok(link_metadata) if link_metadata.file_type().is_symlink() => {
                let target_metadata = match fs::metadata(&entry_path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        link_metadata.clone()
                    }
                    Err(error) => {
                        return Err(RezError::PackageRepository(format!(
                            "Failed to inspect repository entry {}: {}",
                            entry_path.display(),
                            error
                        )));
                    }
                };
                (target_metadata, true)
            }
            Ok(metadata) => (metadata, false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(RezError::PackageRepository(format!(
                    "Failed to inspect repository entry {}: {}",
                    entry_path.display(),
                    error
                )));
            }
        };
        let package_definition = if include_package_definitions && entry_metadata.is_dir() {
            crate::serialise::find_package_definition_file(
                &entry_path,
                crate::serialise::PACKAGE_DEFINITION_EXTENSIONS,
            )?
            .and_then(|definition| {
                definition
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
        } else {
            None
        };
        fingerprints.push(DirectoryEntryFingerprint {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_directory: entry_metadata.is_dir(),
            is_file: entry_metadata.is_file(),
            is_symlink,
            modified: metadata_timestamp(&entry_path, &entry_metadata)?,
            length: entry_metadata.len(),
            package_definition,
        });
    }
    fingerprints.sort_by(|left, right| left.name.cmp(&right.name));

    Ok(DirectoryFingerprint {
        modified: metadata_timestamp(path, &metadata)?,
        entries: fingerprints,
    })
}

fn source_fingerprint(
    path: &Path,
    kind: FamilySourceKind,
    include_package_definitions: bool,
) -> Result<SourceFingerprint> {
    let metadata = fs::metadata(path).map_err(|error| {
        RezError::PackageRepository(format!(
            "Failed to inspect package source {}: {}",
            path.display(),
            error
        ))
    })?;
    let content_hash = if metadata.is_file() {
        let contents = fs::read(path).map_err(|error| {
            RezError::PackageRepository(format!(
                "Failed to read package source {}: {}",
                path.display(),
                error
            ))
        })?;
        Some(crate::util::stable_hash(&contents))
    } else {
        None
    };
    let directory = if metadata.is_dir() {
        Some(directory_fingerprint(path, include_package_definitions)?)
    } else {
        None
    };
    Ok(SourceFingerprint {
        kind,
        path: path.to_string_lossy().into_owned(),
        modified: metadata_timestamp(path, &metadata)?,
        length: metadata.len(),
        content_hash,
        directory,
    })
}

impl CombinedFamilyData {
    fn from_data(data: HashMap<String, serde_json::Value>, path: &Path) -> Result<Self> {
        let (has_versions, versions) = match data.get("versions") {
            None => (false, Vec::new()),
            Some(serde_json::Value::Array(values)) => {
                let versions = values
                    .iter()
                    .enumerate()
                    .map(|(index, value)| {
                        let value = value.as_str().ok_or_else(|| {
                            RezError::PackageRepository(format!(
                                "Combined package {} has a non-string version at versions[{index}]",
                                path.display()
                            ))
                        })?;
                        Version::new(value).map_err(|error| {
                            RezError::PackageRepository(format!(
                                "Combined package {} has invalid version {value:?}: {error}",
                                path.display()
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                (true, versions)
            }
            Some(_) => {
                return Err(RezError::PackageRepository(format!(
                    "Combined package {} field 'versions' must be an array",
                    path.display()
                )));
            }
        };

        let mut version_overrides = Vec::new();
        if let Some(value) = data.get("version_overrides") {
            let overrides = value.as_object().ok_or_else(|| {
                RezError::PackageRepository(format!(
                    "Combined package {} field 'version_overrides' must be a mapping",
                    path.display()
                ))
            })?;
            for (range, attributes) in overrides {
                let range_value = VersionRange::new(range).map_err(|error| {
                    RezError::PackageRepository(format!(
                        "Combined package {} has invalid version override range {range:?}: {error}",
                        path.display()
                    ))
                })?;
                let attributes = attributes.as_object().ok_or_else(|| {
                    RezError::PackageRepository(format!(
                        "Combined package {} override {range:?} must be a mapping",
                        path.display()
                    ))
                })?;
                version_overrides.push((range_value, attributes.clone()));
            }
        }

        Ok(Self {
            data,
            versions,
            has_versions,
            version_overrides,
        })
    }

    fn materialize(
        &self,
        version: &Version,
        allow_unversioned: bool,
    ) -> Result<Option<HashMap<String, serde_json::Value>>> {
        if self.versions.is_empty() {
            if !version.is_empty() || !allow_unversioned {
                return Ok(None);
            }
        } else if !self.versions.contains(version) {
            return Ok(None);
        }

        let mut data = self.data.clone();
        if self.has_versions {
            data.remove("versions");
            data.insert(
                "version".into(),
                serde_json::Value::String(version.to_string()),
            );
            if !self.version_overrides.is_empty() {
                for (range, attributes) in &self.version_overrides {
                    if range.contains_version(version) {
                        for (key, value) in attributes {
                            data.insert(key.clone(), value.clone());
                        }
                    }
                }
                data.remove("version_overrides");
            }
        }
        // Combined Rez resources have no payload base, even if source metadata
        // or a version override supplies one. Do not retain loader provenance.
        data.remove("base");
        Ok(Some(data))
    }
}

/// Directory for repo index files: ~/.rez/repo_index/
fn repo_index_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    let dir = PathBuf::from(home).join(".rez").join("repo_index");
    fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Cache file path for a given repo path (hex hash of canonical path).
fn repo_index_path(repo_path: &Path) -> Option<PathBuf> {
    let dir = repo_index_dir()?;
    let canonical = repo_path.to_string_lossy();
    let hash = crate::util::stable_hash(canonical.as_bytes());
    Some(dir.join(format!("{:016x}.json", hash)))
}

/// Load cached repo index; returns None if missing, corrupt, or the root changed.
fn load_repo_index(repo_path: &Path) -> Option<RepoIndex> {
    let path = repo_index_path(repo_path)?;
    let data = fs::read_to_string(&path).ok()?;
    let idx: RepoIndex = serde_json::from_str(&data).ok()?;
    // Missing metadata must disable reuse; it cannot be treated as a timestamp.
    let current_fingerprint = directory_fingerprint(repo_path, false).ok()?;
    if idx.root_fingerprint.as_ref() != Some(&current_fingerprint) {
        log_trace!(
            "repo",
            "repo index stale (root fingerprint changed or unavailable): {:?}",
            repo_path
        );
        return None;
    }
    log_trace!(
        "repo",
        "repo index loaded: {:?} ({} families)",
        repo_path,
        idx.families.len()
    );
    Some(idx)
}

/// Save repo index to disk. Silently ignores errors.
fn save_repo_index(idx: &RepoIndex) {
    if let Some(path) = repo_index_path(Path::new(&idx.repo_path)) {
        if let Ok(json) = serde_json::to_string(idx) {
            let _ = fs::write(&path, json);
            log_trace!("repo", "repo index saved: {:?}", path);
        }
    }
}

// ============================================================================
// FsRepo
// ============================================================================

/// Filesystem-based package repository
///
/// Structure:
/// ```text
/// location/
///   pkgA/
///     1.0.0/
///       package.yaml  (or package.toml)
///     1.1.0/
///       package.yaml
///   pkgB/
///     2.0.0/
///       package.yaml
/// ```
pub struct FsRepo {
    location: PathBuf,
}

impl FsRepo {
    /// Create a filesystem repository, creating its directory when absent.
    pub fn new(path: &Path) -> Result<Self> {
        Self::open(path, true)?.ok_or_else(|| {
            RezError::PackageRepository(format!(
                "Repository path unexpectedly remained absent after creation: {}",
                path.display()
            ))
        })
    }

    /// Open a filesystem repository, optionally creating its path.
    pub fn open(path: &Path, create_if_missing: bool) -> Result<Option<Self>> {
        log_debug!("repo", "Opening repository: {}", path.display());
        match fs::metadata(path) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(RezError::PackageRepository(format!(
                    "Repository path is not a directory: {}",
                    path.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !create_if_missing => {
                return Ok(None);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                log_info!("repo", "Creating repository path: {}", path.display());
                fs::create_dir_all(path).map_err(|error| {
                    RezError::PackageRepository(format!(
                        "Failed to create repository path {}: {}",
                        path.display(),
                        error
                    ))
                })?;
                let metadata = fs::metadata(path).map_err(|error| {
                    RezError::PackageRepository(format!(
                        "Failed to inspect created repository path {}: {}",
                        path.display(),
                        error
                    ))
                })?;
                if !metadata.is_dir() {
                    return Err(RezError::PackageRepository(format!(
                        "Repository path is not a directory: {}",
                        path.display()
                    )));
                }
            }
            Err(error) => {
                return Err(RezError::PackageRepository(format!(
                    "Failed to inspect repository path {}: {}",
                    path.display(),
                    error
                )));
            }
        }

        log_info!("repo", "opened repository: {}", path.display());
        Ok(Some(Self {
            location: path.to_path_buf(),
        }))
    }

    fn family_sources(&self, family_name: &str) -> Result<Vec<FamilySource>> {
        crate::serialise::validate_rez_package_path(family_name, None)?;
        let entries = fs::read_dir(&self.location).map_err(|error| {
            RezError::PackageRepository(format!(
                "Failed to read repository directory {}: {}",
                self.location.display(),
                error
            ))
        })?;
        let mut sources = Vec::new();
        let expected = [
            (family_name.to_owned(), FamilySourceKind::Directory, true),
            (format!("{family_name}.py"), FamilySourceKind::Python, false),
            (format!("{family_name}.yaml"), FamilySourceKind::Yaml, false),
        ];

        for entry in entries {
            let entry = entry?;
            let filename = entry.file_name();
            let Some((_, kind, expect_directory)) = expected
                .iter()
                .find(|(name, _, _)| filename == name.as_str())
            else {
                continue;
            };
            let path = entry.path();
            let metadata = entry.metadata()?;
            if (*expect_directory && metadata.is_dir())
                || (!*expect_directory && metadata.is_file())
            {
                sources.push(FamilySource { kind: *kind, path });
            }
        }
        sources.sort_by_key(|source| match source.kind {
            FamilySourceKind::Directory => 0,
            FamilySourceKind::Python => 1,
            FamilySourceKind::Yaml => 2,
        });
        Ok(sources)
    }

    fn family_fingerprint(&self, family_name: &str) -> Result<Option<SourceSetFingerprint>> {
        self.source_set_fingerprint(family_name, None, None)
    }

    fn package_fingerprint(
        &self,
        name: &str,
        version: &Version,
        source: Option<&PackageSource>,
    ) -> Result<Option<SourceSetFingerprint>> {
        self.source_set_fingerprint(name, Some(version), source)
    }

    fn source_set_fingerprint(
        &self,
        family_name: &str,
        version: Option<&Version>,
        selected_source: Option<&PackageSource>,
    ) -> Result<Option<SourceSetFingerprint>> {
        let sources: Vec<_> = self
            .family_sources(family_name)?
            .into_iter()
            .filter(|source| {
                selected_source.is_none_or(|selected| {
                    version.is_some_and(|version| source.package_source(version) == *selected)
                })
            })
            .collect();
        if sources.is_empty() {
            return Ok(None);
        }

        let mut fingerprints = sources
            .iter()
            .map(|source| {
                source_fingerprint(
                    &source.path,
                    source.kind,
                    source.kind == FamilySourceKind::Directory,
                )
            })
            .collect::<Result<Vec<_>>>()?;

        if let Some(version) = version {
            for source in &sources {
                if source.kind != FamilySourceKind::Directory {
                    continue;
                }
                let package_dir = source.path.join(version.to_string());
                match fs::metadata(&package_dir) {
                    Ok(metadata) if metadata.is_dir() => {
                        if let Some(path) = self.find_package_file(&package_dir)? {
                            fingerprints.push(source_fingerprint(&path, source.kind, false)?);
                        }
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(RezError::PackageRepository(format!(
                            "Failed to inspect package directory {}: {}",
                            package_dir.display(),
                            error
                        )));
                    }
                }
            }
        }

        Ok(Some(SourceSetFingerprint {
            sources: fingerprints,
        }))
    }

    fn load_combined_family(&self, source: &FamilySource) -> Result<CombinedFamilyData> {
        let data = self.load_package_file(&source.path)?;
        CombinedFamilyData::from_data(data, &source.path)
    }

    fn package_versions_for_source(
        &self,
        family_name: &str,
        source: &FamilySource,
        include_hidden: bool,
        include_ignored: bool,
    ) -> Result<Vec<Version>> {
        if source.kind != FamilySourceKind::Directory {
            let combined = self.load_combined_family(source)?;
            if combined.versions.is_empty() && CONFIG.allow_unversioned_packages {
                return Ok(vec![Version::empty()]);
            }
            return Ok(combined.versions);
        }

        let mut versions = Vec::new();
        for entry in fs::read_dir(&source.path)? {
            let entry = entry?;
            let path = entry.path();
            if !path_is_directory(&path)? || self.find_package_file(&path)?.is_none() {
                continue;
            }

            let version_name = entry.file_name().to_string_lossy().into_owned();
            if !include_hidden && (version_name.starts_with('.') || version_name.starts_with('_')) {
                continue;
            }
            if !include_ignored && is_package_ignored(&self.location, family_name, &version_name) {
                continue;
            }
            versions.push(version_name.parse()?);
        }
        versions.sort();
        Ok(versions)
    }

    /// Enumerate valid directory and combined-file families once per name.
    fn get_family_dirs(&self) -> Result<Vec<String>> {
        let entries: Vec<_> = fs::read_dir(&self.location)
            .map_err(|error| {
                RezError::PackageRepository(format!(
                    "Failed to read repository directory {}: {}",
                    self.location.display(),
                    error
                ))
            })?
            .collect::<std::io::Result<_>>()?;

        let mut names = BTreeSet::new();
        for entry in &entries {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path_is_directory(&path)? {
                if crate::serialise::validate_rez_package_path(&name, None).is_ok() {
                    names.insert(name);
                }
                continue;
            }

            if name == "settings.yaml" {
                continue;
            }
            let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
                continue;
            };
            if !matches!(extension, "py" | "yaml") {
                continue;
            }
            let Some(family_name) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            if crate::serialise::validate_rez_package_path(family_name, None).is_ok() {
                names.insert(family_name.to_owned());
            }
        }

        let families: Vec<String> = names.into_iter().collect();
        log_trace!(
            "repo",
            "scan {:?}: {} entries -> {} families",
            self.location,
            entries.len(),
            families.len()
        );
        Ok(families)
    }

    /// Enumerate the union of versions across sources in precedence order.
    /// The first source wins when multiple sources define the same version.
    fn get_version_dirs(&self, family_name: &str) -> Result<Vec<String>> {
        let mut versions = Vec::new();
        let mut seen = Vec::new();
        for source in self.family_sources(family_name)? {
            let source_versions =
                self.package_versions_for_source(family_name, &source, false, false)?;
            for version in source_versions {
                if !seen.contains(&version) {
                    seen.push(version.clone());
                    versions.push(version.to_string());
                }
            }
        }

        log_trace!(
            "repo",
            "family {}: {} version(s) across sources",
            family_name,
            versions.len()
        );
        Ok(versions)
    }

    /// Find the first configured package definition file in directory.
    fn find_package_file(&self, dir: &Path) -> Result<Option<PathBuf>> {
        crate::serialise::find_package_definition_file(
            dir,
            crate::serialise::PACKAGE_DEFINITION_EXTENSIONS,
        )
    }

    /// Load package data from file (delegates to load_package_data).
    fn load_package_file(&self, path: &Path) -> Result<HashMap<String, serde_json::Value>> {
        load_package_data(path)
    }

    /// Scan all packages in the repository in parallel using rayon.
    /// Uses persistent directory index to skip unchanged families.
    pub fn scan_all(&self) -> Result<HashMap<String, Vec<PackageInfo>>> {
        let cached_idx = load_repo_index(&self.location);
        let families = self.get_family_dirs()?;
        let root_fingerprint = directory_fingerprint(&self.location, false)?;
        log_debug!(
            "repo",
            "scan_all {:?}: {} families (index={})",
            self.location,
            families.len(),
            cached_idx.is_some()
        );

        // Build updated index while scanning
        let idx_families = std::sync::Mutex::new(HashMap::new());

        let results: Vec<Option<(String, Vec<PackageInfo>)>> = families
            .par_iter()
            .map(|family| -> Result<Option<(String, Vec<PackageInfo>)>> {
                let sources = self.family_sources(family)?;
                if sources.is_empty() {
                    return Err(RezError::PackageRepository(format!(
                        "Package family disappeared while scanning: {family}"
                    )));
                }
                let family_modified = sources
                    .iter()
                    .find(|source| source.kind == FamilySourceKind::Directory)
                    .and_then(|source| dir_mtime(&source.path));
                let fingerprint = self.family_fingerprint(family)?.ok_or_else(|| {
                    RezError::PackageRepository(format!(
                        "Package family disappeared while fingerprinting: {family}"
                    ))
                })?;

                let versions = cached_idx
                    .as_ref()
                    .and_then(|index| index.families.get(family.as_str()))
                    .filter(|entry| entry.source.as_ref() == Some(&fingerprint))
                    .map(|entry| {
                        log_trace!("repo", "  family {} (cached)", family);
                        entry.versions.clone()
                    })
                    .map(Ok)
                    .unwrap_or_else(|| self.get_version_dirs(family))?;

                idx_families.lock().unwrap().insert(
                    family.clone(),
                    FamilyIndex {
                        modified: family_modified,
                        source: Some(fingerprint),
                        versions: versions.clone(),
                    },
                );

                let packages: Vec<Option<PackageInfo>> = versions
                    .par_iter()
                    .map(|ver_str| {
                        let version = if ver_str.is_empty() {
                            Version::empty()
                        } else {
                            ver_str.parse::<Version>()?
                        };
                        self.get_package(family, &version)
                    })
                    .collect::<Result<Vec<_>>>()?;
                let pkgs: Vec<PackageInfo> = packages.into_iter().flatten().collect();
                if pkgs.is_empty() {
                    log_trace!("repo", "  family {}: 0 valid packages", family);
                    Ok(None)
                } else {
                    log_trace!("repo", "  family {}: {} version(s)", family, pkgs.len());
                    Ok(Some((family.clone(), pkgs)))
                }
            })
            .collect::<Result<Vec<_>>>()?;
        let results: Vec<(String, Vec<PackageInfo>)> = results.into_iter().flatten().collect();

        // Save updated index
        save_repo_index(&RepoIndex {
            repo_path: self.location.to_string_lossy().to_string(),
            root_fingerprint: Some(root_fingerprint),
            families: idx_families.into_inner().unwrap(),
        });

        let total: usize = results.iter().map(|(_, pkgs)| pkgs.len()).sum();
        log_info!(
            "repo",
            "scan_all {:?}: {} families, {} package(s)",
            self.location,
            results.len(),
            total
        );

        Ok(results.into_iter().collect())
    }
}

impl PackageRepository for FsRepo {
    fn name(&self) -> &str {
        "filesystem"
    }

    fn location(&self) -> &str {
        self.location.to_str().unwrap_or("")
    }

    fn scan_all(&self) -> Result<HashMap<String, Vec<PackageInfo>>> {
        FsRepo::scan_all(self)
    }

    fn iter_family_names(&self) -> Result<Vec<String>> {
        self.get_family_dirs()
    }

    fn iter_versions(&self, name: &str) -> Result<Vec<Version>> {
        self.get_version_dirs(name)?
            .iter()
            .map(|version| {
                if version.is_empty() {
                    Ok(Version::empty())
                } else {
                    version.parse()
                }
            })
            .collect()
    }

    fn get_package(&self, name: &str, version: &Version) -> Result<Option<PackageInfo>> {
        Ok(self
            .get_package_with_source(name, version, None)?
            .map(|(package, _)| package))
    }

    fn get_package_with_source(
        &self,
        name: &str,
        version: &Version,
        source: Option<&PackageSource>,
    ) -> Result<Option<(PackageInfo, Option<PackageSource>)>> {
        let version_str = version.to_string();
        crate::serialise::validate_rez_package_path(
            name,
            (!version_str.is_empty()).then_some(version_str.as_str()),
        )?;
        for family_source in self.family_sources(name)? {
            let origin = family_source.package_source(version);
            if source.is_some_and(|selected| *selected != origin) {
                continue;
            }
            let source = family_source;
            let (package, path, kind) = if source.kind == FamilySourceKind::Directory {
                let package_dir = source.path.join(&version_str);
                match fs::metadata(&package_dir) {
                    Ok(metadata) if metadata.is_dir() => {}
                    Ok(_) => continue,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                }
                if is_package_ignored(&self.location, name, &version_str) {
                    continue;
                }

                let Some(package_file) = self.find_package_file(&package_dir)? else {
                    continue;
                };
                let data = self.load_package_file(&package_file)?;
                (
                    PackageInfo::from_data(data)?,
                    package_dir,
                    PackageSourceKind::Directory,
                )
            } else {
                let combined = self.load_combined_family(&source)?;
                let Some(data) =
                    combined.materialize(version, CONFIG.allow_unversioned_packages)?
                else {
                    continue;
                };
                let kind = match source.kind {
                    FamilySourceKind::Python => PackageSourceKind::Python,
                    FamilySourceKind::Yaml => PackageSourceKind::Yaml,
                    FamilySourceKind::Directory => unreachable!(),
                };
                (PackageInfo::from_data(data)?, source.path.clone(), kind)
            };

            return Ok(Some((package, Some(PackageSource { kind, path }))));
        }
        Ok(None)
    }
}

// ============================================================================
// Cached Filesystem Repository
// ============================================================================

/// Package cache mode driven by resource_caching_maxsize:
/// - 0: disabled (no caching)
/// - -1: unbounded (HashMap)
/// - N>0: LRU with max N entries
#[derive(Clone)]
struct CachedPackage {
    fingerprint: SourceSetFingerprint,
    origin: PackageSource,
    info: PackageInfo,
}

#[derive(Serialize, Deserialize)]
struct MemcachedPackage {
    data: HashMap<String, serde_json::Value>,
    origin: PackageSource,
}

enum PackageCacheStorage {
    Disabled,
    Unbounded(HashMap<(String, String, Option<PackageSource>), CachedPackage>),
    Lru(LruCache<(String, String, Option<PackageSource>), CachedPackage>),
}

/// Cached wrapper around FsRepo.
/// Thread-safe (RwLock) for concurrent read access.
/// Package cache respects config.resource_caching_maxsize (-1=unlimited, 0=off, N=LRU size).
pub struct FsRepoCached {
    inner: FsRepo,
    families_cache: RwLock<Option<(DirectoryFingerprint, Vec<String>)>>,
    versions_cache: RwLock<HashMap<String, (SourceSetFingerprint, Vec<Version>)>>,
    package_cache: RwLock<PackageCacheStorage>,
}

impl FsRepoCached {
    /// Create a new cached repository.
    /// Uses CONFIG.resource_caching_maxsize for package cache: -1=unlimited, 0=disabled, N=LRU size.
    pub fn new(path: &Path, create_if_missing: bool) -> Result<Option<Self>> {
        let Some(inner) = FsRepo::open(path, create_if_missing)? else {
            return Ok(None);
        };
        let maxsize = CONFIG.resource_caching_maxsize;
        let package_cache = match maxsize {
            0 => PackageCacheStorage::Disabled,
            -1 => PackageCacheStorage::Unbounded(HashMap::new()),
            n if n > 0 => {
                let cap = NonZeroUsize::new(n as usize).unwrap_or(NonZeroUsize::new(1).unwrap());
                PackageCacheStorage::Lru(LruCache::new(cap))
            }
            _ => PackageCacheStorage::Unbounded(HashMap::new()),
        };
        Ok(Some(Self {
            inner,
            families_cache: RwLock::new(None),
            versions_cache: RwLock::new(HashMap::new()),
            package_cache: RwLock::new(package_cache),
        }))
    }

    /// Invalidate all caches
    pub fn invalidate(&self) {
        *self.families_cache.write().unwrap() = None;
        self.versions_cache.write().unwrap().clear();
        let mut cache = self.package_cache.write().unwrap();
        match &mut *cache {
            PackageCacheStorage::Disabled => {}
            PackageCacheStorage::Unbounded(m) => m.clear(),
            PackageCacheStorage::Lru(lru) => lru.clear(),
        }
    }
}

impl PackageRepository for FsRepoCached {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn location(&self) -> &str {
        self.inner.location()
    }

    fn iter_family_names(&self) -> Result<Vec<String>> {
        let fingerprint = directory_fingerprint(&self.inner.location, false)?;
        if let Some((cached_fingerprint, families)) = self.families_cache.read().unwrap().as_ref() {
            if cached_fingerprint == &fingerprint {
                return Ok(families.clone());
            }
        }

        let families = self.inner.iter_family_names()?;
        *self.families_cache.write().unwrap() = Some((fingerprint, families.clone()));
        Ok(families)
    }

    fn iter_versions(&self, name: &str) -> Result<Vec<Version>> {
        let source = self.inner.family_fingerprint(name)?;
        if let Some(ref fingerprint) = source {
            if let Some((cached_source, versions)) = self.versions_cache.read().unwrap().get(name) {
                if cached_source == fingerprint {
                    return Ok(versions.clone());
                }
            }
        }

        let versions = self.inner.iter_versions(name)?;
        if let Some(fingerprint) = source {
            self.versions_cache
                .write()
                .unwrap()
                .insert(name.to_string(), (fingerprint, versions.clone()));
        }
        Ok(versions)
    }

    fn get_package(&self, name: &str, version: &Version) -> Result<Option<PackageInfo>> {
        Ok(self
            .get_package_with_source(name, version, None)?
            .map(|(package, _)| package))
    }

    fn get_package_with_source(
        &self,
        name: &str,
        version: &Version,
        source: Option<&PackageSource>,
    ) -> Result<Option<(PackageInfo, Option<PackageSource>)>> {
        let key = (name.to_string(), version.to_string(), source.cloned());
        let fingerprint = self.inner.package_fingerprint(name, version, source)?;

        if let Some(ref fingerprint) = fingerprint {
            let mut cache = self.package_cache.write().unwrap();
            let cached = match &mut *cache {
                PackageCacheStorage::Disabled => None,
                PackageCacheStorage::Unbounded(packages) => packages.get(&key),
                PackageCacheStorage::Lru(packages) => packages.get(&key),
            };
            if let Some(cached) = cached.filter(|cached| &cached.fingerprint == fingerprint) {
                return Ok(Some((cached.info.clone(), Some(cached.origin.clone()))));
            }
        }

        let Some((info, Some(origin))) =
            self.inner.get_package_with_source(name, version, source)?
        else {
            return Ok(None);
        };
        if let Some(fingerprint) = fingerprint {
            let cached = CachedPackage {
                fingerprint,
                origin: origin.clone(),
                info: info.clone(),
            };
            let mut cache = self.package_cache.write().unwrap();
            match &mut *cache {
                PackageCacheStorage::Disabled => {}
                PackageCacheStorage::Unbounded(packages) => {
                    packages.insert(key, cached);
                }
                PackageCacheStorage::Lru(packages) => {
                    packages.put(key, cached);
                }
            }
        }
        Ok(Some((info, Some(origin))))
    }
}

// ============================================================================
// Memcached-backed repository (when memcached_uri set + cache_listdir/cache_package_files)
// ============================================================================

/// Repository wrapper that uses Memcached for listdir and package data when configured.
pub struct FsRepoMemcached {
    inner: FsRepoCached,
    memcache: MemcacheClient,
    repo_path: PathBuf,
    cache_listdir: bool,
    cache_package_files: bool,
}

impl FsRepoMemcached {
    const TTL: u32 = 3600; // 1 hour

    pub fn new(
        path: &Path,
        memcache: MemcacheClient,
        cache_listdir: bool,
        cache_package_files: bool,
        create_if_missing: bool,
    ) -> Result<Option<Self>> {
        let Some(inner) = FsRepoCached::new(path, create_if_missing)? else {
            return Ok(None);
        };
        Ok(Some(Self {
            inner,
            memcache,
            repo_path: path.to_path_buf(),
            cache_listdir,
            cache_package_files,
        }))
    }
}

impl PackageRepository for FsRepoMemcached {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn location(&self) -> &str {
        self.inner.location()
    }

    fn iter_family_names(&self) -> Result<Vec<String>> {
        let cache_key = if self.cache_listdir && self.memcache.is_enabled() {
            let fingerprint =
                source_fingerprint(&self.repo_path, FamilySourceKind::Directory, false)?;
            cache_key_for_source(&cache_key_for_listdir(&self.repo_path), &fingerprint)
        } else {
            None
        };
        if let Some(key) = cache_key.as_deref() {
            if let Some(value) = self.memcache.get(key) {
                if let Ok(families) = serde_json::from_str::<Vec<String>>(&value) {
                    return Ok(families);
                }
            }
        }

        let families = self.inner.iter_family_names()?;
        if let Some(key) = cache_key.as_deref() {
            if let Ok(json) = serde_json::to_string(&families) {
                let _ = self.memcache.set(key, &json, Self::TTL);
            }
        }
        Ok(families)
    }

    fn iter_versions(&self, name: &str) -> Result<Vec<Version>> {
        let cache_key = if self.cache_listdir && self.memcache.is_enabled() {
            self.inner
                .inner
                .family_fingerprint(name)?
                .as_ref()
                .and_then(|fingerprint| {
                    cache_key_for_source(
                        &cache_key_for_versions(&self.repo_path, name),
                        fingerprint,
                    )
                })
        } else {
            None
        };
        if let Some(key) = cache_key.as_deref() {
            if let Some(value) = self.memcache.get(key) {
                if let Ok(strings) = serde_json::from_str::<Vec<String>>(&value) {
                    let parsed = strings
                        .iter()
                        .map(|version| {
                            if version.is_empty() {
                                Ok(Version::empty())
                            } else {
                                version.parse()
                            }
                        })
                        .collect::<Result<Vec<_>>>();
                    if let Ok(versions) = parsed {
                        return Ok(versions);
                    }
                }
            }
        }

        let versions = self.inner.iter_versions(name)?;
        if let Some(key) = cache_key.as_deref() {
            let strings: Vec<String> = versions.iter().map(ToString::to_string).collect();
            if let Ok(json) = serde_json::to_string(&strings) {
                let _ = self.memcache.set(key, &json, Self::TTL);
            }
        }
        Ok(versions)
    }

    fn get_package(&self, name: &str, version: &Version) -> Result<Option<PackageInfo>> {
        Ok(self
            .get_package_with_source(name, version, None)?
            .map(|(package, _)| package))
    }

    fn get_package_with_source(
        &self,
        name: &str,
        version: &Version,
        source: Option<&PackageSource>,
    ) -> Result<Option<(PackageInfo, Option<PackageSource>)>> {
        let version_string = version.to_string();
        crate::serialise::validate_rez_package_path(
            name,
            (!version_string.is_empty()).then_some(version_string.as_str()),
        )?;
        let cache_key = if self.cache_package_files && self.memcache.is_enabled() {
            self.inner
                .inner
                .package_fingerprint(name, version, source)?
                .as_ref()
                .and_then(|fingerprint| {
                    cache_key_for_source(
                        &cache_key_for_package(&self.repo_path, name, &version_string),
                        &(fingerprint, source),
                    )
                })
        } else {
            None
        };
        if let Some(key) = cache_key.as_deref() {
            if let Some(value) = self.memcache.get(key) {
                if let Ok(cached) = serde_json::from_str::<MemcachedPackage>(&value) {
                    if let Ok(info) = PackageInfo::from_data(cached.data) {
                        return Ok(Some((info, Some(cached.origin))));
                    }
                }
            }
        }

        let package = self.inner.get_package_with_source(name, version, source)?;
        if let (Some(key), Some((info, Some(origin)))) = (cache_key.as_deref(), package.as_ref()) {
            let cached = MemcachedPackage {
                data: info.data.clone(),
                origin: origin.clone(),
            };
            if let Ok(json) = serde_json::to_string(&cached) {
                let _ = self.memcache.set(key, &json, Self::TTL);
            }
        }
        Ok(package)
    }
}

// ============================================================================
// MemoryPackageRepository
// ============================================================================

/// In-memory package repository for testing and package building
///
/// Structure:
/// ```text
/// {
///   "foo": {
///     "1.0.0": { "name": "foo", "version": "1.0.0", ... },
///     "1.1.0": { ... }
///   },
///   "bar": {
///     "2.0.0": { ... }
///   }
/// }
/// ```
pub struct MemoryPackageRepository {
    location: String,
    /// Package data: family_name -> version_str -> package_data
    data: HashMap<String, HashMap<String, HashMap<String, serde_json::Value>>>,
}

impl MemoryPackageRepository {
    /// Create new empty in-memory repository
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let location = format!("memory@{}", id);
        Self {
            location,
            data: HashMap::new(),
        }
    }

    /// Add package to repository
    pub fn add_package(&mut self, info: PackageInfo) -> Result<()> {
        let version_str = if info.version.is_empty() {
            "_NO_VERSION".to_string()
        } else {
            info.version.to_string()
        };

        let family = self.data.entry(info.name.clone()).or_default();
        family.insert(version_str, info.data);

        Ok(())
    }

    /// Create repository with initial data
    pub fn with_data(
        data: HashMap<String, HashMap<String, HashMap<String, serde_json::Value>>>,
    ) -> Self {
        let location = format!("memory@{:p}", &data);
        Self { location, data }
    }
}

impl Default for MemoryPackageRepository {
    fn default() -> Self {
        Self::new()
    }
}

impl PackageRepository for MemoryPackageRepository {
    fn name(&self) -> &str {
        "memory"
    }

    fn location(&self) -> &str {
        &self.location
    }

    fn iter_family_names(&self) -> Result<Vec<String>> {
        let mut names: Vec<String> = self.data.keys().cloned().collect();
        names.sort();
        Ok(names)
    }

    fn iter_versions(&self, name: &str) -> Result<Vec<Version>> {
        let family = match self.data.get(name) {
            Some(f) => f,
            None => return Ok(Vec::new()),
        };

        let mut versions: Vec<Version> = family
            .keys()
            .filter(|key| key.as_str() != "_NO_VERSION")
            .map(|version| version.parse())
            .collect::<Result<_>>()?;

        versions.sort();
        Ok(versions)
    }

    fn get_package(&self, name: &str, version: &Version) -> Result<Option<PackageInfo>> {
        let family = match self.data.get(name) {
            Some(f) => f,
            None => return Ok(None),
        };

        let version_str = if version.is_empty() {
            "_NO_VERSION"
        } else {
            &version.to_string()
        };

        let pkg_data = match family.get(version_str) {
            Some(d) => d.clone(),
            None => return Ok(None),
        };

        let info = PackageInfo::from_data(pkg_data)?;
        Ok(Some(info))
    }
}

// ============================================================================
// PackageRepositoryManager
// ============================================================================

/// Singleton managing all package repositories
pub struct PackageRepositoryManager {
    repos: Vec<Box<dyn PackageRepository>>,
}

impl PackageRepositoryManager {
    /// Open existing repositories from configured package search paths.
    ///
    /// Missing paths are absent repositories; invalid or unreadable paths are errors.
    pub fn from_config(config: &crate::config::RezConfig) -> Result<Self> {
        Self::from_paths(&config.expanded_packages_path_os())
    }

    /// Open existing repositories from package search paths.
    ///
    /// Missing paths are absent repositories; invalid or unreadable paths are errors.
    pub fn from_paths(paths: &[PathBuf]) -> Result<Self> {
        let mut repos: Vec<Box<dyn PackageRepository>> = Vec::with_capacity(paths.len());

        for path in paths {
            if let Some(repo) = FsRepo::open(path, false).map_err(|error| {
                RezError::PackageRepository(format!(
                    "Failed to open configured repository {}: {error}",
                    path.display()
                ))
            })? {
                repos.push(Box::new(repo));
            }
        }

        Ok(Self { repos })
    }

    /// Create empty manager
    pub fn new() -> Self {
        Self { repos: Vec::new() }
    }

    /// Add repository
    pub fn add_repo(&mut self, repo: Box<dyn PackageRepository>) {
        self.repos.push(repo);
    }

    /// Scan repositories in search order, keeping the first package for each identity.
    pub fn scan_all(&self) -> Result<Vec<PackageInfo>> {
        let results: Vec<_> = self
            .repos
            .par_iter()
            .map(|repo| {
                repo.scan_all().map_err(|error| {
                    RezError::PackageRepository(format!(
                        "Failed to scan package repository {}: {error}",
                        repo.location()
                    ))
                })
            })
            .collect::<Result<_>>()?;

        let mut seen = std::collections::HashSet::new();
        let mut packages = Vec::new();
        for families in results {
            for family_packages in families.into_values() {
                for package in family_packages {
                    let identity = (package.name.clone(), package.version.clone());
                    if seen.insert(identity) {
                        packages.push(package);
                    }
                }
            }
        }
        Ok(packages)
    }

    /// Get repository by location
    pub fn get_repo(&self, location: &str) -> Option<&dyn PackageRepository> {
        self.repos
            .iter()
            .find(|r| r.location() == location)
            .map(|b| &**b)
    }

    /// Iterate all package family names across repositories (deduplicated).
    pub fn iter_family_names(&self) -> Result<Vec<String>> {
        let mut all_names = std::collections::HashSet::new();
        for repo in &self.repos {
            let names = repo.iter_family_names().map_err(|error| {
                RezError::PackageRepository(format!(
                    "Failed to list package families in repository {}: {error}",
                    repo.location()
                ))
            })?;
            all_names.extend(names);
        }
        let mut result: Vec<String> = all_names.into_iter().collect();
        result.sort();
        Ok(result)
    }

    /// Iterate packages across all repositories (first match wins)
    pub fn iter_packages(&self, name: &str) -> Result<Vec<PackageInfo>> {
        let mut seen_versions = std::collections::HashSet::new();
        let mut packages = Vec::new();

        for repo in &self.repos {
            let versions = repo.iter_versions(name).map_err(|error| {
                RezError::PackageRepository(format!(
                    "Failed to list versions for package family '{}' in repository {}: {error}",
                    name,
                    repo.location()
                ))
            })?;

            for version in versions {
                let version_str = version.to_string();

                // Skip if already seen in earlier repo
                if seen_versions.contains(&version_str) {
                    continue;
                }

                if let Some(pkg) = repo.get_package(name, &version).map_err(|error| {
                    RezError::PackageRepository(format!(
                        "Failed to load package '{}-{}' from repository {}: {error}",
                        name,
                        version,
                        repo.location()
                    ))
                })? {
                    seen_versions.insert(version_str);
                    packages.push(pkg);
                }
            }
        }

        // Sort by version descending (latest first)
        packages.sort_by(|a, b| b.version.cmp(&a.version));

        Ok(packages)
    }

    /// Get latest version of a package
    pub fn get_latest(&self, name: &str) -> Result<Option<PackageInfo>> {
        let packages = self.iter_packages(name)?;
        Ok(packages.into_iter().next())
    }

    /// Get specific package version from first repo that has it
    pub fn get_package(&self, name: &str, version: &Version) -> Result<Option<PackageInfo>> {
        for repo in &self.repos {
            if let Some(pkg) = repo.get_package(name, version)? {
                return Ok(Some(pkg));
            }
        }
        Ok(None)
    }
}

impl Default for PackageRepositoryManager {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Helper functions
// ============================================================================

/// Load package data from a file. **Delegates to serialise** — single source of parsing.
/// Supports .py, .yaml, .yml, .toml. Used by `FsRepo::load_package_file`.
/// Directory-package provenance is attached before canonical parsing can evaluate
/// deferred fields. A definition cannot override its filesystem base; package
/// serialization removes this ephemeral field. Combined-family materialization
/// discards it because Rez combined resources have no base.
pub fn load_package_data(path: &Path) -> Result<HashMap<String, serde_json::Value>> {
    use crate::serialise::{
        detect_format, load_from_file, validate_package_data, FileFormat, PackageDataCache,
    };

    let format = detect_format(path).ok_or_else(|| {
        RezError::PackageRepository(format!(
            "Unsupported package file format: {}",
            path.display()
        ))
    })?;

    if format == FileFormat::Txt {
        return Err(RezError::PackageRepository(format!(
            "Text format is not loadable as package data: {}",
            path.display()
        )));
    }

    let mut data = load_from_file(path, format, PackageDataCache::Enabled)?;
    let base = path.parent().ok_or_else(|| {
        RezError::PackageRepository(format!(
            "Package definition has no parent directory: {}",
            path.display()
        ))
    })?;
    data.insert(
        "base".into(),
        serde_json::Value::String(base.to_string_lossy().into_owned()),
    );
    validate_package_data(&data)?;
    Ok(data)
}

// ============================================================================
// Repository Settings (MEGAPLAN 3.7)
// ============================================================================

/// Repository-level settings loaded from settings.yaml
///
/// These settings override global config for a specific repository.
///
/// Example settings.yaml:
/// ```yaml
/// file_lock_timeout: 60
/// package_filenames: ["package.toml", "package.yaml"]
/// build_directory: "_build"
/// default_relocatable: true
/// name: "Production Packages"
/// disable_memcache: false
/// ```
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct RepoSettings {
    /// Override file lock timeout for this repo (seconds)
    pub file_lock_timeout: Option<u64>,
    /// Override package filenames to search
    pub package_filenames: Option<Vec<String>>,
    /// Override build directory
    pub build_directory: Option<String>,
    /// Whether packages in this repo are relocatable by default
    pub default_relocatable: Option<bool>,
    /// Custom repository name/label
    pub name: Option<String>,
    /// Whether to disable memcache for this repo
    pub disable_memcache: Option<bool>,
}

/// Load repository settings from settings.yaml if present
///
/// Looks for settings.yaml in the repository root directory.
/// Returns None if file doesn't exist or can't be parsed.
///
/// # Arguments
/// * `repo_path` - Repository root path
///
/// # Returns
/// RepoSettings if file exists and is valid, None otherwise
///
/// # Example
/// ```rust,no_run
/// use std::path::Path;
/// use repository::repository::load_repo_settings;
///
/// let repo = Path::new("/packages");
/// if let Some(settings) = load_repo_settings(repo) {
///     if let Some(timeout) = settings.file_lock_timeout {
///         println!("Lock timeout: {}s", timeout);
///     }
/// }
/// ```
pub fn load_repo_settings(repo_path: &Path) -> Option<RepoSettings> {
    let settings_path = repo_path.join("settings.yaml");

    if !settings_path.exists() {
        return None;
    }

    let content = fs::read_to_string(&settings_path).ok()?;
    serde_yaml::from_str(&content).ok()
}

// ============================================================================
// Memcache Integration
// ============================================================================
//
// MemcacheClient is used by: (1) FsRepoMemcached for package/listdir caching during
// resolution when memcached_uri is set; (2) `rez memcache` CLI (flush, stats, warm, poll).

// Rez's vendored memcache client uses `_SOCKET_TIMEOUT = 3` seconds.
const MEMCACHE_SOCKET_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// A normalized memcached endpoint shared by cache and administrative operations.
#[derive(Clone, Debug, PartialEq, Eq)]
enum MemcacheTransport {
    Tcp {
        host: String,
        port: u16,
    },
    #[cfg(unix)]
    Unix {
        path: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MemcacheServer {
    configured_uri: String,
    client_url: String,
    transport: MemcacheTransport,
}

fn memcache_url(uri: &str) -> Result<MemcacheServer> {
    let configured_uri = uri.trim();
    if configured_uri.is_empty() {
        return Err(RezError::Config(
            "memcached_uri contains an empty server URI".into(),
        ));
    }

    if let Some(path) = configured_uri.strip_prefix("unix:") {
        if path.is_empty() {
            return Err(RezError::Config(format!(
                "invalid memcache server URI {:?}: Unix socket path is empty",
                configured_uri
            )));
        }
        #[cfg(not(unix))]
        return Err(RezError::Config(format!(
            "memcache server URI {:?} uses a Unix socket, unsupported on this platform",
            configured_uri
        )));
        #[cfg(unix)]
        return Ok(MemcacheServer {
            configured_uri: configured_uri.to_string(),
            client_url: format!("memcache+unix:{}", path),
            transport: MemcacheTransport::Unix {
                path: path.to_string(),
            },
        });
    }

    let (authority, query, explicit_ipv6) =
        if let Some(url) = configured_uri.strip_prefix("memcache://") {
            if url.contains('#') || url.split_once('/').is_some() {
                return Err(RezError::Config(format!(
                    "invalid memcache server URI {:?}: paths and fragments are unsupported",
                    configured_uri
                )));
            }
            let (authority, query) = url.split_once('?').unwrap_or((url, ""));
            if query.split('&').any(|part| part == "udp=true") {
                return Err(RezError::Config(format!(
                    "invalid memcache server URI {:?}: Rez memcached_uri uses TCP streams, not UDP",
                    configured_uri
                )));
            }
            (authority, query, authority.starts_with('['))
        } else if let Some(authority) = configured_uri.strip_prefix("inet6:") {
            (authority, "", true)
        } else if let Some(authority) = configured_uri.strip_prefix("inet:") {
            (authority, "", false)
        } else {
            (configured_uri, "", false)
        };

    let (host, port, ipv6) = if explicit_ipv6 {
        parse_memcache_ipv6_authority(authority, configured_uri)?
    } else {
        parse_memcache_host_authority(authority, configured_uri)?
    };
    let host_port = if ipv6 {
        format!("[{}]:{}", host, port)
    } else {
        format!("{}:{}", host, port)
    };
    let mut client_url = format!("memcache://{}", host_port);
    if !query.is_empty() {
        client_url.push('?');
        client_url.push_str(query);
    }

    Ok(MemcacheServer {
        configured_uri: configured_uri.to_string(),
        client_url,
        transport: MemcacheTransport::Tcp { host, port },
    })
}

fn parse_memcache_host_authority(
    authority: &str,
    configured_uri: &str,
) -> Result<(String, u16, bool)> {
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) if !port.contains(':') => {
            (host, parse_memcache_port(port, configured_uri)?)
        }
        Some(_) => {
            return Err(RezError::Config(format!(
                "invalid memcache server URI {:?}: expected host[:port]",
                configured_uri
            )));
        }
        None => (authority, 11211),
    };
    if host.is_empty() || host.chars().any(|ch| "[]@".contains(ch)) {
        return Err(RezError::Config(format!(
            "invalid memcache server URI {:?}: expected a non-empty host",
            configured_uri
        )));
    }
    Ok((host.to_string(), port, false))
}

fn parse_memcache_ipv6_authority(
    authority: &str,
    configured_uri: &str,
) -> Result<(String, u16, bool)> {
    let Some(rest) = authority.strip_prefix('[') else {
        return Err(RezError::Config(format!(
            "invalid memcache server URI {:?}: inet6 endpoint must use [address]",
            configured_uri
        )));
    };
    let Some((host, suffix)) = rest.split_once(']') else {
        return Err(RezError::Config(format!(
            "invalid memcache server URI {:?}: missing closing IPv6 bracket",
            configured_uri
        )));
    };
    if host.is_empty() || host.contains('[') {
        return Err(RezError::Config(format!(
            "invalid memcache server URI {:?}: invalid IPv6 address",
            configured_uri
        )));
    }
    let port = if suffix.is_empty() {
        11211
    } else if let Some(port) = suffix.strip_prefix(':') {
        parse_memcache_port(port, configured_uri)?
    } else {
        return Err(RezError::Config(format!(
            "invalid memcache server URI {:?}: expected [address][:port]",
            configured_uri
        )));
    };
    Ok((host.to_string(), port, true))
}

fn parse_memcache_port(port: &str, configured_uri: &str) -> Result<u16> {
    port.parse::<u16>().map_err(|_| {
        RezError::Config(format!(
            "invalid memcache server URI {:?}: port must be an integer from 0 to 65535",
            configured_uri
        ))
    })
}

trait MemcacheIo: std::io::Read + std::io::Write {}
impl<T: std::io::Read + std::io::Write> MemcacheIo for T {}

impl MemcacheServer {
    fn connect_io(&self) -> std::io::Result<Box<dyn MemcacheIo>> {
        let result: std::io::Result<Box<dyn MemcacheIo>> = match &self.transport {
            MemcacheTransport::Tcp { host, port } => {
                use std::net::ToSocketAddrs;
                let addresses = (host.as_str(), *port).to_socket_addrs().map_err(|error| {
                    std::io::Error::new(
                        error.kind(),
                        format!("failed to resolve {}:{}: {}", host, port, error),
                    )
                })?;
                let mut last_error = None;
                let mut connected = None;
                for address in addresses {
                    match std::net::TcpStream::connect_timeout(&address, MEMCACHE_SOCKET_TIMEOUT) {
                        Ok(stream) => {
                            connected = Some(stream);
                            break;
                        }
                        Err(error) => last_error = Some(error),
                    }
                }
                let stream = connected.ok_or_else(|| {
                    last_error.unwrap_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::AddrNotAvailable,
                            "host resolved to no socket addresses",
                        )
                    })
                })?;
                stream.set_read_timeout(Some(MEMCACHE_SOCKET_TIMEOUT))?;
                stream.set_write_timeout(Some(MEMCACHE_SOCKET_TIMEOUT))?;
                Ok(Box::new(stream))
            }
            #[cfg(unix)]
            MemcacheTransport::Unix { path } => {
                let stream = std::os::unix::net::UnixStream::connect(path)?;
                stream.set_read_timeout(Some(MEMCACHE_SOCKET_TIMEOUT))?;
                stream.set_write_timeout(Some(MEMCACHE_SOCKET_TIMEOUT))?;
                Ok(Box::new(stream))
            }
        };
        result
    }

    fn connect(&self) -> Result<Box<dyn MemcacheIo>> {
        self.connect_io().map_err(|error| {
            RezError::System(format!(
                "memcache endpoint {:?} connection failed: {}",
                self.configured_uri, error
            ))
        })
    }

    fn command(&self, command: &str, accepted: &[&str]) -> Result<()> {
        use std::io::{BufRead, BufReader, Write};
        let mut stream = self.connect()?;
        stream
            .write_all(format!("{}\r\n", command).as_bytes())
            .and_then(|()| stream.flush())
            .map_err(|error| {
                RezError::System(format!(
                    "memcache endpoint {:?} command {:?} failed: {}",
                    self.configured_uri, command, error
                ))
            })?;

        let mut response = String::new();
        BufReader::new(stream.as_mut())
            .read_line(&mut response)
            .map_err(|error| {
                RezError::System(format!(
                    "memcache endpoint {:?} command {:?} response failed: {}",
                    self.configured_uri, command, error
                ))
            })?;
        let response = response.trim();
        if accepted.contains(&response) {
            Ok(())
        } else {
            Err(RezError::System(format!(
                "memcache endpoint {:?} command {:?} returned {:?}",
                self.configured_uri, command, response
            )))
        }
    }

    fn read_stats(&self, stream: &mut dyn MemcacheIo) -> Result<HashMap<String, String>> {
        use std::io::{BufRead, BufReader};

        stream
            .write_all(b"stats\r\n")
            .and_then(|()| stream.flush())
            .map_err(|error| {
                RezError::System(format!(
                    "memcache endpoint {:?} stats request failed: {}",
                    self.configured_uri, error
                ))
            })?;

        let mut stats = HashMap::new();
        let mut reader = BufReader::new(stream);
        loop {
            let mut line = String::new();
            let bytes = reader.read_line(&mut line).map_err(|error| {
                RezError::System(format!(
                    "memcache endpoint {:?} stats response failed: {}",
                    self.configured_uri, error
                ))
            })?;
            if bytes == 0 {
                return Err(RezError::System(format!(
                    "memcache endpoint {:?} closed before the stats terminator",
                    self.configured_uri
                )));
            }

            let line = line.trim_end_matches(['\r', '\n']);
            if matches!(line, "END" | "RESET") {
                return Ok(stats);
            }
            let Some(stat) = line.strip_prefix("STAT ") else {
                return Err(RezError::System(format!(
                    "memcache endpoint {:?} returned malformed stats line {:?}",
                    self.configured_uri, line
                )));
            };
            let Some((key, value)) = stat.split_once(' ') else {
                return Err(RezError::System(format!(
                    "memcache endpoint {:?} returned malformed stats line {:?}",
                    self.configured_uri, line
                )));
            };
            if key.is_empty() {
                return Err(RezError::System(format!(
                    "memcache endpoint {:?} returned an empty stats key",
                    self.configured_uri
                )));
            }
            stats.insert(key.to_string(), value.to_string());
        }
    }
}

#[derive(Clone, Debug)]
pub struct MemcacheProbe {
    pub server: String,
    pub set_time: std::time::Duration,
    pub get_time: std::time::Duration,
}

#[derive(Clone, Debug)]
pub enum MemcacheProbeOutcome {
    Available(MemcacheProbe),
    Unavailable { server: String, reason: String },
}

/// Memcache client for package data, listdir, and resolve caching, plus
/// per-server administrative operations used by rez memcache.
pub struct MemcacheClient {
    servers: Vec<String>,
    #[allow(clippy::type_complexity)]
    client: std::sync::Mutex<Option<memcache::Client>>,
    enabled: bool,
}

impl std::fmt::Debug for MemcacheClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemcacheClient")
            .field("servers", &self.servers)
            .field("enabled", &self.enabled)
            .finish()
    }
}

impl MemcacheClient {
    /// Create new memcache client (connect on first use).
    pub fn new(servers: &[String]) -> Self {
        let enabled = !servers.is_empty();
        Self {
            servers: servers.to_vec(),
            client: std::sync::Mutex::new(None),
            enabled,
        }
    }

    fn normalized_servers(&self) -> Result<Vec<MemcacheServer>> {
        self.servers.iter().map(|uri| memcache_url(uri)).collect()
    }

    fn with_client<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&memcache::Client) -> Result<R>,
    {
        if !self.enabled {
            return Err(RezError::Config("memcache not enabled".into()));
        }
        let mut guard = self
            .client
            .lock()
            .map_err(|e| RezError::System(format!("memcache lock poisoned: {}", e)))?;
        if guard.is_none() {
            let urls = self
                .normalized_servers()?
                .into_iter()
                .map(|server| server.client_url)
                .collect::<Vec<_>>();
            let client = memcache::connect(urls).map_err(|e| {
                RezError::System(format!(
                    "memcache connect failed for {:?}: {}",
                    self.servers, e
                ))
            })?;
            *guard = Some(client);
        }
        f(guard.as_ref().unwrap())
    }

    /// Get value from cache. Cache failures are treated as misses.
    pub fn get(&self, key: &str) -> Option<String> {
        if !self.enabled {
            return None;
        }
        self.with_client(|c| Ok(c.get::<String>(key).ok().flatten()))
            .ok()
            .flatten()
    }

    /// Set value in cache. No-op when disabled.
    pub fn set(&self, key: &str, value: &str, ttl: u32) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        self.with_client(|c| {
            c.set(key, value, ttl)
                .map_err(|e| RezError::System(format!("memcache set failed: {}", e)))
        })
    }

    /// Delete value from cache. No-op when disabled.
    pub fn delete(&self, key: &str) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        self.with_client(|c| {
            c.delete(key)
                .map_err(|e| RezError::System(format!("memcache delete failed: {}", e)))?;
            Ok(())
        })
    }

    /// Hard flush all configured memcached servers, including Rez stats reset.
    pub fn flush(&self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let servers = self.normalized_servers()?;
        let mut errors = Vec::new();
        for server in &servers {
            if let Err(error) = server.command("flush_all", &["OK"]) {
                errors.push(error.to_string());
            }
        }
        for server in &servers {
            if let Err(error) = server.command("stats reset", &["RESET", "END"]) {
                errors.push(error.to_string());
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(RezError::System(format!(
                "memcache hard flush had server errors: {}",
                errors.join("; ")
            )))
        }
    }

    /// Reset statistics on every configured server.
    pub fn reset_stats(&self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let servers = self.normalized_servers()?;
        let mut errors = Vec::new();
        for server in &servers {
            if let Err(error) = server.command("stats reset", &["RESET", "END"]) {
                errors.push(error.to_string());
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(RezError::System(format!(
                "memcache stats reset failed: {}",
                errors.join("; ")
            )))
        }
    }

    /// Get statistics from each reachable server; connect failures are skipped like Rez.
    pub fn stats(&self) -> Result<Vec<(String, HashMap<String, String>)>> {
        if !self.enabled {
            return Ok(vec![]);
        }
        let mut rows = Vec::new();
        for server in self.normalized_servers()? {
            let Ok(mut stream) = server.connect_io() else {
                continue;
            };
            let stats = server.read_stats(stream.as_mut())?;
            rows.push((server.configured_uri, stats));
        }
        Ok(rows)
    }

    /// Measure set/get against servers returned by stats, skipping connection races.
    pub fn probe_servers(
        &self,
        available_servers: Option<&[String]>,
    ) -> Result<Vec<MemcacheProbeOutcome>> {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::Instant;

        static NEXT_PROBE_ID: AtomicU64 = AtomicU64::new(0);
        if !self.enabled {
            return Ok(vec![]);
        }

        let servers = self
            .normalized_servers()?
            .into_iter()
            .filter(|server| {
                available_servers.is_none_or(|available| available.contains(&server.configured_uri))
            })
            .collect::<Vec<_>>();
        let mut outcomes = Vec::with_capacity(servers.len());
        for server in servers {
            let mut stream = match server.connect_io() {
                Ok(stream) => stream,
                Err(error) => {
                    outcomes.push(MemcacheProbeOutcome::Unavailable {
                        server: server.configured_uri,
                        reason: error.to_string(),
                    });
                    continue;
                }
            };
            let unique_id = NEXT_PROBE_ID.fetch_add(1, Ordering::Relaxed);
            let key = format!(
                "rez_rs_probe_{}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos(),
                unique_id
            );
            let value = format!("{}:{}", std::process::id(), unique_id);
            let mut stored = false;

            let attempt = (|| -> Result<MemcacheProbe> {
                let mut reader = BufReader::new(stream.as_mut());

                let set_start = Instant::now();
                write!(
                    reader.get_mut(),
                    "set {} 0 30 {}\r\n{}\r\n",
                    key,
                    value.len(),
                    value
                )
                .and_then(|()| reader.get_mut().flush())
                .map_err(|error| {
                    RezError::System(format!(
                        "memcache endpoint {:?} probe set failed: {}",
                        server.configured_uri, error
                    ))
                })?;
                let mut response = String::new();
                reader.read_line(&mut response).map_err(|error| {
                    RezError::System(format!(
                        "memcache endpoint {:?} probe set response failed: {}",
                        server.configured_uri, error
                    ))
                })?;
                if response.trim() != "STORED" {
                    return Err(RezError::System(format!(
                        "memcache endpoint {:?} probe set returned {:?}",
                        server.configured_uri,
                        response.trim()
                    )));
                }
                stored = true;
                let set_time = set_start.elapsed();

                let get_start = Instant::now();
                write!(reader.get_mut(), "get {}\r\n", key)
                    .and_then(|()| reader.get_mut().flush())
                    .map_err(|error| {
                        RezError::System(format!(
                            "memcache endpoint {:?} probe get failed: {}",
                            server.configured_uri, error
                        ))
                    })?;
                response.clear();
                reader.read_line(&mut response).map_err(|error| {
                    RezError::System(format!(
                        "memcache endpoint {:?} probe get response failed: {}",
                        server.configured_uri, error
                    ))
                })?;
                let expected = format!("VALUE {} 0 {}", key, value.len());
                if response.trim() != expected {
                    return Err(RezError::System(format!(
                        "memcache endpoint {:?} probe get returned {:?}, expected {:?}",
                        server.configured_uri,
                        response.trim(),
                        expected
                    )));
                }
                let mut returned_value = vec![0; value.len()];
                reader.read_exact(&mut returned_value).map_err(|error| {
                    RezError::System(format!(
                        "memcache endpoint {:?} probe value read failed: {}",
                        server.configured_uri, error
                    ))
                })?;
                let mut crlf = [0; 2];
                reader.read_exact(&mut crlf).map_err(|error| {
                    RezError::System(format!(
                        "memcache endpoint {:?} probe framing failed: {}",
                        server.configured_uri, error
                    ))
                })?;
                if returned_value != value.as_bytes() || crlf != *b"\r\n" {
                    return Err(RezError::System(format!(
                        "memcache endpoint {:?} probe returned an unexpected value",
                        server.configured_uri
                    )));
                }
                response.clear();
                reader.read_line(&mut response).map_err(|error| {
                    RezError::System(format!(
                        "memcache endpoint {:?} probe terminator read failed: {}",
                        server.configured_uri, error
                    ))
                })?;
                if response.trim() != "END" {
                    return Err(RezError::System(format!(
                        "memcache endpoint {:?} probe ended with {:?}",
                        server.configured_uri,
                        response.trim()
                    )));
                }

                Ok(MemcacheProbe {
                    server: server.configured_uri.clone(),
                    set_time,
                    get_time: get_start.elapsed(),
                })
            })();

            let cleanup = stored
                .then(|| server.command(&format!("delete {}", key), &["DELETED", "NOT_FOUND"]));
            match (attempt, cleanup) {
                (Ok(probe), Some(Ok(()))) => {
                    outcomes.push(MemcacheProbeOutcome::Available(probe));
                }
                (Err(primary), Some(Ok(()))) | (Err(primary), None) => return Err(primary),
                (Ok(_), Some(Err(cleanup))) => {
                    return Err(RezError::System(format!(
                        "memcache endpoint {:?} probe succeeded but cleanup failed: {}",
                        server.configured_uri, cleanup
                    )));
                }
                (Err(primary), Some(Err(cleanup))) => {
                    return Err(RezError::System(format!(
                        "{}; cleanup of probe key on {:?} also failed: {}",
                        primary, server.configured_uri, cleanup
                    )));
                }
                (Ok(_), None) => {
                    return Err(RezError::System(format!(
                        "memcache endpoint {:?} probe completed without a stored key",
                        server.configured_uri
                    )));
                }
            }
        }
        Ok(outcomes)
    }

    /// Check if memcache is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// Generate cache key for package data
///
/// Creates a deterministic cache key from repository path and package info.
/// Format: `rez:pkg:{repo_hash}:{pkg_name}:{version}`
///
/// # Arguments
/// * `repo_path` - Repository root path
/// * `pkg_name` - Package name
/// * `version` - Package version string
///
/// # Returns
/// Cache key string
///
/// # Example
/// ```rust
/// use std::path::Path;
/// use repository::repository::cache_key_for_package;
///
/// let repo = Path::new("/packages");
/// let key = cache_key_for_package(repo, "foo", "1.0.0");
/// // Result: "rez:pkg:a3f5e2d1:foo:1.0.0" (hash varies by repo path)
/// ```
fn repo_hash(repo_path: &Path) -> u64 {
    crate::util::stable_hash(repo_path.to_string_lossy().as_bytes())
}

pub fn cache_key_for_package(repo_path: &Path, pkg_name: &str, version: &str) -> String {
    let h = repo_hash(repo_path);
    // v2 rejects payloads from before filesystem base became authoritative.
    format!("rez:pkg:v2:{:x}:{}:{}", h, pkg_name, version)
}

fn cache_key_for_listdir(repo_path: &Path) -> String {
    format!("rez:listdir:{:x}", repo_hash(repo_path))
}

fn cache_key_for_versions(repo_path: &Path, name: &str) -> String {
    format!("rez:versions:{:x}:{}", repo_hash(repo_path), name)
}

fn cache_key_for_source<T: Serialize>(base_key: &str, source: &T) -> Option<String> {
    let encoded = serde_json::to_vec(source).ok()?;
    Some(format!(
        "{base_key}:source:{:x}",
        crate::util::stable_hash(&encoded)
    ))
}

/// Get cached package data
///
/// # Arguments
/// * `client` - Memcache client
/// * `repo_path` - Repository root path
/// * `pkg_name` - Package name
/// * `version` - Package version string
///
/// # Returns
/// Cached package data if found, None otherwise
pub fn get_cached_package_data(
    client: &MemcacheClient,
    repo_path: &Path,
    pkg_name: &str,
    version: &str,
) -> Option<HashMap<String, serde_json::Value>> {
    if !client.is_enabled() {
        return None;
    }

    let key = cache_key_for_package(repo_path, pkg_name, version);
    let data_str = client.get(&key)?;
    serde_json::from_str(&data_str).ok()
}

/// Cache package data
///
/// # Arguments
/// * `client` - Memcache client
/// * `repo_path` - Repository root path
/// * `pkg_name` - Package name
/// * `version` - Package version string
/// * `data` - Package data to cache
/// * `ttl` - Time-to-live in seconds (0 = no expiration)
pub fn cache_package_data(
    client: &MemcacheClient,
    repo_path: &Path,
    pkg_name: &str,
    version: &str,
    data: &HashMap<String, serde_json::Value>,
    ttl: u32,
) -> Result<()> {
    if !client.is_enabled() {
        return Ok(());
    }

    let key = cache_key_for_package(repo_path, pkg_name, version);
    let data_str = serde_json::to_string(data)?;
    client.set(&key, &data_str, ttl)
}

// ============================================================================
// Tests
// ============================================================================

// ============================================================================
// Repository Write Operations
// ============================================================================

const IGNORE_PREFIX: &str = ".ignore";
const LOCK_PREFIX: &str = ".lock.";

/// File lock for repository write operations
struct LockFile {
    path: PathBuf,
    lock: crate::util::FileLock,
}

impl LockFile {
    /// Create a package/version lock. The file is persistent; ownership is an OS lock.
    fn new(repo_path: &Path, pkg_name: &str, version: Option<&str>) -> Self {
        let filename = if let Some(version) = version {
            format!("{LOCK_PREFIX}{pkg_name}-{version}")
        } else {
            format!("{LOCK_PREFIX}{pkg_name}")
        };
        let path = repo_path.join(filename);
        Self {
            lock: crate::util::FileLock::new(path.clone()),
            path,
        }
    }

    fn acquire(&mut self, timeout_secs: u64) -> Result<()> {
        self.lock
            .acquire(std::time::Duration::from_secs(timeout_secs))
            .map_err(|error| {
                RezError::PackageRepository(format!("Cannot lock {}: {error}", self.path.display()))
            })
    }

    fn release(&mut self) -> Result<()> {
        self.lock.release().map_err(|error| {
            RezError::PackageRepository(format!("Cannot unlock {}: {error}", self.path.display()))
        })
    }
}

impl Drop for LockFile {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

/// Export repository-ready package metadata, excluding build-only Rez fields.
fn installed_package_data(package: &Package) -> Result<HashMap<String, serde_json::Value>> {
    let mut data = package.to_data()?;
    for key in [
        "requires_rez_version",
        "build_system",
        "build_command",
        "preprocess",
        "pre_build_commands",
        "late_variants",
    ] {
        data.remove(key);
    }
    Ok(data)
}

/// Payload work performed while holding the canonical package-version lock.
pub enum PublicationPayload<'a> {
    /// Prepare payload directories under the lock before validating them and publishing metadata.
    /// Unlike staged payloads, existing-directory work is not automatically rolled back.
    Existing(&'a dyn Fn() -> Result<()>),
    /// A version-relative payload tree prepared outside the repository.
    Staged { root: &'a Path, replace: bool },
    /// Merge an exclusively owned tree, retaining destination-only children recursively.
    /// Existing directory permissions are retained; leaf replacements are journaled.
    Merge {
        root: tempfile::TempDir,
        paths: Option<Vec<(Option<usize>, PathBuf)>>,
        metadata: PublicationMetadata,
    },
    /// Transfer an exclusively owned prepared tree; same-volume publication avoids copying.
    Owned {
        root: tempfile::TempDir,
        replace: bool,
        /// Declared version-relative entries for selected copy operations; None synchronizes variant roots.
        paths: Option<Vec<(Option<usize>, PathBuf)>>,
        /// Fail if destination metadata already exists, while holding the version lock.
        require_new: bool,
    },
}

/// Metadata advertised by an opt-in merge publication.
#[derive(Clone, PartialEq, Eq)]
pub enum PublicationMetadata {
    /// Advertise only selected variants with validated installed payloads.
    Installed,
    /// Legacy installer: retain the source variant declarations independently
    /// of its opaque caller-selected payload path, as the five-argument API did.
    Declared {
        /// Original Python namespace, only constructible by validated legacy loading.
        definition: Option<VerifiedPythonDefinition>,
    },
}

/// A Python definition snapshot tied to its canonically validated package data.
/// Private fields prevent unchecked source bytes from bypassing schema or identity checks.
#[derive(Clone, PartialEq, Eq)]
pub struct VerifiedPythonDefinition {
    bytes: Vec<u8>,
    data: HashMap<String, serde_json::Value>,
}

impl VerifiedPythonDefinition {
    fn load(
        path: &Path,
        name: &str,
        version: &str,
    ) -> Result<(Package, crate::serialise::FileFormat, Option<Self>)> {
        let format = crate::serialise::detect_format(path).ok_or_else(|| {
            RezError::PackageRepository("Unsupported legacy definition format".into())
        })?;
        let before = if format == crate::serialise::FileFormat::Py {
            Some(fs::read(path)?)
        } else {
            None
        };
        let data = load_package_data(path)?;
        if let Some(bytes) = &before {
            if fs::read(path)? != *bytes {
                return Err(RezError::PackageRepository(format!(
                    "Python definition changed while loading: {}",
                    path.display()
                )));
            }
        }
        let package = Package::from_data(data)?;
        if package.name != name || package.version.to_string() != version {
            return Err(RezError::PackageRepository(format!(
                "Legacy source identifies {}-{}, expected {name}-{version}",
                package.name, package.version
            )));
        }
        let definition = before
            .map(|bytes| package.to_data().map(|data| Self { bytes, data }))
            .transpose()?;
        Ok((package, format, definition))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PayloadMerge {
    Synchronize { replace: bool },
    Preserve,
}

/// Selected source indices, independently of their merged installed metadata indices.
#[derive(Debug, Default)]
pub struct PublicationResult {
    pub installed_indices: Vec<usize>,
    pub skipped_indices: Vec<usize>,
    /// Source selection index to the actual destination metadata index (None for unvarianted packages).
    pub variant_indices: Vec<(usize, Option<usize>)>,
}

struct PayloadTransaction {
    workspace: tempfile::TempDir,
    changes: Vec<(PathBuf, PathBuf, bool)>,
    created_directories: Vec<PathBuf>,
    repository: PathBuf,
    directory_times: HashMap<PathBuf, (filetime::FileTime, filetime::FileTime)>,
}

impl PayloadTransaction {
    fn new(source: &Path, repository: &Path, destination: &Path, consume: bool) -> Result<Self> {
        let source_kind = fs::symlink_metadata(source)?;
        if crate::util::is_redirect(&source_kind) || !source_kind.is_dir() {
            return Err(RezError::PackageRepository(
                "Staged payload root must be a regular directory".into(),
            ));
        }
        let source = fs::canonicalize(source)?;
        if !fs::symlink_metadata(&source)?.is_dir() {
            return Err(RezError::PackageRepository(
                "Staged payload root is not a directory".into(),
            ));
        }
        let repository = fs::canonicalize(repository)?;
        let destination = fs::canonicalize(destination)?;
        if source.starts_with(&destination) || destination.starts_with(&source) {
            return Err(RezError::PackageRepository(
                "Staged source overlaps its publication destination".into(),
            ));
        }
        let workspace = tempfile::Builder::new()
            .prefix(".rez-publish-")
            .tempdir_in(&repository)?;
        let mut pending = vec![source.clone()];
        while let Some(from) = pending.pop() {
            for entry in fs::read_dir(&from)? {
                let entry = entry?;
                let name = entry.file_name();
                // Native payload names retain their OS identity; the lossy view is
                // used only for the ASCII path-safety predicate, never as a path/key.
                if !foundation::path::is_safe_rez_path_component(&name.to_string_lossy(), false) {
                    return Err(RezError::PackageRepository(format!(
                        "Unsafe staged payload name {:?}",
                        name
                    )));
                }
                let kind = entry.file_type()?;
                if !kind.is_symlink() && !fs::canonicalize(entry.path())?.starts_with(&source) {
                    return Err(RezError::PackageRepository(format!(
                        "Staged payload escapes root: {}",
                        entry.path().display()
                    )));
                }
                let metadata = fs::symlink_metadata(entry.path())?;
                if crate::util::is_redirect(&metadata) && !kind.is_symlink() {
                    return Err(RezError::PackageRepository(format!(
                        "Unsupported staged reparse object {}",
                        entry.path().display()
                    )));
                }
                if kind.is_dir() && !kind.is_symlink() {
                    pending.push(entry.path());
                } else if !kind.is_file() && !kind.is_symlink() {
                    return Err(RezError::PackageRepository(
                        "Staged payload contains a special file".into(),
                    ));
                }
            }
        }
        if !consume {
            crate::package::ops::copy_dir_contents(
                &source,
                &workspace.path().join("payload"),
                false,
                true,
                Some(workspace.path()),
                None,
            )?;
        }
        if consume {
            // Only exclusively owned trees may be moved. Validate the entire tree first;
            // borrowed callers keep their snapshot-copy contract.
            match fs::rename(&source, workspace.path().join("payload")) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
                    return Self::new(&source, &repository, &destination, false);
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(Self {
            workspace,
            changes: Vec::new(),
            created_directories: Vec::new(),
            repository,
            directory_times: HashMap::new(),
        })
    }

    fn remember_directory(&mut self, path: &Path) -> Result<()> {
        if self
            .created_directories
            .iter()
            .any(|directory| directory == path)
            || self.directory_times.contains_key(path)
        {
            return Ok(());
        }
        let metadata = fs::symlink_metadata(path)?;
        if !crate::util::is_redirect(&metadata) && metadata.is_dir() {
            self.directory_times.insert(
                path.to_path_buf(),
                (
                    filetime::FileTime::from_last_access_time(&metadata),
                    filetime::FileTime::from_last_modification_time(&metadata),
                ),
            );
        }
        Ok(())
    }

    fn ensure_directory(&mut self, path: &Path) -> Result<()> {
        let relative = path.strip_prefix(&self.repository).map_err(|_| {
            RezError::PackageRepository("Payload destination escapes repository".into())
        })?;
        let mut directory = self.repository.clone();
        for component in relative.components() {
            directory.push(component);
            match fs::symlink_metadata(&directory) {
                Ok(metadata) if crate::util::is_redirect(&metadata) || !metadata.is_dir() => {
                    return Err(RezError::PackageRepository(format!(
                        "Payload ancestor is not a regular directory: {}",
                        directory.display()
                    )));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    self.remember_directory(
                        directory.parent().expect("generated directory parent"),
                    )?;
                    fs::create_dir(&directory)?;
                    self.created_directories.push(directory.clone());
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn install(&mut self, source: Option<&Path>, destination: &Path) -> Result<()> {
        self.ensure_directory(destination.parent().ok_or_else(|| {
            RezError::PackageRepository("Payload destination has no parent".into())
        })?)?;
        self.remember_directory(destination.parent().expect("payload parent"))?;
        let backup = self
            .workspace
            .path()
            .join(format!("backup-{}", self.changes.len()));
        let existed = match fs::symlink_metadata(destination) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        if existed {
            fs::rename(destination, &backup)?;
        }
        self.changes
            .push((destination.to_path_buf(), backup, existed));
        if let Some(source) = source {
            fs::rename(source, destination)?;
        }
        Ok(())
    }

    // Merge an ancestor payload without replacing other advertised variant roots.
    fn merge(
        &mut self,
        source: &Path,
        destination: &Path,
        protected: &[PathBuf],
        excluded: &[PathBuf],
        policy: PayloadMerge,
    ) -> Result<()> {
        self.ensure_directory(destination)?;
        self.remember_directory(destination)?;
        let replace = policy != PayloadMerge::Synchronize { replace: false };
        let mut names = std::collections::BTreeSet::new();
        match fs::symlink_metadata(source) {
            Ok(metadata) if crate::util::is_redirect(&metadata) || !metadata.is_dir() => {
                return Err(RezError::PackageRepository(format!(
                    "Expected regular staged directory {}",
                    source.display()
                )));
            }
            Ok(_) => {
                for entry in fs::read_dir(source)? {
                    names.insert(entry?.file_name());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if policy == (PayloadMerge::Synchronize { replace: true }) {
            for entry in fs::read_dir(destination)? {
                names.insert(entry?.file_name());
            }
        }
        for name in names {
            let from = source.join(&name);
            let to = destination.join(&name);
            if excluded.contains(&to) {
                if fs::symlink_metadata(&from).is_ok() {
                    return Err(RezError::PackageRepository(format!(
                        "Staged payload contains reserved entry {}",
                        from.display()
                    )));
                }
                continue;
            }
            if protected.contains(&to) {
                continue;
            }
            if protected.iter().any(|path| path.starts_with(&to)) {
                self.merge(&from, &to, protected, excluded, policy)?;
                continue;
            }
            match fs::symlink_metadata(&from) {
                Ok(metadata) => {
                    if policy == PayloadMerge::Preserve
                        && metadata.is_dir()
                        && !crate::util::is_redirect(&metadata)
                        && fs::symlink_metadata(&to).is_ok_and(|existing| {
                            existing.is_dir() && !crate::util::is_redirect(&existing)
                        })
                    {
                        self.merge(&from, &to, protected, excluded, policy)?;
                        continue;
                    }
                    if fs::symlink_metadata(&to).is_ok() && !replace {
                        return Err(RezError::PackageRepository(format!(
                            "Unverified existing payload {}",
                            to.display()
                        )));
                    }
                    self.install(Some(&from), &to)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound && replace => {
                    self.install(None, &to)?
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn rollback(&mut self) -> Result<()> {
        let mut errors = Vec::new();
        for (destination, backup, existed) in self.changes.drain(..).rev() {
            let remove = crate::package::ops::remove_path(&destination);
            if let Err(error) = remove {
                errors.push(error.to_string());
                continue;
            }
            if existed {
                if let Err(error) = fs::rename(backup, destination) {
                    errors.push(error.to_string());
                }
            }
        }
        for directory in self.created_directories.drain(..).rev() {
            if let Err(error) = fs::remove_dir(&directory) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    errors.push(error.to_string());
                }
            }
        }
        // Restore directory times after all child renames/removals, including
        // read_dir atime changes. Existing directory permissions are never changed.
        for (directory, (accessed, modified)) in self.directory_times.drain() {
            if let Err(error) = filetime::set_file_times(directory, accessed, modified) {
                errors.push(error.to_string());
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            // Retain recoverable backups if rollback itself fails.
            let recovery = self.workspace.path().to_path_buf();
            Err(RezError::PackageRepository(format!(
                "Payload rollback failed: {}; recovery directory {}",
                errors.join("; "),
                recovery.display()
            )))
        }
    }
}

/// Publish package metadata after the selected variants have been installed.
///
/// The root definition is the authoritative list of available variants. Existing
/// entries survive only while their variant payload directory remains present.
/// The optional payload callback runs under the version lock before metadata is
/// published. An error aborts publication; the callback must not reenter this publisher.
pub fn publish_package(
    package: &Package,
    version_dir: &Path,
    selected_indices: &[usize],
    payload: Option<PublicationPayload<'_>>,
    format: Option<crate::serialise::FileFormat>,
) -> Result<PublicationResult> {
    if format.is_some_and(|format| {
        !crate::serialise::PACKAGE_DEFINITION_EXTENSIONS.contains(&format.extension())
    }) {
        return Err(RezError::PackageRepository(
            "Unsupported publication format".into(),
        ));
    }
    let declared_metadata = matches!(
        &payload,
        Some(PublicationPayload::Merge {
            metadata: PublicationMetadata::Declared { .. },
            ..
        })
    );
    if declared_metadata
        && (!selected_indices.is_empty()
            || !matches!(&payload,
                Some(PublicationPayload::Merge { paths: Some(paths), .. })
                if paths.iter().all(|(owner, _)| owner.is_none())
            ))
    {
        return Err(RezError::PackageRepository(
            "Legacy declared metadata requires an opaque payload batch without variant selectors"
                .into(),
        ));
    }
    let original_definition = match &payload {
        Some(PublicationPayload::Merge {
            metadata:
                PublicationMetadata::Declared {
                    definition: Some(definition),
                },
            ..
        }) => Some(definition),
        _ => None,
    };
    if let Some(definition) = original_definition {
        if format != Some(crate::serialise::FileFormat::Py) || package.to_data()? != definition.data
        {
            return Err(RezError::PackageRepository(
                "Verified Python definition does not match publication metadata or format".into(),
            ));
        }
    }
    let variant_count = package.variants.len().max(1);
    let mut seen_indices = std::collections::HashSet::new();
    for &index in selected_indices {
        if index >= variant_count {
            return Err(RezError::Build(format!(
                "Package {} has no variant at index {}",
                package.name, index
            )));
        }
        if !seen_indices.insert(index) {
            return Err(RezError::Build(format!(
                "Variant index {} was selected more than once",
                index
            )));
        }
    }
    if selected_indices.is_empty() && !declared_metadata {
        return Ok(PublicationResult::default());
    }

    let version = package.version.to_string();
    crate::serialise::validate_rez_package_path(
        &package.name,
        (!version.is_empty()).then_some(version.as_str()),
    )?;
    let family_dir = if version.is_empty() {
        version_dir
    } else {
        version_dir.parent().ok_or_else(|| {
            RezError::PackageRepository(format!(
                "Package version directory has no family directory: {}",
                version_dir.display()
            ))
        })?
    };
    if family_dir.file_name().and_then(|name| name.to_str()) != Some(package.name.as_str())
        || (!version.is_empty()
            && version_dir.file_name().and_then(|name| name.to_str()) != Some(version.as_str()))
    {
        return Err(RezError::PackageRepository(format!(
            "Package version directory {} does not match {}-{}",
            version_dir.display(),
            package.name,
            version
        )));
    }
    let repository_root = family_dir.parent().ok_or_else(|| {
        RezError::PackageRepository(format!(
            "Package family directory has no repository parent: {}",
            family_dir.display()
        ))
    })?;
    fs::create_dir_all(repository_root)?;
    let canonical_repository = fs::canonicalize(repository_root)?;
    // Existing directory junctions/symlinks must not redirect a package write
    // outside the configured repository, including before version creation.
    for candidate in [family_dir, version_dir] {
        match fs::symlink_metadata(candidate) {
            Ok(_) => {
                let actual = fs::canonicalize(candidate)?;
                if !actual.starts_with(&canonical_repository) {
                    return Err(RezError::PackageRepository(format!(
                        "Publication path escapes repository: {}",
                        candidate.display()
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let mut lock = LockFile::new(
        repository_root,
        &package.name,
        (!version.is_empty()).then_some(version.as_str()),
    );
    lock.acquire(30)?;
    fs::create_dir_all(version_dir)?;
    let canonical_version_dir = fs::canonicalize(version_dir)?;
    let version_dir = canonical_version_dir.as_path();

    let mut outcome = PublicationResult::default();
    let staged_payload = match &payload {
        Some(PublicationPayload::Staged { root, replace }) => Some((*root, *replace, false)),
        Some(PublicationPayload::Owned { root, replace, .. }) => {
            Some((root.path(), *replace, true))
        }
        Some(PublicationPayload::Merge { root, .. }) => Some((root.path(), true, true)),
        _ => None,
    };
    let merge_policy = if matches!(&payload, Some(PublicationPayload::Merge { .. })) {
        PayloadMerge::Preserve
    } else {
        PayloadMerge::Synchronize {
            replace: staged_payload.is_some_and(|(_, replace, _)| replace),
        }
    };
    let mut transaction = None;
    let mut installed_entries = std::collections::HashSet::new();
    let existing_file = crate::serialise::find_package_definition_file(
        version_dir,
        crate::serialise::PACKAGE_DEFINITION_EXTENSIONS,
    )?;
    if matches!(
        &payload,
        Some(PublicationPayload::Owned {
            require_new: true,
            ..
        })
    ) && existing_file.is_some()
    {
        return Err(RezError::PackageMove(format!(
            "Package already exists at destination: {}",
            version_dir.display()
        )));
    }
    if let Some(requested) = format {
        for name in crate::serialise::package_definition_file_names(
            crate::serialise::PACKAGE_DEFINITION_EXTENSIONS,
        )? {
            let candidate = version_dir.join(name);
            if candidate.is_file() && crate::serialise::detect_format(&candidate) != Some(requested)
            {
                return Err(RezError::PackageRepository(format!(
                    "Requested publication format conflicts with existing definition {}",
                    candidate.display()
                )));
            }
        }
    }
    let (path, format, existing_package) = if let Some(path) = existing_file {
        let installed = Package::from_data(load_package_data(&path)?)?;
        if installed.name != package.name || installed.version != package.version {
            return Err(RezError::PackageRepository(format!(
                "Existing package metadata at {} identifies {}-{}, expected {}-{}",
                path.display(),
                installed.name,
                installed.version,
                package.name,
                package.version
            )));
        }
        if installed.hashed_variants != package.hashed_variants {
            return Err(RezError::PackageRepository(format!(
                "Cannot publish {}-{}: installed and source hashed_variants settings differ",
                package.name, version
            )));
        }
        if package.variants.is_empty() && !installed.variants.is_empty() {
            return Err(RezError::PackageRepository(format!(
                "Cannot publish {}-{} as a non-variant package while installed metadata advertises variants",
                package.name, version
            )));
        }
        let installed_format = crate::serialise::detect_format(&path).ok_or_else(|| {
            RezError::PackageRepository(format!(
                "Unsupported package definition format: {}",
                path.display()
            ))
        })?;
        if format.is_some_and(|format| format != installed_format) {
            return Err(RezError::PackageRepository(format!(
                "Requested publication format differs from existing definition {}",
                path.display()
            )));
        }
        (path, installed_format, Some(installed))
    } else {
        let stems = CONFIG.package_definition_stems()?;
        let stem = stems.first().ok_or_else(|| {
            RezError::PackageRepository(
                "No package definition filename is configured for repository publication".into(),
            )
        })?;
        let format = format.unwrap_or(crate::serialise::FileFormat::Yaml);
        (
            version_dir.join(format!("{stem}.{}", format.extension())),
            format,
            None,
        )
    };

    let variant_paths = {
        let mut paths = Vec::<PathBuf>::new();
        for variants in std::iter::once(&package.variants)
            .chain(existing_package.iter().map(|existing| &existing.variants))
        {
            for variant in variants {
                let path =
                    model::package::Variant::compute_subpath(variant, package.hashed_variants)
                        .map(PathBuf::from)
                        .unwrap_or_default();
                if !path.as_os_str().is_empty()
                    && !foundation::path::is_safe_rez_path(&path.to_string_lossy(), false)
                {
                    return Err(RezError::PackageRepository("Unsafe variant subpath".into()));
                }
                if !paths.contains(&path) {
                    paths.push(path);
                }
            }
        }
        for path in paths.iter().filter(|_| !declared_metadata) {
            let mut destination = version_dir.to_path_buf();
            for component in path.components() {
                destination.push(component);
                match fs::symlink_metadata(&destination) {
                    Ok(metadata) if crate::util::is_redirect(&metadata) || !metadata.is_dir() => {
                        return Err(RezError::PackageRepository(format!(
                            "Variant ancestor is not a regular directory: {}",
                            destination.display()
                        )));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        paths
    };
    let mut data = if declared_metadata {
        package.to_data()?
    } else {
        installed_package_data(package)?
    };
    crate::serialise::validate_package_data(&data)?;
    let mut installed_variants: Vec<Vec<String>> = Vec::new();
    if let Some(existing) = existing_package {
        for requirements in &existing.variants {
            let variant_dir =
                model::package::Variant::compute_subpath(requirements, existing.hashed_variants)
                    .map(|subpath| version_dir.join(subpath))
                    .unwrap_or_else(|| version_dir.to_path_buf());
            if variant_dir.is_dir() {
                let requirements = requirements
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>();
                installed_variants.push(requirements);
            }
        }
    }
    let publication = (|| -> Result<()> {
        if let Some(PublicationPayload::Existing(callback)) = &payload {
            callback()?;
        }
        // Declared metadata has one opaque batch, with no inferred variant index.
        let batches: &[usize] = if declared_metadata {
            &[0]
        } else {
            selected_indices
        };
        for &index in batches {
            if let Some((root, replace, consume)) = staged_payload {
                if !replace
                    && is_variant_installed(
                        package,
                        version_dir,
                        index,
                        match &payload {
                            Some(PublicationPayload::Owned { paths: Some(_), .. }) => {
                                Some(crate::constants::PACKAGE_RELEASE_KEYS)
                            }
                            _ => None,
                        },
                    )?
                {
                    outcome.skipped_indices.push(index);
                    let destination_index = package.variants.get(index).and_then(|variant| {
                        let requirements =
                            variant.iter().map(ToString::to_string).collect::<Vec<_>>();
                        installed_variants
                            .iter()
                            .rposition(|installed| *installed == requirements)
                    });
                    outcome.variant_indices.push((index, destination_index));
                    continue;
                }
                if transaction.is_none() {
                    transaction = Some(PayloadTransaction::new(
                        root,
                        repository_root,
                        version_dir,
                        consume,
                    )?);
                }
                let transaction = transaction.as_mut().expect("staged payload transaction");
                let source_root = transaction.workspace.path().join("payload");
                let declared = match &payload {
                    Some(PublicationPayload::Owned {
                        paths: Some(paths), ..
                    })
                    | Some(PublicationPayload::Merge {
                        paths: Some(paths), ..
                    }) => Some(paths),
                    _ => None,
                };
                if let Some(paths) = declared {
                    let subpath = package
                        .variants
                        .get(index)
                        .and_then(|variant| {
                            model::package::Variant::compute_subpath(
                                variant,
                                package.hashed_variants,
                            )
                        })
                        .map(PathBuf::from)
                        .unwrap_or_default();
                    if !declared_metadata {
                        transaction.ensure_directory(&version_dir.join(subpath))?;
                    }
                    for (owner, relative) in paths {
                        if owner.is_some_and(|owner| owner != index) {
                            continue;
                        }
                        if !foundation::path::is_safe_rez_path(&relative.to_string_lossy(), false) {
                            return Err(RezError::PackageRepository(
                                "Unsafe declared payload entry".into(),
                            ));
                        }
                        if !installed_entries.insert(relative.clone()) {
                            continue;
                        }
                        let source = source_root.join(relative);
                        if relative.components().count() == 1
                            && crate::serialise::package_definition_file_names(
                                crate::serialise::PACKAGE_DEFINITION_EXTENSIONS,
                            )?
                            .iter()
                            .any(|name| relative == Path::new(name))
                        {
                            return Err(RezError::PackageRepository(
                                "Declared payload contains package definition".into(),
                            ));
                        }
                        let mut ancestor = source_root.clone();
                        for component in relative.parent().into_iter().flat_map(Path::components) {
                            ancestor.push(component);
                            let metadata = fs::symlink_metadata(&ancestor)?;
                            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                                return Err(RezError::PackageRepository(format!(
                                    "Declared payload ancestor is not a regular directory: {}",
                                    ancestor.display()
                                )));
                            }
                        }
                        let destination = version_dir.join(relative);
                        if merge_policy == PayloadMerge::Preserve
                            && fs::symlink_metadata(&source).is_ok_and(|metadata| {
                                metadata.is_dir() && !crate::util::is_redirect(&metadata)
                            })
                            && fs::symlink_metadata(&destination).is_ok_and(|metadata| {
                                metadata.is_dir() && !crate::util::is_redirect(&metadata)
                            })
                        {
                            transaction.merge(&source, &destination, &[], &[], merge_policy)?;
                        } else {
                            transaction.install(Some(&source), &destination)?;
                        }
                    }
                } else {
                    let relative = package
                        .variants
                        .get(index)
                        .and_then(|variant| {
                            model::package::Variant::compute_subpath(
                                variant,
                                package.hashed_variants,
                            )
                        })
                        .map(PathBuf::from)
                        .unwrap_or_default();
                    let destination = version_dir.join(&relative);
                    let source = source_root.join(&relative);
                    if !source.is_dir() {
                        return Err(RezError::PackageRepository(format!(
                            "Missing staged variant {}",
                            source.display()
                        )));
                    }
                    let includes = source_root.join(".rez/include");
                    if !relative.as_os_str().is_empty() && includes.is_dir() {
                        let destination = version_dir.join(".rez/include");
                        if merge_policy == PayloadMerge::Preserve
                            && fs::symlink_metadata(&destination).is_ok_and(|metadata| {
                                metadata.is_dir() && !crate::util::is_redirect(&metadata)
                            })
                        {
                            transaction.merge(&includes, &destination, &[], &[], merge_policy)?;
                        } else {
                            transaction.install(Some(&includes), &destination)?;
                        }
                    }
                    let protected = variant_paths
                        .iter()
                        .filter(|other| **other != relative && other.starts_with(&relative))
                        .map(|other| version_dir.join(other))
                        .collect::<Vec<_>>();
                    let mut excluded = Vec::new();
                    if relative.as_os_str().is_empty() {
                        excluded = crate::serialise::package_definition_file_names(
                            crate::serialise::PACKAGE_DEFINITION_EXTENSIONS,
                        )?
                        .into_iter()
                        .map(|name| version_dir.join(name))
                        .collect();
                    }
                    transaction.merge(
                        &source,
                        &destination,
                        &protected,
                        &excluded,
                        merge_policy,
                    )?;
                }
            }
            if declared_metadata {
                continue;
            }
            outcome.installed_indices.push(index);
            if package.variants.is_empty() {
                if !version_dir.is_dir() {
                    return Err(RezError::PackageRepository(format!(
                        "Cannot publish uninstalled package {}-{} at {}",
                        package.name,
                        version,
                        version_dir.display()
                    )));
                }
                outcome.variant_indices.push((index, None));
                continue;
            }
            let variant = &package.variants[index];
            let requirements = variant.iter().map(ToString::to_string).collect::<Vec<_>>();
            let subpath =
                model::package::Variant::compute_subpath(variant, package.hashed_variants);
            let variant_dir = subpath
                .map(|subpath| version_dir.join(subpath))
                .unwrap_or_else(|| version_dir.to_path_buf());
            if !variant_dir.is_dir() {
                return Err(RezError::PackageRepository(format!(
                    "Cannot publish uninstalled variant {} of {}-{} at {}",
                    index,
                    package.name,
                    version,
                    variant_dir.display()
                )));
            }
            let destination_index = match installed_variants
                .iter()
                .rposition(|installed| *installed == requirements)
            {
                Some(index) => index,
                None => {
                    installed_variants.push(requirements);
                    installed_variants.len() - 1
                }
            };
            outcome
                .variant_indices
                .push((index, Some(destination_index)));
        }
        if outcome.installed_indices.is_empty() && !declared_metadata {
            return Ok(());
        }
        if !declared_metadata {
            data.insert("variants".into(), serde_json::json!(installed_variants));
        }
        if let Some(definition) = original_definition {
            crate::serialise::atomic_write(&path, &definition.bytes)
        } else {
            crate::serialise::dump_package_data(&data, &path, format, None)
        }
    })();
    if let Err(error) = publication {
        if let Some(mut transaction) = transaction {
            if let Err(rollback) = transaction.rollback() {
                // Keep backups rather than deleting the only recoverable payload.
                let workspace = transaction.workspace.keep();
                return Err(RezError::PackageRepository(format!(
                    "{error}; {rollback}; {workspace:?}"
                )));
            }
        }
        return Err(error);
    }
    Ok(outcome)
}

/// Return whether root metadata matches the current package and advertises an
/// installed variant whose derived payload directory is present.
pub fn is_variant_installed(
    package: &Package,
    version_dir: &Path,
    variant_index: usize,
    ignored_keys: Option<&[&str]>,
) -> Result<bool> {
    if variant_index >= package.variants.len().max(1) {
        return Err(RezError::Build(format!(
            "Package {} has no variant at index {}",
            package.name, variant_index
        )));
    }
    let Some(path) = crate::serialise::find_package_definition_file(
        version_dir,
        crate::serialise::PACKAGE_DEFINITION_EXTENSIONS,
    )?
    else {
        return Ok(false);
    };
    let installed = Package::from_data(load_package_data(&path)?)?;
    if installed.name != package.name
        || installed.version != package.version
        || installed.hashed_variants != package.hashed_variants
    {
        return Ok(false);
    }
    let mut source_data = installed_package_data(package)?;
    let mut installed_data = installed_package_data(&installed)?;
    source_data.remove("variants");
    installed_data.remove("variants");
    if let Some(keys) = ignored_keys {
        for key in keys.iter().copied().chain(["format_version", "base"]) {
            source_data.remove(key);
            installed_data.remove(key);
        }
    }
    if source_data != installed_data {
        return Ok(false);
    }
    let requested = package
        .variants
        .get(variant_index)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let advertised = if package.variants.is_empty() {
        installed.variants.is_empty()
    } else {
        installed
            .variants
            .iter()
            .any(|variant| variant == requested)
    };
    if !advertised {
        return Ok(false);
    }
    let variant_dir = if package.variants.is_empty() {
        version_dir.to_path_buf()
    } else {
        model::package::Variant::compute_subpath(requested, package.hashed_variants)
            .map(|subpath| version_dir.join(subpath))
            .unwrap_or_else(|| version_dir.to_path_buf())
    };
    Ok(variant_dir.is_dir())
}

/// Legacy copy follows file links but rejects directory redirects before staging.
fn validate_legacy_payload(source: &Path) -> Result<()> {
    if !fs::metadata(source)?.is_dir() {
        return Err(RezError::PackageRepository(
            "Legacy payload source is not a directory".into(),
        ));
    }
    let mut pending = vec![source.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if crate::util::is_redirect(&metadata) {
                if !fs::metadata(&path)?.is_file() {
                    return Err(RezError::PackageRepository(format!(
                        "Legacy payload directory links are unsupported: {}",
                        path.display()
                    )));
                }
            } else if metadata.is_dir() {
                pending.push(path);
            } else if !metadata.is_file() {
                return Err(RezError::PackageRepository(format!(
                    "Unsupported legacy payload object {}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

/// Check if directory is empty (no files or subdirs)
fn is_dir_empty(path: &Path) -> Result<bool> {
    if !path.exists() || !path.is_dir() {
        return Ok(true);
    }

    let mut entries = fs::read_dir(path)?;
    Ok(entries.next().is_none())
}

/// Find package definition file in directory or parent.
fn find_package_file(dir: &Path) -> Result<PathBuf> {
    if let Some(path) = crate::serialise::find_package_definition_file(
        dir,
        crate::serialise::PACKAGE_DEFINITION_EXTENSIONS,
    )? {
        return Ok(path);
    }
    if let Some(parent) = dir.parent() {
        if let Some(path) = crate::serialise::find_package_definition_file(
            parent,
            crate::serialise::PACKAGE_DEFINITION_EXTENSIONS,
        )? {
            return Ok(path);
        }
    }
    Err(RezError::PackageRepository(format!(
        "Package definition file not found in {} or parent",
        dir.display()
    )))
}

/// Install a variant into repository
///
/// Copies variant files from source to destination repository structure.
///
/// # Arguments
/// * `src_variant_root` - Source directory containing variant files
/// * `dest_repo_path` - Destination repository root path
/// * `pkg_name` - Package name
/// * `version` - Package version string
/// * `variant_subpath` - Optional subdirectory path for variant
///
/// # Returns
/// Path to installed variant directory
///
/// # Example
/// ```rust,no_run
/// use std::path::Path;
/// use repository::repository::install_variant;
///
/// let src = Path::new("/tmp/build/variant");
/// let repo = Path::new("/packages");
/// let installed = install_variant(src, repo, "mypkg", "1.0.0", Some("platform-linux")).unwrap();
/// // Result: /packages/mypkg/1.0.0/platform-linux/
/// ```
pub fn install_variant(
    src_variant_root: &Path,
    dest_repo_path: &Path,
    pkg_name: &str,
    version: &str,
    variant_subpath: Option<&str>,
) -> Result<PathBuf> {
    crate::serialise::validate_rez_package_path(pkg_name, Some(version))?;
    if variant_subpath.is_some_and(|path| !foundation::path::is_safe_rez_path(path, false)) {
        return Err(RezError::PackageRepository(
            "Unsafe legacy variant subpath".into(),
        ));
    }
    // Metadata and the entire source are checked before even opening the repository lock.
    let definition = find_package_file(src_variant_root)?;
    let (package, format, definition) =
        VerifiedPythonDefinition::load(&definition, pkg_name, version)?;
    validate_legacy_payload(src_variant_root)?;
    let stage = tempfile::tempdir()?;
    let relative = variant_subpath.map(PathBuf::from).unwrap_or_default();
    let excluded = if relative.as_os_str().is_empty() {
        crate::serialise::package_definition_file_names(
            crate::serialise::PACKAGE_DEFINITION_EXTENSIONS,
        )?
        .into_iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    crate::package::ops::copy_dir_contents(
        src_variant_root,
        &stage.path().join(&relative),
        true,
        false,
        Some(stage.path()),
        Some(&excluded),
    )?;
    let paths = fs::read_dir(stage.path())?
        .map(|entry| entry.map(|entry| (None, PathBuf::from(entry.file_name()))))
        .collect::<std::io::Result<Vec<_>>>()?;
    let version_dir = dest_repo_path.join(pkg_name).join(version);
    publish_package(
        &package,
        &version_dir,
        &[],
        Some(PublicationPayload::Merge {
            root: stage,
            paths: Some(paths),
            metadata: PublicationMetadata::Declared { definition },
        }),
        Some(format),
    )?;
    Ok(version_dir.join(relative))
}

/// Remove a package version from repository
///
/// Deletes the package directory. If the family directory becomes empty,
/// it is also removed.
///
/// # Arguments
/// * `repo_path` - Repository root path
/// * `pkg_name` - Package name
/// * `version` - Package version string
pub fn remove_package(repo_path: &Path, pkg_name: &str, version: &str) -> Result<()> {
    crate::serialise::validate_rez_package_path(pkg_name, Some(version))?;
    // Acquire lock for write operation
    let mut lock = LockFile::new(repo_path, pkg_name, Some(version));
    lock.acquire(30)?; // 30 sec timeout

    // Ignore first (prevents visibility during partial deletion)
    ignore_package(repo_path, pkg_name, version)?;

    // Delete package version directory
    let pkg_dir = repo_path.join(pkg_name).join(version);
    if pkg_dir.exists() {
        fs::remove_dir_all(&pkg_dir).map_err(|e| {
            RezError::PackageRepository(format!(
                "Failed to remove package directory {}: {}",
                pkg_dir.display(),
                e
            ))
        })?;
    }

    // Clean up ignore marker
    unignore_package(repo_path, pkg_name, version)?;

    // Remove family dir if empty
    let fam_dir = repo_path.join(pkg_name);
    if is_dir_empty(&fam_dir)? {
        fs::remove_dir(&fam_dir).map_err(|e| {
            RezError::PackageRepository(format!(
                "Failed to remove empty family directory {}: {}",
                fam_dir.display(),
                e
            ))
        })?;
    }

    lock.release()?;
    Ok(())
}

/// Remove a package family, preserving recognizable packages unless forced.
///
/// Returns the number of recognized package versions removed, or None when
/// the repository contains no source for this family.
pub fn remove_package_family(
    repo_path: &Path,
    pkg_name: &str,
    force: bool,
) -> Result<Option<usize>> {
    crate::serialise::validate_rez_package_path(pkg_name, None)?;
    let Some(repo) = FsRepo::open(repo_path, false)? else {
        return Ok(None);
    };
    let sources = repo.family_sources(pkg_name)?;
    if sources.is_empty() {
        return Ok(None);
    }

    let mut package_versions = BTreeSet::new();
    for source in &sources {
        match repo.package_versions_for_source(pkg_name, source, true, true) {
            Ok(versions) => package_versions.extend(versions),
            Err(error) if force => {
                log_debug!(
                    "repo",
                    "forced family removal skips package count for {}: {}",
                    source.path.display(),
                    error
                );
            }
            Err(error) => return Err(error),
        }
    }
    let package_count = package_versions.len();
    if package_count > 0 && !force {
        return Err(RezError::PackageRepository(format!(
            "Cannot remove non-empty package family {pkg_name:?} without force"
        )));
    }

    for source in &sources {
        let result = match source.kind {
            FamilySourceKind::Directory => fs::remove_dir_all(&source.path),
            FamilySourceKind::Python | FamilySourceKind::Yaml => fs::remove_file(&source.path),
        };
        result.map_err(|error| {
            RezError::PackageRepository(format!(
                "Failed to remove package family source {}: {}",
                source.path.display(),
                error
            ))
        })?;
    }

    Ok(Some(package_count))
}

/// Ignore a package version
///
/// Creates a `.ignore{version}` marker file that makes the package invisible
/// to resolves until unignored.
///
/// # Arguments
/// * `repo_path` - Repository root path
/// * `pkg_name` - Package name
/// * `version` - Package version string
pub fn ignore_package(repo_path: &Path, pkg_name: &str, version: &str) -> Result<()> {
    crate::serialise::validate_rez_package_path(
        pkg_name,
        (!version.is_empty()).then_some(version),
    )?;
    let fam_dir = repo_path.join(pkg_name);
    fs::create_dir_all(&fam_dir).map_err(|e| {
        RezError::PackageRepository(format!(
            "Failed to create package family directory {}: {}",
            fam_dir.display(),
            e
        ))
    })?;

    let ignore_file = fam_dir.join(format!("{}{}", IGNORE_PREFIX, version));

    // Skip if already ignored
    if ignore_file.exists() {
        return Ok(());
    }

    fs::File::create(&ignore_file).map_err(|e| {
        RezError::PackageRepository(format!(
            "Failed to create ignore marker {}: {}",
            ignore_file.display(),
            e
        ))
    })?;

    Ok(())
}

/// Unignore a package version
///
/// Removes the `.ignore{version}` marker file, making the package visible again.
///
/// # Arguments
/// * `repo_path` - Repository root path
/// * `pkg_name` - Package name
/// * `version` - Package version string
pub fn unignore_package(repo_path: &Path, pkg_name: &str, version: &str) -> Result<()> {
    crate::serialise::validate_rez_package_path(
        pkg_name,
        (!version.is_empty()).then_some(version),
    )?;
    let ignore_file = repo_path
        .join(pkg_name)
        .join(format!("{}{}", IGNORE_PREFIX, version));

    if ignore_file.exists() {
        fs::remove_file(&ignore_file).map_err(|e| {
            RezError::PackageRepository(format!(
                "Failed to remove ignore marker {}: {}",
                ignore_file.display(),
                e
            ))
        })?;
    }

    Ok(())
}

/// Check if a package version is ignored
///
/// # Arguments
/// * `repo_path` - Repository root path
/// * `pkg_name` - Package name
/// * `version` - Package version string
///
/// # Returns
/// `true` if package is ignored, `false` otherwise
pub fn is_package_ignored(repo_path: &Path, pkg_name: &str, version: &str) -> bool {
    if crate::serialise::validate_rez_package_path(
        pkg_name,
        (!version.is_empty()).then_some(version),
    )
    .is_err()
    {
        return false;
    }
    let ignore_file = repo_path
        .join(pkg_name)
        .join(format!("{}{}", IGNORE_PREFIX, version));
    ignore_file.exists()
}

#[cfg(test)]
#[path = "repository_tests.rs"]
mod tests;
