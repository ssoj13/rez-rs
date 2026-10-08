// SPDX-License-Identifier: Apache-2.0

//! `rez mv` - move packages between repositories.

use std::path::PathBuf;

use clap::Args;

use foundation::errors::{Result, RezError};
use model::config::CONFIG;
use repository::package::ops as package_ops;
use version::VersionedObject;

/// Move a package from one repository to another.
#[derive(Args, Debug)]
pub struct MvArgs {
    /// Package to move (e.g. "foo-1.2.3")
    #[arg(value_name = "PKG")]
    pub package: String,

    /// Source repository path (if not given, lists repos containing the package)
    #[arg(value_name = "PATH")]
    pub src_path: Option<String>,

    /// Destination repository path
    #[arg(short = 'd', long = "dest-path", required = true, value_name = "PATH")]
    pub dest_path: String,

    /// Preserve timestamp of source package
    #[arg(short = 'k', long = "keep-timestamp")]
    pub keep_timestamp: bool,

    /// Move even if package isn't relocatable
    #[arg(short = 'f', long)]
    pub force: bool,

    /// Verbose output
    #[arg(from_global)]
    pub verbose: u8,
}

/// List repositories containing the package (when no PATH specified).
fn list_repos_for_pkg(name: &str, version: &str) -> Result<()> {
    let cfg = &*CONFIG;
    let mut found = Vec::new();

    for base in cfg.expanded_packages_path() {
        let ver_dir = base.join(name).join(version);
        if ver_dir.to_os().is_dir() {
            found.push(base);
        }
    }

    if found.is_empty() {
        return Err(RezError::PackageNotFound(format!("{}-{}", name, version)));
    }

    println!("No action taken. Run again, and set PATH to one of:");
    for repo in &found {
        println!("  {}", repo);
    }
    Ok(())
}

pub fn run(args: &MvArgs) -> Result<()> {
    let obj = VersionedObject::new(&args.package)?;
    let name = obj.name().to_string();
    let version = obj.version().to_string();
    model::serialise::validate_rez_package_path(
        &name,
        (!version.is_empty()).then_some(version.as_str()),
    )?;

    // If no source path given, list matching repos and exit
    let src_base = match &args.src_path {
        Some(p) => PathBuf::from(p),
        None => return list_repos_for_pkg(&name, &version),
    };

    let src_path = src_base.join(&name).join(&version);
    if !src_path.is_dir() {
        return Err(RezError::PackageNotFound(format!(
            "{}-{} not found in {}",
            name,
            version,
            src_base.display()
        )));
    }

    let dest_base = PathBuf::from(&args.dest_path);
    let dest_path = dest_base.join(&name).join(&version);

    let candidate = super::cp::find_pkg_path(&name, &version, &[src_base])?;
    let options = package_ops::CopyOptions {
        force: args.force,
        keep_timestamp: args.keep_timestamp,
        ..Default::default()
    };
    let result = package_ops::move_package(&candidate, &dest_base, &options)?;

    if args.verbose > 0 {
        println!(
            "Moved {}-{}: {} -> {}",
            name,
            version,
            result.src_path.display(),
            result.dest_path.display()
        );
        for item in &result.copied {
            println!("  {}", item);
        }
    } else {
        println!("Moved {}-{} to {}", name, version, dest_path.display());
    }

    Ok(())
}
