// SPDX-License-Identifier: Apache-2.0

//! `rez diff` - compare source code between package versions.

use clap::Args;
use foundation::errors::{Result, RezError};
use repository::package::discover::{get_latest_package, get_package_from_string};

/// Compare source of two packages.
#[derive(Args, Debug)]
pub struct DiffArgs {
    /// Package to diff (e.g. "foo-1.0.0")
    #[arg(value_name = "PKG1")]
    pub pkg1: String,

    /// Package to diff against (default: next higher version)
    #[arg(value_name = "PKG2")]
    pub pkg2: Option<String>,
}

pub fn run(args: &DiffArgs) -> Result<()> {
    // Get first package
    let pkg1_info = get_package_from_string(&args.pkg1, None)?
        .ok_or_else(|| RezError::PackageNotFound(format!("Package '{}' not found", args.pkg1)))?;

    // Convert PackageInfo to Package for field access
    let pkg1 = package_from_info(&pkg1_info)?;

    // Get second package
    let pkg2_info = if let Some(ref s) = args.pkg2 {
        get_package_from_string(s, None)?
            .ok_or_else(|| RezError::PackageNotFound(format!("Package '{}' not found", s)))?
    } else {
        // Find next higher version
        let latest_info = get_latest_package(&pkg1.name, None, None)?.ok_or_else(|| {
            RezError::PackageNotFound(format!("No package family '{}'", pkg1.name))
        })?;

        if latest_info.version == pkg1.version {
            println!(
                "{}-{} is already the latest version",
                pkg1.name, pkg1.version
            );
            return Ok(());
        }
        latest_info
    };

    let pkg2 = package_from_info(&pkg2_info)?;

    // Print diff header
    println!("--- {}-{}", pkg1.name, pkg1.version);
    println!("+++ {}-{}", pkg2.name, pkg2.version);
    println!();

    // Compare fields
    diff_field("description", &pkg1.description, &pkg2.description);
    diff_vec(
        "requires",
        &req_strs(&pkg1.requires),
        &req_strs(&pkg2.requires),
    );
    diff_vec(
        "build_requires",
        &req_strs(&pkg1.build_requires),
        &req_strs(&pkg2.build_requires),
    );
    diff_vec(
        "private_build_requires",
        &req_strs(&pkg1.private_build_requires),
        &req_strs(&pkg2.private_build_requires),
    );
    diff_vec("tools", &pkg1.tools, &pkg2.tools);
    diff_field("commands", &pkg1.commands, &pkg2.commands);
    diff_field("pre_commands", &pkg1.pre_commands, &pkg2.pre_commands);
    diff_field("post_commands", &pkg1.post_commands, &pkg2.post_commands);
    diff_variants(&pkg1.variants, &pkg2.variants);

    Ok(())
}

/// Convert package metadata through the canonical package parser.
fn package_from_info(info: &repository::PackageInfo) -> Result<model::package::Package> {
    info.to_package()
}

fn req_strs(reqs: &[version::Requirement]) -> Vec<String> {
    reqs.iter().map(|r| r.to_string()).collect()
}

fn diff_field(name: &str, a: &Option<String>, b: &Option<String>) {
    if a != b {
        println!("  {name}:");
        if let Some(v) = a {
            println!("    - {v}");
        }
        if let Some(v) = b {
            println!("    + {v}");
        }
    }
}

fn diff_vec(name: &str, a: &[String], b: &[String]) {
    if a != b {
        println!("  {name}:");
        let b_set: std::collections::HashSet<&String> = b.iter().collect();
        let a_set: std::collections::HashSet<&String> = a.iter().collect();
        for item in a {
            if !b_set.contains(item) {
                println!("    - {item}");
            }
        }
        for item in b {
            if !a_set.contains(item) {
                println!("    + {item}");
            }
        }
    }
}

fn diff_variants(a: &[Vec<version::Requirement>], b: &[Vec<version::Requirement>]) {
    let a_strs: Vec<String> = a
        .iter()
        .map(|v| format!("{:?}", v.iter().map(|r| r.to_string()).collect::<Vec<_>>()))
        .collect();
    let b_strs: Vec<String> = b
        .iter()
        .map(|v| format!("{:?}", v.iter().map(|r| r.to_string()).collect::<Vec<_>>()))
        .collect();
    diff_vec("variants", &a_strs, &b_strs);
}

#[cfg(test)]
mod tests {
    use super::{package_from_info, req_strs};
    use repository::PackageInfo;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn package_from_info_rejects_malformed_root_requirement() {
        let info = PackageInfo::from_data(HashMap::from([
            ("name".into(), json!("foo")),
            ("version".into(), json!("1.0")),
            ("requires".into(), json!(["bar-1."])),
        ]))
        .unwrap();

        assert!(package_from_info(&info).is_err());
    }

    #[test]
    fn package_from_info_rejects_malformed_variant_requirement() {
        let info = PackageInfo::from_data(HashMap::from([
            ("name".into(), json!("foo")),
            ("version".into(), json!("1.0")),
            ("variants".into(), json!([["python-3."]])),
        ]))
        .unwrap();

        assert!(package_from_info(&info).is_err());
    }

    #[test]
    fn package_from_info_preserves_valid_requirement_diff_inputs() {
        let info = PackageInfo::from_data(HashMap::from([
            ("name".into(), json!("foo")),
            ("version".into(), json!("1.0")),
            ("requires".into(), json!(["bar-1.2"])),
            ("build_requires".into(), json!(["compiler-2.0"])),
            ("private_build_requires".into(), json!(["internal-3.0"])),
            ("variants".into(), json!([["python-3.11", "toolchain-1"]])),
        ]))
        .unwrap();

        let package = package_from_info(&info).unwrap();

        assert_eq!(req_strs(&package.requires), ["bar-1.2"]);
        assert_eq!(req_strs(&package.build_requires), ["compiler-2.0"]);
        assert_eq!(req_strs(&package.private_build_requires), ["internal-3.0"]);
        assert_eq!(
            package.variants[0]
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["python-3.11", "toolchain-1"]
        );
    }
}
