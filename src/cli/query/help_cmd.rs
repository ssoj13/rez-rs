// SPDX-License-Identifier: Apache-2.0

//! `rez help` - display package help documentation.

use clap::Args;
use foundation::errors::{Result, RezError};
use repository::package::help::PackageHelp;
use version::{Requirement, VersionRange};

use crate::cli::util::parse_path_list;

/// Display help for a package.
#[derive(Args, Debug)]
pub struct HelpCmdArgs {
    /// Package name (optionally with version range, e.g. "python-3.7+").
    #[arg()]
    pub package: Option<String>,

    /// Help section to view (1-based index).
    #[arg(default_value_t = 1)]
    pub section: usize,

    /// List available help sections instead of opening.
    #[arg(short, long)]
    pub list: bool,

    /// Package version (alternative to specifying in package string).
    #[arg(long, value_name = "VER")]
    pub version: Option<String>,

    /// Open the rez manual instead.
    #[arg(short, long)]
    pub manual: bool,

    /// Package search paths (separated by OS path separator).
    #[arg(long, value_name = "PATHS")]
    pub paths: Option<String>,
}

pub fn run(args: &HelpCmdArgs) -> Result<()> {
    // Open rez manual if requested or no package given
    if args.manual || args.package.is_none() {
        PackageHelp::open_rez_manual()?;
        return Ok(());
    }

    let pkg_str = args
        .package
        .as_deref()
        .ok_or_else(|| RezError::PackageRequest("package name is required".into()))?;
    let paths = parse_path_list(args.paths.as_deref());

    // Parse package request to get name + optional range
    let req = Requirement::new(pkg_str)?;
    if req.conflict() {
        return Err(RezError::PackageRequest(
            "Expected a non-conflicting package".into(),
        ));
    }

    // Determine version range: explicit --version overrides request range
    let range = if let Some(ref ver_str) = args.version {
        Some(VersionRange::new(&format!("=={}", ver_str))?)
    } else {
        req.range().cloned()
    };

    let help = PackageHelp::find(req.name(), range.as_ref(), paths.as_deref())?;

    if !help.success() {
        return Err(RezError::PackageNotFound(format!(
            "Could not find a package with help for '{}'",
            pkg_str
        )));
    }

    // Print package info header
    if let Some(ref pkg) = help.package {
        println!("Help found for:");
        println!("{}-{}", pkg.name, pkg.version);

        // Show description if available
        if let Some(desc) = pkg.get("description").and_then(|v| v.as_str()) {
            let desc = desc.trim();
            if !desc.is_empty() {
                println!();
                println!("Description:");
                println!("{}", desc);
                println!();
            }
        }
    }

    if args.list {
        // List all help sections
        print!("{}", help.format_sections());
    } else {
        // Open the requested section (convert 1-based to 0-based)
        let index = args.section.saturating_sub(1);
        if let Err(e) = help.open(index) {
            eprintln!("No such help section.");
            return Err(e);
        }
    }

    Ok(())
}
