// SPDX-License-Identifier: Apache-2.0
//! Repository access, provenance, publication and package payload management.
use foundation::{constants, errors, log_debug, log_info, log_trace, util};
use model::{config, environment, platform, serialise};
pub mod package;
pub mod provider;
pub mod repository;
pub use provider::{
    FilesystemPackageProvider, PackageCandidate, PackageProvenance, PackageProvider,
    PackageVariant, ResourceHandle, ResourceHandleKey, ResourceHandleVariables,
};
pub use repository::*;
