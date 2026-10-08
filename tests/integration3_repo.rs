// SPDX-License-Identifier: Apache-2.0
// Integration tests for FsRepo and related systems
//
// Comprehensive test suite covering:
// - Basic repository operations (iter_families, iter_versions, get_package)
// - Hidden directory handling (. and _ prefixes)
// - Package ignore/unignore functionality
// - Variant installation
// - Package removal
// - Multiple package formats (YAML, TOML, Python)
// - Multi-repo management with priority
// - Repository settings
// - Package copy/move operations

use ::repository::repository::*;
use repository::package::ops::*;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

// ============================================================================
// Test fixtures and helpers
// ============================================================================

/// Create temp repo with test packages
fn create_test_repo() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let repo_path = dir.path().to_path_buf();

    // Create package: foo/1.0.0/package.yaml
    let foo_dir = repo_path.join("foo").join("1.0.0");
    fs::create_dir_all(&foo_dir).unwrap();
    fs::write(
        foo_dir.join("package.yaml"),
        "name: foo\nversion: \"1.0.0\"\ndescription: Test package foo\n",
    )
    .unwrap();

    // Create package: foo/2.0.0/package.yaml
    let foo2_dir = repo_path.join("foo").join("2.0.0");
    fs::create_dir_all(&foo2_dir).unwrap();
    fs::write(
        foo2_dir.join("package.yaml"),
        "name: foo\nversion: \"2.0.0\"\nrequires:\n  - bar-1+\n",
    )
    .unwrap();

    // Create package: bar/1.0.0/package.yaml
    let bar_dir = repo_path.join("bar").join("1.0.0");
    fs::create_dir_all(&bar_dir).unwrap();
    fs::write(
        bar_dir.join("package.yaml"),
        "name: bar\nversion: \"1.0.0\"\ndescription: Test package bar\n",
    )
    .unwrap();

    (dir, repo_path)
}

/// Create empty temp repo
fn create_empty_repo() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let repo_path = dir.path().to_path_buf();
    fs::create_dir_all(&repo_path).unwrap();
    (dir, repo_path)
}

// ============================================================================
// Basic repository operations
// ============================================================================

#[test]
fn test_repo_iter_families() {
    let (_dir, repo_path) = create_test_repo();
    let repo = FsRepo::new(&repo_path).unwrap();

    let families = repo.iter_family_names().unwrap();
    assert_eq!(families.len(), 2);
    assert!(families.contains(&"foo".to_string()));
    assert!(families.contains(&"bar".to_string()));
}

#[test]
fn test_repo_iter_versions() {
    let (_dir, repo_path) = create_test_repo();
    let repo = FsRepo::new(&repo_path).unwrap();

    // foo has 2 versions
    let foo_versions = repo.iter_versions("foo").unwrap();
    assert_eq!(foo_versions.len(), 2);
    assert_eq!(foo_versions[0].to_string(), "1.0.0");
    assert_eq!(foo_versions[1].to_string(), "2.0.0");

    // bar has 1 version
    let bar_versions = repo.iter_versions("bar").unwrap();
    assert_eq!(bar_versions.len(), 1);
    assert_eq!(bar_versions[0].to_string(), "1.0.0");
}

#[test]
fn test_repo_get_package() {
    let (_dir, repo_path) = create_test_repo();
    let repo = FsRepo::new(&repo_path).unwrap();

    let version = "1.0.0".parse().unwrap();
    let pkg = repo.get_package("foo", &version).unwrap();

    assert!(pkg.is_some());
    let pkg = pkg.unwrap();
    assert_eq!(pkg.name, "foo");
    assert_eq!(pkg.version.to_string(), "1.0.0");
    assert!(pkg.get("description").is_some());
}

#[test]
fn test_repo_missing_family() {
    let (_dir, repo_path) = create_test_repo();
    let repo = FsRepo::new(&repo_path).unwrap();

    let versions = repo.iter_versions("nonexistent").unwrap();
    assert!(versions.is_empty());
}

