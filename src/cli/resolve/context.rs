// SPDX-License-Identifier: Apache-2.0

//! `rez context` - inspect and query resolved .rxt context files.

use std::collections::HashMap;
use std::path::Path;

use clap::Args;

use foundation::errors::{Result, RezError};
use resolve::context::ResolvedContext;
use resolve::status;
use rex::types::ShellType;

// =============================================================================
// Args
// =============================================================================

/// Print information about a resolved context, or the current context.
#[derive(Args, Debug)]
pub struct ContextArgs {
    /// Context file (.rxt) to inspect. Reads current context if not supplied.
    #[arg(value_name = "RXT")]
    pub rxt: Option<String>,

    /// Print only the request list (not including implicits).
    #[arg(long = "req", visible_alias = "print-request")]
    pub print_request: bool,

    /// Print only the resolve list.
    #[arg(long = "res", visible_alias = "print-resolve")]
    pub print_resolve: bool,

    /// Print resolved packages in source order rather than alphabetical.
    #[arg(long = "so", visible_alias = "source-order")]
    pub source_order: bool,

    /// Show resolved package URIs rather than root filepaths.
    #[arg(long = "su", visible_alias = "show-uris")]
    pub show_uris: bool,

    /// Print the resolve graph as text (dot format).
    #[arg(long = "pg", visible_alias = "print-graph")]
    pub print_graph: bool,

    /// Interpret the context and print the resulting shell code.
    #[arg(short = 'i', long)]
    pub interpret: bool,

    /// Output format for interpreted code: shell name, "dict", "table", or "json".
    #[arg(short = 'f', long, default_value = "default")]
    pub format: String,

    /// Don't inherit current environment when interpreting.
    #[arg(long)]
    pub no_env: bool,

    /// Diff this context against another .rxt file.
    #[arg(long, value_name = "RXT")]
    pub diff: Option<String>,

    /// Increase verbosity.
    #[arg(from_global)]
    pub verbose: u8,
}

// =============================================================================
// Run
// =============================================================================

pub fn run(args: &ContextArgs) -> Result<()> {
    // Load the context
    let rc = load_context(&args.rxt)?;

    let parent_env: Option<HashMap<String, String>> = if args.no_env {
        Some(HashMap::new())
    } else {
        None
    };

    // Non-interpret modes
    if !args.interpret {
        if args.print_request {
            for req in rc.package_requests() {
                println!("{req}");
            }
            return Ok(());
        }

        if args.print_resolve {
            if let Some(pkgs) = rc.resolved_packages() {
                for pkg in pkgs {
                    if args.show_uris {
                        // URI: repo_path + qualified_name
                        let uri = match &pkg.repo_path {
                            Some(rp) => format!("{}:{}", rp.display(), pkg.qualified_name()),
                            None => pkg.qualified_name(),
                        };
                        println!("{uri}");
                    } else {
                        println!("{}", pkg.qualified_name());
                    }
                }
            }
            return Ok(());
        }

        if args.print_graph {
            if rc.has_graph() {
                if let Some(ref gs) = rc.graph_string {
                    println!("{gs}");
                }
            } else {
                eprintln!("The context does not contain a graph.");
            }
            return Ok(());
        }

        if let Some(ref diff_path) = args.diff {
            let other = ResolvedContext::load(Path::new(diff_path), None)?;
            print_resolve_diff(&rc, &other);
            return Ok(());
        }

        // Default: print context info
        rc.print_info(args.verbose as u32);
        return Ok(());
    }

    // --interpret mode
    let fmt = args.format.as_str();
    match fmt {
        "dict" | "table" | "json" => {
            let env = rc.get_environ(parent_env)?;
            match fmt {
                "table" => {
                    let mut pairs: Vec<_> = env.iter().collect();
                    pairs.sort_by_key(|(k, _)| (*k).clone());
                    for (k, v) in pairs {
                        println!("{k:<40} {v}");
                    }
                }
                "json" => {
                    let json = serde_json::to_string_pretty(&env).unwrap_or_default();
                    println!("{json}");
                }
                _ => {
                    // dict: debug format
                    let mut pairs: Vec<_> = env.iter().collect();
                    pairs.sort_by_key(|(k, _)| (*k).clone());
                    println!("{{");
                    for (k, v) in pairs {
                        println!("  {k:?}: {v:?},");
                    }
                    println!("}}");
                }
            }
        }
        _ => {
            // Shell code output
            let shell =
                if fmt == "default" {
                    None
                } else {
                    Some(ShellType::from_name(fmt).ok_or_else(|| {
                        RezError::Config(format!("Unknown shell/format: {fmt:?}"))
                    })?)
                };
            let code = rc.get_shell_code(shell, parent_env)?;
            println!("{code}");
        }
    }

    Ok(())
}

// =============================================================================
// Helpers
// =============================================================================

/// Load context from explicit path or current REZ_RXT_FILE env.
fn load_context(rxt: &Option<String>) -> Result<ResolvedContext> {
    let path = if let Some(ref p) = rxt {
        std::path::PathBuf::from(p)
    } else if let Some(cf) = status::context_file() {
        cf
    } else {
        return Err(RezError::ResolvedContext(
            "Not in a resolved environment context.".into(),
        ));
    };

    ResolvedContext::load(&path, None)
}

/// Simple diff between two resolved contexts (package-level).
fn print_resolve_diff(a: &ResolvedContext, b: &ResolvedContext) {
    let a_pkgs = pkg_map(a);
    let b_pkgs = pkg_map(b);

    // Added
    for (name, ver) in &b_pkgs {
        if !a_pkgs.contains_key(name.as_str()) {
            println!("+ {name}-{ver}");
        }
    }
    // Removed
    for (name, ver) in &a_pkgs {
        if !b_pkgs.contains_key(name.as_str()) {
            println!("- {name}-{ver}");
        }
    }
    // Changed
    for (name, ver_a) in &a_pkgs {
        if let Some(ver_b) = b_pkgs.get(name.as_str()) {
            if ver_a != ver_b {
                println!("~ {name}: {ver_a} -> {ver_b}");
            }
        }
    }
}

/// Build a name->version map from resolved packages.
fn pkg_map(ctx: &ResolvedContext) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Some(pkgs) = ctx.resolved_packages() {
        for pkg in pkgs {
            map.insert(pkg.name.clone(), pkg.version.to_string());
        }
    }
    map
}
