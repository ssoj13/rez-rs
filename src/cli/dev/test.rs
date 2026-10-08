// SPDX-License-Identifier: Apache-2.0

//! `rez test` - run tests defined in package definition.

use std::env;
use std::path::PathBuf;

use clap::Args;

use foundation::errors::{Result, RezError};
use model::config::CONFIG;
use repository::package::discover::get_developer_package;
use resolve::package::test::{PackageTestRunner, TestRunOn};

use crate::cli::util::expand_tilde;

// ---------------------------------------------------------------------------
// Args
// ---------------------------------------------------------------------------

/// Run tests defined in a package's definition file.
#[derive(Args, Debug)]
pub struct TestArgs {
    /// Package to test. If not provided, uses the developer package
    /// in the current directory.
    #[arg(value_name = "PACKAGE")]
    pub package: Option<String>,

    /// List available tests and exit.
    #[arg(short = 'l', long)]
    pub list: bool,

    /// Run specific tests (comma-separated names).
    #[arg(
        short = 't',
        long = "tests",
        value_name = "NAMES",
        value_delimiter = ','
    )]
    pub tests: Option<Vec<String>>,

    /// Filter tests by run_on stage (default, pre_install, post_install, etc.).
    #[arg(long = "run-on", value_name = "STAGE")]
    pub run_on: Option<String>,

    /// Verbose output.
    #[arg(from_global)]
    pub verbose: u8,

    /// Show what would run without actually running.
    #[arg(long = "dry-run")]
    pub dry_run: bool,

    /// Stop on first test failure.
    #[arg(short = 's', long = "stop-on-fail")]
    pub stop_on_fail: bool,

    /// Package search paths (semicolon-separated on Windows, colon on Unix).
    #[arg(short = 'p', long = "paths", value_name = "PATHS")]
    pub paths: Option<String>,

    /// Don't include local packages in search path.
    #[arg(long = "no-local")]
    pub no_local: bool,

    /// Run tests in current environment (don't create a new resolve).
    /// Tests whose extra requirements aren't met will be skipped.
    #[arg(long)]
    pub inplace: bool,

    /// Extra packages to add to the test environment.
    #[arg(long = "extra-packages", num_args = 1.., value_name = "PKG")]
    pub extra_packages: Vec<String>,
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