#[test]
fn test_repo_missing_version() {
    let (_dir, repo_path) = create_test_repo();
    let repo = FsRepo::new(&repo_path).unwrap();

    let version = "9.9.9".parse().unwrap();
    let pkg = repo.get_package("foo", &version).unwrap();
    assert!(pkg.is_none());
}

// ============================================================================
// Hidden directories
// ============================================================================

#[test]
fn test_repo_hidden_dirs() {
    let (_dir, repo_path) = create_test_repo();

    // Create hidden dirs (should be skipped)
    let hidden1 = repo_path.join(".hidden");
    fs::create_dir_all(hidden1.join("1.0.0")).unwrap();
    fs::write(
        hidden1.join("1.0.0").join("package.yaml"),
        "name: .hidden\nversion: \"1.0.0\"\n",
    )
    .unwrap();

    let hidden2 = repo_path.join("_internal");
    fs::create_dir_all(hidden2.join("1.0.0")).unwrap();
    fs::write(
        hidden2.join("1.0.0").join("package.yaml"),
        "name: _internal\nversion: \"1.0.0\"\n",
    )
    .unwrap();

    let repo = FsRepo::new(&repo_path).unwrap();
    let families = repo.iter_family_names().unwrap();

    // Dot-prefixed dirs are hidden; underscore-prefixed Rez names are valid.
    assert!(!families.contains(&".hidden".to_string()));
    assert!(families.contains(&"_internal".to_string()));
}

#[test]
fn test_repo_hidden_version_dirs() {
    let (_dir, repo_path) = create_test_repo();

    // Create hidden version dir (should be skipped)
    let foo_dir = repo_path.join("foo");
    let hidden_ver = foo_dir.join(".dev");
    fs::create_dir_all(&hidden_ver).unwrap();
    fs::write(
        hidden_ver.join("package.yaml"),
        "name: foo\nversion: \"3.0.0-dev\"\n",
    )
    .unwrap();

    let repo = FsRepo::new(&repo_path).unwrap();
    let versions = repo.iter_versions("foo").unwrap();

    // Should only have 1.0.0 and 2.0.0, not .dev
    assert_eq!(versions.len(), 2);
}

// ============================================================================
// Package ignore/unignore
// ============================================================================

#[test]
fn test_repo_ignore_package() {
    let (_dir, repo_path) = create_test_repo();

    // Initially visible
    let repo = FsRepo::new(&repo_path).unwrap();
    let versions = repo.iter_versions("foo").unwrap();
    assert_eq!(versions.len(), 2);

    // Ignore one version
    ignore_package(&repo_path, "foo", "1.0.0").unwrap();

    // Check ignore marker exists
    assert!(is_package_ignored(&repo_path, "foo", "1.0.0"));
    assert!(!is_package_ignored(&repo_path, "foo", "2.0.0"));
}

#[test]
fn test_repo_unignore_package() {
    let (_dir, repo_path) = create_test_repo();

    // Ignore then unignore
    ignore_package(&repo_path, "foo", "1.0.0").unwrap();
    assert!(is_package_ignored(&repo_path, "foo", "1.0.0"));

    unignore_package(&repo_path, "foo", "1.0.0").unwrap();
    assert!(!is_package_ignored(&repo_path, "foo", "1.0.0"));
}

#[test]
fn test_repo_ignore_cycle() {
    let (_dir, repo_path) = create_test_repo();
    let repo = FsRepo::new(&repo_path).unwrap();

    // Initially visible
    let versions = repo.iter_versions("foo").unwrap();
    assert_eq!(versions.len(), 2);

    // Ignore one version
    ignore_package(&repo_path, "foo", "1.0.0").unwrap();
    assert!(is_package_ignored(&repo_path, "foo", "1.0.0"));

    // iter_versions DOES filter ignored packages
    let after_ignore = repo.iter_versions("foo").unwrap();
    assert_eq!(after_ignore.len(), 1); // Only 2.0.0 visible
    assert_eq!(after_ignore[0].to_string(), "2.0.0");

    // Unignore
    unignore_package(&repo_path, "foo", "1.0.0").unwrap();
    assert!(!is_package_ignored(&repo_path, "foo", "1.0.0"));

    let after_unignore = repo.iter_versions("foo").unwrap();
    assert_eq!(after_unignore.len(), 2); // Both visible again
}

