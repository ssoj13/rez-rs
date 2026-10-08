// SPDX-License-Identifier: Apache-2.0
//! Dependency resolution, resolved environments, suites and bundles.
use crate as resolve;
use foundation::{constants, errors, log_debug, log_info, log_trace, rez_path, util};
use model::{config, environment, platform, serialise};
use rex as shell;
#[cfg(feature = "amqp")]
pub mod amqp;
pub mod bundle_context;
pub mod context;
pub mod package;
pub mod python_vm;
pub mod resolver;
pub mod solver;
pub mod status;
pub mod suite;
pub use context::*;
pub use resolver::*;
