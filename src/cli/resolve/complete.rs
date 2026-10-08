// SPDX-License-Identifier: Apache-2.0

//! `rez complete` - package name completion for shell integration.

use std::path::PathBuf;

use clap::Args;

use foundation::errors::Result;
use repository::package::discover as packages;

// =============================================================================
// Args
// =============================================================================

/// Print package completion strings for shell integration.
#[derive(Args, Debug)]
pub struct CompleteArgs {
    /// Completion prefix (what the user has typed so far).
    #[arg(value_name = "PREFIX")]
    pub prefix: Option<String>,

    /// Package search paths (separated by OS path separator).
    /// Defaults to the configured packages_path.
    #[arg(long)]
    pub paths: Option<String>,

    /// Complete family names only (no version expansion).
    #[arg(long)]
    pub families: bool,
}

// =============================================================================
// Run
// =============================================================================

pub fn run(args: &CompleteArgs) -> Result<()> {
    let prefix = args.prefix.as_deref().unwrap_or("");

    // Parse search paths
    let search_paths: Option<Vec<PathBuf>> = args.paths.as_ref().map(|p| {
        let sep = if cfg!(windows) { ';' } else { ':' };
        p.split(sep)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect()
    });
    let paths_ref = search_paths.as_deref();

    if args.families {
        // Complete family names only
        let families = packages::iter_package_families(paths_ref)?;
        let matches: Vec<_> = families.iter().filter(|f| f.starts_with(prefix)).collect();
        if !matches.is_empty() {
            println!(
                "{}",
                matches
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
    } else {
        // Full completion (family + version)
        let completions = packages::get_completions(prefix, paths_ref)?;
        if !completions.is_empty() {
            println!("{}", completions.join(" "));
        }
    }

    Ok(())
}
