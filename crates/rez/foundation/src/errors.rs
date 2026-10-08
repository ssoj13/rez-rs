// SPDX-License-Identifier: Apache-2.0

//! Error types for rez operations.
//!
//! Mirrors Python rez exceptions.py hierarchy.

use std::path::PathBuf;
use thiserror::Error;

/// Base error type for all rez operations.
#[derive(Debug, Error)]
pub enum RezError {
    // -- Version errors --
    #[error("Version error: {0}")]
    Version(String),

    #[error("Parse error: {0}")]
    Parse(String),

    // -- System / internal --
    #[error("Rez system error: {0}")]
    System(String),

    // -- Config --
    #[error("Configuration error: {0}")]
    Config(String),

    // -- Package errors --
    #[error("Package family not found: {0}")]
    PackageFamilyNotFound(String),

    #[error("Package not found: {0}")]
    PackageNotFound(String),

    #[error("Package request error: {0}")]
    PackageRequest(String),

    #[error("Package metadata error: {msg}")]
    PackageMetadata {
        msg: String,
        path: Option<PathBuf>,
        resource_key: Option<String>,
    },

    #[error("Package command error: {0}")]
    PackageCommand(String),

    #[error("Invalid package: {0}")]
    InvalidPackage(String),

    // -- Resource errors --
    #[error("Resource not found: {0}")]
    ResourceNotFound(String),

    #[error("Resource content error: {0}")]
    ResourceContent(String),

    // -- Resolve / Solver --
    #[error("Resolve error: {0}")]
    Resolve(String),

    #[error("Resolved context error: {0}")]
    ResolvedContext(String),

    // -- Rex --
    #[error("Rex error: {0}")]
    Rex(String),

    #[error("Rex undefined variable: {0}")]
    RexUndefinedVariable(String),

    #[error("Rex stop: {0}")]
    RexStop(String),

    // -- Build --
    #[error("Build error: {0}")]
    Build(String),

    #[error("Build system error: {0}")]
    BuildSystem(String),

    #[error("Build context resolve error: {message}")]
    BuildContextResolve {
        message: String,
        graph: Option<String>,
    },

    #[error("Build process error: {0}")]
    BuildProcess(String),

    // -- Release --
    #[error("Release error: {0}")]
    Release(String),

    #[error("Release VCS error: {0}")]
    ReleaseVcs(String),

    #[error("Release hook error: {0}")]
    ReleaseHook(String),

    #[error("Release hook cancelling: {0}")]
    ReleaseHookCancelling(String),

    // -- Package operations --
    #[error("Package copy error: {0}")]
    PackageCopy(String),

    #[error("Package move error: {0}")]
    PackageMove(String),

    #[error("Package cache error: {0}")]
    PackageCache(String),

    #[error("Package test error: {0}")]
    PackageTest(String),

    #[error("Context bundle error: {0}")]
    ContextBundle(String),

    // -- Suite --
    #[error("Suite error: {0}")]
    Suite(String),

    // -- Repository --
    #[error("Package repository error: {0}")]
    PackageRepository(String),

    // -- Bind --
    #[error("Bind error: {0}")]
    Bind(String),

    // -- Plugin --
    #[error("Plugin error: {0}")]
    Plugin(String),

    // -- Python VM --
    #[error("Python error: {0}")]
    Python(String),

    // -- IO / external --
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Regex error: {0}")]
    Regex(#[from] regex::Error),
}

pub type Result<T> = std::result::Result<T, RezError>;