#[test]
fn test_ignore_nonexistent_package() {
    let (_dir, repo_path) = create_test_repo();

    // Ignoring nonexistent package should create family dir and marker
    ignore_package(&repo_path, "newpkg", "1.0.0").unwrap();
    assert!(is_package_ignored(&repo_path, "newpkg", "1.0.0"));

    // Family dir should exist
    assert!(repo_path.join("newpkg").exists());
}

// ============================================================================
// Variant installation
// ============================================================================

#[test]
fn test_repo_install_variant() {
    let (_dir, repo_path) = create_test_repo();

    // Create variant payload in temp dir
    let temp = TempDir::new().unwrap();
    let variant_root = temp.path().join("variant");
    fs::create_dir_all(&variant_root).unwrap();
    fs::write(variant_root.join("lib.so"), "binary data").unwrap();

    // Create package definition
    fs::write(
        variant_root.join("package.yaml"),
        "name: baz\nversion: \"1.0.0\"\n",
    )
    .unwrap();

    // Install variant
    let installed = install_variant(
        &variant_root,
        &repo_path,
        "baz",
        "1.0.0",
        Some("platform-linux"),
    )
    .unwrap();

    // Verify structure
    assert!(installed.exists());
    assert!(installed.join("lib.so").exists());

    // Package definition should be at version root
    let version_dir = repo_path.join("baz").join("1.0.0");
    assert!(version_dir.join("package.yaml").exists());

    // Verify package is findable via repo
    let repo = FsRepo::new(&repo_path).unwrap();
    let versions = repo.iter_versions("baz").unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].to_string(), "1.0.0");
}

#[test]
fn test_repo_install_variant_no_subpath() {
    let (_dir, repo_path) = create_test_repo();

    let temp = TempDir::new().unwrap();
    let variant_root = temp.path().join("variant");
    fs::create_dir_all(&variant_root).unwrap();
    fs::write(variant_root.join("data.txt"), "content").unwrap();
    fs::write(
        variant_root.join("package.yaml"),
        "name: test\nversion: \"1.0.0\"\n",
    )
    .unwrap();

    // Install without subpath (directly into version dir)
    let installed = install_variant(&variant_root, &repo_path, "test", "1.0.0", None).unwrap();

    assert_eq!(installed, repo_path.join("test").join("1.0.0"));
    assert!(installed.join("data.txt").exists());
    assert!(installed.join("package.yaml").exists());
}

// ============================================================================
// Package removal
// ============================================================================

#[test]
fn test_repo_remove_package() {
    let (_dir, repo_path) = create_test_repo();

    // Verify package exists
    let repo = FsRepo::new(&repo_path).unwrap();
    let version = "1.0.0".parse().unwrap();
    assert!(repo.get_package("foo", &version).unwrap().is_some());

    // Remove package
    repository::remove_package(&repo_path, "foo", "1.0.0").unwrap();

    // Verify package is gone
    let repo = FsRepo::new(&repo_path).unwrap();
    assert!(repo.get_package("foo", &version).unwrap().is_none());

    // Family dir should still exist (has 2.0.0)
    assert!(repo_path.join("foo").exists());

    // Version dir should be gone
    assert!(!repo_path.join("foo").join("1.0.0").exists());
}

#[test]
fn test_repo_remove_package_cleans_family() {
    let (_dir, repo_path) = create_test_repo();

    // Remove all foo versions
    repository::remove_package(&repo_path, "foo", "1.0.0").unwrap();
    repository::remove_package(&repo_path, "foo", "2.0.0").unwrap();

    // Family dir should be gone (was empty)
    assert!(!repo_path.join("foo").exists());
}

#[test]
fn test_repo_remove_nonexistent() {
    let (_dir, repo_path) = create_test_repo();

    // Removing nonexistent package should succeed (no-op)
    repository::remove_package(&repo_path, "nonexistent", "1.0.0").unwrap();
}

