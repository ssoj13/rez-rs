// SPDX-License-Identifier: Apache-2.0
//! Graphical package browser and resolved-context editor.
use ::repository::{package, repository};
use foundation::{constants, errors};
use model::config;
mod gui;
pub use gui::*;
