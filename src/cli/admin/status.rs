// SPDX-License-Identifier: Apache-2.0

//! `rez status` - display system status and environment diagnostics.

use clap::Args;

use foundation::errors::Result;
use resolve::status;

// =============================================================================
// Args
// =============================================================================

/// Report current rez system status and environment info.
#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Object to query (tool, package, context or suite name).
    /// If not provided, a summary of the current environment is shown.
    #[arg(value_name = "OBJECT")]
    pub object: Option<String>,

    /// List visible rez tools, optionally filtered by a glob pattern.
    #[arg(short = 't', long)]
    pub tools: bool,
}

// =============================================================================
// Run
// =============================================================================

pub fn run(args: &StatusArgs) -> Result<()> {
    let matched = if args.tools {
        status::print_visible_tools(args.object.as_deref())?
    } else {
        status::print_info(args.object.as_deref())?
    };

    if matched {
        Ok(())
    } else {
        let object = args.object.as_deref().unwrap_or("<environment>");
        Err(foundation::errors::RezError::Resolve(format!(
            "status query did not match an object: {object}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmatched_tools_filter_returns_cli_error() {
        let args = StatusArgs {
            object: Some("rez_status_tool_that_does_not_exist_93821".into()),
            tools: true,
        };
        assert!(run(&args).is_err());
    }
}