#[test]
fn test_repo_remove_family() {
    let (_dir, repo_path) = create_test_repo();

    repository::remove_package_family(&repo_path, "foo", true).unwrap();

    // Family dir should be gone
    assert!(!repo_path.join("foo").exists());

    // Other families should remain
    assert!(repo_path.join("bar").exists());
}

// ============================================================================
// Package formats
// ============================================================================

#[test]
fn test_repo_with_package_toml() {
    let (_dir, repo_path) = create_empty_repo();

    // Create package with TOML format
    let pkg_dir = repo_path.join("tomlpkg").join("1.0.0");
    fs::create_dir_all(&pkg_dir).unwrap();
    fs::write(
        pkg_dir.join("package.toml"),
        r#"
name = "tomlpkg"
version = "1.0.0"
description = "TOML format package"
"#,
    )
    .unwrap();

    let repo = FsRepo::new(&repo_path).unwrap();
    let families = repo.iter_family_names().unwrap();
    assert!(families.contains(&"tomlpkg".to_string()));

    let version = "1.0.0".parse().unwrap();
    let pkg = repo.get_package("tomlpkg", &version).unwrap().unwrap();
    assert_eq!(pkg.name, "tomlpkg");
    assert_eq!(
        pkg.get("description").unwrap().as_str().unwrap(),
        "TOML format package"
    );
}

#[test]
fn test_repo_with_package_py() {
    let (_dir, repo_path) = create_empty_repo();

    // Create package with Python format (simple, no @early/@late)
    let pkg_dir = repo_path.join("pypkg").join("1.0.0");
    fs::create_dir_all(&pkg_dir).unwrap();
    fs::write(
        pkg_dir.join("package.py"),
        r#"
name = "pypkg"
version = "1.0.0"
description = "Python format package"
"#,
    )
    .unwrap();

    let repo = FsRepo::new(&repo_path).unwrap();
    let version = "1.0.0".parse().unwrap();
    let pkg = repo.get_package("pypkg", &version).unwrap().unwrap();
    assert_eq!(pkg.name, "pypkg");
    assert_eq!(
        pkg.get("description").unwrap().as_str().unwrap(),
        "Python format package"
    );
}

