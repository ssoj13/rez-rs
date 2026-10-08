//! rez gui — graphical package browser and resolver.

use std::path::PathBuf;

use clap::Parser;

/// Open rez GUI to browse packages and resolve environments.
#[derive(Parser, Debug)]
#[command(name = "gui")]
pub struct GuiArgs {
    /// Open this resolved context (.rxt file) on startup
    #[arg(value_name = "FILE")]
    pub file: Option<PathBuf>,
}

pub fn run(args: &GuiArgs) -> foundation::errors::Result<()> {
    gui::run(args.file.clone()).map_err(|e| foundation::errors::RezError::Config(e.to_string()))
}
