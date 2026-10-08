//! `rez bundle` - bundle context into relocatable directory.

use std::path::PathBuf;

use clap::Args;

use foundation::errors::{Result, RezError};
use resolve::bundle_context::{bundle_context, BundleOptions};

/// Bundle a resolved context and its packages into a relocatable directory.
#[derive(Args, Debug)]
pub struct BundleArgs {
    /// Source .rxt context file
    #[arg(value_name = "RXT")]
    pub rxt: PathBuf,

    /// Destination bundle directory (must not exist)
    #[arg(value_name = "DEST_DIR")]
    pub dest_dir: PathBuf,

    /// Skip non-relocatable packages instead of erroring
    #[arg(short = 's', long)]
    pub skip_non_relocatable: bool,

    /// Force bundle even for non-relocatable packages
    #[arg(short, long, conflicts_with = "skip_non_relocatable")]
    pub force: bool,

    /// Verbose output
    #[arg(from_global)]
    pub verbose: u8,

    /// Skip patching of shared libraries (rpath) - libs are patched by default
    #[arg(long = "no-lib-patch")]
    pub no_lib_patch: bool,
}

pub fn run(args: &BundleArgs) -> Result<()> {
    let rxt_path = std::fs::canonicalize(&args.rxt).unwrap_or_else(|_| args.rxt.clone());
    // dest_dir may not exist yet, so we can't canonicalize - use as-is
    let dest_dir = args.dest_dir.clone();

    if !rxt_path.exists() {
        return Err(RezError::ContextBundle(format!(
            "file does not exist: {}",
            rxt_path.display()
        )));
    }

    let options = BundleOptions {
        skip_non_relocatable: args.skip_non_relocatable,
        force: args.force,
        quiet: false,
        verbose: args.verbose > 0,
        patch_libs: !args.no_lib_patch,
    };

    let result = bundle_context(&rxt_path, &dest_dir, Some(options))?;

    if args.verbose > 0 {
        println!("Bundle created at: {}", result.bundle_path.display());
        println!("Packages copied: {}", result.packages_copied.len());
        println!("Total bytes: {}", result.total_bytes);
        println!("Context: {}", result.context_path.display());
    } else {
        println!("{}", result.bundle_path.display());
    }

    Ok(())
}