#[test]
fn test_repo_mixed_formats() {
    let (_dir, repo_path) = create_empty_repo();

    // YAML package
    let yaml_dir = repo_path.join("yamlpkg").join("1.0.0");
    fs::create_dir_all(&yaml_dir).unwrap();
    fs::write(
        yaml_dir.join("package.yaml"),
        "name: yamlpkg\nversion: \"1.0.0\"\n",
    )
    .unwrap();

    // TOML package
    let toml_dir = repo_path.join("tomlpkg").join("1.0.0");
    fs::create_dir_all(&toml_dir).unwrap();
    fs::write(
        toml_dir.join("package.toml"),
        "name = \"tomlpkg\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();

    let repo = FsRepo::new(&repo_path).unwrap();
    let families = repo.iter_family_names().unwrap();
    assert_eq!(families.len(), 2);
    assert!(families.contains(&"yamlpkg".to_string()));
    assert!(families.contains(&"tomlpkg".to_string()));
}

// ============================================================================
// Multi-repo management
// ============================================================================

#[test]
fn test_manager_multi_repo() {
    let (_dir1, repo1_path) = create_test_repo();
    let (_dir2, repo2_path) = create_empty_repo();

    // Add package to repo2
    let pkg_dir = repo2_path.join("qux").join("1.0.0");
    fs::create_dir_all(&pkg_dir).unwrap();
    fs::write(
        pkg_dir.join("package.yaml"),
        "name: qux\nversion: \"1.0.0\"\n",
    )
    .unwrap();

    let mut manager = PackageRepositoryManager::new();
    manager.add_repo(Box::new(FsRepo::new(&repo1_path).unwrap()));
    manager.add_repo(Box::new(FsRepo::new(&repo2_path).unwrap()));

    // foo is in repo1
    let packages = manager.iter_packages("foo").unwrap();
    assert_eq!(packages.len(), 2); // 1.0.0 and 2.0.0

    // qux is in repo2
    let packages = manager.iter_packages("qux").unwrap();
    assert_eq!(packages.len(), 1);
}

#[test]
fn test_manager_priority() {
    let (_dir1, repo1_path) = create_test_repo();
    let (_dir2, repo2_path) = create_empty_repo();

    // Add same package to repo2 with different data
    let pkg_dir = repo2_path.join("foo").join("1.0.0");
    fs::create_dir_all(&pkg_dir).unwrap();
    fs::write(
        pkg_dir.join("package.yaml"),
        "name: foo\nversion: \"1.0.0\"\nrepo: repo2\n",
    )
    .unwrap();

    let mut manager = PackageRepositoryManager::new();
    manager.add_repo(Box::new(FsRepo::new(&repo1_path).unwrap()));
    manager.add_repo(Box::new(FsRepo::new(&repo2_path).unwrap()));

    // First repo wins for same version
    let version = "1.0.0".parse().unwrap();
    let pkg = manager.get_package("foo", &version).unwrap().unwrap();
    // repo1's package doesn't have 'repo' field
    assert!(pkg.get("repo").is_none());
}

#[test]
fn test_manager_latest_version() {
    let (_dir1, repo1_path) = create_test_repo();
    let (_dir2, repo2_path) = create_empty_repo();

    // Add newer version to repo2
    let pkg_dir = repo2_path.join("foo").join("3.0.0");
    fs::create_dir_all(&pkg_dir).unwrap();
    fs::write(
        pkg_dir.join("package.yaml"),
        "name: foo\nversion: \"3.0.0\"\n",
    )
    .unwrap();

    let mut manager = PackageRepositoryManager::new();
    manager.add_repo(Box::new(FsRepo::new(&repo1_path).unwrap()));
    manager.add_repo(Box::new(FsRepo::new(&repo2_path).unwrap()));

    let latest = manager.get_latest("foo").unwrap().unwrap();
    assert_eq!(latest.version.to_string(), "3.0.0");
}

// ============================================================================
// Repository settings
// ============================================================================

#[test]
fn test_repo_settings_yaml() {
    let (_dir, repo_path) = create_empty_repo();

    // Create settings.yaml
    fs::write(
        repo_path.join("settings.yaml"),
        r#"
file_lock_timeout: 120
package_filenames: ["package.toml", "package.yaml"]
name: "Test Repository"
disable_memcache: true
"#,
    )
    .unwrap();

    let settings = load_repo_settings(&repo_path).unwrap();
    assert_eq!(settings.file_lock_timeout, Some(120));
    assert_eq!(settings.name, Some("Test Repository".to_string()));
    assert_eq!(settings.disable_memcache, Some(true));
}

#[test]
fn test_repo_settings_missing() {
    let (_dir, repo_path) = create_empty_repo();

    let settings = load_repo_settings(&repo_path);
    assert!(settings.is_none());
}

// ============================================================================
// Package copy/move operations
// ============================================================================

fn candidate(repo: &Path, name: &str, version: &str) -> repository::PackageCandidate {
    let repository = FsRepo::new(repo).unwrap();
    let version = version::Version::new(version).unwrap();
    let (info, source) = repository
        .get_package_with_source(name, &version, None)
        .unwrap()
        .unwrap();
    repository::PackageCandidate {
        package: info.to_package().unwrap(),
        provenance: Some(repository::PackageProvenance {
            repository_type: "filesystem".into(),
            location: repo.to_string_lossy().into_owned(),
            source,
        }),
    }
}

#[test]
fn test_copy_between_repos() {
    let (_dir1, src_repo) = create_test_repo();
    let (_dir2, dest_repo) = create_empty_repo();

    // Copy foo-1.0.0 from src to dest
    let opts = CopyOptions::default();

    let result = copy_package(&candidate(&src_repo, "foo", "1.0.0"), &dest_repo, &opts).unwrap();
    assert!(!result.copied.is_empty());
    assert!(result.skipped.is_empty());

    // Verify copied package is readable
    let repo = FsRepo::new(&dest_repo).unwrap();
    let versions = repo.iter_versions("foo").unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].to_string(), "1.0.0");

    let version = "1.0.0".parse().unwrap();
    let pkg = repo.get_package("foo", &version).unwrap().unwrap();
    assert_eq!(pkg.name, "foo");
}

