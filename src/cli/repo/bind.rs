// SPDX-License-Identifier: Apache-2.0

//! `rez bind` — bind system software as rez packages.
//!
//! Uses `package::bind` for detection and writing. Supports `--list`, `--quickstart`, `--all`, or PKG.
//! `get_bind_modules` includes builtins + DCC + names from `CONFIG.bind_module_path` (custom bind execution not yet supported).

use std::path::{Path, PathBuf};

use clap::Args;

use foundation::errors::{Result, RezError};
use foundation::log_info;
use model::config::CONFIG;
use repository::package::bind::{
    bind_all, bind_package, get_bind_modules, BindFormat, BindOptions,
};
use version::Requirement;

/// Bind existing system software as rez packages.
#[derive(Args, Debug)]
pub struct BindArgs {
    /// Package to bind (e.g. "platform", "arch", "python")
    #[arg(value_name = "PKG")]
    pub pkg: Option<String>,

    /// Bind standard system packages (platform, arch, os, python, etc.)
    #[arg(short = 'q', long)]
    pub quickstart: bool,

    /// Bind all modules: detect and install every available software (DCC, tools, etc.)
    #[arg(short = 'a', long)]
    pub all: bool,

    /// Install to release packages path instead of local
    #[arg(short = 'r', long)]
    pub release: bool,

    /// Custom install path (overridden by --release)
    #[arg(short = 'i', long, value_name = "PATH")]
    pub install_path: Option<PathBuf>,

    /// List all available bind modules
    #[arg(short, long)]
    pub list: bool,

    /// Search for the bind module but do not perform the bind
    #[arg(short, long)]
    pub search: bool,

    /// Do not bind the package's dependencies
    #[arg(long)]
    pub no_deps: bool,

    /// Verbose output
    #[arg(from_global)]
    pub verbose: u8,

    /// Output format: py (default) or yaml
    #[arg(long, default_value = "py")]
    pub format: String,

    /// For python: use venv --copies (copy binaries instead of symlinks, more relocatable)
    #[arg(long)]
    pub copies: bool,
}

/// Resolve path with tilde expansion for home directory
fn resolve_path(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if s.starts_with('~') {
        if let Some(home) = dirs_or_home() {
            return PathBuf::from(s.replacen('~', &home.to_string_lossy(), 1));
        }
    }
    p.to_path_buf()
}

/// Get home directory from HOME or USERPROFILE env vars
fn dirs_or_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

pub fn run(args: &BindArgs) -> Result<()> {
    let fmt = match args.format.as_str() {
        "yaml" | "yml" => BindFormat::Yaml,
        _ => BindFormat::Py,
    };

    let install_path = if args.release {
        CONFIG.expanded_release_path_for_bind().to_os()
    } else if let Some(ref p) = args.install_path {
        // Resolve ~ to home directory
        resolve_path(p)
    } else {
        CONFIG.expanded_local_packages_path().to_os()
    };
    log_info!(
        "bind",
        "Install path: {}",
        foundation::rez_path::RezPath::from_os(&install_path)
    );

    // --list: show available bind modules
    if args.list {
        println!("{:<15} BIND MODULE", "PACKAGE");
        println!("{:<15} -----------", "-------");
        for name in get_bind_modules() {
            println!("{:<15} built-in", name);
        }
        return Ok(());
    }

    // --all: detect and bind every available module (full software scan)
    if args.all {
        log_info!("bind", "Binding all modules (dependency order)");
        let opts = BindOptions {
            python_copy: args.copies,
            ..Default::default()
        };
        let results = bind_all(&install_path, &[], fmt, &opts)?;
        println!(
            "\nBound {} package(s) into {}",
            results.len(),
            install_path.display()
        );
        if args.verbose > 0 {
            for p in &results {
                println!("  {}", p.display());
            }
        }
        return Ok(());
    }

    // --quickstart: bind all standard packages
    if args.quickstart {
        log_info!("bind", "Quickstart: binding Rez standard packages");
        let names = [
            "platform",
            "arch",
            "os",
            "python",
            "rez",
            "setuptools",
            "pip",
        ];
        let mut success_count = 0;
        let mut fail_count = 0;

        let opts = BindOptions {
            python_copy: args.copies,
            no_deps: true,
            ..Default::default()
        };
        for name in &names {
            print!("Binding {}... ", name);
            match bind_package(name, &install_path, None, fmt, &opts) {
                Ok(paths) => {
                    println!("ok ({} version(s))", paths.len());
                    success_count += 1;
                }
                Err(e) => {
                    println!("skipped ({})", e);
                    fail_count += 1;
                }
            }
        }

        println!(
            "\nBound {}/{} packages into {}",
            success_count,
            names.len(),
            install_path.display()
        );
        if fail_count > 0 {
            println!("({} skipped - software not found on system)", fail_count);
        }
        return Ok(());
    }

    // PKG required for remaining operations
    let package_request = args.pkg.as_ref().ok_or_else(|| {
        RezError::Bind("PKG required (or use --list / --quickstart / --all)".into())
    })?;
    let requirement = Requirement::new(package_request)?;
    if requirement.conflict() {
        return Err(RezError::Bind(format!(
            "Cannot bind a conflict requirement: {}",
            package_request
        )));
    }
    let version_range = requirement.range().filter(|range| !range.is_any()).cloned();
    let package_name = requirement.name();

    // --search: just check if the bind module exists
    if args.search {
        let modules = get_bind_modules();
        if modules.iter().any(|module| module == package_name) {
            println!("Bind module found: {} (built-in)", package_name);
        } else {
            return Err(RezError::Bind(format!(
                "No bind module found for: {}",
                package_name
            )));
        }
        return Ok(());
    }

    // Perform the bind
    let opts = BindOptions {
        python_copy: args.copies,
        version_range,
        no_deps: args.no_deps,
        ..Default::default()
    };
    if args.verbose > 0 {
        println!(
            "Binding {} into {}...",
            package_request,
            install_path.display()
        );
    }
    let paths = bind_package(package_name, &install_path, None, fmt, &opts)?;
    for p in &paths {
        println!("Bound package -> {}", p.display());
    }

    Ok(())
}