/// Execute the `rez test` command.
pub fn run(args: &TestArgs) -> Result<()> {
    if args.inplace
        && (!args.extra_packages.is_empty()
            || args.paths.as_deref().is_some_and(|paths| !paths.is_empty())
            || args.no_local)
    {
        return Err(RezError::PackageTest(
            "Cannot use --inplace in combination with --extra-packages/--paths/--no-local"
                .to_string(),
        ));
    }

    // Resolve package search paths
    let pkg_paths = resolve_paths(args)?;

    // Load the package to test
    let (package, developer_package) = load_package(args, &pkg_paths)?;
    let pkg_name = package.qualified_name();

    // Retain developer-source provenance for lifecycle evaluation.
    let verbose = if args.verbose > 0 { 2 } else { 0 };
    let mut runner = match developer_package {
        Some(source) => PackageTestRunner::from_developer_package(source, pkg_paths, verbose),
        None => PackageTestRunner::new(package, pkg_paths, verbose),
    };
    runner.dry_run = args.dry_run;
    runner.stop_on_fail = args.stop_on_fail;
    runner.inplace = args.inplace;
    runner.extra_packages = args.extra_packages.clone();

    if args.inplace {
        eprintln!("Running tests in current environment");
    }

    // Parse run_on filter
    let run_on_filter: Option<Vec<TestRunOn>> = args
        .run_on
        .as_ref()
        .map(|stage| vec![TestRunOn::from_str_val(stage)]);

    // Get available test names (filtered by run_on if specified)
    let all_names = runner.test_names(run_on_filter.as_deref())?;

    if all_names.is_empty() {
        eprintln!("No tests found in {}", pkg_name);
        return Ok(());
    }

    // --list: print test names and exit
    if args.list {
        eprintln!("Tests defined in {}:", pkg_name);
        for name in &all_names {
            println!("{}", name);
        }
        return Ok(());
    }

    // Determine which tests to run
    let run_names = select_test_names(
        &runner,
        &all_names,
        args.tests.as_deref(),
        run_on_filter.as_deref(),
        &pkg_name,
    )?;
    if run_names.is_empty() {
        match &args.run_on {
            Some(stage) => eprintln!("No tests with '{}' run_on tag found in {}", stage, pkg_name),
            None => eprintln!("No tests with 'default' run_on tag found in {}", pkg_name),
        }
        return Ok(());
    }

    eprintln!("Running {} test(s) from {}...\n", run_names.len(), pkg_name);

    // Execute tests
    let mut exit_code = 0;
    for test_name in &run_names {
        if runner.stopped_on_fail {
            break;
        }
        match runner.run_test(test_name) {
            Ok(result) => {
                if (result.status.is_failed() || result.status.is_error()) && exit_code == 0 {
                    exit_code = 1;
                }
            }
            Err(e) => {
                eprintln!("Error running test '{}': {}", test_name, e);
                if exit_code == 0 {
                    exit_code = 1;
                }
                if args.stop_on_fail {
                    break;
                }
            }
        }
    }

    // Print summary
    println!();
    runner.print_summary();
    println!();

    if exit_code != 0 {
        return Err(RezError::PackageTest(format!(
            "tests exited with code {}",
            exit_code
        )));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Select explicitly requested tests or the tests for the requested lifecycle stage.
fn select_test_names(
    runner: &PackageTestRunner,
    available_names: &[String],
    requested: Option<&[String]>,
    run_on: Option<&[TestRunOn]>,
    package_name: &str,
) -> Result<Vec<String>> {
    if let Some(requested) = requested {
        requested
            .iter()
            .map(|name| {
                if available_names.contains(name) {
                    Ok(name.clone())
                } else {
                    Err(RezError::PackageTest(format!(
                        "Test '{}' not found in {}. Available tests: {}",
                        name,
                        package_name,
                        available_names.join(", ")
                    )))
                }
            })
            .collect()
    } else if run_on.is_some() {
        Ok(available_names.to_vec())
    } else {
        runner.test_names(Some(&[TestRunOn::Default]))
    }
}

/// Resolve package search paths from args and config.
fn resolve_paths(args: &TestArgs) -> Result<Vec<PathBuf>> {
    if let Some(ref paths_str) = args.paths {
        // User-specified paths
        let sep = if cfg!(windows) { ';' } else { ':' };
        Ok(paths_str
            .split(sep)
            .filter(|s| !s.is_empty())
            .map(|s| PathBuf::from(expand_tilde(s)))
            .collect())
    } else if args.no_local {
        // All configured paths except local
        let local = CONFIG.expanded_local_packages_path();
        Ok(CONFIG
            .expanded_packages_path()
            .into_iter()
            .filter(|p| *p != local)
            .map(|p| p.to_os())
            .collect())
    } else {
        Ok(CONFIG
            .expanded_packages_path()
            .into_iter()
            .map(|p| p.to_os())
            .collect())
    }
}

/// Load the package to test, either from a string request or cwd.
fn load_package(
    args: &TestArgs,
    pkg_paths: &[PathBuf],
) -> Result<(
    model::package::Package,
    Option<model::package::DeveloperPackage>,
)> {
    match &args.package {
        Some(pkg_str) => {
            // Load the complete package definition from the selected package path.
            let info = repository::package::discover::get_latest_package_from_string(
                pkg_str,
                Some(pkg_paths),
            )?;
            match info {
                Some(pi) => pi.to_package().map(|package| (package, None)),
                None => Err(RezError::PackageNotFound(format!(
                    "Package not found: {}",
                    pkg_str
                ))),
            }
        }
        None => {
            // Load developer package from current directory
            let cwd = env::current_dir().map_err(|e| {
                RezError::PackageTest(format!("Failed to get current directory: {}", e))
            })?;
            let dev_pkg = get_developer_package(&cwd)?;
            Ok((dev_pkg.package.clone(), Some(dev_pkg)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn select_test_names_honors_run_on_stage() {
        let mut package =
            model::package::Package::new("foo", version::Version::new("1.0").unwrap());
        package.tests = Some(HashMap::from([
            ("default_test".to_string(), json!("echo default")),
            (
                "release_test".to_string(),
                json!({"command": "echo release", "run_on": "pre_release"}),
            ),
        ]));
        let runner = PackageTestRunner::new(package, Vec::new(), 0);
        let stage = [TestRunOn::PreRelease];
        let available_names = runner.test_names(Some(&stage)).unwrap();

        let selected =
            select_test_names(&runner, &available_names, None, Some(&stage), "foo-1.0").unwrap();

        assert_eq!(selected, vec!["release_test"]);
    }
}