#[test]
fn test_copy_overwrites() {
    let (_dir1, src_repo) = create_test_repo();
    let (_dir2, dest_repo) = create_empty_repo();

    // Create existing package at dest
    let dst = dest_repo.join("foo").join("1.0.0");
    copy_package(
        &candidate(&src_repo, "foo", "1.0.0"),
        &dest_repo,
        &CopyOptions::default(),
    )
    .unwrap();
    fs::write(dst.join("old.txt"), "old data").unwrap();

    // Copy with overwrite=false (should skip)
    let opts = CopyOptions {
        overwrite: false,
        ..Default::default()
    };
    let result = copy_package(&candidate(&src_repo, "foo", "1.0.0"), &dest_repo, &opts).unwrap();
    assert!(!result.skipped.is_empty());
    assert!(result.copied.is_empty());

    // Copy with overwrite=true (should replace)
    let opts = CopyOptions {
        overwrite: true,
        ..Default::default()
    };
    let result = copy_package(&candidate(&src_repo, "foo", "1.0.0"), &dest_repo, &opts).unwrap();
    assert!(result.skipped.is_empty());
    assert!(!result.copied.is_empty());

    // Unrelated top-level payload is retained by reference copy.
    assert!(dst.join("old.txt").exists());
    // New package file should exist
    assert!(dst.join("package.yaml").exists());
}

#[test]
fn test_move_between_repos() {
    let (_dir1, src_repo) = create_test_repo();
    let (_dir2, dest_repo) = create_empty_repo();

    // Move bar-1.0.0 from src to dest
    let src = src_repo.join("bar").join("1.0.0");

    // Verify source exists
    assert!(src.exists());

    let result = move_package(
        &candidate(&src_repo, "bar", "1.0.0"),
        &dest_repo,
        &CopyOptions::default(),
    )
    .unwrap();
    assert!(!result.copied.is_empty());

    // Reference move retains payload and hides the source.
    assert!(src.exists());
    assert!(repository::is_package_ignored(&src_repo, "bar", "1.0.0"));

    // Dest should have package
    let repo = FsRepo::new(&dest_repo).unwrap();
    let version = "1.0.0".parse().unwrap();
    let pkg = repo.get_package("bar", &version).unwrap().unwrap();
    assert_eq!(pkg.name, "bar");
}

#[test]
fn test_ops_copy_and_verify() {
    let (_dir1, src_repo) = create_test_repo();
    let (_dir2, dest_repo) = create_empty_repo();

    // Copy foo-1.0.0
    let opts = CopyOptions::default();
    let result = copy_package(&candidate(&src_repo, "foo", "1.0.0"), &dest_repo, &opts).unwrap();
    assert!(!result.copied.is_empty());

    // Verify copied package is readable
    let repo = FsRepo::new(&dest_repo).unwrap();
    let versions = repo.iter_versions("foo").unwrap();
    assert_eq!(versions.len(), 1);

    let version = "1.0.0".parse().unwrap();
    let pkg = repo.get_package("foo", &version).unwrap().unwrap();
    assert_eq!(pkg.name, "foo");
    assert_eq!(pkg.version.to_string(), "1.0.0");
}

// ============================================================================
// Edge cases
// ============================================================================

#[test]
fn test_repo_empty_family_dir() {
    let (_dir, repo_path) = create_empty_repo();

    // Create empty family dir (no versions)
    fs::create_dir_all(repo_path.join("empty")).unwrap();

    let repo = FsRepo::new(&repo_path).unwrap();
    let families = repo.iter_family_names().unwrap();

    // Empty family should still appear in family list
    assert!(families.contains(&"empty".to_string()));

    // But has no versions
    let versions = repo.iter_versions("empty").unwrap();
    assert!(versions.is_empty());
}

