// SPDX-License-Identifier: Apache-2.0
//! Native build adapters, acquisition, Pip conversion and release hooks.
use foundation::{constants, errors, log_debug, log_info, util};
use model::{config, install, platform, serialise};
use repository::repository;
use rex as shell;
mod package {
    pub(crate) use model::package::{DeveloperPackage, Package, Variant};
}
#[cfg(feature = "amqp")]
use resolve::amqp;
pub mod builders;
pub mod pip;
pub mod release_hooks;
pub use builders::*;
