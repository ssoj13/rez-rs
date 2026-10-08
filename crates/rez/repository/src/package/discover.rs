// SPDX-License-Identifier: Apache-2.0

//! High-level package discovery API.
//!
//! Finds and iterates packages across repositories. Uses `CONFIG.packages_path` and
//! `PackageRepositoryManager` to search multiple repos in priority order.
//!
//! # Main functions
//! - `iter_package_families` — list all package names across repos
//! - `iter_packages` — packages matching name + optional version range
//! - `get_package` — exact (name, version) lookup
//! - `get_latest_package` — latest version matching criteria
//! - `get_developer_package` — load source package from dir (delegates to `DeveloperPackage::from_path` with full validation)
//! - `get_completions` — tab-completion for package names
//!
//! # Used by
//! - `cli/dev/build`, `cli/dev/test`, `cli/dev/release` → `get_developer_package`
//! - `cli/query/search` → `iter_package_families`, `iter_packages`
//! - `cli/resolve/complete` → `get_completions`

use crate::config::CONFIG;
use crate::errors::{Result, RezError};
use crate::repository::{PackageInfo, PackageRepositoryManager};
use model::package::DeveloperPackage;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use version::{Version, VersionRange};

// ============================================================================
// PackageSearchPath
// ============================================================================

/// A list of package repository paths for searching.
///
/// Wraps a list of filesystem paths to package repositories.
/// Provides convenience methods for searching across all repos.
pub struct PackageSearchPath {
    pub paths: Vec<PathBuf>,
}

impl PackageSearchPath {
    /// Create from explicit list of paths.
    pub fn new(paths: Vec<PathBuf>) -> Self {
        Self { paths }
    }

    /// Create from CONFIG.packages_path.
    pub fn from_config() -> Self {
        Self {
            paths: CONFIG.expanded_packages_path_os(),
        }
    }

    /// Iterate packages matching name and optional range.
    pub fn iter_packages(
        &self,
        name: &str,
        range: Option<&VersionRange>,
    ) -> Result<Vec<PackageInfo>> {
        iter_packages(name, range, Some(&self.paths))
    }

    /// Get all family names across all paths.
    pub fn iter_families(&self) -> Result<Vec<String>> {
        iter_package_families(Some(&self.paths))
    }
}

/// Get effective search paths (explicit or from config).
fn effective_paths(paths: Option<&[PathBuf]>) -> Vec<PathBuf> {
    match paths {
        Some(p) => p.to_vec(),
        None => CONFIG.expanded_packages_path_os(),
    }
}

/// Scan all packages in one pass — parallel scan per repo, merge with priority.
/// Each FsRepo::scan_all() parallelizes internally; repo scans run in parallel too.
pub fn iter_all_packages(paths: Option<&[PathBuf]>) -> Result<Vec<PackageInfo>> {
    let paths = effective_paths(paths);
    crate::log_info!("discover", "iter_all_packages: {} path(s)", paths.len());

    let manager = PackageRepositoryManager::from_paths(&paths)?;
    crate::log_info!(
        "discover",
        "iter_all_packages: scanning configured repositories"
    );
    let packages = manager.scan_all()?;

    crate::log_info!(
        "discover",
        "iter_all_packages: {} total package(s)",
        packages.len()
    );
    Ok(packages)
}

// ============================================================================
// Package query functions
// ============================================================================

/// Iterate over all package family names across repositories.
///
/// Returns a deduplicated sorted list of family names found in all repos.
/// If paths is None, uses CONFIG.packages_path.
///
/// # Arguments
/// * `paths` - Repository paths to search, or None for config default
pub fn iter_package_families(paths: Option<&[PathBuf]>) -> Result<Vec<String>> {
    let paths = effective_paths(paths);
    crate::log_info!("discover", "iter_package_families: {} path(s)", paths.len());
    for (i, p) in paths.iter().enumerate() {
        crate::log_trace!("discover", "  path {}: {}", i + 1, p.display());
    }

    let manager = PackageRepositoryManager::from_paths(&paths)?;
    crate::log_debug!("discover", "built repository manager from search paths");
    let result = manager.iter_family_names()?;
    crate::log_info!("discover", "total {} unique package families", result.len());
    Ok(result)
}

