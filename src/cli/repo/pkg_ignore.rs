//! `rez pkg-ignore` - ignore or unignore packages from resolution.
//!
//! Manages `.ignore` marker files in package directories.

use clap::Args;
use foundation::errors::{Result, RezError};
use model::config::{RezConfig, CONFIG};
use std::fs;
use std::path::PathBuf;
use version::VersionedObject;

/// Ignore or unignore a package version in a repository.
#[derive(Args, Debug)]
pub struct PkgIgnoreArgs {
    /// Exact package to (un)ignore, e.g. 'foo-1.2.3'
    #[arg(value_name = "PKG")]
    pub pkg: String,

    /// Repository path containing the package
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Unignore (make visible again)
    #[arg(short = 'u', long)]
    pub unignore: bool,

    /// Allow ignoring packages that don't exist yet
    #[arg(short = 'a', long)]
    pub allow_missing: bool,
}

pub fn run(args: &PkgIgnoreArgs) -> Result<()> {
    let (pkg_name, pkg_version) = parse_versioned_pkg(&args.pkg)?;

    // No PATH given: list repos where the package might live
    if args.path.is_none() {
        println!("No action taken. Run again, and set PATH to one of:");
        let mut found = false;
        for path in &CONFIG.packages_path {
            let repo_path = RezConfig::expand_path(path);
            let pkg_dir = repo_path.join(&pkg_name).join(&pkg_version);
            if args.allow_missing || pkg_dir.to_os().exists() {
                println!("  {}", repo_path);
                found = true;
            }
        }
        if !found && !args.allow_missing {
            return Err(RezError::Config(format!(
                "Package '{}' not found in any configured repository",
                args.pkg
            )));
        }
        return Ok(());
    }

    let path_arg = args
        .path
        .as_ref()
        .ok_or_else(|| RezError::Config("repository PATH is required".into()))?;
    let repo_path = RezConfig::expand_path(path_arg.to_str().unwrap_or(""));
    let pkg_dir = repo_path.join(&pkg_name).join(&pkg_version);
    let ignore_file = pkg_dir.join(".ignore");

    if args.unignore {
        // Remove .ignore marker
        if ignore_file.to_os().exists() {
            fs::remove_file(ignore_file.to_os())
                .map_err(|e| RezError::Config(format!("Cannot remove .ignore: {}", e)))?;
            println!("Package is now visible to resolves once more");
        } else {
            println!("No action taken - package was already visible");
        }
    } else {
        // Create .ignore marker
        if !pkg_dir.to_os().exists() && !args.allow_missing {
            return Err(RezError::Config(format!(
                "Package '{}' not found at {}",
                args.pkg, pkg_dir
            )));
        }
        if ignore_file.to_os().exists() {
            println!("No action taken - package was already ignored");
        } else {
            if !pkg_dir.to_os().exists() {
                fs::create_dir_all(pkg_dir.to_os())
                    .map_err(|e| RezError::Config(format!("Cannot create dir: {}", e)))?;
            }
            fs::write(ignore_file.to_os(), "")
                .map_err(|e| RezError::Config(format!("Cannot write .ignore: {}", e)))?;
            println!("Package is now ignored and will not be visible to resolves");
        }
    }

    Ok(())
}

/// Parse "name-1.2.3" into (name, version_string) using VersionedObject.
fn parse_versioned_pkg(pkg: &str) -> Result<(String, String)> {
    let obj = VersionedObject::new(pkg)?;
    if obj.version().is_empty() {
        return Err(RezError::Config(format!(
            "Invalid package format '{}'. Expected 'name-version' (e.g. 'foo-1.2.3')",
            pkg
        )));
    }
    Ok((obj.name().to_string(), obj.version().to_string()))
}
