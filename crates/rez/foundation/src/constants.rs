//! Rez constants, enums, and package attribute definitions.
//!
//! Ported from Python rez solver, resolver, and build subsystems.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Rez uses readable variant subpaths unless hashing is explicitly enabled.
pub const DEFAULT_HASHED_VARIANTS: bool = false;

// ---------------------------------------------------------------------------
// Solver
// ---------------------------------------------------------------------------

/// Internal solver version for benchmark tracking
pub const SOLVER_VERSION: u32 = 2;

/// How variants are selected during solve
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VariantSelectMode {
    /// Prefer latest version
    #[default]
    VersionPriority = 0,
    /// Prefer most intersecting range
    IntersectionPriority = 1,
}

/// Solver internal status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SolverStatus {
    /// Solve has not yet started
    Pending,
    /// Solve completed successfully
    Solved,
    /// Current solve exhausted, must split to continue
    Exhausted,
    /// Solve is not possible
    Failed,
    /// Solve contains a dependency cycle
    Cyclic,
    /// Solve started but not yet complete
    Unsolved,
}

impl fmt::Display for SolverStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Solved => write!(f, "solved"),
            Self::Exhausted => write!(f, "exhausted"),
            Self::Failed => write!(f, "failed"),
            Self::Cyclic => write!(f, "cyclic"),
            Self::Unsolved => write!(f, "unsolved"),
        }
    }
}

/// Callback return values during solver progress
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SolverCallbackReturn {
    /// Continue the solve
    KeepGoing,
    /// Abort the solve
    Abort,
    /// Stop and set to most recent failure
    Fail,
}

// ---------------------------------------------------------------------------
// Resolver
// ---------------------------------------------------------------------------

/// High-level resolver status (wraps solver)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ResolverStatus {
    /// Resolve has not yet started
    Pending,
    /// Resolve completed successfully
    Solved,
    /// Resolve is not possible
    Failed,
    /// Resolve was stopped by user callback
    Aborted,
}

impl fmt::Display for ResolverStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Solved => write!(f, "solved"),
            Self::Failed => write!(f, "failed"),
            Self::Aborted => write!(f, "aborted"),
        }
    }
}

// ---------------------------------------------------------------------------
// Resolved Context
// ---------------------------------------------------------------------------

/// Context serialization format version
pub const CONTEXT_SERIALIZE_VERSION: (u32, u32) = (4, 9);

/// Rez tools visibility in resolved environments
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RezToolsVisibility {
    /// Don't expose rez in resolved env
    Never = 0,
    /// Append rez to PATH
    #[default]
    Append = 1,
    /// Prepend rez to PATH
    Prepend = 2,
}

/// Suite visibility when entering new environments
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SuiteVisibility {
    /// Don't keep any suites visible
    Never = 0,
    /// Keep suites visible in any new env
    #[default]
    Always = 1,
    /// Keep only the parent suite visible
    Parent = 2,
    /// Keep all suites, parent takes precedence
    ParentPriority = 3,
}

/// Version locking mode for context patching
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PatchLock {
    /// No locking
    NoLock,
    /// Minor version updates only (X.*)
    Lock2,
    /// Patch version updates only (X.X.*)
    Lock3,
    /// Build version updates only (X.X.X.*)
    Lock4,
    /// Exact version lock
    Lock,
}

impl PatchLock {
    /// Number of version tokens to lock, or None for no-lock/exact-lock
    pub fn rank(&self) -> Option<usize> {
        match self {
            Self::NoLock => None,
            Self::Lock2 => Some(1),
            Self::Lock3 => Some(2),
            Self::Lock4 => Some(3),
            Self::Lock => None,
        }
    }
}

impl fmt::Display for PatchLock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoLock => write!(f, "no_lock"),
            Self::Lock2 => write!(f, "lock_2"),
            Self::Lock3 => write!(f, "lock_3"),
            Self::Lock4 => write!(f, "lock_4"),
            Self::Lock => write!(f, "lock"),
        }
    }
}

// ---------------------------------------------------------------------------
// Build
// ---------------------------------------------------------------------------

/// Build type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BuildType {
    /// Local developer build
    Local = 0,
    /// Central/release build
    Central = 1,
}

impl fmt::Display for BuildType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => write!(f, "local"),
            Self::Central => write!(f, "central"),
        }
    }
}

// ---------------------------------------------------------------------------
// Package attribute categories
// ---------------------------------------------------------------------------

/// Attributes set at release time
pub const PACKAGE_RELEASE_KEYS: &[&str] = &[
    "timestamp",
    "revision",
    "changelog",
    "release_message",
    "previous_version",
    "previous_revision",
    "vcs",
];

/// Attributes only relevant during build (not installed)
pub const PACKAGE_BUILD_ONLY_KEYS: &[&str] = &[
    "requires_rez_version",
    "build_system",
    "build_command",
    "preprocess",
    "pre_build_commands",
];

/// Attributes that contain rex (command execution) code
pub const PACKAGE_REX_KEYS: &[&str] = &[
    "pre_commands",
    "commands",
    "post_commands",
    "pre_build_commands",
    "pre_test_commands",
];

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_solver_status_display() {
        assert_eq!(SolverStatus::Pending.to_string(), "pending");
        assert_eq!(SolverStatus::Failed.to_string(), "failed");
        assert_eq!(SolverStatus::Cyclic.to_string(), "cyclic");
    }

    #[test]
    fn test_resolver_status_display() {
        assert_eq!(ResolverStatus::Solved.to_string(), "solved");
        assert_eq!(ResolverStatus::Aborted.to_string(), "aborted");
    }

    #[test]
    fn test_patch_lock_rank() {
        assert_eq!(PatchLock::NoLock.rank(), None);
        assert_eq!(PatchLock::Lock2.rank(), Some(1));
        assert_eq!(PatchLock::Lock3.rank(), Some(2));
        assert_eq!(PatchLock::Lock4.rank(), Some(3));
        assert_eq!(PatchLock::Lock.rank(), None);
    }

    #[test]
    fn test_build_type_display() {
        assert_eq!(BuildType::Local.to_string(), "local");
        assert_eq!(BuildType::Central.to_string(), "central");
    }

    #[test]
    fn test_variant_select_mode_rez_names() {
        assert_eq!(
            serde_json::from_str::<VariantSelectMode>(r#""intersection_priority""#).unwrap(),
            VariantSelectMode::IntersectionPriority
        );
        assert_eq!(
            serde_json::to_string(&VariantSelectMode::VersionPriority).unwrap(),
            r#""version_priority""#
        );
        assert!(serde_json::from_str::<VariantSelectMode>(r#""unknown""#).is_err());
    }

    #[test]
    fn test_variant_select_mode_default() {
        assert_eq!(
            VariantSelectMode::default(),
            VariantSelectMode::VersionPriority
        );
    }

    #[test]
    fn test_package_attribute_keys() {
        assert!(PACKAGE_RELEASE_KEYS.contains(&"timestamp"));
        assert!(PACKAGE_BUILD_ONLY_KEYS.contains(&"build_command"));
        assert!(PACKAGE_REX_KEYS.contains(&"commands"));
    }
}