/// Iterate over packages matching name and optional version range.
///
/// Packages from earlier repositories in the search path take precedence -
/// duplicate (name, version) pairs from later repos are ignored.
///
/// # Arguments
/// * `name` - Package name, e.g. "maya"
/// * `range` - Optional version range filter
/// * `paths` - Repository paths, or None for config default
pub fn iter_packages(
    name: &str,
    range: Option<&VersionRange>,
    paths: Option<&[PathBuf]>,
) -> Result<Vec<PackageInfo>> {
    let paths = effective_paths(paths);
    crate::log_info!(
        "discover",
        "iter_packages: name={} range={:?} paths={}",
        name,
        range,
        paths.len()
    );
    let manager = PackageRepositoryManager::from_paths(&paths)?;

    // Manager handles dedup (earlier repo wins for same version)
    let all_packages = manager.iter_packages(name)?;
    crate::log_debug!(
        "discover",
        "iter_packages: {} raw package(s) for {}",
        all_packages.len(),
        name
    );

    // Apply range filter
    let result = match range {
        Some(r) => all_packages
            .into_iter()
            .filter(|p| r.contains_version(&p.version))
            .collect(),
        None => all_packages,
    };
    crate::log_info!(
        "discover",
        "iter_packages: {} package(s) for {}",
        result.len(),
        name
    );
    Ok(result)
}

/// Get a specific package by exact name and version.
///
/// Searches repositories in order, returns first match.
pub fn get_package(
    name: &str,
    version: &Version,
    paths: Option<&[PathBuf]>,
) -> Result<Option<PackageInfo>> {
    let paths = effective_paths(paths);
    let manager = PackageRepositoryManager::from_paths(&paths)?;
    manager.get_package(name, version)
}

/// Get the latest package version matching name and optional range.
///
/// Returns the highest version found across all repositories.
pub fn get_latest_package(
    name: &str,
    range: Option<&VersionRange>,
    paths: Option<&[PathBuf]>,
) -> Result<Option<PackageInfo>> {
    let packages = iter_packages(name, range, paths)?;
    // iter_packages returns sorted descending by version (from manager)
    Ok(packages.into_iter().next())
}

/// Get a developer package from a directory.
///
/// Delegates to `DeveloperPackage::from_path` — single path with full `validate_package_data`.
/// Probe order: configured filename stems, then .py, .yaml, .toml, .yml.
pub fn get_developer_package(path: &Path) -> Result<DeveloperPackage> {
    crate::log_info!(
        "discover",
        "loading developer package from {}",
        path.display()
    );
    DeveloperPackage::from_path(path)
}

/// Get tab-completion suggestions for a package name prefix.
///
/// If the prefix contains '-', '@', or '#', also includes version completions.
/// Supports prefix operators '!' and '~' for conflict/weak requirements.
pub fn get_completions(prefix: &str, paths: Option<&[PathBuf]>) -> Result<Vec<String>> {
    let mut work_prefix = prefix;
    let mut op_prefix = "";

    // Handle ! and ~ prefix operators
    if let Some(first) = work_prefix.as_bytes().first() {
        if *first == b'!' || *first == b'~' {
            op_prefix = &work_prefix[..1];
            work_prefix = &work_prefix[1..];
        }
    }

    // Check if prefix contains a version separator
    let mut family_name: Option<String> = None;
    for ch in ['-', '@', '#'] {
        if let Some(pos) = work_prefix.find(ch) {
            family_name = Some(work_prefix[..pos].to_string());
            break;
        }
    }

    let families = iter_package_families(paths)?;
    let mut words = HashSet::new();

    // Match family names
    if family_name.is_none() {
        for name in &families {
            if name.starts_with(work_prefix) {
                words.insert(name.clone());
            }
        }

        // If exactly one family matches, expand to include versions
        if words.len() == 1 {
            family_name = words.iter().next().cloned();
        }
    }

    // Match versioned names if we have a family
    if let Some(ref fam) = family_name {
        let packages = iter_packages(fam, None, paths)?;
        for pkg in &packages {
            let qualified = if pkg.version.is_truthy() {
                format!("{}-{}", pkg.name, pkg.version)
            } else {
                pkg.name.clone()
            };
            if qualified.starts_with(work_prefix) {
                words.insert(qualified);
            }
        }
    }

    // Apply operator prefix back
    let mut result: Vec<String> = if op_prefix.is_empty() {
        words.into_iter().collect()
    } else {
        words
            .into_iter()
            .map(|w| format!("{}{}", op_prefix, w))
            .collect()
    };

    result.sort();
    Ok(result)
}

