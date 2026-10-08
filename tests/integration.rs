// Integration tests: filesystem repo -> package parsing -> solver pipeline
//
// Uses fixture packages under tests/fixtures/packages/ to verify end-to-end
// resolution without mocking.

use model::package::Package;
use repository::provider::MemoryPackageProvider;
use repository::{FsRepo, PackageRepository};
use resolve::solver::Solver;
use std::path::PathBuf;
use version::{Requirement, Version};

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/packages")
}

// ---------------------------------------------------------------------------
// Repository: filesystem scanning
// ---------------------------------------------------------------------------

#[test]
fn test_repo_reads_family_names() {
    let repo = FsRepo::new(&fixture_path()).unwrap();
    let families = repo.iter_family_names().unwrap();
    assert!(families.contains(&"foo".to_string()));
    assert!(families.contains(&"bar".to_string()));
    assert!(families.contains(&"baz".to_string()));
    assert_eq!(families.len(), 3);
}

#[test]
fn test_repo_reads_versions() {
    let repo = FsRepo::new(&fixture_path()).unwrap();

    let foo_versions = repo.iter_versions("foo").unwrap();
    assert_eq!(foo_versions.len(), 3);
    // Versions should be sorted ascending
    assert_eq!(foo_versions[0], Version::new("1.0.0").unwrap());
    assert_eq!(foo_versions[1], Version::new("1.1.0").unwrap());
    assert_eq!(foo_versions[2], Version::new("2.0.0").unwrap());

    let bar_versions = repo.iter_versions("bar").unwrap();
    assert_eq!(bar_versions.len(), 2);

    let baz_versions = repo.iter_versions("baz").unwrap();
    assert_eq!(baz_versions.len(), 1);
}

#[test]
fn test_repo_nonexistent_family() {
    let repo = FsRepo::new(&fixture_path()).unwrap();
    let versions = repo.iter_versions("nonexistent").unwrap();
    assert!(versions.is_empty());
}

// ---------------------------------------------------------------------------
// Repository: package loading
// ---------------------------------------------------------------------------

#[test]
fn test_repo_loads_package_info() {
    let repo = FsRepo::new(&fixture_path()).unwrap();
    let ver = Version::new("2.0.0").unwrap();
    let info = repo.get_package("foo", &ver).unwrap().unwrap();
    assert_eq!(info.name, "foo");
    assert_eq!(info.version, ver);
    // Raw data should contain requires
    assert!(info.data.contains_key("requires"));
}

#[test]
fn test_repo_loads_simple_package() {
    let repo = FsRepo::new(&fixture_path()).unwrap();
    let ver = Version::new("1.0.0").unwrap();
    let info = repo.get_package("bar", &ver).unwrap().unwrap();
    assert_eq!(info.name, "bar");
    assert_eq!(info.version, ver);
    // No requires in bar-1.0.0
    assert!(!info.data.contains_key("requires"));
}

#[test]
fn test_repo_missing_version_returns_none() {
    let repo = FsRepo::new(&fixture_path()).unwrap();
    let ver = Version::new("9.9.9").unwrap();
    let result = repo.get_package("foo", &ver).unwrap();
    assert!(result.is_none());
}

// ---------------------------------------------------------------------------
// Package::from_data via repo
// ---------------------------------------------------------------------------

#[test]
fn test_package_from_repo_data() {
    let repo = FsRepo::new(&fixture_path()).unwrap();
    let ver = Version::new("2.0.0").unwrap();
    let info = repo.get_package("foo", &ver).unwrap().unwrap();
    let pkg = Package::from_data(info.data).unwrap();
    assert_eq!(pkg.name, "foo");
    assert_eq!(pkg.version, ver);
    assert_eq!(pkg.requires.len(), 2); // bar-2+ and baz-1
    assert_eq!(pkg.tools, vec!["foo_tool"]);
    assert!(pkg.commands.is_some());
}

#[test]
fn test_package_with_single_dep() {
    let repo = FsRepo::new(&fixture_path()).unwrap();
    let ver = Version::new("1.1.0").unwrap();
    let info = repo.get_package("foo", &ver).unwrap().unwrap();
    let pkg = Package::from_data(info.data).unwrap();
    assert_eq!(pkg.name, "foo");
    assert_eq!(pkg.requires.len(), 1); // bar-1+
    assert_eq!(pkg.description, Some("Foo package".into()));
}

