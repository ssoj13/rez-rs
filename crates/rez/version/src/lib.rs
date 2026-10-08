// SPDX-License-Identifier: Apache-2.0

//! Version system - tokens, versions, ranges, requirements.
//!
//! Ported from Python rez version module.

#![allow(clippy::module_inception)]

mod bound;
mod range;
mod requirement;
mod token;
mod version;

pub use bound::{Bound, LowerBound, UpperBound};
pub use range::{ContainmentMode, VersionRange};
pub use requirement::{Requirement, RequirementList, VersionedObject};
pub use token::{AlphanumericToken, SubToken};
pub use version::Version;

/// Check if a version string is contained in a range string.
/// Used by rex intersects() to align with resolver logic.
pub fn version_in_range(version_str: &str, range_str: &str) -> bool {
    let version = match Version::new(version_str) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let range = match VersionRange::new(range_str) {
        Ok(r) => r,
        Err(_) => return false,
    };
    range.contains_version(&version)
}

/// Check if two range strings intersect (have any version in common).
pub fn ranges_intersect(range_a: &str, range_b: &str) -> bool {
    let r1 = match VersionRange::new(range_a) {
        Ok(r) => r,
        Err(_) => return false,
    };
    let r2 = match VersionRange::new(range_b) {
        Ok(r) => r,
        Err(_) => return false,
    };
    r1.intersects(&r2)
}
