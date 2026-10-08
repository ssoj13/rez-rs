// SPDX-License-Identifier: Apache-2.0

//! `rez selftest` - run built-in cargo test suite.

use clap::Args;
use foundation::errors::{Result, RezError};

/// Run rez self-tests.
#[derive(Args, Debug)]
pub struct SelftestArgs {
    /// Specific test names to run
    #[arg(value_name = "TEST")]
    pub tests: Vec<String>,

    /// Verbose output
    #[arg(from_global)]
    pub verbose: u8,
}

pub fn run(args: &SelftestArgs) -> Result<()> {
    println!("rez-rs self-test");
    println!("================");
    println!();
    println!("To run the full test suite:");
    println!("  cargo test --workspace");
    println!();
    println!("For verbose output:");
    println!("  cargo test --workspace -- --nocapture");
    println!();

    // Try running cargo test if available
    let mut cmd = std::process::Command::new("cargo");
    cmd.arg("test").arg("--workspace");

    if args.verbose > 0 {
        cmd.arg("--").arg("--nocapture");
    }

    if !args.tests.is_empty() {
        for t in &args.tests {
            cmd.arg(t);
        }
    }

    // Try to run it - if cargo not available, just print instructions
    match cmd.status() {
        Ok(status) => {
            let code = status.code().unwrap_or(1);
            if code != 0 {
                return Err(RezError::PackageTest(format!(
                    "cargo test exited with code {}",
                    code
                )));
            }
            Ok(())
        }
        Err(_) => {
            eprintln!("Note: 'cargo' not found. Run tests manually with 'cargo test --workspace'.");
            Ok(())
        }
    }
}
