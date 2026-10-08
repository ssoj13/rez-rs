// SPDX-License-Identifier: Apache-2.0

//! `rez cp` - copy packages between repositories.

use std::path::PathBuf;

use clap::Args;

use foundation::errors::{Result, RezError};
use model::config::CONFIG;
use repository::package::ops::{self as package_ops, CopyOptions, CopyResult};
use version::VersionedObject;

/// Copy a package from one repository to another.
#[derive(Args, Debug)]
pub struct CpArgs {
    /// Package to copy (e.g. "foo-1.2.3")
    #[arg(value_name = "PKG")]
    pub package: Option<String>,

    /// Destination repository path
    #[arg(long = "dest-path", short = 'd', value_name = "PATH")]
    pub dest_path: Option<String>,

    /// Set package search path (ignores --no-local if set)
    #[arg(long, value_name = "PATHS")]
    pub paths: Option<String>,

    /// Don't search local packages
    #[arg(long = "no-local")]
    pub no_local: bool,

    /// Copy to a different package version
    #[arg(long, value_name = "VERSION")]
    pub reversion: Option<String>,

    /// Copy to a different package name
    #[arg(long, value_name = "NAME")]
    pub rename: Option<String>,

    /// Overwrite existing package/variants
    #[arg(short = 'o', long)]
    pub overwrite: bool,

    /// Follow symlinks when copying payload
    #[arg(long = "follow-symlinks")]
    pub follow_symlinks: bool,

    /// Preserve timestamp of source package
    #[arg(short = 'k', long = "keep-timestamp")]
    pub keep_timestamp: bool,

    /// Copy even if package isn't relocatable
    #[arg(short = 'f', long)]
    pub force: bool,

    /// Allow copy into empty target repository
    #[arg(long = "allow-empty")]
    pub allow_empty: bool,

    /// Dry run mode
    #[arg(long = "dry-run")]
    pub dry_run: bool,

    /// Select variant indices to copy (zero-indexed)
    #[arg(long, num_args = 1.., value_name = "INDEX")]
    pub variants: Option<Vec<usize>>,

    /// Verbose output
    #[arg(from_global)]
    pub verbose: u8,
}

/// Parse "name-version" or "name-version@/path" into (name, version, optional src_path).
fn parse_pkg_spec(spec: &str) -> Result<(String, String, Option<PathBuf>)> {
    // Check for "@path" syntax
    let (pkg_part, src_path) = if let Some(idx) = spec.find('@') {
        let path = PathBuf::from(&spec[idx + 1..]);
        (&spec[..idx], Some(path))
    } else {
        (spec, None)
    };

    let obj = VersionedObject::new(pkg_part)?;
    let version = obj.version().to_string();
    model::serialise::validate_rez_package_path(
        obj.name(),
        (!version.is_empty()).then_some(version.as_str()),
    )?;
    Ok((obj.name().to_string(), version, src_path))
}

/// Resolve source package path from search paths.
pub(super) fn find_pkg_path(
    name: &str,
    version: &str,
    search_paths: &[PathBuf],
) -> Result<repository::PackageCandidate> {
    use repository::{FilesystemPackageProvider, PackageProvider};
    let provider = FilesystemPackageProvider::from_paths(search_paths)?;
    let version = version::Version::new(version)?;
    provider
        .get_candidates(name, &version::VersionRange::any())?
        .into_iter()
        .find(|candidate| candidate.package.version == version)
        .ok_or_else(|| RezError::PackageNotFound(format!("{name}-{version}")))
}

/// Get search paths based on args.
fn get_search_paths(args: &CpArgs) -> Vec<PathBuf> {
    if let Some(ref paths_str) = args.paths {
        paths_str
            .split(if cfg!(windows) { ';' } else { ':' })
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect()
    } else if args.no_local {
        // Nonlocal: skip local_packages_path
        let cfg = &*CONFIG;
        let local = cfg.expanded_local_packages_path();
        cfg.expanded_packages_path()
            .into_iter()
            .filter(|p| p != &local)
            .map(|p| p.to_os())
            .collect()
    } else {
        CONFIG
            .expanded_packages_path()
            .into_iter()
            .map(|p| p.to_os())
            .collect()
    }
}

