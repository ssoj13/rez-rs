// SPDX-License-Identifier: Apache-2.0

//! `rez view` - display package definition and metadata.

use clap::Args;
use foundation::errors::{Result, RezError};
use repository::package::discover::{get_package_from_string, iter_packages};
use std::path::PathBuf;
use version::VersionedObject;

use crate::cli::util::parse_path_list;

/// View the contents of a package.
#[derive(Args, Debug)]
pub struct ViewArgs {
    /// Package to view (e.g. "python", "python-3.7").
    #[arg(
        required_unless_present = "source_paths",
        conflicts_with = "source_paths"
    )]
    pub package: Option<String>,

    /// Developer source directories or definition files for batch build metadata.
    #[arg(long = "source-path", requires = "building", conflicts_with_all = ["all", "brief"])]
    pub source_paths: Vec<PathBuf>,

    /// Resolve each developer build variant using the native build lifecycle.
    #[arg(long, requires = "source_paths")]
    pub building: bool,

    /// Verify build readiness against installed repositories without prospective sources.
    #[arg(long, requires = "building")]
    pub installed_only: bool,

    /// Show all versions of the package.
    #[arg(short, long)]
    pub all: bool,

    /// Brief output (name + version only).
    #[arg(short, long)]
    pub brief: bool,

    /// Output format.
    #[arg(short, long, value_enum, default_value_t = ViewFormat::Yaml)]
    pub format: ViewFormat,

    /// Package search paths (separated by OS path separator).
    #[arg(long, value_name = "PATHS")]
    pub paths: Option<String>,
}

#[derive(clap::ValueEnum, Clone, Debug)]
pub enum ViewFormat {
    Yaml,
    Json,
}

pub fn run(args: &ViewArgs) -> Result<()> {
    let paths = parse_path_list(args.paths.as_deref());
    if !args.source_paths.is_empty() {
        if !matches!(args.format, ViewFormat::Json) {
            return Err(RezError::Build(
                "Batch build metadata requires --format json".into(),
            ));
        }
        let metadata = build_system::builders::metadata::collect(
            &args.source_paths,
            paths,
            args.installed_only,
        )?;
        println!(
            "{}",
            serde_json::to_string(&metadata).map_err(|error| {
                RezError::Build(format!("Failed to serialize batch build metadata: {error}"))
            })?
        );
        return Ok(());
    }
    let package = args.package.as_deref().ok_or_else(|| {
        RezError::PackageNotFound("A package or --source-path is required".into())
    })?;

    if args.all {
        return show_all_versions(package, paths.as_deref(), args);
    }

    // Parse package request to extract name + optional version
    let obj = VersionedObject::new(package)?;
    let name = obj.name();

    // Get specific version or latest
    let pkg = if obj.version().is_truthy() {
        get_package_from_string(package, paths.as_deref())?
    } else {
        repository::package::discover::get_latest_package(name, None, paths.as_deref())?
    };

    let pkg =
        pkg.ok_or_else(|| RezError::PackageNotFound(format!("No matches found for '{package}'")))?;

    if !args.brief {
        println!("URI:");
        // Show repo path if available
        if let Some(path) = pkg.get("_repo_path").and_then(|v| v.as_str()) {
            println!("{}/{}-{}", path, pkg.name, pkg.version);
        } else {
            println!("{}-{}", pkg.name, pkg.version);
        }
        println!();
        println!("CONTENTS:");
    }

    print_package_data(&pkg.data, &args.format);
    Ok(())
}

/// Show all versions of a package.
fn show_all_versions(name: &str, paths: Option<&[PathBuf]>, args: &ViewArgs) -> Result<()> {
    // Parse name part only (strip version if given)
    let obj = VersionedObject::new(name)?;
    let family = obj.name();

    let packages = iter_packages(family, None, paths)?;
    if packages.is_empty() {
        return Err(RezError::PackageNotFound(format!(
            "No matches found for '{}'",
            name
        )));
    }

    for pkg in &packages {
        if args.brief {
            println!("{}-{}", pkg.name, pkg.version);
        } else {
            println!("---");
            println!("{}-{}:", pkg.name, pkg.version);
            print_package_data(&pkg.data, &args.format);
            println!();
        }
    }
    Ok(())
}

/// Print package data in the chosen format, excluding internal fields.
fn print_package_data(
    data: &std::collections::HashMap<String, serde_json::Value>,
    format: &ViewFormat,
) {
    // Filter out internal fields starting with '_'
    let filtered: serde_json::Map<String, serde_json::Value> = data
        .iter()
        .filter(|(k, _)| !k.starts_with('_'))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    match format {
        ViewFormat::Yaml => {
            let yaml = serde_yaml::to_string(&filtered).unwrap_or_default();
            print!("{}", yaml);
        }
        ViewFormat::Json => {
            let json = serde_json::to_string_pretty(&filtered).unwrap_or_default();
            println!("{}", json);
        }
    }
}
