// SPDX-License-Identifier: Apache-2.0

//! `rez build` - build package from source using detected build system.

use std::env;
use std::path::PathBuf;

use clap::Args;

use build_system::builders::{BuildProcess, BuildResult, BuildSystemType};
use foundation::errors::{Result, RezError};
use foundation::log_info;
use model::config::CONFIG;
use repository::package::discover::get_developer_package;

// ---------------------------------------------------------------------------
// Args
// ---------------------------------------------------------------------------

/// Build a package from source.
#[derive(Args, Debug)]
pub struct BuildArgs {
    /// Build system to use (cmake, make, python, pip, cargo, nodejs, bun, custom, noop).
    /// If not specified, it is auto-detected from the source directory.
    #[arg(short = 'b', long = "build-system", value_name = "SYS")]
    pub build_system: Option<String>,

    /// Install the build to the local packages path.
    #[arg(short = 'i', long)]
    pub install: bool,

    /// Custom install path (overrides local_packages_path).
    #[arg(
        short = 'p',
        long = "prefix",
        visible_alias = "install-path",
        value_name = "PATH"
    )]
    pub install_path: Option<PathBuf>,

    /// Clean the build directory before rebuilding.
    #[arg(short = 'c', long)]
    pub clean: bool,

    /// Build specific variant index (zero-indexed).
    #[arg(long = "variants", visible_alias = "variant", value_name = "INDEX", num_args = 1..)]
    pub variant: Option<Vec<usize>>,

    /// Build process type (currently only "local" is supported).
    #[arg(long, default_value_t = String::from("local"))]
    pub process: String,

    /// Override package definition path (source directory).
    #[arg(long = "source-path", value_name = "PATH")]
    pub source_path: Option<PathBuf>,

    /// Don't include local packages path in search.
    #[arg(long = "no-local")]
    pub no_local: bool,

    /// Display a resolve failure graph when the shared graph pipeline is available.
    #[arg(long = "fail-graph")]
    pub fail_graph: bool,

    /// Create build scripts instead of performing the full build.
    #[arg(short = 's', long)]
    pub scripts: bool,

    /// Extra arguments passed to the build system (after '--').
    #[arg(long = "build-args", visible_alias = "ba", value_name = "ARGS")]
    pub build_args: Option<String>,

    /// Extra arguments for child build system (e.g. make args under cmake).
    #[arg(long = "child-build-args", visible_alias = "cba", value_name = "ARGS")]
    pub child_build_args: Option<String>,

    /// Force rebuild even if variant is already installed.
    #[arg(short = 'f', long)]
    pub force: bool,

    /// Subdirectory tag for organizing packages (e.g. "dcc", "studio/tools").
    /// Package installs to <install_path>/<tag>/<name>/<version>/.
    #[arg(short = 't', long)]
    pub tag: Option<String>,

    /// Positional build and child-build argument groups after `--` separators.
    #[arg(last = true, value_name = "ARGS", num_args = 0..)]
    pub passthrough_args: Vec<String>,
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