pub fn run(args: &CpArgs) -> Result<()> {
    let pkg_str = args
        .package
        .as_deref()
        .ok_or_else(|| RezError::PackageRequest("PKG argument required".into()))?;

    if args.dest_path.is_none() && args.rename.is_none() && args.reversion.is_none() {
        return Err(RezError::PackageRequest(
            "--dest-path must be specified unless --rename or --reversion are used".into(),
        ));
    }

    let (name, version, explicit_src) = parse_pkg_spec(pkg_str)?;

    // Explicit source directories must match the requested filesystem identity.
    let paths = if let Some(path) = explicit_src {
        let version_component = (!version.is_empty()).then_some(version.as_str());
        let family = if version_component.is_some()
            && path.file_name().and_then(|part| part.to_str()) == version_component
        {
            path.parent().filter(|parent| {
                parent.file_name().and_then(|part| part.to_str()) == Some(name.as_str())
            })
        } else if version_component.is_none()
            && path.file_name().and_then(|part| part.to_str()) == Some(name.as_str())
        {
            Some(path.as_path())
        } else {
            None
        };
        vec![family
            .and_then(|family| family.parent())
            .map(PathBuf::from)
            .unwrap_or(path)]
    } else {
        get_search_paths(args)
    };
    let candidate = find_pkg_path(&name, &version, &paths)?;
    let src_path = candidate
        .package
        .base
        .as_ref()
        .ok_or_else(|| RezError::PackageCopy("Source has no payload root".into()))?;
    let provenance = candidate
        .provenance
        .as_ref()
        .ok_or_else(|| RezError::PackageCopy("Source has no repository provenance".into()))?;
    let dest_name = args.rename.as_deref().unwrap_or(&name);
    let dest_version = args.reversion.as_deref().unwrap_or(&version);
    model::serialise::validate_rez_package_path(
        dest_name,
        (!dest_version.is_empty()).then_some(dest_version),
    )?;
    let dest_base = args
        .dest_path
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&provenance.location));

    // Safety: check if dest repo is empty
    if !args.allow_empty && dest_base.is_dir() {
        let is_empty = std::fs::read_dir(&dest_base)
            .map(|mut d| d.next().is_none())
            .unwrap_or(true);
        if is_empty {
            return Err(RezError::PackageCopy(
                "Destination repository is empty. Use --allow-empty to proceed.\n\
                 Verify --dest-path is the repository root, not a package path."
                    .into(),
            ));
        }
    }

    let dest_path = dest_base.join(dest_name).join(dest_version);

    if args.dry_run {
        println!(
            "[dry-run] Would copy {}-{} -> {}/{}",
            name,
            version,
            dest_path.display(),
            dest_name
        );
        return Ok(());
    }

    let opts = CopyOptions {
        overwrite: args.overwrite,
        follow_symlinks: args.follow_symlinks,
        keep_timestamp: args.keep_timestamp,
        force: args.force,
        variants: args.variants.clone(),
        destination_name: args.rename.clone(),
        destination_version: args
            .reversion
            .as_deref()
            .map(version::Version::new)
            .transpose()?,
        ..Default::default()
    };

    let result: CopyResult = package_ops::copy_package(&candidate, &dest_base, &opts)?;

    // Report results
    if !result.copied.is_empty() {
        if args.verbose > 0 {
            println!(
                "Copied {}-{}: {} -> {}",
                name,
                version,
                src_path.display(),
                dest_path.display()
            );
            for item in &result.copied {
                println!("  {}", item);
            }
        } else {
            println!("Copied {}-{} to {}", name, version, dest_path.display());
        }
    }

    if !result.skipped.is_empty() {
        eprintln!(
            "Skipped (target exists, use --overwrite): {}",
            result.skipped.join(", ")
        );
    }

    Ok(())
}