#[test]
fn test_package_no_deps() {
    let repo = FsRepo::new(&fixture_path()).unwrap();
    let ver = Version::new("1.0.0").unwrap();
    let info = repo.get_package("baz", &ver).unwrap().unwrap();
    let pkg = Package::from_data(info.data).unwrap();
    assert_eq!(pkg.name, "baz");
    assert!(pkg.requires.is_empty());
    assert_eq!(pkg.description, Some("Base package".into()));
}

// ---------------------------------------------------------------------------
// Solver: end-to-end resolution with MemoryPackageProvider
// ---------------------------------------------------------------------------

/// Load all fixture packages into a MemoryPackageProvider.
fn build_provider() -> MemoryPackageProvider {
    let repo = FsRepo::new(&fixture_path()).unwrap();
    let mut provider = MemoryPackageProvider::new();

    for family in repo.iter_family_names().unwrap() {
        for version in repo.iter_versions(&family).unwrap() {
            if let Ok(Some(info)) = repo.get_package(&family, &version) {
                let pkg = Package::from_data(info.data)
                    .unwrap_or_else(|_| Package::new(&family, version.clone()));
                provider.add(pkg);
            }
        }
    }
    provider
}

#[test]
fn test_solver_resolves_simple_request() {
    let provider = build_provider();
    // Request foo-1.0.0 exactly (no deps)
    let reqs = vec![Requirement::new("foo-1.0").unwrap()];
    let mut solver = Solver::new(reqs, &provider, false, None).unwrap();
    solver.solve().unwrap();

    let resolved = solver.resolved_packages();
    assert!(resolved.is_some());
    let pkgs = resolved.unwrap();
    // foo-1.0.0 has no dependencies, should resolve to just foo
    let names: Vec<&str> = pkgs.iter().map(|p| p.name()).collect();
    assert!(names.contains(&"foo"));
}

#[test]
fn test_solver_resolves_with_transitive_deps() {
    let provider = build_provider();
    // foo-2 requires bar-2+ and baz-1; bar-2 requires baz-1
    let reqs = vec![Requirement::new("foo-2").unwrap()];
    let mut solver = Solver::new(reqs, &provider, false, None).unwrap();
    solver.solve().unwrap();

    let resolved = solver.resolved_packages();
    assert!(resolved.is_some());
    let pkgs = resolved.unwrap();
    let names: Vec<&str> = pkgs.iter().map(|p| p.name()).collect();
    assert!(names.contains(&"foo"));
    assert!(names.contains(&"bar"));
    assert!(names.contains(&"baz"));
    assert_eq!(pkgs.len(), 3);

    // Verify correct versions picked
    for pv in &pkgs {
        match pv.name() {
            "foo" => assert_eq!(*pv.version(), Version::new("2.0.0").unwrap()),
            "bar" => assert_eq!(*pv.version(), Version::new("2.0.0").unwrap()),
            "baz" => assert_eq!(*pv.version(), Version::new("1.0.0").unwrap()),
            other => panic!("unexpected package: {other}"),
        }
    }
}

#[test]
fn test_solver_resolves_foo_1_1_with_deps() {
    let provider = build_provider();
    // foo-1.1 requires bar-1+; bar has 1.0 and 2.0, solver picks latest matching
    let reqs = vec![Requirement::new("foo-1.1").unwrap()];
    let mut solver = Solver::new(reqs, &provider, false, None).unwrap();
    solver.solve().unwrap();

    let resolved = solver.resolved_packages();
    assert!(resolved.is_some());
    let pkgs = resolved.unwrap();
    let names: Vec<&str> = pkgs.iter().map(|p| p.name()).collect();
    assert!(names.contains(&"foo"));
    assert!(names.contains(&"bar"));
}

#[test]
fn test_solver_multiple_requests() {
    let provider = build_provider();
    // Request both foo-1.0 and baz-1
    let reqs = vec![
        Requirement::new("foo-1.0").unwrap(),
        Requirement::new("baz-1").unwrap(),
    ];
    let mut solver = Solver::new(reqs, &provider, false, None).unwrap();
    solver.solve().unwrap();

    let resolved = solver.resolved_packages();
    assert!(resolved.is_some());
    let pkgs = resolved.unwrap();
    let names: Vec<&str> = pkgs.iter().map(|p| p.name()).collect();
    assert!(names.contains(&"foo"));
    assert!(names.contains(&"baz"));
}

// ---------------------------------------------------------------------------
// CLI binary: integration tests via std::process::Command
// ---------------------------------------------------------------------------