#[test]
fn test_repo_version_dir_no_package_file() {
    let (_dir, repo_path) = create_empty_repo();

    // Create version dir without package definition
    let ver_dir = repo_path.join("broken").join("1.0.0");
    fs::create_dir_all(&ver_dir).unwrap();
    fs::write(ver_dir.join("readme.txt"), "no package file").unwrap();

    let repo = FsRepo::new(&repo_path).unwrap();

    // Should appear in families
    assert!(repo.has_family("broken"));

    // Should NOT appear in versions (no valid package file)
    let versions = repo.iter_versions("broken").unwrap();
    assert!(versions.is_empty());
}

#[test]
fn test_copy_package_to_self() {
    let (_dir, repo_path) = create_test_repo();

    let opts = CopyOptions::default();

    // Copying to same path should error
    let result = copy_package(&candidate(&repo_path, "foo", "1.0.0"), &repo_path, &opts);
    assert!(result.is_err());
}

#[test]
fn test_move_package_dest_exists() {
    let (_dir1, src_repo) = create_test_repo();
    let (_dir2, dest_repo) = create_empty_repo();

    // Create existing package at dest
    let dst = dest_repo.join("foo").join("1.0.0");
    fs::create_dir_all(&dst).unwrap();
    fs::write(dst.join("package.yaml"), "name: foo\nversion: '1.0.0'\n").unwrap();
    fs::write(dst.join("payload.txt"), "existing destination").unwrap();

    let result = move_package(
        &candidate(&src_repo, "foo", "1.0.0"),
        &dest_repo,
        &CopyOptions::default(),
    );

    // A real destination package rejects the move without hiding the source.
    assert!(result.is_err());
    assert_eq!(
        fs::read_to_string(dst.join("payload.txt")).unwrap(),
        "existing destination"
    );
    assert!(FsRepo::new(&src_repo)
        .unwrap()
        .get_package("foo", &version::Version::new("1.0.0").unwrap())
        .unwrap()
        .is_some());
}

// ============================================================================
// Memory package info handling
// ============================================================================

#[test]
fn test_package_info_from_data() {
    let mut data = HashMap::new();
    data.insert("name".to_string(), serde_json::json!("test"));
    data.insert("version".to_string(), serde_json::json!("1.0.0"));
    data.insert("description".to_string(), serde_json::json!("Test package"));

    let info = PackageInfo::from_data(data).unwrap();
    assert_eq!(info.name, "test");
    assert_eq!(info.version.to_string(), "1.0.0");
    assert_eq!(
        info.get("description").unwrap().as_str().unwrap(),
        "Test package"
    );
}

#[test]
fn test_package_info_missing_name() {
    let mut data = HashMap::new();
    data.insert("version".to_string(), serde_json::json!("1.0.0"));

    let result = PackageInfo::from_data(data);
    assert!(result.is_err());
}

#[test]
fn test_package_info_unversioned() {
    let mut data = HashMap::new();
    data.insert("name".to_string(), serde_json::json!("unver"));
    // No version field

    let info = PackageInfo::from_data(data).unwrap();
    assert_eq!(info.name, "unver");
    assert!(info.version.is_empty());
}

// ============================================================================
// Memcache client stub tests
// ============================================================================

#[test]
fn test_memcache_client_disabled() {
    let client = MemcacheClient::new(&[]);
    assert!(!client.is_enabled());

    let result = client.get("test");
    assert!(result.is_none());

    client.set("key", "value", 60).unwrap();
    assert!(client.get("key").is_none()); // Stub doesn't cache
}

#[test]
fn test_memcache_client_enabled_is_lazy() {
    let servers = vec!["127.0.0.1:11211".to_string()];
    let client = MemcacheClient::new(&servers);

    // Creating a configured client must not require a live server.
    assert!(client.is_enabled());
}

#[test]
fn test_cache_key_generation() {
    let (_dir, repo_path) = create_empty_repo();
    let key1 = cache_key_for_package(&repo_path, "foo", "1.0.0");
    let key2 = cache_key_for_package(&repo_path, "foo", "1.0.0");

    // Same inputs should generate same key
    assert_eq!(key1, key2);

    // Different version should generate different key
    let key3 = cache_key_for_package(&repo_path, "foo", "2.0.0");
    assert_ne!(key1, key3);
}
