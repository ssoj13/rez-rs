// SPDX-License-Identifier: Apache-2.0

//! `rez rm` - remove packages from repository.

use std::path::{Path, PathBuf};

use clap::Args;

use foundation::errors::{Result, RezError};
use repository::package::ops as package_ops;
use version::VersionedObject;

/// Remove package(s) from a repository.
#[derive(Args, Debug)]
pub struct RmArgs {
    /// Remove specified package (e.g. "foo-1.2.3")
    #[arg(short = 'p', long, group = "target", value_name = "PKG")]
    pub package: Option<String>,

    /// Remove package family (must be empty unless --force-family)
    #[arg(short = 'f', long, group = "target", value_name = "NAME")]
    pub family: Option<String>,

    /// Force remove package family even if not empty
    #[arg(long = "force-family")]
    pub force_family: bool,

    /// Dry run mode
    #[arg(long = "dry-run")]
    pub dry_run: bool,

    /// Repository path containing the package(s) to remove
    #[arg(value_name = "PATH")]
    pub path: Option<String>,
}

/// Remove a single versioned package.
fn rm_package(name: &str, version: &str, repo_path: &Path, dry_run: bool) -> Result<()> {
    if dry_run {
        return Err(RezError::PackageCopy(
            "--dry-run is not supported with --package".into(),
        ));
    }

    let removed = package_ops::remove_package(name, version, repo_path)?;
    if removed {
        println!("Package removed.");
    } else {
        return Err(RezError::PackageNotFound(format!("{}-{}", name, version)));
    }
    Ok(())
}

/// Remove a package family through the repository's shared guard and deletion path.
fn rm_family(name: &str, repo_path: &Path, force: bool, dry_run: bool) -> Result<()> {
    if dry_run {
        return Err(RezError::PackageCopy(
            "--dry-run is not supported with --family".into(),
        ));
    }

    match package_ops::remove_package_family(name, repo_path, force)? {
        Some(_) => {
            println!("Package family removed.");
            Ok(())
        }
        None => Err(RezError::PackageFamilyNotFound(name.to_string())),
    }
}

pub fn run(args: &RmArgs) -> Result<()> {
    let repo_path = args.path.as_ref().map(PathBuf::from);

    if let Some(ref pkg_str) = args.package {
        let repo = repo_path
            .ok_or_else(|| RezError::PackageRequest("Must specify PATH with --package".into()))?;
        let obj = VersionedObject::new(pkg_str)?;
        rm_package(obj.name(), &obj.version().to_string(), &repo, args.dry_run)
    } else if let Some(ref family_name) = args.family {
        let repo = repo_path
            .ok_or_else(|| RezError::PackageRequest("Must specify PATH with --family".into()))?;
        // Validate: should be just a name, not versioned
        let obj = VersionedObject::new(family_name)?;
        if !obj.version().is_empty() {
            return Err(RezError::PackageRequest(
                "Expected package name, not a versioned object".into(),
            ));
        }
        model::serialise::validate_rez_package_path(obj.name(), None)?;
        rm_family(obj.name(), &repo, args.force_family, args.dry_run)
    } else {
        Err(RezError::PackageRequest(
            "Must specify either --package or --family".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::rm_family;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn family_removal_preserves_ignored_packages_without_force() {
        let root = tempdir().expect("temporary repository");
        let version = root.path().join("foo").join("1.0");
        fs::create_dir_all(&version).expect("version directory");
        fs::write(version.join("package.yaml"), "name: foo\nversion: '1.0'\n")
            .expect("package definition");
        fs::write(root.path().join("foo").join(".ignore1.0"), "").expect("ignore marker");

        let error = rm_family("foo", root.path(), false, false)
            .expect_err("ignored but recognizable package must be preserved");
        assert!(error.to_string().contains("non-empty"));
        assert!(version.exists());

        rm_family("foo", root.path(), true, false).expect("forced removal");
        assert!(!root.path().join("foo").exists());
    }

    #[test]
    fn family_removal_handles_combined_sources_and_missing_families() {
        let root = tempdir().expect("temporary repository");
        fs::write(
            root.path().join("foo.yaml"),
            "name: foo\nversions: ['1.0']\n",
        )
        .expect("combined family");
        assert!(rm_family("foo", root.path(), false, false).is_err());
        rm_family("foo", root.path(), true, false).expect("forced combined removal");
        assert!(!root.path().join("foo.yaml").exists());

        let error = rm_family("missing", root.path(), true, false)
            .expect_err("missing family must remain an error");
        assert!(error.to_string().contains("not found"));
    }
}