/// Get a package by parsing a versioned string like "foo-1.2.3".
pub fn get_package_from_string(
    txt: &str,
    paths: Option<&[PathBuf]>,
) -> Result<Option<PackageInfo>> {
    let obj: version::VersionedObject = txt.parse().map_err(|e: RezError| {
        RezError::PackageRequest(format!("Invalid package string '{}': {}", txt, e))
    })?;

    get_package(obj.name(), obj.version(), paths)
}

/// Get latest package matching a request string like "foo-1.2+".
pub fn get_latest_package_from_string(
    txt: &str,
    paths: Option<&[PathBuf]>,
) -> Result<Option<PackageInfo>> {
    let req = version::Requirement::new(txt)?;
    let range = req.range().cloned();
    get_latest_package(req.name(), range.as_ref(), paths)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::MemoryPackageRepository;
    use std::collections::HashMap;

    /// Helper: create a MemoryRepo with predefined packages.
    fn make_repo(packages: &[(&str, &str)]) -> MemoryPackageRepository {
        let mut repo = MemoryPackageRepository::new();
        for (name, version) in packages {
            let mut data = HashMap::new();
            data.insert("name".to_string(), serde_json::json!(name));
            if !version.is_empty() {
                data.insert("version".to_string(), serde_json::json!(version));
            }
            let info = PackageInfo::from_data(data).unwrap();
            repo.add_package(info).unwrap();
        }
        repo
    }

    /// Helper: create a manager from multiple repos.
    fn make_manager(repos: Vec<MemoryPackageRepository>) -> PackageRepositoryManager {
        let mut manager = PackageRepositoryManager::new();
        for repo in repos {
            manager.add_repo(Box::new(repo));
        }
        manager
    }

    // -- iter_packages tests --

    #[test]
    fn test_iter_packages_no_range() {
        let repo = make_repo(&[("maya", "2020.0"), ("maya", "2023.0"), ("maya", "2024.0")]);
        let mut manager = PackageRepositoryManager::new();
        manager.add_repo(Box::new(repo));

        let packages = manager.iter_packages("maya").unwrap();
        assert_eq!(packages.len(), 3);

        // Sorted descending (latest first)
        assert_eq!(packages[0].version.to_string(), "2024.0");
        assert_eq!(packages[1].version.to_string(), "2023.0");
        assert_eq!(packages[2].version.to_string(), "2020.0");
    }

    #[test]
    fn test_iter_packages_with_range_filter() {
        let repo = make_repo(&[("maya", "2020.0"), ("maya", "2023.0"), ("maya", "2024.0")]);
        let mut manager = PackageRepositoryManager::new();
        manager.add_repo(Box::new(repo));

        let all = manager.iter_packages("maya").unwrap();
        let range = VersionRange::new(">=2023").unwrap();
        let filtered: Vec<_> = all
            .into_iter()
            .filter(|p| range.contains_version(&p.version))
            .collect();

        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].version.to_string(), "2024.0");
        assert_eq!(filtered[1].version.to_string(), "2023.0");
    }

    #[test]
    fn test_iter_packages_priority() {
        let repo1 = make_repo(&[("pkg", "1.0")]);
        let repo2 = make_repo(&[("pkg", "1.0"), ("pkg", "2.0")]);
        let manager = make_manager(vec![repo1, repo2]);

        let packages = manager.iter_packages("pkg").unwrap();
        assert_eq!(packages.len(), 2); // 1.0 from repo1, 2.0 from repo2
    }

    #[test]
    fn test_iter_packages_nonexistent() {
        let repo = make_repo(&[("foo", "1.0")]);
        let mut manager = PackageRepositoryManager::new();
        manager.add_repo(Box::new(repo));

        let packages = manager.iter_packages("nonexistent").unwrap();
        assert!(packages.is_empty());
    }

    // -- get_package tests --

    #[test]
    fn test_get_package_found() {
        let repo = make_repo(&[("foo", "1.0"), ("foo", "2.0")]);
        let mut manager = PackageRepositoryManager::new();
        manager.add_repo(Box::new(repo));

        let v = Version::new("1.0").unwrap();
        let pkg = manager.get_package("foo", &v).unwrap();
        assert!(pkg.is_some());
        assert_eq!(pkg.unwrap().version.to_string(), "1.0");
    }

    #[test]
    fn test_get_package_not_found() {
        let repo = make_repo(&[("foo", "1.0")]);
        let mut manager = PackageRepositoryManager::new();
        manager.add_repo(Box::new(repo));

        let v = Version::new("9.9.9").unwrap();
        let pkg = manager.get_package("foo", &v).unwrap();
        assert!(pkg.is_none());
    }

    // -- get_latest tests --

    #[test]
    fn test_get_latest_no_range() {
        let repo = make_repo(&[("maya", "2020.0"), ("maya", "2024.0"), ("maya", "2023.0")]);
        let mut manager = PackageRepositoryManager::new();
        manager.add_repo(Box::new(repo));

        let latest = manager.get_latest("maya").unwrap();
        assert!(latest.is_some());
        assert_eq!(latest.unwrap().version.to_string(), "2024.0");
    }

    #[test]
    fn test_get_latest_with_range() {
        let repo = make_repo(&[("maya", "2020.0"), ("maya", "2024.0"), ("maya", "2023.0")]);
        let mut manager = PackageRepositoryManager::new();
        manager.add_repo(Box::new(repo));

        let all = manager.iter_packages("maya").unwrap();
        let range = VersionRange::new("<2024").unwrap();
        let latest = all.into_iter().find(|p| range.contains_version(&p.version));

        assert!(latest.is_some());
        assert_eq!(latest.unwrap().version.to_string(), "2023.0");
    }

    #[test]
    fn test_get_latest_no_match() {
        let repo = make_repo(&[("foo", "1.0")]);
        let mut manager = PackageRepositoryManager::new();
        manager.add_repo(Box::new(repo));

        let latest = manager.get_latest("nonexistent").unwrap();
        assert!(latest.is_none());
    }

    // -- PackageSearchPath tests --

    #[test]
    fn test_search_path_new() {
        let sp = PackageSearchPath::new(vec![PathBuf::from("/tmp/repo1")]);
        assert_eq!(sp.paths.len(), 1);
    }

    #[test]
    fn test_missing_search_path_is_not_created_and_search_succeeds_empty() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing-repository");
        let paths = [missing.clone()];

        assert!(iter_package_families(Some(&paths)).unwrap().is_empty());
        assert!(iter_all_packages(Some(&paths)).unwrap().is_empty());
        assert!(!missing.exists());
    }

    #[test]
    fn test_invalid_search_path_errors_instead_of_being_skipped() {
        let temp = tempfile::tempdir().unwrap();
        let file_path = temp.path().join("not-a-repository");
        std::fs::write(&file_path, "file").unwrap();
        let paths = [file_path];

        assert!(iter_package_families(Some(&paths)).is_err());
        assert!(iter_all_packages(Some(&paths)).is_err());
    }

    #[test]
    fn test_full_scan_propagates_package_definition_errors_without_partial_success() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        let valid_definition = repo.join("good").join("1.0").join("package.yaml");
        let invalid_definition = repo.join("broken").join("1.0").join("package.yaml");
        std::fs::create_dir_all(valid_definition.parent().unwrap()).unwrap();
        std::fs::create_dir_all(invalid_definition.parent().unwrap()).unwrap();
        std::fs::write(&valid_definition, "name: good\nversion: \"1.0\"\n").unwrap();
        std::fs::write(&invalid_definition, "name: broken\nversion: [invalid\n").unwrap();
        let paths = [repo.to_path_buf()];

        let families = iter_package_families(Some(&paths)).unwrap();
        assert_eq!(families, vec!["broken", "good"]);
        assert!(iter_all_packages(Some(&paths)).is_err());
    }

    // -- Developer package tests --

    #[test]
    fn test_developer_package_no_file() {
        let dir = std::env::temp_dir().join("rez_test_no_pkg");
        let _ = std::fs::create_dir_all(&dir);

        let result = get_developer_package(&dir);
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_developer_package_from_yaml() {
        let dir = std::env::temp_dir().join("rez_test_dev_pkg_yaml");
        let _ = std::fs::create_dir_all(&dir);

        let yaml_content = "name: testpkg\nversion: \"1.0.0\"\n";
        std::fs::write(dir.join("package.yaml"), yaml_content).unwrap();

        let dev_pkg = get_developer_package(&dir).unwrap();
        assert_eq!(dev_pkg.name(), "testpkg");
        assert_eq!(dev_pkg.version().to_string(), "1.0.0");
        assert!(dev_pkg.filepath.ends_with("package.yaml"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_developer_package_from_toml() {
        let dir = std::env::temp_dir().join("rez_test_dev_pkg_toml");
        let _ = std::fs::create_dir_all(&dir);

        let toml_content = "name = \"mypkg\"\nversion = \"2.0.0\"\n";
        std::fs::write(dir.join("package.toml"), toml_content).unwrap();

        let dev_pkg = get_developer_package(&dir).unwrap();
        assert_eq!(dev_pkg.name(), "mypkg");
        assert_eq!(dev_pkg.version().to_string(), "2.0.0");
        assert!(dev_pkg.filepath.ends_with("package.toml"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_developer_package_yaml_precedes_toml() {
        let dir = std::env::temp_dir().join("rez_test_dev_pkg_priority");
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(
            dir.join("package.toml"),
            "name = \"toml_pkg\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("package.yaml"),
            "name: yaml_pkg\nversion: \"2.0.0\"\n",
        )
        .unwrap();

        let dev_pkg = get_developer_package(&dir).unwrap();
        assert_eq!(dev_pkg.name(), "yaml_pkg"); // upstream YAML precedes additive TOML

        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- Full pipeline integration test --

    #[test]
    fn test_full_pipeline() {
        let repo1 = make_repo(&[("maya", "2020.0"), ("maya", "2023.0"), ("houdini", "19.5")]);
        let repo2 = make_repo(&[("maya", "2024.0"), ("nuke", "14.0")]);
        let manager = make_manager(vec![repo1, repo2]);

        // All maya versions
        let maya_pkgs = manager.iter_packages("maya").unwrap();
        assert_eq!(maya_pkgs.len(), 3);
        assert_eq!(maya_pkgs[0].version.to_string(), "2024.0");
        assert_eq!(maya_pkgs[1].version.to_string(), "2023.0");
        assert_eq!(maya_pkgs[2].version.to_string(), "2020.0");

        // Latest maya
        let latest = manager.get_latest("maya").unwrap().unwrap();
        assert_eq!(latest.version.to_string(), "2024.0");

        // Specific version
        let v = Version::new("2023.0").unwrap();
        let specific = manager.get_package("maya", &v).unwrap().unwrap();
        assert_eq!(specific.name, "maya");
        assert_eq!(specific.version.to_string(), "2023.0");

        // Range filter
        let range = VersionRange::new(">=2023").unwrap();
        let filtered: Vec<_> = manager
            .iter_packages("maya")
            .unwrap()
            .into_iter()
            .filter(|p| range.contains_version(&p.version))
            .collect();
        assert_eq!(filtered.len(), 2);
    }

    // -- get_completions with memory repo --

    #[test]
    fn test_completions_logic() {
        // Test completion matching logic directly
        let names = [
            "maya".to_string(),
            "maya_utils".to_string(),
            "houdini".to_string(),
        ];

        let matching: Vec<_> = names
            .iter()
            .filter(|n| n.starts_with("may"))
            .cloned()
            .collect();

        assert_eq!(matching.len(), 2);
        assert!(matching.contains(&"maya".to_string()));
        assert!(matching.contains(&"maya_utils".to_string()));
    }

    #[test]
    fn test_completions_with_operator() {
        // Test ! and ~ prefix handling
        let prefix = "!maya";
        let mut work = prefix;
        let mut op = "";

        if let Some(first) = work.as_bytes().first() {
            if *first == b'!' || *first == b'~' {
                op = &work[..1];
                work = &work[1..];
            }
        }

        assert_eq!(op, "!");
        assert_eq!(work, "maya");

        let result = format!("{}{}", op, "maya-2020.0");
        assert_eq!(result, "!maya-2020.0");
    }

    #[test]
    fn test_completions_version_separator() {
        // Test detection of version separator in prefix
        let prefix = "maya-202";
        let mut family: Option<&str> = None;
        for ch in ['-', '@', '#'] {
            if let Some(pos) = prefix.find(ch) {
                family = Some(&prefix[..pos]);
                break;
            }
        }
        assert_eq!(family, Some("maya"));
    }
}