/// Execute the `rez build` command.
pub fn run(args: &BuildArgs) -> Result<()> {
    // Determine working directory
    let working_dir = match &args.source_path {
        Some(p) => p.clone(),
        None => env::current_dir()
            .map_err(|e| RezError::Build(format!("Failed to get current directory: {}", e)))?,
    };

    if !working_dir.is_dir() {
        return Err(RezError::Build(format!(
            "Working directory does not exist: {}",
            working_dir.display()
        )));
    }

    // Load developer package from working dir
    log_info!("build", "Loading package from {}", working_dir.display());
    let dev_pkg = get_developer_package(&working_dir)?;
    let pkg = &dev_pkg.package;

    log_info!(
        "build",
        "Building {} from {}",
        pkg.qualified_name(),
        working_dir.display()
    );
    eprintln!(
        "Building package: {} ({})",
        pkg.qualified_name(),
        working_dir.display()
    );

    // Determine install path
    let install_path = CONFIG.build_install_path(pkg, args.install_path.as_deref(), false);
    let tag = args
        .tag
        .as_deref()
        .or_else(|| CONFIG.resolve_tag_path(&pkg.tags));
    let install_path = if let Some(tag) = tag {
        if !foundation::path::is_safe_rez_path(tag, false) {
            return Err(RezError::Build(format!(
                "Build tag path must be a non-empty relative path: {tag:?}"
            )));
        }
        install_path.join(tag)
    } else {
        install_path
    };

    // Build system: CLI --build-system overrides package.build_system
    let build_sys_name = args.build_system.as_deref().or(pkg.build_system.as_deref());

    // Validate build system name if provided
    if let Some(name) = build_sys_name {
        let valid_names: Vec<&str> = BuildSystemType::all().iter().map(|t| t.name()).collect();
        if !valid_names.contains(&name) {
            return Err(RezError::BuildSystem(format!(
                "Unknown build system '{}'. Valid options: {}",
                name,
                valid_names.join(", ")
            )));
        }
    }

    // Create build process
    let mut bp = BuildProcess::new(
        &working_dir,
        pkg.build_command.clone(),
        build_sys_name,
        None,
    )?;

    // Set full package for enhanced env vars and pre_build_commands
    bp.set_package(pkg.clone(), Some(dev_pkg.clone()))?;
    bp.verbose = true;

    // Forward extra build args from CLI. Rez accepts named groups or positional
    // groups separated by `--` and a second `--` for child build arguments.
    let mut arg_groups = args.passthrough_args.split(|arg| arg == "--");
    let build_args = arg_groups.next().unwrap_or_default();
    let child_build_args = arg_groups.next().unwrap_or_default();
    if args.build_args.is_some() && !build_args.is_empty() {
        return Err(RezError::Build(
            "Use --build-args or arguments after `--`, not both".into(),
        ));
    }
    if args.child_build_args.is_some() && !child_build_args.is_empty() {
        return Err(RezError::Build(
            "Use --child-build-args or arguments after the second `--`, not both".into(),
        ));
    }
    bp.build_args = args.build_args.as_ref().map_or_else(
        || build_args.to_vec(),
        |args_str| args_str.split_whitespace().map(String::from).collect(),
    );
    bp.child_build_args = args.child_build_args.as_ref().map_or_else(
        || child_build_args.to_vec(),
        |args_str| args_str.split_whitespace().map(String::from).collect(),
    );

    eprintln!("Build system: {}", bp.build_system.name());

    // Determine variant indices
    let variant_indices = args.variant.as_deref();

    // Validate variant indices
    if let Some(indices) = variant_indices {
        let max_var = pkg.num_variants();
        for &idx in indices {
            if max_var > 0 && idx >= max_var {
                return Err(RezError::Build(format!(
                    "Variant index {} out of range (package has {} variants)",
                    idx, max_var
                )));
            }
        }
    }

    // Execute build
    let results = match bp.build(
        &install_path,
        args.clean,
        args.install,
        variant_indices,
        args.force,
        args.scripts,
    ) {
        Ok(results) => results,
        Err(RezError::BuildContextResolve { message, graph }) => {
            if args.fail_graph {
                if let Some(graph) = graph {
                    let image_path = crate::cli::util::view_graph(&graph)?;
                    eprintln!("Resolve graph image: {}", image_path.display());
                } else {
                    eprintln!("the failed build resolve context did not generate a graph.");
                }
            }
            return Err(RezError::BuildContextResolve {
                message,
                graph: None,
            });
        }
        Err(error) => return Err(error),
    };

    // Report results
    let mut any_failed = false;
    for (i, result) in results.iter().enumerate() {
        for line in format_build_result(i, result) {
            eprintln!("{line}");
        }
        any_failed |= !result.success;
    }

    if any_failed {
        Err(RezError::Build("One or more builds failed".into()))
    } else if args.scripts {
        eprintln!("Build scripts created.");
        Ok(())
    } else {
        eprintln!("All builds succeeded.");
        Ok(())
    }
}

