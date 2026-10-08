// SPDX-License-Identifier: Apache-2.0

//! Shared types and filesystem primitives for the Rez workspace.

pub mod constants;
pub mod errors;
pub mod filesystem;
pub mod logging;
pub mod path;
pub mod patterns;
pub mod rez_path;
pub mod util;

pub use errors::{Result, RezError};
pub use rez_path::RezPath;

/// Home directory, honoring explicit overrides before native account discovery.
pub fn home() -> Option<String> {
    ["HOME", "USERPROFILE"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.is_empty())
        // Resolved subprocesses may intentionally have no home environment variables.
        // dirs uses Windows known folders or the Unix account database in that case.
        .or_else(|| dirs::home_dir().map(|path| path.to_string_lossy().into_owned()))
}
