// SPDX-License-Identifier: Apache-2.0

//! `rez plugins` - list plugins provided by a package.

use clap::Args;
use foundation::errors::Result;
use repository::package::discover::{iter_package_families, iter_packages};

use crate::cli::util::parse_path_list;

/// List plugins for a package.
#[derive(Args, Debug)]
pub struct PluginsArgs {
    /// Package to list plugins for
    #[arg(value_name = "PKG")]
    pub pkg: String,

    /// Set package search paths
    #[arg(long)]
    pub paths: Option<String>,
}

pub fn run(args: &PluginsArgs) -> Result<()> {
    let pkg_paths = parse_path_list(args.paths.as_deref());

    // Search all packages for plugin_for matching our pkg
    let families = iter_package_families(pkg_paths.as_deref())?;
    let mut plugins: Vec<String> = Vec::new();

    for family in &families {
        let pkgs = iter_packages(family, None, pkg_paths.as_deref())?;
        for info in &pkgs {
            // Check plugin_for field
            if let Some(pf) = info.data.get("plugin_for").and_then(|v| v.as_array()) {
                for name in pf {
                    if name.as_str() == Some(&args.pkg) {
                        plugins.push(format!("{}-{}", info.name, info.version));
                        break;
                    }
                }
            }
        }
    }

    if plugins.is_empty() {
        println!("package '{}' has no plugins.", args.pkg);
    } else {
        plugins.sort();
        for p in &plugins {
            println!("{p}");
        }
    }
    Ok(())
}