fn format_build_result(index: usize, result: &BuildResult) -> Vec<String> {
    let mut lines = if result.success {
        vec![format!(
            "Build {}: OK ({:.1}s)",
            index + 1,
            result.elapsed_secs
        )]
    } else {
        vec![format!(
            "Build {}: FAILED ({:.1}s): {}",
            index + 1,
            result.elapsed_secs,
            result.error.as_deref().unwrap_or("unknown error")
        )]
    };

    if let Some(path) = &result.install_path {
        lines.push(format!(
            "  Installed to: {}",
            foundation::rez_path::RezPath::from_os(path)
        ));
    }
    if let Some(path) = &result.build_env_script {
        lines.push(format!("  Build environment script: {}", path.display()));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct BuildCli {
        #[command(flatten)]
        build: BuildArgs,
    }

    #[test]
    fn rez_build_short_flags_select_scripts_and_build_system() {
        let defaults = BuildCli::try_parse_from(["rez"]).unwrap();
        assert!(!defaults.build.scripts);
        assert_eq!(defaults.build.build_system, None);

        let scripts = BuildCli::try_parse_from(["rez", "-s"]).unwrap();
        assert!(scripts.build.scripts);
        assert_eq!(scripts.build.build_system, None);

        let build_system = BuildCli::try_parse_from(["rez", "-b", "cmake"]).unwrap();
        assert_eq!(build_system.build.build_system.as_deref(), Some("cmake"));
        assert!(!build_system.build.scripts);
    }

    #[test]
    fn rez_build_long_flags_match_short_flag_behavior() {
        let scripts = BuildCli::try_parse_from(["rez", "--scripts"]).unwrap();
        assert!(scripts.build.scripts);

        let build_system = BuildCli::try_parse_from(["rez", "--build-system", "make"]).unwrap();
        assert_eq!(build_system.build.build_system.as_deref(), Some("make"));
        assert!(!build_system.build.scripts);
    }

    #[test]
    fn rez_build_prefix_selects_installation_and_preserves_source_override() {
        for flag in ["-p", "--prefix", "--install-path"] {
            let parsed = BuildCli::try_parse_from([
                "rez",
                flag,
                "destination with spaces",
                "--source-path",
                "source with spaces",
            ])
            .unwrap();
            assert_eq!(
                parsed.build.install_path,
                Some(PathBuf::from("destination with spaces"))
            );
            assert_eq!(
                parsed.build.source_path,
                Some(PathBuf::from("source with spaces"))
            );
        }
        assert!(
            BuildCli::try_parse_from(["rez", "--prefix", "one", "--install-path", "two"]).is_err()
        );
    }

    #[test]
    fn rez_build_accepts_positional_build_and_child_build_argument_groups() {
        let build = BuildCli::try_parse_from([
            "rez",
            "--build-system",
            "cmake",
            "--",
            "-DVALUE=1",
            "--",
            "-j4",
        ])
        .unwrap();

        assert_eq!(build.build.passthrough_args, ["-DVALUE=1", "--", "-j4"]);
    }

    #[test]
    fn rez_build_accepts_upstream_build_argument_aliases() {
        let build = BuildCli::try_parse_from(["rez", "--ba=-j4"]).unwrap();
        assert_eq!(build.build.build_args.as_deref(), Some("-j4"));

        let child = BuildCli::try_parse_from(["rez", "--cba=-j4"]).unwrap();
        assert_eq!(child.build.child_build_args.as_deref(), Some("-j4"));
    }

    #[test]
    fn build_result_reports_script_path_without_changing_normal_result() {
        let script_path = PathBuf::from("build/rez-build-env.sh");
        let mut scripted = BuildResult::ok(PathBuf::from("build"), 1.25);
        scripted.build_env_script = Some(script_path.clone());
        let lines = format_build_result(0, &scripted);
        assert_eq!(lines[0], "Build 1: OK (1.2s)");
        assert!(lines
            .iter()
            .any(|line| line.contains(&script_path.display().to_string())));

        let normal = BuildResult::ok(PathBuf::from("build"), 1.25);
        let lines = format_build_result(0, &normal);
        assert_eq!(lines, vec!["Build 1: OK (1.2s)"]);
    }
}
