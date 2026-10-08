//! Shell integration - rex DSL, shell types, and wrapper generation.
//!
//! Provides shell-specific environment setup and command execution.

pub mod rex;
pub mod types;
mod wire;
pub mod wrapper;

use crate as shell;
#[doc(hidden)]
pub use wire::{apply_rex_actions, rex_environ};

// Re-export commonly used types
pub use rex::*;
pub use types::*;