/// Build a Command pointing at the compiled rez binary.
fn rez_cmd() -> std::process::Command {
    std::process::Command::new(env!("CARGO_BIN_EXE_rez"))
}

#[test]
fn test_cli_help() {
    let output = rez_cmd()
        .arg("--help")
        .output()
        .expect("failed to run rez --help");
    assert!(output.status.success(), "--help should exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("rez"), "help output must mention 'rez'");
}

#[test]
fn test_cli_python_version() {
    let output = rez_cmd()
        .args(["python", "--version"])
        .output()
        .expect("failed to run rez python --version");
    assert!(output.status.success(), "python --version should exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("RustPython"),
        "python --version must mention RustPython, got: {stdout}"
    );
}

#[test]
fn test_cli_python_exec() {
    let output = rez_cmd()
        .args(["python", "-c", "print(1+1)"])
        .output()
        .expect("failed to run rez python -c");
    assert!(output.status.success(), "python -c should exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "2", "print(1+1) should produce '2'");
}

#[test]
fn test_cli_python_import() {
    let output = rez_cmd()
        .args(["python", "-c", "import sys; print(sys.platform)"])
        .output()
        .expect("failed to run rez python -c import");
    assert!(
        output.status.success(),
        "python import sys should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.trim().is_empty(),
        "sys.platform should produce output"
    );
}

#[test]
fn test_cli_config() {
    let output = rez_cmd()
        .arg("config")
        .output()
        .expect("failed to run rez config");
    assert!(
        output.status.success(),
        "config should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Full config dump is JSON, must contain at least one known key
    assert!(
        stdout.contains("packages_path"),
        "config output should contain 'packages_path', got: {stdout}"
    );
}

#[test]
fn test_cli_status() {
    let output = rez_cmd()
        .arg("status")
        .output()
        .expect("failed to run rez status");
    assert!(
        output.status.success(),
        "status should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_cli_search_no_path() {
    // With no real packages path, search should exit non-zero or report no results
    let output = rez_cmd()
        .args(["search", "nonexistent_pkg_xyz"])
        .output()
        .expect("failed to run rez search");
    // search exits 1 when no packages found
    assert!(
        !output.status.success(),
        "search for nonexistent package should fail"
    );
}

#[test]
fn test_cli_env_no_packages() {
    // env with a package that doesn't exist should error
    let output = rez_cmd()
        .args(["env", "nonexistent_pkg_xyz"])
        .output()
        .expect("failed to run rez env");
    assert!(
        !output.status.success(),
        "env with nonexistent package should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.is_empty(), "env error should produce stderr output");
}

#[test]
fn test_cli_unknown_command() {
    let output = rez_cmd()
        .arg("nonexistent")
        .output()
        .expect("failed to run rez nonexistent");
    assert!(
        !output.status.success(),
        "unknown command should exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error") || stderr.contains("unrecognized") || stderr.contains("invalid"),
        "stderr should indicate an error for unknown command, got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// package.py repo: RustPython VM integration
// ---------------------------------------------------------------------------

/// Fixture path for package.py-based packages (parsed via RustPython VM).
fn fixture_path_py() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/packages_py")
}

#[test]
fn test_py_repo_reads_family_names() {
    let repo = FsRepo::new(&fixture_path_py()).unwrap();
    let families = repo.iter_family_names().unwrap();
    assert!(families.contains(&"testpkg".to_string()));
    assert!(families.contains(&"python".to_string()));
    assert!(families.contains(&"platpkg".to_string()));
    assert!(families.contains(&"msvc".to_string()));
    assert!(families.contains(&"gcc".to_string()));
    assert_eq!(families.len(), 5);
}

#[test]
fn test_py_repo_reads_versions() {
    let repo = FsRepo::new(&fixture_path_py()).unwrap();
    let versions = repo.iter_versions("testpkg").unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0], Version::new("1.0.0").unwrap());

    let py_versions = repo.iter_versions("python").unwrap();
    assert_eq!(py_versions.len(), 1);
    assert_eq!(py_versions[0], Version::new("3.0.0").unwrap());
}

#[test]
fn test_py_repo_loads_testpkg_metadata() {
    let repo = FsRepo::new(&fixture_path_py()).unwrap();
    let ver = Version::new("1.0.0").unwrap();
    let info = repo.get_package("testpkg", &ver).unwrap().unwrap();

    assert_eq!(info.name, "testpkg");
    assert_eq!(info.version, ver);
    // Raw data must contain keys set in package.py
    assert!(info.data.contains_key("name"));
    assert!(info.data.contains_key("version"));
    assert!(info.data.contains_key("description"));
    assert!(info.data.contains_key("requires"));
    assert!(info.data.contains_key("tools"));
}

#[test]
fn test_py_package_from_data() {
    let repo = FsRepo::new(&fixture_path_py()).unwrap();
    let ver = Version::new("1.0.0").unwrap();
    let info = repo.get_package("testpkg", &ver).unwrap().unwrap();
    let pkg = Package::from_data(info.data).unwrap();

    assert_eq!(pkg.name, "testpkg");
    assert_eq!(pkg.version, ver);
    assert_eq!(pkg.description, Some("Test package with package.py".into()));
    assert_eq!(pkg.requires.len(), 1); // python-3
    assert_eq!(pkg.tools, vec!["testpkg_tool"]);
}

/// Proves RustPython VM actually executes the code: sys.platform conditional
/// produces different requires on Windows vs Linux.
#[test]
fn test_py_platform_conditional() {
    let repo = FsRepo::new(&fixture_path_py()).unwrap();
    let ver = Version::new("1.0.0").unwrap();
    let info = repo.get_package("platpkg", &ver).unwrap().unwrap();
    let pkg = Package::from_data(info.data).unwrap();

    assert_eq!(pkg.name, "platpkg");
    assert_eq!(pkg.requires.len(), 1);

    // On Windows sys.platform == "win32" -> requires = ["msvc"]
    // On Linux sys.platform == "linux" -> requires = ["gcc"]
    if cfg!(target_os = "windows") {
        let req_str = pkg.requires[0].to_string();
        assert!(
            req_str.contains("msvc"),
            "on Windows platpkg should require msvc, got: {req_str}"
        );
    } else {
        let req_str = pkg.requires[0].to_string();
        assert!(
            req_str.contains("gcc"),
            "on Linux platpkg should require gcc, got: {req_str}"
        );
    }
}

/// Build MemoryPackageProvider from the package.py fixtures.
fn build_provider_py() -> MemoryPackageProvider {
    let repo = FsRepo::new(&fixture_path_py()).unwrap();
    let mut provider = MemoryPackageProvider::new();

    for family in repo.iter_family_names().unwrap() {
        for version in repo.iter_versions(&family).unwrap() {
            if let Ok(Some(info)) = repo.get_package(&family, &version) {
                let pkg = Package::from_data(info.data)
                    .unwrap_or_else(|_| Package::new(&family, version.clone()));
                provider.add(pkg);
            }
        }
    }
    provider
}

#[test]
fn test_py_solver_resolves_testpkg() {
    let provider = build_provider_py();
    // testpkg-1 requires python-3 -> should resolve both
    let reqs = vec![Requirement::new("testpkg-1").unwrap()];
    let mut solver = Solver::new(reqs, &provider, false, None).unwrap();
    solver.solve().unwrap();

    let resolved = solver.resolved_packages();
    assert!(resolved.is_some());
    let pkgs = resolved.unwrap();
    let names: Vec<&str> = pkgs.iter().map(|p| p.name()).collect();
    assert!(names.contains(&"testpkg"), "testpkg must be resolved");
    assert!(names.contains(&"python"), "python dep must be resolved");
    assert_eq!(pkgs.len(), 2);
}

#[test]
fn test_py_solver_resolves_platpkg() {
    let provider = build_provider_py();
    // platpkg-1 requires msvc (win) or gcc (linux)
    let reqs = vec![Requirement::new("platpkg-1").unwrap()];
    let mut solver = Solver::new(reqs, &provider, false, None).unwrap();
    solver.solve().unwrap();

    let resolved = solver.resolved_packages();
    assert!(resolved.is_some());
    let pkgs = resolved.unwrap();
    let names: Vec<&str> = pkgs.iter().map(|p| p.name()).collect();
    assert!(names.contains(&"platpkg"), "platpkg must be resolved");
    assert_eq!(pkgs.len(), 2); // platpkg + its platform dep

    if cfg!(target_os = "windows") {
        assert!(
            names.contains(&"msvc"),
            "on Windows platpkg should pull in msvc"
        );
    } else {
        assert!(
            names.contains(&"gcc"),
            "on Linux platpkg should pull in gcc"
        );
    }
}
