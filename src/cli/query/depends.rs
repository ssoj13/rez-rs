// SPDX-License-Identifier: Apache-2.0

//! `rez depends` - reverse dependency lookup for packages.

use clap::Args;
use foundation::errors::Result;
use repository::package::search::get_reverse_dependencies;
use version::Version;

use crate::cli::util::parse_path_list;

/// Perform a reverse package dependency lookup.
#[derive(Args, Debug)]
pub struct DependsArgs {
    /// Package that other packages depend on
    #[arg(value_name = "PKG")]
    pub pkg: String,

    /// Dependency tree depth limit
    #[arg(short = 'd', long)]
    pub depth: Option<usize>,

    /// Set package search paths
    #[arg(long)]
    pub paths: Option<String>,

    /// Include build requirements
    #[arg(short = 'b', long)]
    pub build_requires: bool,

    /// Include private build requirements
    #[arg(short = 'p', long)]
    pub private_build_requires: bool,

    /// Quiet mode
    #[arg(short = 'q', long)]
    pub quiet: bool,
}

pub fn run(args: &DependsArgs) -> Result<()> {
    let pkg_paths = parse_path_list(args.paths.as_deref());

    // Parse package name and optional version from "pkg-1.2.3" format
    let (pkg_name, version) = parse_pkg_str(&args.pkg)?;

    // Get reverse dependencies
    let deps = get_reverse_dependencies(
        &pkg_name,
        version.as_ref(),
        args.depth,
        pkg_paths.as_deref(),
    )?;

    if deps.is_empty() {
        if !args.quiet {
            println!("No packages depend on '{}'", args.pkg);
        }
        return Ok(());
    }

    // Sort and display results
    let mut names: Vec<&String> = deps.keys().collect();
    names.sort();

    for dep_name in names {
        let reqs = &deps[dep_name];
        let req_strs: Vec<String> = reqs.iter().map(|r| r.to_string()).collect();
        println!("{}: {}", dep_name, req_strs.join(", "));
    }

    Ok(())
}

/// Parse package string "name-1.2.3" into (name, version).
fn parse_pkg_str(pkg: &str) -> Result<(String, Option<Version>)> {
    // Try parsing as versioned object
    if let Ok(obj) = pkg.parse::<version::VersionedObject>() {
        let name = obj.name().to_string();
        let version = obj.version().clone();
        return Ok((name, Some(version)));
    }

    // Fallback: just a name
    Ok((pkg.to_string(), None))
}
