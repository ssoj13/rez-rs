// SPDX-License-Identifier: Apache-2.0

//! Resolved context — resolved environments and .rxt serialization.
//!
//! Ported from Python rez resolved_context.py. Holds resolved packages, ephemerals, and
//! generates shell scripts via `execute_shell`. `create_build_context` resolves build_requires for `rez build`.
//!
//! # Main flow
//! - `ResolvedContext::resolve` → Resolver → Solver → packages
//! - `execute_shell` → RexExecutor → rex commands → spawn subshell
//! - `create_build_context` — resolves build deps for a package variant

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use super::resolver::{ResolvedPackageInfo, Resolver};
use super::solver::SolverState;
use crate::config::CONFIG;
use crate::constants::{
    PatchLock, ResolverStatus, RezToolsVisibility, SolverCallbackReturn, VariantSelectMode,
    CONTEXT_SERIALIZE_VERSION,
};
use crate::errors::{Result, RezError};
use crate::package::filter::PackageFilterList;
use crate::package::order::{PackageOrderList, PackageOrderPod};
use crate::package::Package;
use crate::shell::rex::{Action, ActionInterpreter, OutputStyle, PythonInterpreter, RexExecutor};
use crate::shell::types::Shell;
use crate::shell::types::{create_shell, ShellType};
use crate::{log_debug, log_info};
use repository::provider::{
    FilesystemPackageProvider, PackageProvider, PackageVariant, ResourceHandle,
};
use version::Requirement;

/// File extension for saved resolved contexts.
pub const RXT_EXTENSION: &str = ".rxt";

/// Rez-rs version string embedded in saved contexts.
const REZ_RS_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Match local repository ownership without rewriting the recorded provenance.
fn local_resolve_names(packages: &[ResolvedPackageInfo], local_path: &Path) -> String {
    packages
        .iter()
        .filter(|package| {
            package
                .repo_path
                .as_ref()
                .or(package.root.as_ref())
                .is_some_and(|path| {
                    foundation::util::relative_to_authority(local_path, path)
                        .ok()
                        .flatten()
                        .is_some()
                })
        })
        .map(|package| package.qualified_name())
        .collect::<Vec<_>>()
        .join(" ")
}

// =============================================================================
// ResolveOptions - builder for configuring a resolve
// =============================================================================

/// Builder for creating a ResolvedContext with custom settings.
pub struct ResolveOptions {
    pub package_paths: Option<Vec<PathBuf>>,
    pub package_filter: Option<PackageFilterList>,
    pub package_orderers: Option<PackageOrderList>,
    pub variant_select_mode: VariantSelectMode,
    pub timestamp: Option<u64>,
    pub building: bool,
    pub testing: bool,
    pub caching: bool,
    pub package_cache_async: Option<bool>,
    pub package_caching: Option<bool>,
    pub add_implicit: bool,
    /// None inherits configured implicits; Some(empty) explicitly clears them.
    pub implicit_packages: Option<Vec<Requirement>>,
    pub verbosity: u32,
    pub max_fails: i32,
    pub time_limit: i32,
}

impl Default for ResolveOptions {
    fn default() -> Self {
        Self {
            package_paths: None,
            package_filter: None,
            package_orderers: None,
            variant_select_mode: CONFIG.variant_select_mode,
            timestamp: None,
            building: false,
            testing: false,
            caching: true,
            package_cache_async: None,
            package_caching: None,
            add_implicit: true,
            implicit_packages: None,
            verbosity: 0,
            max_fails: -1,
            time_limit: -1,
        }
    }
}

impl ResolveOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_paths(mut self, paths: Vec<PathBuf>) -> Self {
        self.package_paths = Some(paths);
        self
    }

    pub fn with_variant_select_mode(mut self, mode: VariantSelectMode) -> Self {
        self.variant_select_mode = mode;
        self
    }

    pub fn with_building(mut self, building: bool) -> Self {
        self.building = building;
        self
    }

    pub fn with_testing(mut self, testing: bool) -> Self {
        self.testing = testing;
        self
    }

    pub fn with_timestamp(mut self, ts: u64) -> Self {
        self.timestamp = Some(ts);
        self
    }

    /// Limit resolver backtracking failures and elapsed solve time; `-1` disables each limit.
    pub fn with_limits(mut self, max_fails: i32, time_limit: i32) -> Self {
        self.max_fails = max_fails;
        self.time_limit = time_limit;
        self
    }

    pub fn with_filter(mut self, filter: PackageFilterList) -> Self {
        self.package_filter = Some(filter);
        self
    }

    pub fn with_verbosity(mut self, v: u32) -> Self {
        self.verbosity = v;
        self
    }
}

/// Options for the shared resolved-context shell execution path.
struct ShellExecutionOptions<'a> {
    shell: Option<ShellType>,
    command: Option<&'a str>,
    parent_environ: Option<HashMap<String, String>>,
    quiet: bool,
    inherited: bool,
    callback: Option<&'a RexExecutionCallback>,
    capture_output: bool,
}

/// Outcome of running the shared resolved-context shell path.
#[doc(hidden)]
pub enum ShellExecution {
    Status(ExitStatus),
    Output(Output),
}

/// Additional Rex code executed by a resolved context before a command.
#[derive(Debug, Clone)]
#[doc(hidden)]
pub struct RexExecutionCallback {
    #[doc(hidden)]
    pub package_name: String,
    #[doc(hidden)]
    pub name: String,
    #[doc(hidden)]
    pub code: String,
    #[doc(hidden)]
    pub package: Option<crate::package::Variant>,
    #[doc(hidden)]
    pub bindings: HashMap<String, serde_json::Value>,
    #[doc(hidden)]
    pub developer: bool,
}

// =============================================================================
// ResolveDiff - result of comparing two resolved contexts
// =============================================================================

/// Differences between two resolved contexts.
#[derive(Debug, Clone)]
pub struct ResolveDiff {
    /// Packages that are newer in 'other' compared to 'self'.
    pub newer: Vec<(String, String)>,
    /// Packages that are older in 'other' compared to 'self'.
    pub older: Vec<(String, String)>,
    /// Packages present in 'self' but not in 'other'.
    pub added: Vec<(String, String)>,
    /// Packages present in 'other' but not in 'self'.
    pub removed: Vec<(String, String)>,
}

impl ResolveDiff {
    /// Check if there are any differences.
    pub fn is_empty(&self) -> bool {
        self.newer.is_empty()
            && self.older.is_empty()
            && self.added.is_empty()
            && self.removed.is_empty()
    }
}

// =============================================================================
// Helpers
// =============================================================================

/// Simple ISO 8601 timestamp formatting (YYYY-MM-DDTHH:MM:SSZ).
/// Avoids external chrono dependency for tracking timestamps.
fn chrono_lite(epoch_secs: u64) -> String {
    const SECS_PER_DAY: u64 = 86400;
    const SECS_PER_HOUR: u64 = 3600;
    const SECS_PER_MIN: u64 = 60;

    // Calculate days since epoch (1970-01-01)
    let days = epoch_secs / SECS_PER_DAY;
    let rem_secs = epoch_secs % SECS_PER_DAY;

    let hours = rem_secs / SECS_PER_HOUR;
    let minutes = (rem_secs % SECS_PER_HOUR) / SECS_PER_MIN;
    let seconds = rem_secs % SECS_PER_MIN;

    // Convert days to year/month/day
    let mut year = 1970;
    let mut remaining_days = days;

    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if remaining_days < days_in_year {
            break;
        }
        remaining_days -= days_in_year;
        year += 1;
    }

    let month_days = if is_leap_year(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut month = 1;
    for &days_in_month in &month_days {
        if remaining_days < days_in_month {
            break;
        }
        remaining_days -= days_in_month;
        month += 1;
    }
    let day = remaining_days + 1;

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

#[allow(clippy::manual_is_multiple_of)]
fn is_leap_year(year: u64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

// =============================================================================
// ResolvedContext - the main user-facing resolved environment
// =============================================================================

pub type ContextTools = HashMap<String, (String, Vec<String>)>;
pub type ContextToolConflicts = HashMap<String, Vec<String>>;
pub type ContextToolSnapshot = (ContextTools, ContextToolConflicts);

/// A resolved package environment that can be saved, loaded, and executed.
///
/// This is the main Rez entry point for creating, saving, loading and executing
/// resolved environments. A ResolvedContext can be saved to file (`.rxt`) and
/// loaded at a later date, reconstructing the equivalent environment. It can
/// spawn interactive and non-interactive shells in any supported shell type.
#[derive(Debug)]
pub struct ResolvedContext {
    // -- Resolve settings --
    pub status: ResolverStatus,
    package_requests: Vec<Requirement>,
    pub implicit_packages: Vec<Requirement>,
    pub package_paths: Vec<PathBuf>,
    pub package_filter: Option<PackageFilterList>,
    pub package_orderers: Option<PackageOrderList>,
    pub variant_select_mode: VariantSelectMode,
    pub timestamp: u64,
    pub requested_timestamp: Option<u64>,
    pub building: bool,
    pub testing: bool,
    pub caching: bool,
    pub verbosity: u32,

    // -- Resolve results --
    resolved_packages: Option<Vec<ResolvedPackageInfo>>,
    resolved_ephemerals: Option<Vec<Requirement>>,
    pub failure_description: Option<String>,
    pub graph_string: Option<String>,
    pub from_cache: bool,

    // -- Metrics --
    pub solve_time: f64,
    pub load_time: f64,
    pub num_loaded_packages: i64,

    // -- Metadata (captured at creation) --
    pub rez_version: String,
    pub rez_path: String,
    pub user: String,
    pub host: String,
    pub platform_str: String,
    pub arch: String,
    pub os_str: String,
    pub created: u64,

    // -- Suite --
    pub parent_suite_path: Option<String>,
    pub suite_context_name: Option<String>,

    // -- Patch --
    pub default_patch_lock: PatchLock,
    pub patch_locks: HashMap<String, PatchLock>,

    // -- Execution settings --
    pub append_sys_path: bool,
    pub package_caching: bool,
    pub package_cache_async: bool,

    // -- Load info --
    pub load_path: Option<PathBuf>,
}

impl ResolvedContext {
    // =========================================================================
    // Construction
    // =========================================================================

    /// Create a new resolved context by running a resolve.
    ///
    /// This is the primary constructor. It creates a Resolver, runs it with the
    /// given provider, and stores the result.
    pub fn resolve(
        requests: Vec<Requirement>,
        provider: &dyn PackageProvider,
        mut opts: ResolveOptions,
    ) -> Result<Self> {
        let resolve_started = Instant::now();
        log_info!(
            "context",
            "Resolving context for {} request(s)",
            requests.len()
        );
        let now = epoch_secs();
        let timestamp = opts.timestamp.unwrap_or(now);
        let package_cache_async = opts
            .package_cache_async
            .unwrap_or(CONFIG.package_cache_async);
        let package_filter = opts.package_filter.clone();
        let package_orderers = opts.package_orderers.clone();
        let package_paths = opts.package_paths.unwrap_or_default();
        log_debug!(
            "context",
            "Package paths: {} entry(ies)",
            package_paths.len()
        );

        // Select configured or explicitly supplied implicits once for solving and provenance.
        let implicit_packages = if opts.add_implicit {
            match opts.implicit_packages.take() {
                Some(requirements) => requirements,
                None => CONFIG
                    .resolved_implicit_packages()
                    .iter()
                    .map(|request| {
                        Requirement::new(request).map_err(|error| {
                            RezError::Config(format!(
                                "invalid implicit_packages request {request:?}: {error}"
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
            }
        } else {
            Vec::new()
        };
        let mut all_requests = requests.clone();
        all_requests.extend(implicit_packages.iter().cloned());

        // Build and run resolver
        let mut resolver = Resolver::new(all_requests, package_paths.clone())
            .with_variant_mode(opts.variant_select_mode);
        if let Some(filter) = opts.package_filter.take() {
            resolver = resolver.with_filter(filter);
        }
        if let Some(orderers) = opts.package_orderers.take() {
            resolver = resolver.with_orderers(orderers);
        }
        resolver = resolver
            .with_building(opts.building)
            .with_caching(opts.caching);
        if let Some(ts) = opts.timestamp {
            resolver = resolver.with_timestamp(ts);
        }
        if opts.max_fails != -1 || opts.time_limit != -1 {
            let max_fails = opts.max_fails;
            let time_limit = opts.time_limit;
            resolver = resolver.with_callback(move |state: &SolverState| {
                if max_fails != -1 && i64::from(state.num_fails) >= i64::from(max_fails) {
                    return (
                        SolverCallbackReturn::Fail,
                        format!(
                            "fail limit reached: aborted after {} failures",
                            state.num_fails
                        ),
                    );
                }
                if time_limit != -1
                    && resolve_started.elapsed().as_secs_f64() > f64::from(time_limit)
                {
                    return (
                        SolverCallbackReturn::Abort,
                        "time limit exceeded".to_string(),
                    );
                }
                (SolverCallbackReturn::KeepGoing, String::new())
            });
        }

        resolver.resolve(provider)?;

        // Capture system info
        let host = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "unknown".to_string());

        let mut ctx = Self {
            status: resolver.status(),
            package_requests: requests,
            implicit_packages,
            package_paths,
            package_filter,
            package_orderers,
            variant_select_mode: opts.variant_select_mode,
            timestamp,
            requested_timestamp: opts.timestamp,
            building: opts.building,
            testing: opts.testing,
            caching: opts.caching,
            verbosity: opts.verbosity,

            resolved_packages: None,
            resolved_ephemerals: None,
            failure_description: resolver.failure_description().map(|s| s.to_string()),
            graph_string: resolver.graph_str().map(|s| s.to_string()),
            from_cache: resolver.from_cache(),

            solve_time: resolver.solve_time,
            load_time: resolver.load_time,
            num_loaded_packages: resolver.num_loaded_packages,

            rez_version: REZ_RS_VERSION.to_string(),
            rez_path: std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            user,
            host,
            platform_str: crate::platform::SYSTEM.platform.name().to_string(),
            arch: crate::platform::SYSTEM.arch.to_string(),
            os_str: crate::platform::SYSTEM.os.to_string(),
            created: now,

            parent_suite_path: None,
            suite_context_name: None,

            default_patch_lock: PatchLock::NoLock,
            patch_locks: HashMap::new(),

            append_sys_path: CONFIG.append_sys_path,
            package_caching: opts
                .package_caching
                .unwrap_or(!opts.building || CONFIG.package_cache_during_build),
            load_path: None,
            package_cache_async,
        };

        // Copy resolved packages if successful
        if resolver.status() == ResolverStatus::Solved {
            ctx.resolved_packages = resolver.resolved_packages().map(|pkgs| pkgs.to_vec());
            ctx.resolved_ephemerals = resolver.resolved_ephemerals().map(|e| e.to_vec());
            log_info!(
                "context",
                "Resolve succeeded: {} package(s)",
                ctx.resolved_packages.as_ref().map(|p| p.len()).unwrap_or(0)
            );
        }

        // Track context creation for analytics
        ctx.update_package_cache()?;
        ctx.track_context("created");

        Ok(ctx)
    }

    /// Create a ResolvedContext for building a package variant.
    ///
    /// Resolves build_requires + private_build_requires (and transitive) via
    /// `CONFIG.expanded_packages_path_os`. Used by `BuildProcess` when package has build deps.
    /// An explicit build path saves build.rxt; metadata queries pass None for no filesystem output.
    /// A supplied provider uses the same request/options path as ordinary filesystem builds.
    /// Queries may return a failed context to retain its complete request; only success is saved.
    pub fn create_build_context(
        package: &Package,
        variant_index: Option<usize>,
        build_path: Option<&Path>,
        provider: Option<&dyn PackageProvider>,
        package_paths: Option<Vec<PathBuf>>,
        require_success: bool,
    ) -> Result<(Self, Option<PathBuf>)> {
        use crate::package::Variant;

        let variant = match variant_index {
            Some(index) => Variant::new(package.clone(), index)?,
            None => Variant::from_package(package.clone()),
        };
        let requests = variant.build_request();
        let req_strs: Vec<String> = requests.iter().map(|r| r.to_string()).collect();
        eprintln!("Resolving build environment: {}", req_strs.join(" "));

        let package_paths = package_paths.unwrap_or_else(|| CONFIG.expanded_packages_path_os());
        let filesystem_provider;
        let provider = match provider {
            Some(provider) => provider,
            None => {
                filesystem_provider = FilesystemPackageProvider::from_paths(&package_paths)?;
                &filesystem_provider
            }
        };

        let opts = ResolveOptions {
            building: true,
            package_paths: Some(package_paths),
            add_implicit: true,
            ..ResolveOptions::default()
        };

        let ctx = Self::resolve(requests, provider, opts)?;
        if require_success && !ctx.success() {
            return Err(RezError::BuildContextResolve {
                message: ctx
                    .failure_description
                    .clone()
                    .unwrap_or_else(|| "Build environment resolution failed".into()),
                graph: ctx.graph_string.clone(),
            });
        }

        let rxt_path = build_path
            .filter(|_| ctx.success())
            .map(|path| path.join("build.rxt"));
        if let Some(path) = &rxt_path {
            ctx.save(path)?;
        }

        Ok((ctx, rxt_path))
    }

    /// Create a minimal/empty context (for testing or manual construction).
    pub fn empty() -> Self {
        let now = epoch_secs();
        Self {
            status: ResolverStatus::Pending,
            package_requests: Vec::new(),
            implicit_packages: Vec::new(),
            package_paths: Vec::new(),
            package_filter: None,
            package_orderers: None,
            variant_select_mode: CONFIG.variant_select_mode,
            timestamp: now,
            requested_timestamp: None,
            building: false,
            testing: false,
            caching: true,
            verbosity: 0,

            resolved_packages: None,
            resolved_ephemerals: None,
            failure_description: None,
            graph_string: None,
            from_cache: false,

            solve_time: 0.0,
            load_time: 0.0,
            num_loaded_packages: 0,

            rez_version: REZ_RS_VERSION.to_string(),
            rez_path: String::new(),
            user: String::new(),
            host: String::new(),
            platform_str: crate::platform::SYSTEM.platform.name().to_string(),
            arch: crate::platform::SYSTEM.arch.to_string(),
            os_str: crate::platform::SYSTEM.os.to_string(),
            created: now,

            parent_suite_path: None,
            suite_context_name: None,

            default_patch_lock: PatchLock::NoLock,
            patch_locks: HashMap::new(),

            append_sys_path: CONFIG.append_sys_path,
            package_caching: true,
            load_path: None,
            package_cache_async: CONFIG.package_cache_async,
        }
    }

    // =========================================================================
    // Accessors
    // =========================================================================

    /// True if the context resolved successfully.
    pub fn success(&self) -> bool {
        self.status == ResolverStatus::Solved
    }

    /// True if the resolve has a dependency graph.
    pub fn has_graph(&self) -> bool {
        self.graph_string.is_some()
    }

    /// Get the explicit package requests.
    pub fn package_requests(&self) -> &[Requirement] {
        &self.package_requests
    }

    /// Get all requested packages, optionally including implicit ones.
    pub fn requested_packages(&self, include_implicit: bool) -> Vec<Requirement> {
        let mut reqs = self.package_requests.clone();
        if include_implicit {
            reqs.extend(self.implicit_packages.iter().cloned());
        }
        reqs
    }

    /// Get resolved package list (None if not solved).
    pub fn resolved_packages(&self) -> Option<&[ResolvedPackageInfo]> {
        self.resolved_packages.as_deref()
    }

    /// Get resolved ephemerals (None if not solved).
    pub fn resolved_ephemerals(&self) -> Option<&[Requirement]> {
        self.resolved_ephemerals.as_deref()
    }

    /// Find a resolved package by name.
    pub fn get_resolved_package(&self, name: &str) -> Option<&ResolvedPackageInfo> {
        self.resolved_packages
            .as_ref()
            .and_then(|pkgs| pkgs.iter().find(|p| p.name == name))
    }

    /// Set the load path (for contexts loaded from disk).
    pub fn set_load_path(&mut self, path: impl Into<PathBuf>) {
        self.load_path = Some(path.into());
    }

    /// Set parent suite info (used by suites).
    pub fn set_parent_suite(&mut self, suite_path: &str, context_name: &str) {
        self.parent_suite_path = Some(suite_path.to_string());
        self.suite_context_name = Some(context_name.to_string());
    }

    // =========================================================================
    // Patching
    // =========================================================================

    /// Get a 'patched' request.
    ///
    /// A patched request is a copy of this context's request, with some changes
    /// applied. This can then be used to create a new, 'patched' context.
    ///
    /// New requests override originals only if their conflict/weak flags match.
    /// Requests prefixed with '^' act as subtractions (remove the named package).
    ///
    /// # Arguments
    /// * `package_requests` - Overriding requests as strings (e.g. "foo-2", "^bar", "~baz<3")
    /// * `package_subtractions` - Package names to remove from the base request
    /// * `strict` - If true, use resolved packages as the base instead of original requests
    /// * `rank` - If > 1, add weak upper-bound constraints so versions can only increase
    ///   in the given rank and further (e.g. rank=3 locks major+minor)
    pub fn get_patched_request(
        &self,
        package_requests: Option<Vec<String>>,
        package_subtractions: Option<Vec<String>>,
        strict: bool,
        rank: usize,
    ) -> Vec<Requirement> {
        // Assemble base request
        let mut request: Vec<Option<Requirement>> = if strict {
            // Use resolved packages as exact requirements
            self.resolved_packages
                .as_ref()
                .map(|pkgs| {
                    pkgs.iter()
                        .filter_map(|p| Requirement::new(&p.qualified_name()).ok())
                        .map(Some)
                        .collect()
                })
                .unwrap_or_default()
        } else {
            // Use original explicit requests
            self.requested_packages(false)
                .into_iter()
                .map(Some)
                .collect()
        };

        let mut package_requests = package_requests.unwrap_or_default();
        let mut subtractions = package_subtractions.unwrap_or_default();

        // Convert '^foo'-style requests to subtractions
        let mut remove_idxs = Vec::new();
        for (i, req_str) in package_requests.iter().enumerate() {
            if let Some(rest) = req_str.strip_prefix('^') {
                if let Ok(req) = Requirement::new(rest) {
                    subtractions.push(req.name().to_string());
                }
                remove_idxs.push(i);
            }
        }
        for i in remove_idxs.into_iter().rev() {
            package_requests.remove(i);
        }

        // Apply subtractions
        if !subtractions.is_empty() {
            for entry in &mut request {
                if let Some(req) = entry {
                    if subtractions.iter().any(|s| s == req.name()) {
                        *entry = None;
                    }
                }
            }
        }

        // Apply overrides: replace matching (name + conflict + weak), append rest
        if !package_requests.is_empty() {
            // Build index: name -> first position in request
            let mut name_idx: HashMap<String, usize> = HashMap::new();
            for (i, entry) in request.iter().enumerate() {
                if let Some(req) = entry {
                    name_idx.entry(req.name().to_string()).or_insert(i);
                }
            }

            let mut appended = Vec::new();
            for req_str in &package_requests {
                let Ok(new_req) = Requirement::new(req_str) else {
                    continue;
                };
                let name = new_req.name().to_string();

                if let Some(&idx) = name_idx.get(&name) {
                    if let Some(old_req) = &request[idx] {
                        // Replace only if conflict+weak flags match
                        if old_req.conflict() == new_req.conflict()
                            && old_req.weak() == new_req.weak()
                        {
                            request[idx] = Some(new_req);
                            name_idx.remove(&name);
                        } else {
                            appended.push(new_req);
                        }
                    } else {
                        appended.push(new_req);
                    }
                } else {
                    appended.push(new_req);
                }
            }

            // Collect non-None + appended
            let mut result: Vec<Requirement> = request.into_iter().flatten().collect();
            result.extend(appended);
            request = result.into_iter().map(Some).collect();
        }

        // Add rank limiters as weak upper-bound constraints
        if !strict && rank > 1 {
            let override_names: std::collections::HashSet<String> = package_requests
                .iter()
                .filter_map(|s| Requirement::new(s).ok())
                .filter(|r| !r.conflict())
                .map(|r| r.name().to_string())
                .collect();

            if let Some(ref pkgs) = self.resolved_packages {
                for pkg in pkgs {
                    if override_names.contains(&pkg.name) {
                        continue;
                    }
                    if pkg.version.len() >= rank {
                        let trimmed = pkg.version.trim(rank - 1);
                        let upper = trimmed.next();
                        let limiter = format!("~{}<{}", pkg.name, upper);
                        if let Ok(req) = Requirement::new(&limiter) {
                            request.push(Some(req));
                        }
                    }
                }
            }
        }

        request.into_iter().flatten().collect()
    }

    /// Get package request list with patching applied (simple version for CLI).
    ///
    /// Takes the original resolved requests and applies patch modifications:
    /// - Existing packages can have their version range replaced or intersected
    /// - New packages can be added to the request list
    ///
    /// # Arguments
    /// * `patch_requests` - List of requirements to apply as patches
    /// * `strict` - If true, patch ranges are intersected with originals
    /// * `_rank` - Unused for now (kept for API compatibility)
    ///
    /// # Returns
    /// * `Ok(Vec<Requirement>)` - Patched request list
    /// * `Err(RezError)` - If patch conflicts with original (strict mode only)
    pub fn get_patched_request_simple(
        &self,
        patch_requests: &[Requirement],
        strict: bool,
        _rank: u32,
    ) -> Result<Vec<Requirement>> {
        let orig = self.package_requests();

        if patch_requests.is_empty() {
            return Ok(orig.to_vec());
        }

        let mut patched: Vec<Requirement> = orig.to_vec();

        for patch_req in patch_requests {
            let name = patch_req.name();

            // Find if this package already exists in the original requests
            if let Some(idx) = patched.iter().position(|r| r.name() == name) {
                if strict {
                    // Intersect ranges
                    let orig_range = patched[idx].range();
                    let patch_range = patch_req.range();

                    match (orig_range, patch_range) {
                        (Some(orig_r), Some(patch_r)) => {
                            let intersected = orig_r.intersection(patch_r);
                            if let Some(new_range) = intersected {
                                patched[idx] = Requirement::construct(name, Some(new_range));
                            } else {
                                return Err(RezError::Resolve(format!(
                                    "Patch request '{}' conflicts with original '{}'",
                                    patch_req, patched[idx]
                                )));
                            }
                        }
                        _ => {
                            patched[idx] = patch_req.clone();
                        }
                    }
                } else {
                    patched[idx] = patch_req.clone();
                }
            } else {
                // Add new package
                patched.push(patch_req.clone());
            }
        }

        Ok(patched)
    }

    // =========================================================================
    // Serialization (JSON)
    // =========================================================================

    /// Serialize context to a JSON value.
    pub fn to_json(&self) -> Result<serde_json::Value> {
        let resolved_pkgs: Vec<serde_json::Value> = self
            .resolved_packages
            .as_ref()
            .map(|pkgs| {
                pkgs.iter()
                    .map(|package| {
                        let handle = package.resource_handle.as_ref().ok_or_else(|| {
                            RezError::ResolvedContext(format!(
                                "cannot serialize {} without repository provenance",
                                package.qualified_name()
                            ))
                        })?;
                        serde_json::to_value(handle).map_err(|error| {
                            RezError::ResolvedContext(format!(
                                "failed to serialize handle for {}: {}",
                                package.qualified_name(),
                                error
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();

        let resolved_ephs: Vec<String> = self
            .resolved_ephemerals
            .as_ref()
            .map(|e| e.iter().map(|r| r.to_string()).collect())
            .unwrap_or_default();

        let patch_locks: HashMap<String, String> = self
            .patch_locks
            .iter()
            .map(|(k, v)| (k.clone(), v.to_string()))
            .collect();

        let package_filter = self
            .package_filter
            .as_ref()
            .map(PackageFilterList::to_pod)
            .unwrap_or_else(|| serde_json::json!([]));
        let package_orderers = self
            .package_orderers
            .as_ref()
            .map(PackageOrderList::to_pod)
            .transpose()?;
        let doc = serde_json::json!({
            "serialize_version": format!("{}.{}", CONTEXT_SERIALIZE_VERSION.0, CONTEXT_SERIALIZE_VERSION.1),

            "timestamp": self.timestamp,
            "requested_timestamp": self.requested_timestamp,
            "package_filter": package_filter,
            "package_orderers": package_orderers,
            "building": self.building,
            "testing": self.testing,
            "caching": self.caching,
            "verbosity": self.verbosity,
            "variant_select_mode": match self.variant_select_mode {
                VariantSelectMode::VersionPriority => "version_priority",
                VariantSelectMode::IntersectionPriority => "intersection_priority",
            },
            "implicit_packages": self.implicit_packages.iter().map(|r| r.to_string()).collect::<Vec<_>>(),
            "package_requests": self.package_requests.iter().map(|r| r.to_string()).collect::<Vec<_>>(),
            "package_paths": self.package_paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),

            "append_sys_path": self.append_sys_path,
            "package_caching": self.package_caching,
            "package_cache_async": self.package_cache_async,

            "default_patch_lock": self.default_patch_lock.to_string(),
            "patch_locks": patch_locks,

            "rez_version": self.rez_version,
            "rez_path": self.rez_path,
            "user": self.user,
            "host": self.host,
            "platform": self.platform_str,
            "arch": self.arch,
            "os": self.os_str,
            "created": self.created,

            "parent_suite_path": self.parent_suite_path,
            "suite_context_name": self.suite_context_name,

            "status": self.status.to_string(),
            "failure_description": self.failure_description,
            "graph": self.graph_string,

            "from_cache": self.from_cache,
            "solve_time": self.solve_time,
            "load_time": self.load_time,
            "num_loaded_packages": self.num_loaded_packages,

            "resolved_packages": resolved_pkgs,
            "resolved_ephemerals": resolved_ephs,
        });

        Ok(doc)
    }

    /// Deserialize context from a JSON value.
    pub fn from_json(doc: &serde_json::Value, options: Option<&ResolveOptions>) -> Result<Self> {
        let serialize_version = doc
            .get("serialize_version")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                RezError::ResolvedContext("missing or invalid serialize_version".into())
            })?;
        let parsed_version = serialize_version
            .split('.')
            .map(str::parse::<u32>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|error| {
                RezError::ResolvedContext(format!(
                    "invalid serialize_version {:?}: {}",
                    serialize_version, error
                ))
            })?;
        if parsed_version.len() != 2 {
            return Err(RezError::ResolvedContext(format!(
                "invalid serialize_version {:?}: expected major.minor",
                serialize_version
            )));
        }
        if parsed_version
            .as_slice()
            .cmp(&[CONTEXT_SERIALIZE_VERSION.0, CONTEXT_SERIALIZE_VERSION.1])
            == std::cmp::Ordering::Greater
        {
            return Err(RezError::ResolvedContext(format!(
                "unsupported serialize_version {:?}; this build supports up to {}.{}",
                serialize_version, CONTEXT_SERIALIZE_VERSION.0, CONTEXT_SERIALIZE_VERSION.1
            )));
        }
        if parsed_version.as_slice().cmp(&[4, 0]) == std::cmp::Ordering::Less {
            return Err(RezError::ResolvedContext(format!(
                "unsupported legacy serialize_version {:?}: pre-4.0 resource-handle migration requires path-aware loading",
                serialize_version
            )));
        }

        let value = |key: &str| -> Result<&serde_json::Value> {
            doc.get(key)
                .ok_or_else(|| RezError::ResolvedContext(format!("missing required field {key:?}")))
        };
        let string = |key: &str| -> Result<String> {
            value(key)?
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| RezError::ResolvedContext(format!("{key} must be a string")))
        };
        let nullable_string = |key: &str| -> Result<Option<String>> {
            match value(key)? {
                serde_json::Value::Null => Ok(None),
                serde_json::Value::String(text) => Ok(Some(text.clone())),
                _ => Err(RezError::ResolvedContext(format!(
                    "{key} must be a string or null"
                ))),
            }
        };
        let unsigned = |key: &str| -> Result<u64> {
            value(key)?.as_u64().ok_or_else(|| {
                RezError::ResolvedContext(format!("{key} must be a non-negative integer"))
            })
        };
        let number = |key: &str| -> Result<f64> {
            value(key)?
                .as_f64()
                .filter(|number| number.is_finite())
                .ok_or_else(|| RezError::ResolvedContext(format!("{key} must be a finite number")))
        };
        let boolean = |key: &str| -> Result<bool> {
            value(key)?
                .as_bool()
                .ok_or_else(|| RezError::ResolvedContext(format!("{key} must be a boolean")))
        };
        let optional_boolean = |key: &str, default: bool| -> Result<bool> {
            match doc.get(key) {
                None => Ok(default),
                Some(serde_json::Value::Bool(value)) => Ok(*value),
                Some(_) => Err(RezError::ResolvedContext(format!(
                    "{key} must be a boolean"
                ))),
            }
        };
        let optional_unsigned = |key: &str, default: u64| -> Result<u64> {
            match doc.get(key) {
                None => Ok(default),
                Some(value) => value.as_u64().ok_or_else(|| {
                    RezError::ResolvedContext(format!("{key} must be a non-negative integer"))
                }),
            }
        };
        let optional_signed = |key: &str, default: i64| -> Result<i64> {
            match doc.get(key) {
                None => Ok(default),
                Some(value) => value.as_i64().ok_or_else(|| {
                    RezError::ResolvedContext(format!("{key} must be a signed integer"))
                }),
            }
        };

        let status = match string("status")?.as_str() {
            "pending" => ResolverStatus::Pending,
            "solved" => ResolverStatus::Solved,
            "failed" => ResolverStatus::Failed,
            "aborted" => ResolverStatus::Aborted,
            other => {
                return Err(RezError::ResolvedContext(format!(
                    "status has unknown value {other:?}"
                )));
            }
        };

        let parse_reqs = |key: &str, required: bool| -> Result<Vec<Requirement>> {
            let Some(raw) = doc.get(key) else {
                return if required {
                    Err(RezError::ResolvedContext(format!(
                        "missing required field {key:?}"
                    )))
                } else {
                    Ok(Vec::new())
                };
            };
            let items = raw.as_array().ok_or_else(|| {
                RezError::ResolvedContext(format!("{key} must be an array of requirement strings"))
            })?;
            items
                .iter()
                .enumerate()
                .map(|(index, raw)| {
                    let text = raw.as_str().ok_or_else(|| {
                        RezError::ResolvedContext(format!("{key}[{index}] must be a string"))
                    })?;
                    Requirement::new(text).map_err(|error| {
                        RezError::ResolvedContext(format!(
                            "{key}[{index}] has invalid requirement {text:?}: {error}"
                        ))
                    })
                })
                .collect()
        };

        let variant_select_mode = match doc.get("variant_select_mode") {
            None => CONFIG.variant_select_mode,
            Some(serde_json::Value::String(mode)) => match mode.as_str() {
                "version_priority" => VariantSelectMode::VersionPriority,
                "intersection_priority" => VariantSelectMode::IntersectionPriority,
                other => {
                    return Err(RezError::ResolvedContext(format!(
                        "variant_select_mode has unknown value {other:?}"
                    )));
                }
            },
            Some(_) => {
                return Err(RezError::ResolvedContext(
                    "variant_select_mode must be a string".into(),
                ));
            }
        };
        let package_filter = match doc.get("package_filter") {
            None => PackageFilterList::new(),
            Some(value) => PackageFilterList::from_pod(value).map_err(|error| {
                RezError::ResolvedContext(format!("package_filter is invalid: {error}"))
            })?,
        };
        let package_orderers = match doc.get("package_orderers") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::Array(values)) if values.is_empty() => None,
            Some(serde_json::Value::Array(values)) => {
                let pods = values
                    .iter()
                    .enumerate()
                    .map(|(index, value)| {
                        serde_json::from_value::<PackageOrderPod>(value.clone()).map_err(|error| {
                            RezError::ResolvedContext(format!(
                                "package_orderers[{index}] is invalid: {error}"
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                Some(PackageOrderList::from_pod(pods).map_err(|error| {
                    RezError::ResolvedContext(format!("package_orderers are invalid: {error}"))
                })?)
            }
            Some(_) => {
                return Err(RezError::ResolvedContext(
                    "package_orderers must be an array or null".into(),
                ));
            }
        };
        let package_requests = parse_reqs("package_requests", true)?;
        let implicit_packages = parse_reqs("implicit_packages", true)?;
        let resolved_ephemerals = parse_reqs("resolved_ephemerals", false)?;

        let package_paths = value("package_paths")?
            .as_array()
            .ok_or_else(|| {
                RezError::ResolvedContext("package_paths must be an array of strings".into())
            })?
            .iter()
            .enumerate()
            .map(|(index, raw)| {
                raw.as_str().map(PathBuf::from).ok_or_else(|| {
                    RezError::ResolvedContext(format!("package_paths[{index}] must be a string"))
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let parse_patch_lock = |field: &str, value: &str| -> Result<PatchLock> {
            match value {
                "no_lock" => Ok(PatchLock::NoLock),
                "lock_2" => Ok(PatchLock::Lock2),
                "lock_3" => Ok(PatchLock::Lock3),
                "lock_4" => Ok(PatchLock::Lock4),
                "lock" => Ok(PatchLock::Lock),
                other => Err(RezError::ResolvedContext(format!(
                    "{field} has unknown patch lock {other:?}"
                ))),
            }
        };
        let default_patch_lock = match doc.get("default_patch_lock") {
            None => PatchLock::NoLock,
            Some(serde_json::Value::String(value)) => {
                parse_patch_lock("default_patch_lock", value)?
            }
            Some(_) => {
                return Err(RezError::ResolvedContext(
                    "default_patch_lock must be a string".into(),
                ));
            }
        };
        let patch_locks = match doc.get("patch_locks") {
            None => HashMap::new(),
            Some(serde_json::Value::Object(object)) => object
                .iter()
                .map(|(package, raw_lock)| {
                    let value = raw_lock.as_str().ok_or_else(|| {
                        RezError::ResolvedContext(format!(
                            "patch_locks[{package:?}] must be a string"
                        ))
                    })?;
                    Ok((
                        package.clone(),
                        parse_patch_lock(&format!("patch_locks[{package:?}]"), value)?,
                    ))
                })
                .collect::<Result<HashMap<_, _>>>()?,
            Some(_) => {
                return Err(RezError::ResolvedContext(
                    "patch_locks must be an object of patch lock strings".into(),
                ));
            }
        };

        let requested_timestamp = match doc.get("requested_timestamp") {
            None => Some(0),
            Some(serde_json::Value::Null) => None,
            Some(value) => Some(value.as_u64().ok_or_else(|| {
                RezError::ResolvedContext(
                    "requested_timestamp must be a non-negative integer or null".into(),
                )
            })?),
        };
        let testing = optional_boolean("testing", false)?;
        let caching = boolean("caching")?;
        let verbosity = optional_unsigned("verbosity", 0)?;
        let from_cache = optional_boolean("from_cache", false)?;
        let solve_time = number("solve_time")?;
        let load_time = number("load_time")?;
        let num_loaded_packages = optional_signed("num_loaded_packages", -1)?;
        if num_loaded_packages < -1 {
            return Err(RezError::ResolvedContext(
                "num_loaded_packages must be -1 or non-negative".into(),
            ));
        }
        let rez_version = string("rez_version")?;
        let rez_path = string("rez_path")?;
        let user = string("user")?;
        let host = string("host")?;
        let platform_str = string("platform")?;
        let arch = string("arch")?;
        let os_str = string("os")?;
        let timestamp = unsigned("timestamp")?;
        let created = unsigned("created")?;
        let failure_description = nullable_string("failure_description")?;
        let graph_string = nullable_string("graph")?;
        let parent_suite_path = match doc.get("parent_suite_path") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(value)) => Some(value.clone()),
            Some(_) => {
                return Err(RezError::ResolvedContext(
                    "parent_suite_path must be a string or null".into(),
                ));
            }
        };
        let suite_context_name = match doc.get("suite_context_name") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(value)) => Some(value.clone()),
            Some(_) => {
                return Err(RezError::ResolvedContext(
                    "suite_context_name must be a string or null".into(),
                ));
            }
        };

        // Resolve Rez handles only after validating the complete context schema.
        let package_handles = value("resolved_packages")?.as_array().ok_or_else(|| {
            RezError::ResolvedContext("resolved_packages must be an array".into())
        })?;
        let provider = FilesystemPackageProvider::from_paths(&[])?;
        let building = boolean("building")?;
        let resolved_packages = Some(
            package_handles
                .iter()
                .enumerate()
                .map(|(index, raw_handle)| -> Result<ResolvedPackageInfo> {
                    let handle = ResourceHandle::from_json(raw_handle, Some(&format!("resolved_packages[{index}]")))?;
                    let candidate = provider
                        .get_candidate_for_handle(&handle)
                        .map_err(|error| {
                            RezError::ResolvedContext(format!(
                                "resolved_packages[{index}] handle {:?} could not be resolved: {error}",
                                handle.key
                            ))
                        })?;
                    let variant = candidate.into_variant(handle.variables.index, building).map_err(|error| {
                        RezError::ResolvedContext(format!("resolved_packages[{index}].variables.index is invalid: {error}"))
                    })?;
                    let resolved = ResolvedPackageInfo::from_solver_variant(&variant);
                    if resolved.resource_handle.as_ref() != Some(&handle) {
                        return Err(RezError::ResolvedContext(format!(
                            "resolved_packages[{index}] handle does not match the selected package source"
                        )));
                    }
                    Ok(resolved)
                })
                .collect::<Result<Vec<_>>>()?,
        );

        let mut context = Self {
            status,
            package_requests,
            implicit_packages,
            package_paths,
            package_filter: Some(package_filter),
            package_orderers,
            variant_select_mode,
            timestamp,
            requested_timestamp,
            building,
            testing,
            caching,
            verbosity: u32::try_from(verbosity).map_err(|_| {
                RezError::ResolvedContext("verbosity exceeds the supported integer range".into())
            })?,

            resolved_packages,
            resolved_ephemerals: if resolved_ephemerals.is_empty() {
                None
            } else {
                Some(resolved_ephemerals)
            },
            failure_description,
            graph_string,
            from_cache,

            solve_time,
            load_time,
            num_loaded_packages,

            rez_version,
            rez_path,
            user,
            host,
            platform_str,
            arch,
            os_str,
            created,

            parent_suite_path,
            suite_context_name,

            default_patch_lock,
            patch_locks,

            append_sys_path: optional_boolean("append_sys_path", true)?,
            package_caching: optional_boolean("package_caching", true)?,
            package_cache_async: optional_boolean("package_cache_async", true)?,
            load_path: None,
        };
        if let Some(options) = options {
            if let Some(enabled) = options.package_caching {
                context.package_caching = enabled;
            }
            if let Some(asynchronous) = options.package_cache_async {
                context.package_cache_async = asynchronous;
            }
        }
        context.update_package_cache()?;
        Ok(context)
    }

    /// Open the configured payload cache without creating an absent configured root.
    fn package_cache(
        &self,
        config: &crate::config::RezConfig,
    ) -> Result<Option<crate::package::cache::PackageCache>> {
        if !self.package_caching {
            return Ok(None);
        }
        let Some(path) = config.cache_packages_path.as_deref() else {
            return Ok(None);
        };
        match crate::package::cache::PackageCache::new(
            crate::config::RezConfig::expand_path(path).to_os(),
        ) {
            Ok(cache) => Ok(Some(cache)),
            Err(RezError::PackageCache(error)) => {
                eprintln!("Package caching disabled: {error}");
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn update_package_cache(&self) -> Result<()> {
        if self.status != ResolverStatus::Solved
            || !self.package_caching
            || !CONFIG.write_package_cache
            || CONFIG.cache_packages_path.is_none()
        {
            return Ok(());
        }
        if crate::platform::SystemInfo::rez_bin_path(&std::env::current_exe()?)?.is_none() {
            return Ok(());
        }
        let Some(cache) = self.package_cache(&CONFIG)? else {
            return Ok(());
        };
        let provider = FilesystemPackageProvider::from_paths(&[])?;
        let variants = self
            .resolved_packages
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .filter_map(|package| package.resource_handle.as_ref())
            .map(|handle| {
                provider
                    .get_candidate_for_handle(handle)?
                    .into_variant(handle.variables.index, self.building)
            })
            .collect::<Result<Vec<PackageVariant>>>()?;
        cache.add_variants(&variants, self.package_cache_async, None)
    }

    /// Cache roots are execution bindings only; serialized handles and source bases remain original.
    fn execution_packages(
        &self,
        config: Option<&crate::config::RezConfig>,
    ) -> Result<Vec<ResolvedPackageInfo>> {
        let config = config.unwrap_or(&CONFIG);
        let mut packages = self.resolved_packages.clone().unwrap_or_default();
        if !config.read_package_cache {
            return Ok(packages);
        }
        let Some(cache) = self.package_cache(config)? else {
            return Ok(packages);
        };
        let provider = FilesystemPackageProvider::from_paths(&[])?;
        for package in &mut packages {
            let Some(handle) = &package.resource_handle else {
                continue;
            };
            let variant = provider
                .get_candidate_for_handle(handle)?
                .into_variant(handle.variables.index, self.building)?;
            let handle = crate::package::cache::VariantHandle::from_variant(&variant)?;
            if let Some(root) = cache.touch_cached(&handle)? {
                package.root = Some(root);
            }
        }
        Ok(packages)
    }

    // =========================================================================
    // File I/O
    // =========================================================================

    /// Save context to a JSON file (.rxt).
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut doc = self.to_json()?;
        Self::adjust_locations(&mut doc, path, true)?;
        let content = serde_json::to_string_pretty(&doc)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Load context from a JSON or YAML file (.rxt).
    pub fn load(path: &Path, options: Option<&ResolveOptions>) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let mut doc: serde_json::Value = if content.starts_with('{') {
            serde_json::from_str(&content).map_err(|error| {
                RezError::ResolvedContext(format!(
                    "failed to parse context {}: {error}",
                    path.display()
                ))
            })?
        } else {
            serde_yaml::from_str(&content).map_err(|error| {
                RezError::ResolvedContext(format!(
                    "failed to parse context {}: {error}",
                    path.display()
                ))
            })?
        };
        Self::adjust_locations(&mut doc, path, false).map_err(|error| {
            RezError::ResolvedContext(format!("failed to load {}: {error}", path.display()))
        })?;
        let mut ctx = Self::from_json(&doc, options).map_err(|error| {
            RezError::ResolvedContext(format!("failed to load {}: {error}", path.display()))
        })?;
        ctx.load_path = Some(path.to_path_buf());

        // Track context load for analytics
        ctx.track_context("sourced");

        Ok(ctx)
    }

    /// Apply bundle serialization policy without changing historical search paths.
    fn adjust_locations(doc: &mut serde_json::Value, context_path: &Path, out: bool) -> Result<()> {
        let absolute = std::path::absolute(context_path)?;
        let parent = absolute.parent().ok_or_else(|| {
            RezError::ResolvedContext("context path has no parent directory".into())
        })?;
        if !parent.join("bundle.yaml").try_exists()? {
            return Ok(());
        }
        let bundle_path = parent.canonicalize()?;
        let handles = doc
            .get_mut("resolved_packages")
            .and_then(serde_json::Value::as_array_mut)
            .ok_or_else(|| {
                RezError::ResolvedContext("resolved_packages must be an array".into())
            })?;
        for (index, raw_handle) in handles.iter_mut().enumerate() {
            let mut handle = ResourceHandle::from_json(
                raw_handle,
                Some(&format!("resolved_packages[{index}]")),
            )?;
            handle.adjust_location(&bundle_path, out).map_err(|error| {
                RezError::ResolvedContext(format!(
                    "resolved_packages[{index}] repository location: {error}"
                ))
            })?;
            raw_handle["variables"]["location"] = serde_json::json!(handle.variables.location);
        }
        Ok(())
    }

    /// Get the context for the current env (from REZ_RXT_FILE), if any.
    pub fn get_current(options: Option<&ResolveOptions>) -> Option<Result<Self>> {
        let filepath = std::env::var("REZ_RXT_FILE").ok()?;
        let path = Path::new(&filepath);
        if !path.exists() {
            return None;
        }
        Some(Self::load(path, options))
    }

    /// True if this is the currently sourced context.
    pub fn is_current(&self) -> bool {
        let Some(load_path) = &self.load_path else {
            return false;
        };
        let Ok(filepath) = std::env::var("REZ_RXT_FILE") else {
            return false;
        };
        let rxt_path = Path::new(&filepath);
        rxt_path.exists() && load_path == rxt_path
    }

    // =========================================================================
    // Execution helpers (require success)
    // =========================================================================

    /// Guard: return error if context is not successfully solved.
    fn require_success(&self) -> Result<()> {
        if self.status == ResolverStatus::Solved {
            Ok(())
        } else {
            Err(RezError::ResolvedContext(
                "Cannot perform operation in a failed context".to_string(),
            ))
        }
    }

    /// Get the environment dict resulting from interpreting this context.
    ///
    /// Returns only the overrides (vars set by rex), not merged with parent.
    /// Matches Python rez: when parent is None, target_environ starts empty.
    /// Uses a PythonInterpreter to capture env var state without spawning a shell.
    pub fn get_environ(
        &self,
        parent_environ: Option<HashMap<String, String>>,
    ) -> Result<HashMap<String, String>> {
        self.get_environ_with_callback(parent_environ, None)
    }

    #[doc(hidden)]
    pub fn get_environ_with_callback(
        &self,
        parent_environ: Option<HashMap<String, String>>,
        callback: Option<&RexExecutionCallback>,
    ) -> Result<HashMap<String, String>> {
        self.require_success()?;
        let interp = PythonInterpreter::new();
        let mut executor = RexExecutor::new(interp, parent_environ, false);
        self.execute_rex(&mut executor, callback)?;
        Ok(executor.env().clone())
    }

    /// Get the list of actions resulting from interpreting this context.
    pub fn get_actions(
        &self,
        parent_environ: Option<HashMap<String, String>>,
    ) -> Result<Vec<Action>> {
        self.require_success()?;
        let interp = PythonInterpreter::new();
        let mut executor = RexExecutor::new(interp, parent_environ, false);
        self.execute_rex(&mut executor, None)?;
        Ok(executor.actions().to_vec())
    }

    /// Get shell code resulting from interpreting this context.
    pub fn get_shell_code(
        &self,
        shell: Option<ShellType>,
        parent_environ: Option<HashMap<String, String>>,
    ) -> Result<String> {
        self.require_success()?;
        let shell_type = shell.unwrap_or({
            if cfg!(windows) {
                ShellType::Cmd
            } else {
                ShellType::Bash
            }
        });
        let sh = create_shell(shell_type);
        let mut executor = RexExecutor::new(sh, parent_environ, true);

        // Set RXT file path if available
        if let Some(ref lp) = self.load_path {
            if lp.is_file() {
                executor.setenv("REZ_RXT_FILE", lp.display().to_string());
            }
        }

        self.execute_rex(&mut executor, None)?;
        let shell: &dyn Shell = executor.interpreter().as_ref();
        Ok(Shell::get_output(shell, OutputStyle::File))
    }

    /// Run a command within the resolved environment (subprocess, not a shell).
    ///
    /// This applies the context to an environ dict, then runs the command in
    /// that namespace. Shell-specific commands (aliases) are NOT available.
    pub fn execute_command(
        &self,
        args: &[&str],
        parent_environ: Option<HashMap<String, String>>,
    ) -> Result<std::process::Output> {
        self.execute_command_with_callback(args, parent_environ, None)
    }

    #[doc(hidden)]
    pub fn execute_command_with_callback(
        &self,
        args: &[&str],
        parent_environ: Option<HashMap<String, String>>,
        callback: Option<&RexExecutionCallback>,
    ) -> Result<std::process::Output> {
        self.require_success()?;
        let env = self.get_environ_with_callback(parent_environ, callback)?;

        if args.is_empty() {
            return Err(RezError::ResolvedContext(
                "No command specified".to_string(),
            ));
        }

        let output = Command::new(args[0]).args(&args[1..]).envs(&env).output()?;

        Ok(output)
    }

    /// Spawn an interactive or non-interactive shell with the resolved env.
    ///
    /// If `command` is None, an interactive shell is opened.
    /// If `command` is Some, the command is run in a non-interactive shell.
    /// Explicit parent environments are used as supplied. Without one, clean-shell
    /// policy takes precedence; otherwise `inherited` selects the host or empty baseline.
    pub fn execute_shell(
        &self,
        shell: Option<ShellType>,
        command: Option<&str>,
        parent_environ: Option<HashMap<String, String>>,
        quiet: bool,
        inherited: bool,
    ) -> Result<ExitStatus> {
        match self.execute_shell_internal(ShellExecutionOptions {
            shell,
            command,
            parent_environ,
            quiet,
            inherited,
            callback: None,
            capture_output: false,
        })? {
            ShellExecution::Status(status) => Ok(status),
            ShellExecution::Output(output) => Ok(output.status),
        }
    }

    #[doc(hidden)]
    pub fn execute_shell_output(
        &self,
        shell: Option<ShellType>,
        command: &str,
        parent_environ: Option<HashMap<String, String>>,
        quiet: bool,
        inherited: bool,
        callback: Option<&RexExecutionCallback>,
    ) -> Result<Output> {
        match self.execute_shell_internal(ShellExecutionOptions {
            shell,
            command: Some(command),
            parent_environ,
            quiet,
            inherited,
            callback,
            capture_output: true,
        })? {
            ShellExecution::Output(output) => Ok(output),
            ShellExecution::Status(_) => {
                unreachable!("captured shell execution must return output")
            }
        }
    }

    fn execute_shell_internal(&self, options: ShellExecutionOptions<'_>) -> Result<ShellExecution> {
        let ShellExecutionOptions {
            shell,
            command,
            parent_environ,
            quiet,
            inherited,
            callback,
            capture_output,
        } = options;
        self.require_success()?;

        let shell_type = shell.unwrap_or_else(crate::shell::types::detect_shell);
        let sh = create_shell(shell_type);

        // Capture one parent baseline and use it for both Rex evaluation and the child.
        // Explicit environments win; clean-shell policy applies only when none is supplied.
        let host_parent = std::env::vars().collect::<HashMap<_, _>>();
        let parent = parent_environ.unwrap_or_else(|| {
            if CONFIG.clean_shell_environment {
                crate::environment::clean_environ(
                    crate::platform::Platform::current(),
                    &host_parent,
                    &CONFIG.parent_variables,
                )
            } else if inherited {
                host_parent
            } else {
                HashMap::new()
            }
        });
        let mut executor = RexExecutor::new(sh, Some(parent.clone()), true);

        // Set REZ_RXT_FILE if we have a load path
        if let Some(ref lp) = self.load_path {
            if lp.is_file() {
                executor.setenv("REZ_RXT_FILE", lp.display().to_string());
            }
        }

        // Set REZ_CONTEXT_FILE (path to context .rxt file)
        if let Some(ref lp) = self.load_path {
            executor.setenv("REZ_CONTEXT_FILE", lp.display().to_string());
        }

        // Set REZ_SHELL_INIT_TIMESTAMP
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        executor.setenv("REZ_SHELL_INIT_TIMESTAMP", ts.to_string());

        // REZ_SHELL_INTERACTIVE: "1" for interactive, "0" for command
        executor.setenv(
            "REZ_SHELL_INTERACTIVE",
            if command.is_none() { "1" } else { "0" },
        );

        self.execute_rex(&mut executor, callback)?;

        // Write context script to temp file
        let sh_ref: &dyn Shell = executor.interpreter().as_ref();
        let context_code = Shell::get_output(sh_ref, OutputStyle::File);
        let tempdir = tempfile::Builder::new().prefix("rez_context-").tempdir()?;
        let tmpdir = tempdir.path();

        let ext = shell_type.file_ext();
        let context_file = tmpdir.join(format!("context.{ext}"));
        std::fs::write(&context_file, &context_code)?;

        // Resolve the shell against the parent process before installing the
        // child environment. On Unix, Command searches the child's PATH, which
        // can intentionally be empty or package-controlled after env_clear.
        let shell_executable = Path::new(shell_type.executable());
        let shell_executable = if shell_executable.is_absolute() {
            if !shell_executable.is_file() {
                return Err(RezError::System(format!(
                    "Shell executable '{}' is not a file",
                    shell_executable.display()
                )));
            }
            shell_executable.to_path_buf()
        } else {
            PathBuf::from(
                crate::shell::types::find_executable(shell_type.executable(), None).ok_or_else(
                    || {
                        RezError::System(format!(
                            "Could not find shell executable '{}' on the parent PATH",
                            shell_type.executable()
                        ))
                    },
                )?,
            )
        };

        // Build shell command with the resolved absolute executable so child
        // PATH changes cannot affect the shell process that applies the context.
        let mut cmd = Command::new(&shell_executable);
        // The context script applies Rex once. Starting from the exact parent
        // snapshot avoids host-environment leaks and a second Rex evaluation.
        cmd.env_clear().envs(&parent);

        match command {
            Some(c) => {
                // Non-interactive: source context script then run command
                let cf = context_file.display().to_string();
                match shell_type {
                    ShellType::Cmd => {
                        // /D disables AutoRun (conda init, etc.) for clean env isolation
                        let wrapper = tmpdir.join("rez-shell.bat");
                        std::fs::write(
                            &wrapper,
                            format!("@echo off\r\ncall \"{}\"\r\n{}\r\n", cf, c),
                        )?;
                        cmd.args(["/D", "/c", &wrapper.display().to_string()]);
                    }
                    ShellType::PowerShell => {
                        // Create wrapper .ps1 for clean execution
                        let wrapper = tmpdir.join("rez-shell.ps1");
                        let source_statement =
                            ShellType::PowerShell.join_command(&[".".into(), cf], false, None);
                        std::fs::write(
                            &wrapper,
                            format!("{source_statement}\n{c}\n{}\n", shell_type.exit_command()),
                        )?;
                        cmd.args([
                            "-ExecutionPolicy",
                            "Bypass",
                            "-File",
                            &wrapper.display().to_string(),
                        ]);
                    }
                    ShellType::Csh
                    | ShellType::Tcsh
                    | ShellType::Zsh
                    | ShellType::Bash
                    | ShellType::Sh
                    | ShellType::Gitbash => {
                        let cf_fwd = cf.replace('\\', "/");
                        let source_command =
                            if matches!(shell_type, ShellType::Csh | ShellType::Tcsh) {
                                "source"
                            } else {
                                "."
                            };
                        let source_statement =
                            shell_type.join_command(&[source_command.into(), cf_fwd], false, None);
                        let wrapped = if c.is_empty() {
                            source_statement
                        } else if matches!(shell_type, ShellType::Csh | ShellType::Tcsh) {
                            format!("{source_statement}; {c}")
                        } else {
                            format!("{source_statement} && {c}")
                        };
                        cmd.args(["-c", &wrapped]);
                    }
                }
            }
            _ => {
                // Interactive shell with context script sourced
                match shell_type {
                    ShellType::Bash | ShellType::Sh | ShellType::Gitbash => {
                        let cf_fwd = context_file.display().to_string().replace('\\', "/");
                        cmd.args(["--rcfile", &cf_fwd]);
                        if !quiet {
                            cmd.arg("-i");
                        }
                    }
                    ShellType::Cmd => {
                        // /D disables AutoRun (conda init, etc.) for clean env isolation
                        let wrapper = tmpdir.join("rez-shell.bat");
                        std::fs::write(
                            &wrapper,
                            format!("@echo off\r\ncall \"{}\"\r\n", context_file.display()),
                        )?;
                        cmd.args(["/D", "/k", &wrapper.display().to_string()]);
                    }
                    ShellType::PowerShell => {
                        cmd.args([
                            "-ExecutionPolicy",
                            "Bypass",
                            "-NoExit",
                            "-File",
                            &context_file.display().to_string(),
                        ]);
                    }
                    ShellType::Csh | ShellType::Tcsh | ShellType::Zsh => {
                        // csh/tcsh/zsh: no --rcfile; use wrapper that sources context then exec -i
                        let ext = shell_type.file_ext();
                        let wrapper = tmpdir.join(format!("rez-interactive.{}", ext));
                        let cf = context_file.display().to_string().replace('\\', "/");
                        let source_command =
                            if matches!(shell_type, ShellType::Csh | ShellType::Tcsh) {
                                "source"
                            } else {
                                "."
                            };
                        let source_statement =
                            shell_type.join_command(&[source_command.into(), cf], false, None);
                        let exec_arguments =
                            [shell_executable.display().to_string(), "-i".to_string()];
                        let exec_command = shell_type.join_command(&exec_arguments, false, None);
                        std::fs::write(
                            &wrapper,
                            format!("{source_statement}\nexec {exec_command}\n"),
                        )?;
                        cmd.arg(&wrapper);
                    }
                }
            }
        }

        if capture_output {
            Ok(ShellExecution::Output(cmd.output()?))
        } else {
            cmd.stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit());
            Ok(ShellExecution::Status(cmd.status()?))
        }
    }

    // =========================================================================
    // Printing / Display
    // =========================================================================

    /// Print a summary of the resolved context.
    pub fn print_info(&self, verbosity: u32) {
        let status_str =
            if self.status == ResolverStatus::Failed || self.status == ResolverStatus::Aborted {
                "resolve failed,"
            } else {
                "resolved"
            };

        println!(
            "{} by {}@{}, using rez-rs v{}",
            status_str, self.user, self.host, self.rez_version
        );

        if let Some(ts) = self.requested_timestamp.filter(|ts| *ts != 0) {
            println!("packages released after timestamp {} were ignored", ts);
        }
        println!();

        if verbosity > 0 {
            println!("search paths:");
            for path in &self.package_paths {
                println!("  {}", path.display());
            }
            println!();
        }

        // Requested packages
        println!("requested packages:");
        for req in &self.package_requests {
            println!("  {req}");
        }
        for req in &self.implicit_packages {
            println!("  {req}  (implicit)");
        }
        println!();

        // Resolved or failed
        if self.status == ResolverStatus::Failed || self.status == ResolverStatus::Aborted {
            if let Some(ref desc) = self.failure_description {
                println!("The context failed to resolve:\n{desc}");
            }
            return;
        }

        println!("resolved packages:");
        if let Some(ref pkgs) = self.resolved_packages {
            let mut sorted: Vec<_> = pkgs.iter().collect();
            sorted.sort_by(|a, b| a.name.cmp(&b.name));
            for pkg in sorted {
                let loc = pkg
                    .repo_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                println!("  {}  {}", pkg.qualified_name(), loc);
            }
        }

        if let Some(ref ephs) = self.resolved_ephemerals {
            for eph in ephs {
                println!("  {eph}  (ephemeral)");
            }
        }

        if verbosity > 0 {
            println!();
            let actual = self.solve_time - self.load_time;
            println!("resolve details:");
            println!("  load time:    {:.2} secs", self.load_time);
            println!("  solve time:   {:.2} secs", actual);
            println!("  from cache:   {}", self.from_cache);
            if let Some(ref lp) = self.load_path {
                println!("  rxt file:     {}", lp.display());
            }
        }
    }

    // =========================================================================
    // Env changes tracking
    // =========================================================================

    /// Get the environment changes this context makes relative to a parent.
    pub fn get_key_changes(
        &self,
        parent_environ: Option<HashMap<String, String>>,
    ) -> Result<ContextChanges> {
        self.require_success()?;
        let parent = parent_environ
            .clone()
            .unwrap_or_else(|| std::env::vars().collect());
        let resolved = self.get_environ(parent_environ)?;

        let mut added = HashMap::new();
        let mut removed = Vec::new();
        let mut modified = HashMap::new();

        for (key, new_val) in &resolved {
            match parent.get(key) {
                Some(old_val) if old_val != new_val => {
                    modified.insert(key.clone(), (old_val.clone(), new_val.clone()));
                }
                None => {
                    added.insert(key.clone(), new_val.clone());
                }
                _ => {}
            }
        }

        for key in parent.keys() {
            if !resolved.contains_key(key) {
                removed.push(key.clone());
            }
        }

        Ok(ContextChanges {
            added,
            removed,
            modified,
        })
    }

    // =========================================================================
    // Internal: rex execution
    // =========================================================================

    /// Execute all context actions on a RexExecutor.
    ///
    /// Sets system env vars (REZ_USED_*, per-package vars) then runs
    /// package commands (pre_commands, commands, post_commands).
    fn execute_rex<I: ActionInterpreter>(
        &self,
        executor: &mut RexExecutor<I>,
        callback: Option<&RexExecutionCallback>,
    ) -> Result<()> {
        crate::config::ensure_valid()?;
        let execution_packages = self.execution_packages(None)?;
        let resolved_pkgs = execution_packages.as_slice();
        let ephemerals = self.resolved_ephemerals.as_deref().unwrap_or(&[]);

        let request_str: String = self
            .package_requests
            .iter()
            .map(|r| r.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let implicit_str: String = self
            .implicit_packages
            .iter()
            .map(|r| r.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let resolve_str: String = resolved_pkgs
            .iter()
            .map(|p| p.qualified_name())
            .collect::<Vec<_>>()
            .join(" ");
        let package_paths_str: String = self
            .package_paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(if cfg!(windows) { ";" } else { ":" });
        let req_ts_str = self.requested_timestamp.unwrap_or(0).to_string();

        // System setup
        executor.comment("system setup");
        for (name, value) in CONFIG.recipe_environment() {
            executor.setenv(&name, value);
        }
        executor.setenv("REZ_USED", &self.rez_path);
        executor.setenv("REZ_USED_VERSION", &self.rez_version);
        executor.setenv("REZ_USED_TIMESTAMP", self.timestamp.to_string());
        executor.setenv("REZ_USED_REQUESTED_TIMESTAMP", &req_ts_str);
        executor.setenv("REZ_USED_REQUEST", &request_str);
        executor.setenv("REZ_USED_IMPLICIT_PACKAGES", &implicit_str);
        executor.setenv("REZ_USED_RESOLVE", &resolve_str);
        executor.setenv("REZ_USED_PACKAGES_PATH", &package_paths_str);

        if !ephemerals.is_empty() {
            let eph_str: String = ephemerals
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join(" ");
            executor.setenv("REZ_USED_EPH_RESOLVE", &eph_str);
        }

        if self.building {
            executor.setenv("REZ_BUILD_ENV", "1");
        }

        // REZ_USED_LOCAL_RESOLVE: space-separated list of packages from local_packages_path
        if !self.from_cache {
            let local_path = CONFIG.expanded_local_packages_path().to_os();
            executor.setenv(
                "REZ_USED_LOCAL_RESOLVE",
                local_resolve_names(resolved_pkgs, &local_path),
            );
        }

        // Rez-1 variables share the normal Rex action path and explicit opt-in policy.
        if CONFIG.rez_1_environment_variables && !CONFIG.disable_rez_1_compatibility {
            let legacy_request = [request_str.as_str(), implicit_str.as_str()].join(" ");
            let legacy_request = legacy_request.trim();
            executor.setenv("REZ_VERSION", &self.rez_version);
            executor.setenv("REZ_PATH", self.rez_path.replace('\\', "/"));
            executor.setenv("REZ_REQUEST", legacy_request);
            executor.setenv("REZ_RESOLVE", &resolve_str);
            executor.setenv("REZ_RAW_REQUEST", legacy_request);
            executor.setenv("REZ_RESOLVE_MODE", "latest");
        }

        // Per-package variables
        executor.comment("package variables");
        for pkg in resolved_pkgs {
            let prefix = format!("REZ_{}", pkg.name.to_uppercase().replace('.', "_"));

            executor.setenv(&format!("{prefix}_VERSION"), pkg.version.to_string());

            let major = pkg
                .version
                .get(0)
                .map(|t| t.to_string())
                .unwrap_or_default();
            let minor = pkg
                .version
                .get(1)
                .map(|t| t.to_string())
                .unwrap_or_default();
            let patch = pkg
                .version
                .get(2)
                .map(|t| t.to_string())
                .unwrap_or_default();
            executor.setenv(&format!("{prefix}_MAJOR_VERSION"), &major);
            executor.setenv(&format!("{prefix}_MINOR_VERSION"), &minor);
            executor.setenv(&format!("{prefix}_PATCH_VERSION"), &patch);

            if let Some(ref rp) = pkg.repo_path {
                executor.setenv(&format!("{prefix}_BASE"), rp.display().to_string());
            }
            if let Some(ref root) = pkg.root {
                if let Some(original) = self
                    .resolved_packages
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .find(|original| original.name == pkg.name)
                    .and_then(|original| original.root.as_ref().or(original.repo_path.as_ref()))
                    .filter(|original| *original != root)
                {
                    executor.setenv(
                        &format!("{prefix}_ORIG_ROOT"),
                        original.display().to_string(),
                    );
                }
                executor.setenv(&format!("{prefix}_ROOT"), root.display().to_string());
            } else if let Some(ref rp) = pkg.repo_path {
                executor.setenv(&format!("{prefix}_ROOT"), rp.display().to_string());
            }
        }

        // Ephemeral variables
        if !ephemerals.is_empty() {
            executor.comment("ephemeral variables");
            for eph in ephemerals {
                let name = eph.name();
                if let Some(stripped) = name.strip_prefix('.') {
                    let uname = stripped.to_uppercase().replace('.', "_");
                    let varname = format!("REZ_EPH_{uname}_REQUEST");
                    let range_s = eph.range().map(|r| r.to_string()).unwrap_or_default();
                    executor.setenv(&varname, &range_s);
                }
            }
        }

        // Execute commands via RustPython (matching Python rez resolved_context.py:2093).
        // Commands are Python code executed with proper bindings (env, this, resolve, etc.)
        executor.comment("package commands");
        exec_rex_py(executor, resolved_pkgs, self, callback)?;

        // Bundle post_commands.py: run after package commands if loading from a bundle (rez resolved_context.py:1744).
        execute_bundle_post_commands_if_present(executor, self, resolved_pkgs)?;

        // Post system setup (matches Python resolved_context.py:2146-2161)
        executor.comment("post system setup");

        // Retain visible suites before adding system and Rez tool paths.
        let suite_mode = CONFIG.suite_visibility;
        if suite_mode != crate::constants::SuiteVisibility::Never {
            let visible = crate::suite::Suite::visible_suite_paths(None);
            if !visible.is_empty() {
                let suite_paths = match suite_mode {
                    crate::constants::SuiteVisibility::Always => visible,
                    // Reference parent_priority currently retains only the parent too.
                    crate::constants::SuiteVisibility::Parent
                    | crate::constants::SuiteVisibility::ParentPriority => self
                        .parent_suite_path
                        .as_ref()
                        .map(PathBuf::from)
                        .into_iter()
                        .collect(),
                    crate::constants::SuiteVisibility::Never => Vec::new(),
                };
                for path in suite_paths {
                    executor.appendenv("PATH", path.join("bin").to_string_lossy().into_owned());
                }
            }
        }

        // System paths go after every package command so resolved packages always
        // shadow same-named host tools (e.g. Xcode's /usr/bin/Rez on case-insensitive
        // macOS). A configured standard_system_paths replaces the OS defaults.
        if self.append_sys_path {
            let configured = &crate::config::CONFIG.standard_system_paths;
            let paths = if configured.is_empty() {
                repository::package::bind::discover_sys_paths()
            } else {
                configured.clone()
            };
            for entry in &paths {
                executor.appendenv("PATH", entry);
            }
        }

        // Add rez binary dir to PATH based on rez_tools_visibility config
        let vis = crate::config::CONFIG.rez_tools_visibility;
        if vis != RezToolsVisibility::Never {
            if let Ok(exe) = std::env::current_exe() {
                if let Some(dir) = exe.parent() {
                    let dir_str = dir.to_string_lossy();
                    match vis {
                        RezToolsVisibility::Append => executor.appendenv("PATH", dir_str.as_ref()),
                        RezToolsVisibility::Prepend => {
                            executor.prependenv("PATH", dir_str.as_ref())
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }

    // =========================================================================
    // Diff, Tools, and Graph methods
    // =========================================================================

    /// Get the difference between resolved packages in this context and another.
    ///
    /// Compares package names and versions. The diff is expressed from the point
    /// of view of 'self' - a newer package means the package in 'other' is newer.
    pub fn get_resolve_diff(&self, other: &ResolvedContext) -> ResolveDiff {
        let self_pkgs = self.resolved_packages.as_ref();
        let other_pkgs = other.resolved_packages.as_ref();

        let mut newer = Vec::new();
        let mut older = Vec::new();
        let mut added = Vec::new();
        let mut removed = Vec::new();

        // Build maps: package name -> (name, version)
        let self_map: HashMap<_, _> = self_pkgs
            .map(|pkgs| {
                pkgs.iter()
                    .map(|p| (p.name.clone(), (p.name.clone(), p.version.to_string())))
                    .collect()
            })
            .unwrap_or_default();

        let other_map: HashMap<_, _> = other_pkgs
            .map(|pkgs| {
                pkgs.iter()
                    .map(|p| (p.name.clone(), (p.name.clone(), p.version.to_string())))
                    .collect()
            })
            .unwrap_or_default();

        // Compare versions
        for (name, (self_name, self_ver_str)) in &self_map {
            if let Some((_, other_ver_str)) = other_map.get(name) {
                // Both have the package - compare versions
                if let (Ok(self_ver), Ok(other_ver)) = (
                    version::Version::new(self_ver_str),
                    version::Version::new(other_ver_str),
                ) {
                    if other_ver > self_ver {
                        newer.push((self_name.clone(), format!("{}-{}", name, other_ver_str)));
                    } else if self_ver > other_ver {
                        older.push((self_name.clone(), format!("{}-{}", name, self_ver_str)));
                    }
                }
            } else {
                // In self but not in other
                added.push((self_name.clone(), format!("{}-{}", name, self_ver_str)));
            }
        }

        // Find packages in other but not in self
        for (name, (other_name, other_ver_str)) in &other_map {
            if !self_map.contains_key(name) {
                removed.push((other_name.clone(), format!("{}-{}", name, other_ver_str)));
            }
        }

        ResolveDiff {
            newer,
            older,
            added,
            removed,
        }
    }

    /// Print a formatted diff between this context and another.
    pub fn print_resolve_diff(&self, other: &ResolvedContext) {
        let diff = self.get_resolve_diff(other);

        if diff.is_empty() {
            println!("No differences");
            return;
        }

        if !diff.newer.is_empty() {
            println!("Newer packages in other:");
            for (name, qname) in &diff.newer {
                println!("  {} -> {}", name, qname);
            }
            println!();
        }

        if !diff.older.is_empty() {
            println!("Older packages in other:");
            for (name, qname) in &diff.older {
                println!("  {} -> {}", name, qname);
            }
            println!();
        }

        if !diff.added.is_empty() {
            println!("Added packages (in self, not in other):");
            for (_, qname) in &diff.added {
                println!("  {}", qname);
            }
            println!();
        }

        if !diff.removed.is_empty() {
            println!("Removed packages (in other, not in self):");
            for (_, qname) in &diff.removed {
                println!("  {}", qname);
            }
        }
    }

    /// Get tools provided by resolved packages.
    ///
    /// Returns a map: package_name -> (variant_str, tool_list). When
    /// request_only is true, only explicitly requested packages are included.
    /// Package-declared tools are merged with executables found under package
    /// roots, bin/, and venv/Scripts or venv/bin.
    pub fn get_tools(&self, request_only: bool) -> Result<HashMap<String, (String, Vec<String>)>> {
        self.get_tools_with_conflicts(request_only)
            .map(|(tools, _)| tools)
    }

    /// Get tools that appear in multiple resolved variants.
    ///
    /// Returns a map: tool_name -> sorted package names that provide it.
    pub fn get_conflicting_tools(
        &self,
        request_only: bool,
    ) -> Result<HashMap<String, Vec<String>>> {
        self.get_tools_with_conflicts(request_only)
            .map(|(_, conflicts)| conflicts)
    }

    /// Resolve the tool map and conflicts from one consistent package snapshot.
    pub fn get_tools_with_conflicts(&self, request_only: bool) -> Result<ContextToolSnapshot> {
        self.require_success()?;

        let requested_names: std::collections::HashSet<&str> = self
            .package_requests
            .iter()
            .filter(|request| !request.conflict())
            .map(|request| request.name())
            .collect();
        let lb_ctx = python_runtime::LateBindingContext {
            request: self
                .package_requests
                .iter()
                .map(|r| (r.name().to_string(), r.to_string()))
                .collect(),
            implicits: self
                .implicit_packages
                .iter()
                .map(|r| (r.name().to_string(), r.to_string()))
                .collect(),
            system: [
                ("platform".into(), self.platform_str.clone()),
                ("arch".into(), self.arch.clone()),
                ("os".into(), self.os_str.clone()),
            ]
            .into(),
            building: self.building,
            testing: self.testing,
        };

        let mut tools_map = HashMap::new();
        if let Some(ref pkgs) = self.resolved_packages {
            for pkg in pkgs {
                if request_only && !requested_names.contains(pkg.name.as_str()) {
                    continue;
                }

                let variant_str = pkg.qualified_name();
                let mut tools = Vec::new();
                let mut seen_tools = std::collections::HashSet::new();

                // 1. Package-declared tools (late-bound with in_context)
                if let Some(mut pkg_data) = pkg.load_data()? {
                    let root = pkg
                        .root
                        .as_ref()
                        .or(pkg.repo_path.as_ref())
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    let base = pkg
                        .repo_path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    pkg_data.insert("root".into(), serde_json::Value::String(root));
                    pkg_data.insert("base".into(), serde_json::Value::String(base));
                    if let Some(idx) = pkg.variant_index {
                        pkg_data.insert(
                            "variant_index".into(),
                            serde_json::Value::Number(idx.into()),
                        );
                    }
                    if let Some(tools_val) = crate::serialise::resolve_field_for_context(
                        &pkg_data,
                        "tools",
                        Some(&lb_ctx),
                    )? {
                        if let Some(arr) = tools_val.as_array() {
                            for tool in arr.iter().filter_map(serde_json::Value::as_str) {
                                if seen_tools.insert(tool.to_string()) {
                                    tools.push(tool.to_string());
                                }
                            }
                        }
                    }
                }

                // 2. Filesystem scan: bin/ and venv/Scripts|venv/bin
                if let Some(ref root) = pkg.root {
                    let tool_dirs = [
                        root.join("bin"),
                        root.join("venv")
                            .join(if cfg!(windows) { "Scripts" } else { "bin" }),
                    ];
                    for tool_dir in tool_dirs {
                        if tool_dir.is_dir() {
                            if let Ok(entries) = std::fs::read_dir(&tool_dir) {
                                for entry in entries.flatten() {
                                    if let Ok(ft) = entry.file_type() {
                                        if ft.is_file() {
                                            if let Some(name) = entry.file_name().to_str() {
                                                let tool_name = if cfg!(windows) {
                                                    name.strip_suffix(".exe")
                                                        .or_else(|| name.strip_suffix(".bat"))
                                                        .or_else(|| name.strip_suffix(".cmd"))
                                                        .unwrap_or(name)
                                                } else {
                                                    name
                                                };
                                                if seen_tools.insert(tool_name.to_string()) {
                                                    tools.push(tool_name.to_string());
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                if !tools.is_empty() {
                    tools_map.insert(pkg.name.clone(), (variant_str, tools));
                }
            }
        }

        let mut providers: HashMap<String, HashMap<String, String>> = HashMap::new();
        for (package_name, (variant, tools)) in &tools_map {
            for tool in tools {
                providers
                    .entry(tool.clone())
                    .or_default()
                    .entry(variant.clone())
                    .or_insert_with(|| package_name.clone());
            }
        }
        let conflicts = providers
            .into_iter()
            .filter_map(|(tool, variants)| {
                if variants.len() <= 1 {
                    return None;
                }
                let mut packages: Vec<_> = variants.into_values().collect();
                packages.sort();
                Some((tool, packages))
            })
            .collect();

        Ok((tools_map, conflicts))
    }

    /// Find a command in the resolved environment (matches Python rez behavior).
    ///
    /// Uses get_environ() and searches PATH — so finds commands from package
    /// commands (e.g. venv/Scripts, bin/) not just root/bin.
    /// Returns (package_name, path_to_command) if found.
    pub fn which(&self, cmd: &str) -> Option<(String, PathBuf)> {
        let env = self.get_environ(None).ok()?;
        let path_str = env.get("PATH").or(env.get("Path"))?;
        let path_sep = if cfg!(windows) { ';' } else { ':' };
        let candidates: Vec<String> = if cfg!(windows) {
            vec![
                format!("{}.exe", cmd),
                format!("{}.bat", cmd),
                format!("{}.cmd", cmd),
                cmd.to_string(),
            ]
        } else {
            vec![cmd.to_string()]
        };

        for dir_str in path_str.split(path_sep) {
            let dir_str = dir_str.trim();
            if dir_str.is_empty() {
                continue;
            }
            let dir = crate::config::RezConfig::expand_path(dir_str).to_os();
            if !dir.is_dir() {
                continue;
            }
            for candidate in &candidates {
                let full = dir.join(candidate);
                if full.is_file() {
                    let pkg = self.resolved_packages.as_ref().and_then(|pkgs| {
                        pkgs.iter().find(|package| {
                            package.root.as_ref().is_some_and(|root| {
                                foundation::util::relative_to_authority(root, &full)
                                    .ok()
                                    .flatten()
                                    .is_some()
                            })
                        })
                    });
                    let name = pkg.map(|p| p.name.clone()).unwrap_or_default();
                    return Some((name, full));
                }
            }
        }
        None
    }

    /// Generate DOT format dependency graph.
    ///
    /// Nodes are resolved packages (name-version), edges are requirements.
    pub fn graph_as_dot(&self) -> String {
        let mut dot = String::from("digraph resolve_graph {\n");
        dot.push_str("  node [shape=box, style=filled, fillcolor=\"#AAFFAA\"];\n");

        if let Some(ref pkgs) = self.resolved_packages {
            // Add nodes and edges
            for pkg in pkgs {
                let node_name = format!("{}-{}", pkg.name, pkg.version);
                for req in &pkg.requires {
                    let dep_name = req.name();
                    // Find the resolved version of the dependency
                    let dep_node = pkgs
                        .iter()
                        .find(|p| p.name == dep_name)
                        .map(|p| format!("{}-{}", p.name, p.version))
                        .unwrap_or_else(|| dep_name.to_string());

                    dot.push_str(&format!("  \"{}\" -> \"{}\";\n", node_name, dep_node));
                }
            }
        }

        dot.push_str("}\n");
        dot
    }

    /// Generate simpler dependency graph in DOT format.
    ///
    /// Only includes package names (no versions) and direct dependencies.
    /// Excludes conflict and weak requirements.
    pub fn get_dependency_graph_as_dot(&self) -> String {
        use std::collections::HashSet;

        let mut dot = String::from("digraph dependency_graph {\n");
        dot.push_str("  node [shape=ellipse, style=filled, fillcolor=\"#AAFFAA\", fontsize=10];\n");

        let mut edges = HashSet::new();

        if let Some(ref pkgs) = self.resolved_packages {
            // Add nodes
            for pkg in pkgs {
                dot.push_str(&format!("  \"{}\";\n", pkg.name));
            }

            // Add edges (only non-conflict, non-weak requirements)
            for pkg in pkgs {
                for req in &pkg.requires {
                    if !req.conflict() && !req.weak() {
                        let edge = (pkg.name.clone(), req.name().to_string());
                        if edges.insert(edge.clone()) {
                            dot.push_str(&format!("  \"{}\" -> \"{}\";\n", edge.0, edge.1));
                        }
                    }
                }
            }
        }

        dot.push_str("}\n");
        dot
    }

    // =========================================================================
    // Context Tracking (AMQP analytics)
    // =========================================================================

    // Helper functions for timestamp formatting

    /// Build tracking payload with context info for AMQP analytics.
    ///
    /// Returns JSON payload with action, timestamp, host, user, package info.
    /// Called after resolve (action="created") or load (action="sourced").
    pub fn tracking_payload(&self, action: &str) -> serde_json::Value {
        use crate::config::CONFIG;

        // Build base payload with rez version, host, user
        let mut data = serde_json::json!({
            "action": action,
            "rez_version": self.rez_version,
            "host": self.host,
            "user": self.user,
        });

        // Add timestamp in ISO 8601 format
        if let Ok(duration) = SystemTime::now().duration_since(UNIX_EPOCH) {
            let secs = duration.as_secs();
            // Simple ISO 8601 formatting without external deps
            let datetime = chrono_lite(secs);
            data["timestamp"] = serde_json::Value::String(datetime);
        }

        // Add package requests (original user request)
        let requests: Vec<String> = self
            .package_requests
            .iter()
            .map(|r| r.to_string())
            .collect();
        data["package_requests"] = serde_json::Value::Array(
            requests
                .into_iter()
                .map(serde_json::Value::String)
                .collect(),
        );

        // Add resolved packages (qualified names)
        if let Some(ref pkgs) = self.resolved_packages {
            let resolved: Vec<String> = pkgs
                .iter()
                .map(|p| format!("{}-{}", p.name, p.version))
                .collect();
            data["resolved_packages"] = serde_json::Value::Array(
                resolved
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            );
        } else {
            data["resolved_packages"] = serde_json::Value::Array(vec![]);
        }

        // Add metrics
        data["solve_time"] = serde_json::Value::Number(
            serde_json::Number::from_f64(self.solve_time).unwrap_or(serde_json::Number::from(0)),
        );
        data["status"] = serde_json::Value::String(self.status.to_string());

        // Add context file path if loaded from file
        if let Some(ref path) = self.load_path {
            data["context_file"] = serde_json::Value::String(path.display().to_string());
        }

        // Merge extra fields from config
        if let Some(obj) = data.as_object_mut() {
            for (key, value) in CONFIG.context_tracking_extra_fields.iter() {
                obj.insert(key.clone(), value.clone());
            }
        }

        data
    }

    /// Track context usage by publishing to AMQP.
    ///
    /// Called after resolve ("created") or load ("sourced") if tracking enabled.
    /// Checks config.context_tracking_host to enable/disable.
    /// Uses context_tracking_amqp for exchange, routing_key, credentials.
    pub fn track_context(&self, action: &str) {
        use crate::config::CONFIG;

        // Check if tracking is enabled (host is non-empty)
        if CONFIG.context_tracking_host.is_empty() {
            return;
        }

        // Build payload
        let payload = self.tracking_payload(action);

        // Debug output if enabled
        if CONFIG.debug_context_tracking {
            eprintln!("[rez-rs] Context tracking payload ({}):", action);
            if let Ok(pretty) = serde_json::to_string_pretty(&payload) {
                eprintln!("{}", pretty);
            }
        }

        // Optional: log to file (uncomment to enable file logging)
        // let log_path = std::env::temp_dir().join("rez_context_tracking.jsonl");
        // if let Ok(mut file) = std::fs::OpenOptions::new()
        //     .create(true)
        //     .append(true)
        //     .open(&log_path)
        // {
        //     use std::io::Write;
        //     if let Ok(json) = serde_json::to_string(&payload) {
        //         let _ = writeln!(file, "{}", json);
        //     }
        // }

        // Publish to AMQP (fire-and-forget; failures logged only)
        #[cfg(feature = "amqp")]
        if let Err(e) = crate::amqp::publish_context_tracking(action, &payload) {
            if CONFIG.debug_context_tracking {
                eprintln!("[rez-rs] Context tracking AMQP publish failed: {}", e);
            }
        }
    }
}

// ============================================================================
// Helper functions for timestamp formatting
// ============================================================================

/// Python preamble for rex command execution via RustPython.
/// Defines proxy classes: _RexEnv, _EnvVar, _VariantBinding, _DictBinding, etc.
/// Injected data comes via `_rex_data` global (JSON dict).
/// REX preamble code injected before package commands.
#[doc(hidden)]
macro_rules! rex_preamble {
    ($tail:literal) => { concat!(r#"
_actions = []

class EscapedString:
    """Ordered Rex literal/expandable segments, matching rez.rex.EscapedString."""
    def __init__(self, value, is_literal=False):
        self.strings = [(is_literal, value)]
    def copy(self):
        result = EscapedString("")
        result.strings = self.strings[:]
        return result
    def _add(self, value, is_literal):
        if self.strings and self.strings[-1][0] == is_literal:
            self.strings[-1] = (is_literal, self.strings[-1][1] + value)
        else:
            self.strings.append((is_literal, value))
    def literal(self, value):
        self._add(value, True)
        return self
    def expandable(self, value):
        self._add(value, False)
        return self
    l = literal
    e = expandable
    def __str__(self):
        return "".join(value for _, value in self.strings)
    def __repr__(self):
        return "EscapedString(%r)" % self.strings
    def __eq__(self, other):
        return str(self) == other if isinstance(other, str) else isinstance(other, EscapedString) and self.strings == other.strings
    def __add__(self, other):
        result = self.copy()
        for is_literal, value in self.promote(other).strings:
            result._add(value, is_literal)
        return result
    def __radd__(self, other):
        return self.promote(other).__add__(self)
    def formatted(self, func):
        result = self.copy()
        result.strings = [(is_literal, value if is_literal else func(value)) for is_literal, value in self.strings]
        return result
    def expanduser(self):
        import os
        return self.formatted(os.path.expanduser)
    def split(self, delimiter=None):
        """Same as string.split(), but retains literal/expandable structure.

        Returns:
            List of `EscapedString`.
        """
        result = []
        strings = self.strings[:]
        current = None

        while strings:
            is_literal, value = strings[0]
            parts = value.split(delimiter, 1)
            if len(parts) > 1:
                value1, value2 = parts
                strings[0] = (is_literal, value2)
                out = EscapedString(value1, is_literal)
                push = True
            else:
                strings = strings[1:]
                out = EscapedString(value, is_literal)
                push = False

            if current is None:
                current = out
            else:
                current = current + out
            if push:
                result.append(current)
                current = None

        if current:
            result.append(current)
        return result

    @classmethod
    def join(cls, sep, values):
        iterator = iter(values)
        try:
            result = cls.promote(next(iterator))
        except StopIteration:
            return cls("")
        for value in iterator:
            result = result + sep + value
        return result
    @classmethod
    def promote(cls, value):
        return value if isinstance(value, cls) else cls(value)
    @classmethod
    def demote(cls, value):
        return str(value) if isinstance(value, cls) else value
    @classmethod
    def disallow(cls, value):
        if isinstance(value, cls):
            raise TypeError("The command does not accept literal or expandable")
        return value

def literal(value):
    return EscapedString(value, True)
def expandable(value):
    return EscapedString(value)

import re as _re
from string import Formatter as _Formatter

class _NamespaceFormatter(_Formatter):
    _env_regex = _re.compile(r"\$\{([^\{\}]+?)\}|\$([a-zA-Z_]+[a-zA-Z0-9_]*)")
    def __init__(self, namespace):
        self.namespace = namespace
    def format(self, value, *args, **kwargs):
        def escape_envvar(match):
            return "${{%s}}" % next(part for part in match.groups() if part is not None)
        value = self._env_regex.sub(escape_envvar, value)
        previous = self.namespace
        if kwargs:
            self.namespace = dict(previous)
            self.namespace.update(kwargs)
        try:
            return _Formatter.format(self, value, *args, **kwargs)
        finally:
            self.namespace = previous
    def format_field(self, value, spec):
        if isinstance(value, EscapedString):
            value = str(value.formatted(str))
        if isinstance(value, str):
            value = self.format(value)
        return super().format_field(value, spec)
    def get_value(self, key, args, kwargs):
        if isinstance(key, str):
            if not key:
                raise ValueError("zero length field name in format")
            return kwargs[key] if key in kwargs else self.namespace[key]
        return super().get_value(key, args, kwargs)

_formatter = _NamespaceFormatter(globals())

def format(value):
    return _formatter.format(str(value))

def _expand_text(value):
    # ActionManager only formats strings; objects get their plain string repr.
    if not isinstance(value, (str, EscapedString)):
        return str(value)
    try:
        return EscapedString.promote(value).formatted(format)
    except (KeyError, ValueError):
        return value

def expandvars(value, format=True):
    if format:
        value = str(_expand_text(value))
    segments = [{"literal": is_literal, "text": text}
                for is_literal, text in EscapedString.promote(value).strings]
    state = dict(_rex_data["environment"])
    return rex_environ(_actions, state, segments)

def _expand_val(value):
    value = EscapedString.promote(_expand_text(value))
    return [{"literal": is_literal, "text": text} for is_literal, text in value.strings]

def _raw_code(value, format=True):
    value = str(EscapedString.disallow(value))
    return str(_expand_text(value)) if format else value

def _env_action(kind, key, value=None, friends=None):
    record = {"t": kind, "k": str(_expand_text(key))}
    if kind != "unset":
        record["v"] = _expand_val(value)
    if kind == "reset":
        record["f"] = friends
    _actions.append(record)

def setenv(key, value):
    _env_action("set", key, value)
def unsetenv(key):
    _env_action("unset", key)
def resetenv(key, value, friends=None):
    _env_action("reset", key, value, friends)
def prependenv(key, value):
    _env_action("prepend", key, value)
def appendenv(key, value):
    _env_action("append", key, value)

class _EnvVar:
    def __init__(self, key, env):
        self._key = key
        self._env = env
    def prepend(self, value):
        prependenv(self._key, value)
    def append(self, value):
        appendenv(self._key, value)
    def set(self, value):
        setenv(self._key, value)
    def reset(self, value, friends=None):
        resetenv(self._key, value, friends)
    def unset(self):
        unsetenv(self._key)
    @property
    def name(self):
        return self._key
    def get(self):
        return getenv(self._key)
    def __str__(self):
        return self.get()
    def __repr__(self):
        return "EnvironmentVariable(%r, %r)" % (self._key, self.get())
    def __bool__(self):
        try:
            return bool(self.get())
        except _RexUndefinedVariableError:
            return False
    def __eq__(self, other):
        return self.get() == (other.get() if isinstance(other, _EnvVar) else other)
    def value(self):
        # Alias for get()
        return self.get()
    def setdefault(self, value):
        # Set only if not already set
        if not self:
            self.set(value)

from collections.abc import MutableMapping as _MutableMapping

class _RexEnv(_MutableMapping):
    def __init__(self):
        # Rez caches parent names even when their values are not inherited.
        self._vars = {key: _EnvVar(key, self) for key in _rex_data["environment"]["parent"]}
    def __getattr__(self, key):
        if key.startswith("__") and key.endswith("__"):
            return object.__getattribute__(self, key)
        return self[key]
    def __getitem__(self, key):
        # Indexing must bypass attribute lookup: variables may be named keys/get/items.
        if key not in self._vars:
            self._vars[key] = _EnvVar(key, self)
        return self._vars[key]
    def __setitem__(self, key, value):
        self[key].set(value)
    def __delitem__(self, key):
        # Like Rez, deleting a mapping entry drops its cached proxy, not its value.
        del self._vars[key]
    def __contains__(self, key):
        return key in self._vars
    def __iter__(self):
        return iter(self._vars)
    def __len__(self):
        return len(self._vars)
    def __setattr__(self, key, value):
        if key == "_vars" or (key.startswith("__") and key.endswith("__")):
            object.__setattr__(self, key, value)
        else:
            self[key] = value

def alias(key, value):
    _actions.append({"t": "alias", "name": str(_expand_text(key)), "cmd": str(_expand_text(value))})
def info(value=''):
    _actions.append({"t": "info", "msg": _expand_val(value)})
def error(value):
    _actions.append({"t": "error", "msg": _expand_val(value)})
def source(value):
    _actions.append({"t": "source", "path": _expand_val(value)})
def command(value):
    if isinstance(value, (str, EscapedString)):
        _actions.append({"t": "command", "cmd": _raw_code(value, format=False)})
    else:
        values = iter(value)
        first = EscapedString.disallow(next(values))
        args = [EscapedString(str(first), True)] + [EscapedString.promote(item) for item in values]
        _actions.append({"t": "command_args", "args": [
            [{"literal": is_literal, "text": text} for is_literal, text in item.strings]
            for item in args
        ]})
from rez.exceptions import RexError as _RexError
from rez.exceptions import RexStopError as _RexStopError
from rez.exceptions import RexUndefinedVariableError as _RexUndefinedVariableError

def stop(msg, *nargs):
    # Stop is an execution exception, not a recorded interpreter action.
    raise _RexStopError(msg % nargs)
def comment(value):
    _actions.append({"t": "comment", "msg": str(_expand_text(value))})
def shebang():
    _actions.append({"t": "shebang"})
def optionvars(name, default=None):
    value = _rex_data.get("optionvars") or {}
    parts = name.split(".")
    for index, key in enumerate(parts):
        if not isinstance(value, dict):
            raise RuntimeError("Optionvar %r is invalid because %r is not a dict" % (name, ".".join(parts[:index])))
        if key not in value:
            return default
        value = value[key]
    return value

def undefined(key):
    return not defined(key)
def defined(key):
    query = {"operation": "defined", "key": str(_expand_text(key))}
    return rex_environ(_actions, _rex_data["environment"], query)
def getenv(key):
    query = {"operation": "getenv", "key": str(_expand_text(key))}
    return rex_environ(_actions, _rex_data["environment"], query)

class _VersionBinding:
    def __init__(self, ver_str, tokens):
        self._s = ver_str
        self._t = tokens
    @property
    def major(self): return self._t[0] if len(self._t) > 0 else None
    @property
    def minor(self): return self._t[1] if len(self._t) > 1 else None
    @property
    def patch(self): return self._t[2] if len(self._t) > 2 else None
    def __getitem__(self, i):
        if isinstance(i, slice): return tuple(self._t[i])
        try:
            return self._t[i]
        except IndexError:
            return None
    def __len__(self): return len(self._t)
    def __str__(self): return self._s
    def __iter__(self): return iter(self._t)
    def as_tuple(self): return tuple(self._t)

class _VariantBinding:
    def __init__(self, data):
        self._d = data
        self.name = data["name"]
        self.version = _VersionBinding(data["version"], data.get("tokens", []))
        # Paths are prepared by the active native interpreter.
        self.root = data.get("root", "")
        self.base = data.get("base", "")
    def __getattr__(self, attr):
        if attr in self._d: return self._d[attr]
        raise AttributeError("package %s has no attribute '%s'" % (self.name, attr))
    def __str__(self): return "%s-%s" % (self.name, self.version)

class _DictBinding:
    def __init__(self, data, label="item"):
        self._data = data
        self._label = label
    def __getattr__(self, name):
        if name.startswith('_'): return object.__getattribute__(self, name)
        if name in self._data: return self._data[name]
        raise AttributeError("%s does not exist: '%s'" % (self._label, name))
    def __getitem__(self, name):
        if name in self._data: return self._data[name]
        raise AttributeError("%s does not exist: '%s'" % (self._label, name))
    def __contains__(self, name):
        return name in self._data
    def get(self, name, default=None):
        return self._data.get(name, default)
    def get_range(self, name, default=None):
        requirement = self._data.get(name)
        if requirement:
            return _Requirement(requirement).range
        return _RequirementRange(default) if default is not None else None

class _SystemInfo:
    def __init__(self, data):
        for name in ("rez_version", "platform", "arch", "os", "variant", "shell",
                     "user", "home", "hostname", "rez_bin_path", "paths", "environ"):
            setattr(self, name, data[name])
        self._fqdn = None
    @property
    def fqdn(self):
        if self._fqdn is None:
            import socket
            self._fqdn = socket.getfqdn()
        return self._fqdn
    @property
    def domain(self):
        parts = self.fqdn.split(".", 1)
        return parts[1] if len(parts) > 1 else ""
    @property
    def is_production_rez_install(self):
        return bool(self.rez_bin_path)
    @property
    def selftest_is_running(self):
        import os
        return os.getenv("__REZ_SELFTEST_RUNNING") == "1"

class _ScopeContext:
    """Stub for scope context manager used in package.py config overrides.
    In rex commands() scope is typically empty; full scope is used at package load time."""
    def __init__(self):
        self._stack = [{}]
        self._current_scope = None
    def __call__(self, name):
        self._current_scope = name
        return self
    def __enter__(self):
        self._stack.append({})
        return self
    def __exit__(self, *args):
        top = self._stack.pop()
        if self._stack:
            scope_name = self._current_scope if self._current_scope else 'default'
            if scope_name not in self._stack[-1]:
                self._stack[-1][scope_name] = {}
            self._stack[-1][scope_name].update(top)
    def __setattr__(self, k, v):
        if k.startswith('_'):
            object.__setattr__(self, k, v)
        else:
            self._stack[-1][k] = v
    def __getattr__(self, k):
        for d in reversed(self._stack):
            if k in d:
                return d[k]
        return self
    def to_dict(self):
        return dict(self._stack[0]) if self._stack else {}

scope = _ScopeContext()

def _exec_rex(code, filename="<rex>"):
    namespace = globals()
    saved = dict(namespace)
    import traceback
    try:
        compiled = compile(code, filename, "exec")
    except Exception:
        raise _RexError("Failed to compile %s:\n\n%s" % (filename, traceback.format_exc()))
    import os as _rex_os
    saved_environ = _rex_os.environ
    # Use an interpreter-local dictionary: _Environ writes would mutate the host.
    keys = set(_rex_data["environment"]["parent"]) | set(_rex_data["environment"]["current"])
    keys.update(action["k"] for action in _actions if "k" in action)
    _rex_os.environ = {key: getenv(key) for key in keys if defined(key)}
    try:
        exec(compiled, namespace)
    finally:
        _rex_os.environ = saved_environ
        namespace.clear()
        namespace.update(saved)

def _bind_intersects(requirement_query, range_query, range_type, variant_type, version_type):
    # Capture genuine types and native operations before package globals can replace names.
    def intersects(obj, range_):
        if isinstance(obj, str):
            return requirement_query(obj, range_)
        if isinstance(obj, range_type):
            other = str(obj)
        elif isinstance(obj, variant_type):
            other = str(obj.version)
        elif isinstance(obj, version_type):
            other = str(obj)
        else:
            raise RuntimeError("Invalid type %s passed as first arg to 'intersects'" % type(obj))
        return range_query("intersects", range_, other)
    return intersects

intersects = _bind_intersects(intersects, _rez_version_native, _RequirementRange,
                             _VariantBinding, _VersionBinding)
del _bind_intersects

# Initialize from injected _rex_data
env = _RexEnv()
system = _SystemInfo(_rex_data.get("system", {}))
building = _rex_data.get("building", False)
testing = _rex_data.get("testing", False)

_variants = {}
for _p in _rex_data.get("packages", []):
    _variants[_p["name"]] = _VariantBinding(_p)
resolve = _DictBinding(_variants, "package")
request = _DictBinding(_rex_data.get("request", {}), "request")
implicits = _DictBinding(_rex_data.get("implicits", {}), "implicits")
ephemerals = _DictBinding(_rex_data.get("ephemerals", {}), "ephemeral")

"#, $tail) };
}

pub const REX_PREAMBLE: &str = rex_preamble!(
    r#"# Execute all package commands in 3-phase order
for _phase in ("pre_commands", "commands", "post_commands"):
    for _pkg_data in _rex_data["packages"]:
        _cmds = _pkg_data.get(_phase)
        if not _cmds:
            continue
        this = _VariantBinding(_pkg_data)
        version = this.version
        root = this.root
        base = this.base
        _actions.append({"t": "pkg_begin", "phase": _phase, "name": _pkg_data["name"], "version": _pkg_data["version"]})
        _exec_rex(_cmds, _phase + ".py")

_execution_callback = _rex_data.get("execution_callback")
if _execution_callback:
    _callback_package = _execution_callback.get("package")
    this = _VariantBinding(_callback_package) if _callback_package is not None else _variants.get(_execution_callback["package_name"])
    if this is None:
        raise RuntimeError("callback package is not present in resolved context: " + _execution_callback["package_name"])
    version = this.version
    root = this.root
    base = this.base
    for _binding_name, _binding_value in _execution_callback["bindings"].items():
        globals()[_binding_name] = _DictBinding(_binding_value, _binding_name) if isinstance(_binding_value, dict) else _binding_value
    _actions.append({"t": "pkg_begin", "phase": _execution_callback["name"], "name": this.name, "version": str(this.version)})
    _exec_rex(_execution_callback["code"], _execution_callback["name"] + ".py")
"#
);

/// Preamble for bundle post_commands.py: same bindings as package rex, but exec user code once (no package loop).
/// Injected: _rex_data, _post_code.
#[doc(hidden)]
pub const REX_POST_BINDINGS: &str = rex_preamble!(
    r#"class _DummyPackage:
    root = base = name = ''
    version = _VersionBinding('', [])
this = list(_variants.values())[0] if _variants else _DummyPackage()
version = this.version
root = this.root
base = this.base

_exec_rex(_post_code, 'post_commands.py')
"#
);

/// Execute package commands via embedded RustPython with full binding support.
/// Replaces the old text parser with real Python execution (env, this, resolve, etc.).
use rex::apply_rex_actions;

fn rex_version_tokens(version: &version::Version) -> Vec<serde_json::Value> {
    (0..version.len())
        .map(|index| {
            let token = version.get(index).expect("valid token index").to_string();
            if token == "0" || !token.starts_with('0') {
                token.parse::<i64>().map_or_else(
                    |_| serde_json::Value::String(token.clone()),
                    |number| serde_json::Value::Number(number.into()),
                )
            } else {
                serde_json::Value::String(token)
            }
        })
        .collect()
}

fn rex_package_data(
    resolved_pkgs: &[ResolvedPackageInfo],
    ctx: &ResolvedContext,
    interpreter: Option<&dyn ActionInterpreter>,
) -> Result<Vec<serde_json::Value>> {
    // Late binding context for in_context=true evaluation
    let lb_ctx = python_runtime::LateBindingContext {
        request: ctx
            .package_requests
            .iter()
            .map(|r| (r.name().to_string(), r.to_string()))
            .collect(),
        implicits: ctx
            .implicit_packages
            .iter()
            .map(|r| (r.name().to_string(), r.to_string()))
            .collect(),
        system: [
            ("platform".into(), ctx.platform_str.clone()),
            ("arch".into(), ctx.arch.clone()),
            ("os".into(), ctx.os_str.clone()),
        ]
        .into(),
        building: ctx.building,
        testing: ctx.testing,
    };

    // Preserve complete canonical metadata; evaluated fields and resolved identity win.
    resolved_pkgs
        .iter()
        .map(|pkg| -> Result<serde_json::Value> {
            let root = pkg
                .root
                .as_ref()
                .or(pkg.repo_path.as_ref())
                .map(|p| {
                    let path = p.display().to_string();
                    interpreter.map_or_else(|| path.clone(), |i| i.normalize_path(&path))
                })
                .unwrap_or_default();
            let base = pkg
                .repo_path
                .as_ref()
                .map(|p| {
                    let path = p.display().to_string();
                    interpreter.map_or_else(|| path.clone(), |i| i.normalize_path(&path))
                })
                .unwrap_or_default();
            let tokens = rex_version_tokens(&pkg.version);

            let mut pkg_obj = serde_json::json!({
                "name": pkg.name,
                "version": pkg.version.to_string(),
                "tokens": tokens,
                "root": root,
                "base": base,
                "variant_index": pkg.variant_index,
                "pre_commands": pkg.pre_commands,
                "commands": pkg.commands,
                "post_commands": pkg.post_commands,
            });

            let mut include_config = None;

            // Load raw package data and re-evaluate late-bound tools/requires with in_context=true
            if let Some(raw_data) = pkg.load_data()? {
                include_config = raw_data.get("config").cloned();
                for (key, value) in &raw_data {
                    pkg_obj
                        .as_object_mut()
                        .expect("package binding object")
                        .entry(key.clone())
                        .or_insert_with(|| value.clone());
                }
                let mut pkg_data = raw_data;
                pkg_data.insert("root".into(), serde_json::Value::String(root.clone()));
                pkg_data.insert("base".into(), serde_json::Value::String(base.clone()));
                if let Some(idx) = pkg.variant_index {
                    pkg_data.insert(
                        "variant_index".into(),
                        serde_json::Value::Number(idx.into()),
                    );
                }

                if let Some(tools_val) =
                    crate::serialise::resolve_field_for_context(&pkg_data, "tools", Some(&lb_ctx))?
                {
                    pkg_obj["tools"] = tools_val;
                }
                if let Some(req_val) = crate::serialise::resolve_field_for_context(
                    &pkg_data,
                    "requires",
                    Some(&lb_ctx),
                )? {
                    pkg_obj["requires"] = req_val;
                }
            }

            // Materialize decorated command includes from the installed package base.
            // Preserve the complete function source so decorators and module namespaces survive.
            for (field, source) in [
                ("pre_commands", pkg.pre_commands.as_deref()),
                ("commands", pkg.commands.as_deref()),
                ("post_commands", pkg.post_commands.as_deref()),
            ] {
                if let Some(source) = source {
                    pkg_obj[field] =
                        serde_json::Value::String(crate::serialise::source_with_includes(
                            source,
                            pkg.repo_path.as_deref(),
                            include_config.as_ref(),
                            true,
                        )?);
                }
            }

            Ok(pkg_obj)
        })
        .collect::<Result<Vec<_>>>()
}

fn exec_rex_py<I: ActionInterpreter>(
    executor: &mut RexExecutor<I>,
    resolved_pkgs: &[ResolvedPackageInfo],
    ctx: &ResolvedContext,
    callback: Option<&RexExecutionCallback>,
) -> Result<()> {
    // Check if any package has commands at all
    let has_cmds = resolved_pkgs
        .iter()
        .any(|p| p.commands.is_some() || p.pre_commands.is_some() || p.post_commands.is_some());
    if !has_cmds && callback.is_none() {
        return Ok(());
    }

    let packages_json = rex_package_data(resolved_pkgs, ctx, Some(executor.interpreter()))?;

    let request_map: serde_json::Map<String, serde_json::Value> = ctx
        .package_requests
        .iter()
        .map(|r| {
            (
                r.name().to_string(),
                serde_json::Value::String(r.to_string()),
            )
        })
        .collect();
    let implicits_map: serde_json::Map<String, serde_json::Value> = ctx
        .implicit_packages
        .iter()
        .map(|r| {
            (
                r.name().to_string(),
                serde_json::Value::String(r.to_string()),
            )
        })
        .collect();
    let ephemerals_map: serde_json::Map<String, serde_json::Value> = ctx
        .resolved_ephemerals
        .as_ref()
        .map(|ephs| {
            ephs.iter()
                .filter_map(|e| {
                    let name = e.name();
                    let stripped = name.strip_prefix('.')?;
                    Some((
                        stripped.to_string(),
                        serde_json::Value::String(e.to_string()),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();

    let system_parent = std::env::vars().collect::<HashMap<_, _>>();
    let execution_callback = callback
        .map(|callback| -> Result<serde_json::Value> {
            let (package, base, config) = if let Some(variant) = &callback.package {
                if variant.name() != callback.package_name {
                    return Err(RezError::Rex("Callback package identity mismatch".into()));
                }
                let parent = &variant.parent;
                let mut data = serde_json::to_value(parent.to_data()?)?;
                data["tokens"] = serde_json::json!(rex_version_tokens(&parent.version));
                data["base"] = serde_json::json!(parent
                    .base
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default());
                data["root"] = serde_json::json!(variant
                    .root()
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default());
                for key in ["base", "root"] {
                    let path = data[key].as_str().expect("callback path string");
                    data[key] = serde_json::json!(executor.interpreter().normalize_path(path));
                }
                data["index"] = serde_json::json!(variant.index);
                data["variant_index"] = serde_json::json!(variant.index);
                data["is_variant"] = serde_json::json!(variant.is_variant());
                data["variant_requires"] = serde_json::json!(variant
                    .variant_requires
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>());
                data["requires"] = serde_json::json!(variant
                    .requires()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>());
                (Some(data), parent.base.as_deref(), parent.config.as_ref())
            } else {
                let index = resolved_pkgs
                    .iter()
                    .position(|package| package.name == callback.package_name)
                    .ok_or_else(|| {
                        RezError::Rex(format!(
                            "Callback package {} is not present in the context",
                            callback.package_name
                        ))
                    })?;
                (
                    None,
                    resolved_pkgs[index].repo_path.as_deref(),
                    packages_json[index].get("config"),
                )
            };
            let code = crate::serialise::source_with_includes(
                &callback.code,
                if callback.developer { None } else { base },
                config,
                true,
            )?;
            Ok(serde_json::json!({
                "package_name": callback.package_name,
                "name": callback.name,
                "package": package,
                "bindings": callback.bindings,
                "code": code,
            }))
        })
        .transpose()?;
    let rex_data = serde_json::json!({
        "optionvars": CONFIG.optionvars,
        "catch_rex_errors": CONFIG.catch_rex_errors,
        "environment": {
            "parent": executor.manager().parent_env(),
            "current": executor.env(),
            "separators": executor.manager().separators(),
            "separator": executor.manager().env_sep(""),
        },
        "execution_callback": execution_callback,
        "system": crate::platform::SYSTEM.rex_data(&system_parent)?,
        "building": ctx.building,
        "testing": ctx.testing,
        "packages": packages_json,
        "request": request_map,
        "implicits": implicits_map,
        "ephemerals": ephemerals_map,
    });

    let mut inject = HashMap::new();
    inject.insert("_rex_data".to_string(), rex_data);

    let result = crate::python_vm::exec_py_globals_for_rex(
        REX_PREAMBLE,
        "<rex>",
        Some(&inject),
        &[
            "_rex_data",
            "EscapedString",
            "literal",
            "expandable",
            "_expand_text",
            "_formatter",
            "_NamespaceFormatter",
            "_Formatter",
            "_re",
            "format",
            "expandvars",
            "_expand_val",
            "_raw_code",
            "_RexEnv",
            "_EnvVar",
            "_VariantBinding",
            "_DictBinding",
            "_VersionBinding",
            "_SystemInfo",
            "_variants",
            "_p",
            "_phase",
            "_pkg_data",
            "_cmds",
            "env",
            "system",
            "resolve",
            "request",
            "implicits",
            "ephemerals",
            "building",
            "testing",
            "this",
            "version",
            "root",
            "base",
            "scope",
            "_RexError",
            "_RexStopError",
            "_RexUndefinedVariableError",
            "_exec_rex",
            "_env_action",
            "setenv",
            "unsetenv",
            "resetenv",
            "prependenv",
            "appendenv",
            "optionvars",
            "shebang",
            "getenv",
            "defined",
            "alias",
            "info",
            "error",
            "source",
            "command",
            "stop",
            "comment",
            "undefined",
            "intersects",
        ],
    );

    let globals = result?;

    let actions = globals
        .get("_actions")
        .ok_or_else(|| RezError::Rex("Python Rex bridge did not return actions".into()))?;
    apply_rex_actions(executor, actions)
}

/// Execute bundle post_commands.py if loading from a bundle dir that contains it.
/// Matches Python rez resolved_context.py:_execute_bundle_post_actions_callback.
fn execute_bundle_post_commands_if_present<I: ActionInterpreter>(
    executor: &mut RexExecutor<I>,
    ctx: &ResolvedContext,
    resolved_pkgs: &[ResolvedPackageInfo],
) -> Result<()> {
    let Some(ref load_path) = ctx.load_path else {
        return Ok(());
    };
    let bundle_dir = if load_path.is_file() {
        load_path.parent().map(|p| p.to_path_buf())
    } else {
        Some(load_path.clone())
    };
    let Some(bundle_dir) = bundle_dir else {
        return Ok(());
    };
    if !bundle_dir.join("bundle.yaml").exists() {
        return Ok(());
    }
    let post_path = bundle_dir.join("post_commands.py");
    if !post_path.try_exists()? {
        return Ok(());
    }
    let content = std::fs::read_to_string(&post_path)?;
    executor.comment("bundle post-commands");
    exec_rex_post_commands(executor, ctx, &content, resolved_pkgs)
}

/// Run post_commands.py rex code with same bindings as package commands (env, resolve, this, etc.).
fn exec_rex_post_commands<I: ActionInterpreter>(
    executor: &mut RexExecutor<I>,
    ctx: &ResolvedContext,
    post_code: &str,
    resolved_pkgs: &[ResolvedPackageInfo],
) -> Result<()> {
    let packages_json = rex_package_data(resolved_pkgs, ctx, Some(executor.interpreter()))?;

    let request_map: serde_json::Map<String, serde_json::Value> = ctx
        .package_requests
        .iter()
        .map(|r| {
            (
                r.name().to_string(),
                serde_json::Value::String(r.to_string()),
            )
        })
        .collect();
    let implicits_map: serde_json::Map<String, serde_json::Value> = ctx
        .implicit_packages
        .iter()
        .map(|r| {
            (
                r.name().to_string(),
                serde_json::Value::String(r.to_string()),
            )
        })
        .collect();
    let ephemerals_map: serde_json::Map<String, serde_json::Value> = ctx
        .resolved_ephemerals
        .as_ref()
        .map(|ephs| {
            ephs.iter()
                .filter_map(|e| {
                    let name = e.name();
                    let stripped = name.strip_prefix('.')?;
                    Some((
                        stripped.to_string(),
                        serde_json::Value::String(e.to_string()),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();

    let system_parent = std::env::vars().collect::<HashMap<_, _>>();
    let rex_data = serde_json::json!({
        "optionvars": CONFIG.optionvars,
        "catch_rex_errors": CONFIG.catch_rex_errors,
        "environment": {
            "parent": executor.manager().parent_env(),
            "current": executor.env(),
            "separators": executor.manager().separators(),
            "separator": executor.manager().env_sep(""),
        },
        "system": crate::platform::SYSTEM.rex_data(&system_parent)?,
        "building": ctx.building,
        "testing": ctx.testing,
        "packages": packages_json,
        "request": request_map,
        "implicits": implicits_map,
        "ephemerals": ephemerals_map,
    });

    let mut inject = HashMap::new();
    inject.insert("_rex_data".to_string(), rex_data);
    inject.insert(
        "_post_code".to_string(),
        serde_json::Value::String(post_code.to_string()),
    );

    let result = crate::python_vm::exec_py_globals_for_rex(
        REX_POST_BINDINGS,
        "<post_commands>",
        Some(&inject),
        &[
            "_rex_data",
            "_post_code",
            "EscapedString",
            "literal",
            "expandable",
            "_expand_text",
            "_formatter",
            "_NamespaceFormatter",
            "_Formatter",
            "_re",
            "format",
            "expandvars",
            "_expand_val",
            "_raw_code",
            "_RexEnv",
            "_EnvVar",
            "_VariantBinding",
            "_DictBinding",
            "_VersionBinding",
            "_SystemInfo",
            "_variants",
            "_p",
            "env",
            "system",
            "resolve",
            "request",
            "implicits",
            "ephemerals",
            "building",
            "testing",
            "this",
            "version",
            "root",
            "base",
            "scope",
            "_RexError",
            "_RexStopError",
            "_RexUndefinedVariableError",
            "_exec_rex",
            "_env_action",
            "setenv",
            "unsetenv",
            "resetenv",
            "prependenv",
            "appendenv",
            "optionvars",
            "shebang",
            "getenv",
            "defined",
            "alias",
            "info",
            "error",
            "source",
            "command",
            "stop",
            "comment",
            "undefined",
            "intersects",
        ],
    );

    let globals = result?;
    let actions = globals
        .get("_actions")
        .ok_or_else(|| RezError::Rex("Python Rex bridge did not return actions".into()))?;
    apply_rex_actions(executor, actions)
}

impl std::fmt::Display for ResolvedContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let req_str: String = self
            .requested_packages(true)
            .iter()
            .map(|r| r.to_string())
            .collect::<Vec<_>>()
            .join(" ");

        if self.status == ResolverStatus::Solved {
            let res_str: String = self
                .resolved_packages
                .as_ref()
                .map(|pkgs| {
                    pkgs.iter()
                        .map(|p| p.qualified_name())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            write!(f, "solved({req_str} ==> {res_str})")
        } else {
            write!(f, "ResolvedContext:{}({req_str})", self.status)
        }
    }
}

// =============================================================================
// ContextChanges - tracks env var modifications
// =============================================================================

/// Summary of environment variable changes a context makes.
#[derive(Clone, Debug, Default)]
pub struct ContextChanges {
    /// New env vars added by the context.
    pub added: HashMap<String, String>,
    /// Env vars removed by the context.
    pub removed: Vec<String>,
    /// Modified env vars: key -> (old_value, new_value).
    pub modified: HashMap<String, (String, String)>,
}

impl ContextChanges {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.modified.is_empty()
    }
}

// =============================================================================
// Helpers
// =============================================================================

/// Get current epoch time in seconds.
fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// chrono_lite and is_leap_year moved above for forward reference

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use repository::provider::{ResourceHandleKey, ResourceHandleVariables};
    use std::str::FromStr;
    use version::Version;

    #[cfg(windows)]
    fn context_short_path(path: &Path) -> PathBuf {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};

        #[link(name = "kernel32")]
        extern "system" {
            fn GetShortPathNameW(long_path: *const u16, short_path: *mut u16, size: u32) -> u32;
        }

        let input = path
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let required = unsafe { GetShortPathNameW(input.as_ptr(), std::ptr::null_mut(), 0) };
        assert!(required > 0, "{}", std::io::Error::last_os_error());
        let mut output = vec![0; required as usize];
        let written = unsafe { GetShortPathNameW(input.as_ptr(), output.as_mut_ptr(), required) };
        assert!(
            written > 0 && written < required,
            "{}",
            std::io::Error::last_os_error()
        );
        let short = PathBuf::from(std::ffi::OsString::from_wide(&output[..written as usize]));
        assert_ne!(
            foundation::util::path_key(&short),
            foundation::util::path_key(path),
            "Windows regression requires an actual 8.3 alias"
        );
        short
    }

    #[cfg(windows)]
    fn context_alias_package(root: PathBuf) -> ResolvedPackageInfo {
        ResolvedPackageInfo {
            name: "python".into(),
            version: Version::new("1").unwrap(),
            variant_index: None,
            resource_handle: None,
            requires: Vec::new(),
            repo_path: None,
            root: Some(root),
            commands: None,
            pre_commands: None,
            post_commands: None,
        }
    }

    #[cfg(windows)]
    #[test]
    fn which_retains_package_owner_across_windows_path_aliases() {
        let temporary = tempfile::tempdir().unwrap();
        let authority = temporary
            .path()
            .join("Context package authority with spaces");
        let bin = authority.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("python.exe"), b"which fixture").unwrap();
        let long = authority.canonicalize().unwrap();
        let short = context_short_path(&long);
        let normal = foundation::util::path_key(&long);

        for (root, path_root) in [(short.clone(), long.clone()), (normal, short.clone())] {
            let mut package = context_alias_package(root.clone());
            let path = path_root.join("bin");
            package.commands = Some(format!(
                "env.PATH = {}\n",
                serde_json::to_string(&path.to_string_lossy()).unwrap()
            ));
            let mut context = ResolvedContext::empty();
            context.status = ResolverStatus::Solved;
            context.resolved_packages = Some(vec![package]);
            let (owner, command) = context.which("python").expect("fixture command");
            assert_eq!(owner, "python");
            assert_eq!(command, path.join("python.exe"));
            assert_eq!(
                context.resolved_packages.as_ref().unwrap()[0].root,
                Some(root)
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn local_resolve_retains_provenance_across_windows_path_aliases() {
        let temporary = tempfile::tempdir().unwrap();
        let authority = temporary
            .path()
            .join("Context local repository with spaces");
        std::fs::create_dir_all(authority.join("python/1")).unwrap();
        let long = authority.canonicalize().unwrap();
        let short = context_short_path(&long);
        let mut package = context_alias_package(short.join("python/1"));
        package.repo_path = Some(short.clone());
        let original = package.clone();
        assert_eq!(
            local_resolve_names(std::slice::from_ref(&package), &long),
            "python-1"
        );
        assert_eq!(
            local_resolve_names(std::slice::from_ref(&package), &short),
            "python-1"
        );
        assert_eq!(package.repo_path, original.repo_path);
        assert_eq!(package.root, original.root);

        // Missing repository provenance uses the payload root, including mixed aliases.
        package.repo_path = None;
        assert_eq!(
            local_resolve_names(std::slice::from_ref(&package), &long),
            "python-1"
        );
        let outside = temporary
            .path()
            .join("Context local repository with spaces sibling");
        std::fs::create_dir_all(&outside).unwrap();
        package.repo_path = Some(outside);
        assert!(local_resolve_names(&[package], &long).is_empty());
    }

    #[test]
    fn cached_roots_are_transient_and_input_policy_precedes_cache_update() {
        let owned = tempfile::tempdir().unwrap();
        let repo = owned.path().join("repo");
        let cache_root = owned.path().join("cache");
        std::fs::create_dir(&cache_root).unwrap();
        write_package(
            &repo,
            "cache_probe",
            "1",
            "name: cache_probe\nversion: '1'\ncachable: true\ncommands: |\n  env.PROBE_ROOT = this.root\n",
        );
        std::fs::write(repo.join("cache_probe/1/payload"), "cached").unwrap();
        let handle = ResourceHandle {
            key: ResourceHandleKey::FilesystemVariant,
            variables: ResourceHandleVariables {
                repository_type: "filesystem".into(),
                location: repo.to_string_lossy().into_owned(),
                name: "cache_probe".into(),
                version: Some("1".into()),
                index: None,
                ext: None,
            },
        };
        let provider = FilesystemPackageProvider::from_paths(&[]).unwrap();
        let variant = provider
            .get_candidate_for_handle(&handle)
            .unwrap()
            .into_variant(None, false)
            .unwrap();
        let cache = crate::package::cache::PackageCache::new(&cache_root).unwrap();
        let config = crate::config::RezConfig {
            cache_packages_path: Some(cache_root.to_string_lossy().into_owned()),
            read_package_cache: true,
            ..Default::default()
        };
        let (cached, _) = cache.add_variant(&variant, true, Some(&config)).unwrap();
        let mut context = ResolvedContext::empty();
        context.status = ResolverStatus::Solved;
        context.resolved_packages = Some(vec![ResolvedPackageInfo::from_solver_variant(&variant)]);
        let original = context.to_json().unwrap();
        let bindings = context.execution_packages(Some(&config)).unwrap();
        assert_eq!(bindings[0].root.as_ref(), Some(&cached));
        assert_eq!(bindings[0].repo_path, variant.variant.parent.base);
        assert_eq!(context.to_json().unwrap(), original);
        context.package_caching = false;
        assert_eq!(
            context.execution_packages(Some(&config)).unwrap()[0].root,
            variant.variant.root()
        );
        let loaded = ResolvedContext::from_json(
            &original,
            Some(&ResolveOptions {
                package_caching: Some(false),
                package_cache_async: Some(false),
                ..Default::default()
            }),
        )
        .unwrap();
        assert!(!loaded.package_caching && !loaded.package_cache_async);
        assert_eq!(
            loaded.resolved_packages.unwrap()[0]
                .resource_handle
                .as_ref(),
            Some(&handle)
        );
    }

    #[cfg(windows)]
    #[test]
    fn resolved_shell_completion_preserves_context_native_status() {
        let mut context = ResolvedContext::empty();
        context.status = ResolverStatus::Solved;
        let comspec = std::env::var("COMSPEC").expect("Windows command processor");
        assert!(Path::new(&comspec).is_absolute() && Path::new(&comspec).is_file());
        let native_command = ShellType::PowerShell.join_command(
            &[comspec, "/D".into(), "/C".into(), "exit".into(), "7".into()],
            false,
            None,
        );
        for (command, expected) in [
            (native_command.as_str(), 7),
            ("Write-Error failed", 1),
            (
                "Set-StrictMode -Version Latest\nRemove-Variable LASTEXITCODE -ErrorAction SilentlyContinue\nWrite-Error failed",
                1,
            ),
            ("", 0),
            ("exit 7", 7),
        ] {
            let status = context
                .execute_shell(
                    Some(ShellType::PowerShell),
                    Some(command),
                    Some(std::env::vars().collect()),
                    true,
                    true,
                )
                .unwrap();
            assert_eq!(status.code(), Some(expected), "command={command}");
        }
        context.resolved_packages = Some(vec![ResolvedPackageInfo {
            name: "context_probe".into(),
            version: Version::from_str("1.0").unwrap(),
            variant_index: None,
            resource_handle: None,
            requires: Vec::new(),
            repo_path: None,
            root: None,
            pre_commands: None,
            commands: Some(format!(
                "command({})",
                serde_json::to_string(&native_command).unwrap()
            )),
            post_commands: None,
        }]);
        let status = context
            .execute_shell(
                Some(ShellType::PowerShell),
                Some("Write-Output done"),
                Some(std::env::vars().collect()),
                true,
                true,
            )
            .unwrap();
        assert_eq!(
            status.code(),
            Some(7),
            "context native failure retains Rez priority"
        );
    }

    #[test]
    fn resolve_implicit_selection_preserves_inherit_override_and_disable() {
        let repo = tempfile::tempdir().unwrap();
        write_package(
            repo.path(),
            "explicit_probe",
            "1",
            "name: explicit_probe\nversion: '1'\n",
        );
        write_package(
            repo.path(),
            "implicit_probe",
            "1",
            "name: implicit_probe\nversion: '1'\n",
        );
        let provider = FilesystemPackageProvider::from_path(repo.path()).unwrap();
        let configured = CONFIG
            .resolved_implicit_packages()
            .iter()
            .map(|request| Requirement::new(request))
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let custom = vec![Requirement::new("implicit_probe").unwrap()];
        for (add_implicit, selection, expected) in [
            (true, None, configured),
            (true, Some(Vec::new()), Vec::new()),
            (true, Some(custom.clone()), custom.clone()),
            (false, None, Vec::new()),
            (false, Some(custom), Vec::new()),
        ] {
            let context = ResolvedContext::resolve(
                vec![Requirement::new("explicit_probe").unwrap()],
                &provider,
                ResolveOptions {
                    add_implicit,
                    implicit_packages: selection,
                    package_paths: Some(vec![repo.path().to_path_buf()]),
                    caching: false,
                    ..ResolveOptions::default()
                },
            )
            .unwrap();
            assert_eq!(context.implicit_packages, expected);
            assert_eq!(&context.requested_packages(true)[1..], expected.as_slice());
        }
    }

    fn handle_context(
        repo_path: &Path,
        key: &str,
        name: &str,
        version: &str,
        index: serde_json::Value,
    ) -> serde_json::Value {
        let mut variables = serde_json::json!({
            "repository_type": "filesystem",
            "location": repo_path.to_string_lossy(),
            "name": name,
            "version": version,
            "index": index,
        });
        if key.ends_with(".combined") {
            variables["ext"] = serde_json::json!("yaml");
        }
        serde_json::json!({
            "serialize_version": "4.9",
            "timestamp": 0,
            "building": false,
            "caching": false,
            "implicit_packages": [],
            "package_requests": [],
            "package_paths": [repo_path],
            "rez_version": "3.3.0",
            "rez_path": "",
            "user": "test",
            "host": "test",
            "platform": "linux",
            "arch": "x86_64",
            "os": "Linux",
            "created": 0,
            "status": "solved",
            "failure_description": null,
            "solve_time": 0.0,
            "load_time": 0.0,
            "graph": null,
            "resolved_packages": [{"key": key, "variables": variables}]
        })
    }

    fn write_package(repo: &Path, name: &str, version: &str, definition: &str) {
        let package_dir = repo.join(name).join(version);
        std::fs::create_dir_all(&package_dir).expect("create package directory");
        std::fs::write(package_dir.join("package.yaml"), definition)
            .expect("write package definition");
    }
    #[test]
    fn exact_handles_ignore_unavailable_historical_search_paths() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = temporary.path().join("exact");
        write_package(
            &repository,
            "external",
            "1",
            "name: external\nversion: '1'\ncustom: preserved\ncommands: |\n  env.EXACT_ROOT = '{this.root}'\n",
        );
        let bad_path = temporary.path().join("not_a_repository");
        std::fs::write(&bad_path, "ordinary file").unwrap();
        let mut document = handle_context(
            &repository,
            "filesystem.variant",
            "external",
            "1",
            serde_json::Value::Null,
        );
        document["package_paths"] = serde_json::json!([temporary.path().join("missing"), bad_path]);
        for yaml in [false, true] {
            let file = temporary.path().join(if yaml {
                "context.yaml.rxt"
            } else {
                "context.json.rxt"
            });
            let content = if yaml {
                serde_yaml::to_string(&document).unwrap()
            } else {
                serde_json::to_string_pretty(&document).unwrap()
            };
            std::fs::write(&file, content).unwrap();
            let context = ResolvedContext::load(&file, None).unwrap();
            assert_eq!(
                serde_json::json!(context.package_paths),
                document["package_paths"]
            );
            let env = context.get_environ(Some(HashMap::new())).unwrap();
            assert_eq!(
                env["EXACT_ROOT"],
                repository.join("external").join("1").display().to_string()
            );
        }
    }

    #[test]
    fn exact_combined_handles_preserve_source_identity_through_cache_collisions() {
        use repository::provider::PackageProvider;
        let repository = tempfile::tempdir().unwrap();
        write_package(
            repository.path(),
            "collision",
            "1",
            "name: collision\nversion: '1'\ndescription: directory\n",
        );
        std::fs::write(repository.path().join("collision.py"), "name = 'collision'\nversions = ['1']\ndescription = 'python'\ncustom = 'python metadata'\n@late\ndef tools():\n    return ['python_tool'] if in_context() else []\ndef commands():\n    env.SELECTED = '{this.custom}'\n").unwrap();
        std::fs::write(
            repository.path().join("collision.yaml"),
            "name: collision\nversions: ['1']\ndescription: yaml\ncustom: yaml metadata\ntools: [yaml_tool]\ncommands: |\n  env.SELECTED = '{this.custom}'\n",
        )
        .unwrap();
        let provider = FilesystemPackageProvider::from_path(repository.path()).unwrap();
        for _ in 0..2 {
            assert_eq!(
                provider
                    .get_candidates("collision", &version::VersionRange::any())
                    .unwrap()[0]
                    .package
                    .description
                    .as_deref(),
                Some("directory")
            );
            for (key, ext, description) in [
                ("filesystem.variant", None, "directory"),
                ("filesystem.variant.combined", Some("yaml"), "yaml"),
                ("filesystem.variant.combined", Some("py"), "python"),
            ] {
                let mut document = handle_context(
                    repository.path(),
                    key,
                    "collision",
                    "1",
                    serde_json::Value::Null,
                );
                if let Some(ext) = ext {
                    document["resolved_packages"][0]["variables"]["ext"] = serde_json::json!(ext);
                }
                document["package_paths"] = serde_json::json!([]);
                let handle: ResourceHandle =
                    serde_json::from_value(document["resolved_packages"][0].clone()).unwrap();
                let candidate = provider.get_candidate_for_handle(&handle).unwrap();
                assert_eq!(candidate.package.description.as_deref(), Some(description));
                if ext.is_some() {
                    assert_eq!(
                        candidate.package.attributes["custom"],
                        format!("{description} metadata")
                    );
                }
                let variant = candidate.into_variant(None, false).unwrap();
                assert_eq!(
                    variant
                        .provenance
                        .as_ref()
                        .unwrap()
                        .resource_handle("collision", variant.version(), None)
                        .as_ref(),
                    Some(&handle)
                );
                let restored = ResolvedContext::from_json(&document, None).unwrap();
                if ext.is_some() {
                    assert_eq!(
                        restored.get_environ(Some(HashMap::new())).unwrap()["SELECTED"],
                        format!("{description} metadata")
                    );
                    assert_eq!(
                        restored.get_tools(false).unwrap()["collision"],
                        (
                            "collision-1".to_string(),
                            vec![format!("{description}_tool")]
                        )
                    );
                }
                assert_eq!(
                    restored.resolved_packages().unwrap()[0]
                        .resource_handle
                        .as_ref(),
                    Some(&handle)
                );
            }
        }
    }

    #[test]
    fn bundled_context_locations_survive_move_and_preserve_external_handles() {
        let temporary = tempfile::tempdir().unwrap();
        let original = temporary.path().join("bundle_a");
        let bundled_repo = original.join("repo");
        let external_repo = temporary.path().join("external");
        write_package(
            &bundled_repo,
            "bundled",
            "1",
            "name: bundled\nversion: '1'\ncommands: |\n  env.BUNDLED_ROOT = '{this.root}'\n",
        );
        write_package(
            &external_repo,
            "external",
            "1",
            "name: external\nversion: '1'\ncommands: |\n  env.EXTERNAL_ROOT = '{this.root}'\n",
        );
        std::fs::write(original.join("bundle.yaml"), "{}\n").unwrap();
        std::fs::write(
            original.join("post_commands.py"),
            "setenv('BUNDLE_POST', 'executed')\n",
        )
        .unwrap();
        let mut document = handle_context(
            &bundled_repo,
            "filesystem.variant",
            "bundled",
            "1",
            serde_json::Value::Null,
        );
        document["resolved_packages"].as_array_mut().unwrap().push(
            handle_context(
                &external_repo,
                "filesystem.variant",
                "external",
                "1",
                serde_json::Value::Null,
            )["resolved_packages"][0]
                .clone(),
        );
        document["package_paths"] =
            serde_json::json!([bundled_repo, temporary.path().join("historical_missing")]);
        let context = ResolvedContext::from_json(&document, None).unwrap();
        let file = original.join("context.rxt");
        context.save(&file).unwrap();
        let serialized: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(
            serialized["resolved_packages"][0]["variables"]["location"],
            "repo"
        );
        assert_eq!(
            serialized["resolved_packages"][1]["variables"]["location"],
            document["resolved_packages"][1]["variables"]["location"]
        );
        assert_eq!(
            context.to_json().unwrap()["resolved_packages"],
            document["resolved_packages"]
        );
        std::fs::write(
            original.join("context_yaml.rxt"),
            serde_yaml::to_string(&serialized).unwrap(),
        )
        .unwrap();
        let moved = temporary.path().join("bundle_b");
        std::fs::rename(&original, &moved).unwrap();
        assert!(!original.exists());
        for name in ["context.rxt", "context_yaml.rxt"] {
            let restored = ResolvedContext::load(&moved.join(name), None).unwrap();
            assert_eq!(
                serde_json::json!(restored.package_paths),
                document["package_paths"]
            );
            let env = restored.get_environ(Some(HashMap::new())).unwrap();
            assert_eq!(
                env["BUNDLED_ROOT"],
                moved
                    .join("repo/bundled/1")
                    .canonicalize()
                    .unwrap()
                    .display()
                    .to_string()
            );
            assert_eq!(
                env["EXTERNAL_ROOT"],
                external_repo
                    .join("external")
                    .join("1")
                    .display()
                    .to_string()
            );
            assert_eq!(env["BUNDLE_POST"], "executed");
            assert_eq!(
                restored.resolved_packages().unwrap()[1]
                    .resource_handle
                    .as_ref()
                    .unwrap()
                    .variables
                    .location,
                external_repo.to_string_lossy()
            );
            restored.save(&moved.join("saved_again.rxt")).unwrap();
            let again: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(moved.join("saved_again.rxt")).unwrap(),
            )
            .unwrap();
            assert_eq!(
                again["resolved_packages"][0]["variables"]["location"],
                "repo"
            );
        }
        let mut empty_location = serialized.clone();
        empty_location["resolved_packages"][0]["variables"]["location"] = serde_json::json!("");
        std::fs::write(
            moved.join("empty_location.rxt"),
            serde_json::to_string(&empty_location).unwrap(),
        )
        .unwrap();
        let error = ResolvedContext::load(&moved.join("empty_location.rxt"), None).unwrap_err();
        assert!(error.to_string().contains("non-empty location"), "{error}");
        let mut invalid = serialized;
        invalid["resolved_packages"][0]["variables"]
            .as_object_mut()
            .unwrap()
            .remove("index");
        std::fs::write(
            moved.join("invalid.rxt"),
            serde_json::to_string(&invalid).unwrap(),
        )
        .unwrap();
        let error = ResolvedContext::load(&moved.join("invalid.rxt"), None).unwrap_err();
        assert!(error.to_string().contains("variables.index"), "{error}");
    }

    #[test]
    fn test_get_tools_propagates_late_binding_errors() {
        let repo = tempfile::tempdir().expect("repository tempdir");
        write_package(
            repo.path(),
            "late_tools",
            "1.0.0",
            "name: late_tools\nversion: 1.0.0\ntools: |\n  def tools():\n      if in_context():\n          raise ValueError('context tools failed')\n      return []\n",
        );
        let provider = FilesystemPackageProvider::from_path(repo.path()).unwrap();
        let context = ResolvedContext::resolve(
            vec![Requirement::new("late_tools").unwrap()],
            &provider,
            ResolveOptions {
                package_paths: Some(vec![repo.path().to_path_buf()]),
                add_implicit: false,
                caching: false,
                ..ResolveOptions::default()
            },
        )
        .expect("resolve package with non-context late tools");

        let error = context.get_tools(false).unwrap_err();
        assert!(
            error.to_string().contains("context tools failed"),
            "{error}"
        );
    }

    #[test]
    fn test_get_tools_rejects_failed_contexts() {
        let mut context = ResolvedContext::empty();
        context.status = ResolverStatus::Failed;

        let error = context.get_tools(false).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Cannot perform operation in a failed context"),
            "{error}"
        );
    }

    #[test]
    fn test_get_tools_request_scope_and_conflict_providers() {
        let repo = tempfile::tempdir().expect("repository tempdir");
        write_package(
            repo.path(),
            "root",
            "1.0.0",
            "name: root\nversion: 1.0.0\nrequires:\n  - dep\ntools:\n  - shared\n  - shared\n",
        );
        write_package(
            repo.path(),
            "dep",
            "1.0.0",
            "name: dep\nversion: 1.0.0\ntools:\n  - shared\n",
        );

        let provider = FilesystemPackageProvider::from_path(repo.path()).unwrap();
        let context = ResolvedContext::resolve(
            vec![Requirement::new("root").unwrap()],
            &provider,
            ResolveOptions {
                package_paths: Some(vec![repo.path().to_path_buf()]),
                add_implicit: false,
                caching: false,
                ..ResolveOptions::default()
            },
        )
        .expect("resolve root and dependency");

        let (all_tools, all_conflicts) = context
            .get_tools_with_conflicts(false)
            .expect("tools for all resolved packages");
        assert!(all_tools.contains_key("root"));
        assert!(all_tools.contains_key("dep"));
        assert_eq!(
            all_conflicts.get("shared"),
            Some(&vec!["dep".into(), "root".into()])
        );

        let (requested_tools, requested_conflicts) = context
            .get_tools_with_conflicts(true)
            .expect("tools for explicitly requested packages");
        assert_eq!(
            requested_tools
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["root"]
        );
        assert!(requested_conflicts.is_empty());
        assert_eq!(requested_tools["root"].1, vec!["shared"]);
    }

    #[test]
    fn test_rex_system_binding_properties() {
        let state = serde_json::json!({
            "system": crate::platform::SYSTEM.rex_data(&HashMap::new()).unwrap(),
            "environment": {"parent": {}, "current": {}, "separators": {}, "separator": ":"},
            "packages": [],
        });
        let code = r#"
assert isinstance(system.rez_version, str)
assert system.variant == ["platform-%s" % system.platform, "arch-%s" % system.arch, "os-%s" % system.os]
assert isinstance(system.shell, str)
assert isinstance(system.user, str)
assert isinstance(system.home, str)
assert isinstance(system.hostname, str)
assert system.is_production_rez_install == bool(system.rez_bin_path)
assert isinstance(system.selftest_is_running, bool)
import socket
_saved_getfqdn = socket.getfqdn
_calls = []
def _getfqdn():
    _calls.append(True)
    return "workstation.example.test"
socket.getfqdn = _getfqdn
try:
    assert system.fqdn == "workstation.example.test"
    assert system.domain == "example.test"
    assert system.fqdn == "workstation.example.test"
    assert len(_calls) == 1
finally:
    socket.getfqdn = _saved_getfqdn
setenv("SYSTEM_CHECK", "accepted")
"#;
        let inject = HashMap::from([
            ("_rex_data".to_owned(), state),
            ("_post_code".to_owned(), serde_json::json!(code)),
        ]);
        let globals = crate::python_vm::exec_py_globals_for_rex(
            REX_POST_BINDINGS,
            "<system_properties>",
            Some(&inject),
            &[],
        )
        .unwrap();
        assert_eq!(globals["_actions"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_rex_public_bindings_and_namespace_isolation() {
        let state = serde_json::json!({
            "environment": {
                "parent": {"PARENT": "inherited", "KEY_A": "$KEY_B", "KEY_B": "TARGET", "$KEY_B": "reference", "TARGET": "double"},
                "current": {},
                "separators": {},
                "separator": ":"
            },
            "optionvars": {"pipeline": {"enabled": false}, "scalar": 3},
            "system": crate::platform::SYSTEM.rex_data(&HashMap::new()).unwrap(),
            "packages": []
        });
        let code = r#"
number = 7
nested = "{number:03}"
assert format("{nested}") == "007"
assert format("$PARENT") == "${PARENT}"
assert format("{{number}}") == "{number}"
assert optionvars("pipeline.enabled", True) is False
assert optionvars("pipeline.missing", 99) == 99
try:
    optionvars("scalar.child")
except RuntimeError:
    pass
else:
    raise AssertionError("invalid nested optionvar accepted")
setenv("GLOBAL", "a")
env.GLOBAL.append("b")
prependenv("GLOBAL", "c")
assert getenv("GLOBAL") == "c:a:b"
resetenv("GLOBAL", literal("$PARENT"))
assert getenv("GLOBAL") == "$PARENT"
unsetenv("GLOBAL")
assert undefined("GLOBAL")
assert defined("PARENT")
assert getenv("$KEY_A") == "reference"
assert getenv("$KEY_B") == "double"
assert defined("$KEY_A")
setenv("EXPANDED", expandvars("$PARENT"))
assert getenv("EXPANDED") == "inherited"
assert expandvars(literal("$PARENT"), format=False) == "$PARENT"
assert expandvars(literal("$PARENT")) == "inherited"
assert _DictBinding({"tool": "tool-1+<3"}).get_range("tool").intersects("2")
assert _DictBinding({"tool": "tool"}).get_range("tool").is_any()
assert _DictBinding({"tool": "~tool"}).get_range("tool") is None
assert _DictBinding({}).get_range("tool", "2").intersects("2")
assert _DictBinding({}).get_range("tool") is None
assert intersects("foo", ">=99")
assert not intersects("!foo-1", "1")
from rez.exceptions import RexError, RexStopError, RexUndefinedVariableError
assert issubclass(RexStopError, RexError)
assert issubclass(RexUndefinedVariableError, RexError)
try:
    getenv("MISSING_SHARED_EXCEPTION")
except RexUndefinedVariableError:
    pass
else:
    raise AssertionError("undefined did not use shared Rez exception")
try:
    stop("shared %s", "exception")
except RexStopError as exc:
    assert str(exc) == "shared exception"
else:
    raise AssertionError("stop did not use shared Rez exception")
class CustomStop(RexStopError):
    pass
try:
    raise CustomStop("subclass")
except RexStopError:
    pass
assert intersects(_VersionBinding("1", [1]), "1.2")
assert intersects(_RequirementRange(""), "1.2")
class DerivedVersion(_VersionBinding):
    pass
class DerivedRange(_RequirementRange):
    pass
class DerivedVariant(_VariantBinding):
    pass
assert intersects(DerivedVersion("1", [1]), "1.2")
assert intersects(DerivedRange("1"), "1.2")
assert intersects(DerivedVariant({"name":"foo", "version":"1", "tokens":[1], "root":"", "base":""}), "1.2")
derived_range = DerivedRange("1")
for binding_name in ("_VariantBinding", "_VersionBinding", "_RequirementRange"):
    fake_type = type(binding_name, (), {"__str__": lambda self: "1", "version": "1"})
    saved_type = globals()[binding_name]
    globals()[binding_name] = fake_type
    saved_native = _rez_version_native
    _rez_version_native = lambda *args: True
    try:
        try:
            intersects(fake_type(), "1")
        except RuntimeError:
            pass
        else:
            raise AssertionError("same-named fake binding accepted")
        assert not intersects(derived_range, "2")
    finally:
        globals()[binding_name] = saved_type
        _rez_version_native = saved_native
v = _VersionBinding("1.2.3alpha", [1, 2, "3alpha"])
assert (v.major, v.minor, v.patch) == (1, 2, "3alpha")
assert len(v) == 3 and v[1] == 2 and v[:2] == (1, 2)
assert str(v) == "1.2.3alpha" and v[5] is None
assert v.as_tuple() == (1, 2, "3alpha")
foo = _VariantBinding({"name":"foo", "version":"1", "tokens":[1], "root":"", "base":""})
maya = _VariantBinding({"name":"maya", "version":"2020.1", "tokens":[2020,1], "root":"", "base":""})
assert intersects(foo, "1") and not intersects(foo, "0")
assert intersects(maya, "2019+") and not intersects(maya, "<=2019")
for label, name in (("request", "foo.bar"), ("ephemeral", "foo.bar")):
    binding = _DictBinding({name: ("." if label == "ephemeral" else "") + name + "-1"}, label)
    absent = _DictBinding({}, label)
    assert intersects(binding.get(name, "0"), "1")
    assert intersects(absent.get(name, "0"), "1")
    assert not intersects(absent.get(name, "foo.bar-0"), "1")
    assert intersects(binding.get_range(name, "0"), "1")
    assert not intersects(absent.get_range(name, "0"), "1")
    assert intersects(absent.get_range("foo", "==1.2.3"), "1.2")
    assert not intersects(absent.get_range("foo", "==1.2.3"), "1.4")
    resolved = _DictBinding({"foo": ("." if label == "ephemeral" else "") + "foo-1.4.5"}, label)
    assert intersects(resolved.get_range("foo", "==1.2.3"), "1.4")
    for missing in (absent.get(name), absent.get_range(name)):
        try:
            intersects(missing, "0")
        except RuntimeError:
            pass
        else:
            raise AssertionError("missing range silently accepted")
try:
    intersects(object(), "1")
except RuntimeError:
    pass
else:
    raise AssertionError("invalid intersects object accepted")
env._PRIVATE = "private"
assert getenv("_PRIVATE") == "private"
info()
info(value="message")
error(value="error")
alias(key="tool", value="tool --help")
comment(value="comment")
command(value=["program", "argument with spaces", literal("$PARENT"), expandable("$PARENT"), "literal{namespace}"])
source(value="setup")
shebang()
assert repr(literal("x")) == "EscapedString([(True, 'x')])"
assert EscapedString.join(":", [literal("a"), literal("b")]).strings == [(True, "a"), (False, ":"), (True, "b")]
_exec_rex("leaked = object(); setenv('ISOLATED', 'yes')")
assert "leaked" not in globals()
setenv("RESULT", "accepted")
"#;
        let inject = HashMap::from([
            ("_rex_data".to_owned(), state),
            ("_post_code".to_owned(), serde_json::json!(code)),
        ]);
        let globals = crate::python_vm::exec_py_globals_for_rex(
            REX_POST_BINDINGS,
            "<public_rex_bindings>",
            Some(&inject),
            &[],
        )
        .expect("complete Rex public bindings execute");
        let mut executor = RexExecutor::new(
            crate::shell::rex::PythonInterpreter::new(),
            Some(HashMap::from([("PARENT".into(), "inherited".into())])),
            false,
        );
        let arguments = globals["_actions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["t"] == "command_args")
            .unwrap();
        assert_eq!(
            arguments["args"],
            serde_json::json!([
                [{"literal": true, "text": "program"}],
                [{"literal": false, "text": "argument with spaces"}],
                [{"literal": true, "text": "$PARENT"}],
                [{"literal": false, "text": "$PARENT"}],
                [{"literal": false, "text": "literal{namespace}"}]
            ])
        );
        apply_rex_actions(&mut executor, &globals["_actions"]).unwrap();
        assert_eq!(executor.getenv("RESULT").unwrap(), "accepted");
        assert_eq!(executor.getenv("ISOLATED").unwrap(), "yes");
        assert_eq!(executor.getenv("_PRIVATE").unwrap(), "private");
    }

    #[test]
    fn test_rex_reference_environment_corpus() {
        use crate::shell::rex::PythonInterpreter;
        // Semantic vectors from Python Rez test_rex.py: native VM + canonical manager,
        // not a replacement Python executor. Parent-policy variants are exercised
        // separately by the ActionManager tests.
        let cases = [
            (
                "assignments",
                "env.FOO='foo'; setenv('BAH','bah'); env.EEK=env.FOO",
                serde_json::json!({}),
                serde_json::json!({}),
                serde_json::json!({"FOO":"foo","BAH":"bah","EEK":"foo"}),
            ),
            (
                "pend_first_reference",
                "appendenv('FOO','test1'); env.FOO.append('test2'); env.FOO.append('test3'); env.BAH.prepend('A'); prependenv('BAH','B'); env.BAH.append('C')",
                serde_json::json!({}),
                serde_json::json!({}),
                serde_json::json!({"FOO":"test1:test2:test3","BAH":"B:A:C"}),
            ),
            (
                "internal_control",
                "env.FOO='foo'; setenv('BAH','bah'); env.EEK='foo'\nif env.FOO == 'foo': env.FOO_VALID=1\nif env.FOO == env.EEK: comment('comparison ok')",
                serde_json::json!({}),
                serde_json::json!({}),
                serde_json::json!({"FOO":"foo","BAH":"bah","EEK":"foo","FOO_VALID":"1"}),
            ),
            (
                "parent_missing",
                "if defined('EXT') and env.EXT == 'alpha':\n    env.EXT_FOUND=1\n    env.EXT.append('beta')\nelse:\n    env.EXT_FOUND=0\n    assert undefined('EXT')",
                serde_json::json!({}),
                serde_json::json!({}),
                serde_json::json!({"EXT_FOUND":"0"}),
            ),
            (
                "parent_first_pend_overwrites",
                "if defined('EXT') and env.EXT == 'alpha':\n    env.EXT_FOUND=1\n    env.EXT.append('beta')\nelse:\n    env.EXT_FOUND=0",
                serde_json::json!({"EXT":"alpha"}),
                serde_json::json!({}),
                serde_json::json!({"EXT_FOUND":"1","EXT":"beta"}),
            ),
            (
                "expansion",
                "env.FOO='foo'; env.DOG='$FOO'; env.BAH='${FOO}'; env.EEK='${BAH}'; assert env.BAH == 'foo'; assert getenv('EEK') == 'foo'\nif defined('EXT') and getenv('EXT') == 'alpha': env.FEE='${EXT}'",
                serde_json::json!({"EXT":"alpha"}),
                serde_json::json!({}),
                serde_json::json!({"FOO":"foo","DOG":"foo","BAH":"foo","EEK":"foo","FEE":"alpha"}),
            ),
            (
                "configured_separators",
                "appendenv('FOO','test1'); env.FOO.append('test2'); env.FOO.append('test3'); env.BAH.prepend('A'); prependenv('BAH','B'); env.BAH.append('C')",
                serde_json::json!({}),
                serde_json::json!({"FOO":",","BAH":" "}),
                serde_json::json!({"FOO":"test1,test2,test3","BAH":"B A C"}),
            ),
            (
                "literal_expandable",
                "env.A='hello'; env.FOO=expandable('$A'); env.BAH=expandable('${A}'); env.EEK=literal('$A')",
                serde_json::json!({}),
                serde_json::json!({}),
                serde_json::json!({"A":"hello","FOO":"hello","BAH":"hello","EEK":"$A"}),
            ),
            (
                "mixed_literal_expandable",
                "env.BAH='omg'; env.FOO.append('$BAH'); env.FOO.append(literal('${BAH}')); env.FOO.append(expandable('like, ').l('$SHE said, ').e('$BAH'))",
                serde_json::json!({}),
                serde_json::json!({}),
                serde_json::json!({"BAH":"omg","FOO":"omg:${BAH}:like, $SHE said, omg"}),
            ),
            (
                "mapping_bool",
                "assert 'A' in env.keys(); assert 'B' not in env.keys(); assert 'A' in env; assert 'B' not in env; assert (env.get('B') or 'not b') == 'not b'; setenv('MAPPING','accepted')",
                serde_json::json!({"A":"foo"}),
                serde_json::json!({}),
                serde_json::json!({"MAPPING":"accepted"}),
            ),
        ];
        for (label, code, parent, separators, expected) in cases {
            let inject = HashMap::from([
                (
                    "_rex_data".into(),
                    serde_json::json!({
                        "environment":{"parent":parent,"current":{},"separators":separators,"separator":":"},
                        "system":crate::platform::SYSTEM.rex_data(&HashMap::new()).unwrap(),
                        "packages":[]
                    }),
                ),
                ("_post_code".into(), serde_json::json!(code)),
            ]);
            let globals = crate::python_vm::exec_py_globals_for_rex(
                REX_POST_BINDINGS,
                label,
                Some(&inject),
                &[],
            )
            .unwrap_or_else(|error| panic!("{label}: {error}"));
            let mut executor = RexExecutor::new(
                PythonInterpreter::new(),
                Some(serde_json::from_value(parent).unwrap()),
                false,
            );
            executor.manager_mut().set_env_sep("", ":");
            for (key, value) in separators.as_object().unwrap() {
                executor
                    .manager_mut()
                    .set_env_sep(key, value.as_str().unwrap());
            }
            apply_rex_actions(&mut executor, &globals["_actions"])
                .unwrap_or_else(|error| panic!("{label}: {error}"));
            let actual = serde_json::to_value(executor.env()).unwrap();
            assert_eq!(actual, expected, "{label}");
        }
    }

    #[test]
    fn test_rex_reference_error_corpus() {
        let cases = [
            ("getenv_undefined", "getenv('NOTEXIST')", "undefined", true),
            ("proxy_undefined", "info(env.NOTEXIST)", "undefined", true),
            (
                "native_error",
                "raise Exception('non rex-specific error')",
                "rex",
                true,
            ),
            (
                "shared_rex_error",
                "from rez.exceptions import RexError; raise RexError('explicit Rex error')",
                "rex",
                true,
            ),
            (
                "stop_subclass",
                "from rez.exceptions import RexStopError\nclass CustomStop(RexStopError): pass\nraise CustomStop('subclass stop')",
                "stop",
                true,
            ),
            (
                "uncaught_runtime",
                "raise ValueError('runtime policy')",
                "python",
                false,
            ),
            (
                "uncaught_runtime_syntax",
                "raise SyntaxError('runtime syntax')",
                "python",
                false,
            ),
            ("compile_always_rex", "if :", "rex", false),
        ];
        for (label, code, expected, catch) in cases {
            let inject = HashMap::from([
                (
                    "_rex_data".into(),
                    serde_json::json!({
                        "environment":{"parent":{},"current":{},"separators":{},"separator":":"},
                        "catch_rex_errors":catch,
                        "system":crate::platform::SYSTEM.rex_data(&HashMap::new()).unwrap(),
                        "packages":[]
                    }),
                ),
                ("_post_code".into(), serde_json::json!(code)),
            ]);
            let error = crate::python_vm::exec_py_globals_for_rex(
                REX_POST_BINDINGS,
                label,
                Some(&inject),
                &[],
            )
            .expect_err(label);
            match expected {
                "undefined" => assert!(
                    matches!(error, RezError::RexUndefinedVariable(_)),
                    "{label}: {error}"
                ),
                "rex" => assert!(matches!(error, RezError::Rex(_)), "{label}: {error}"),
                "python" => assert!(matches!(error, RezError::Python(_)), "{label}: {error}"),
                "stop" => assert!(
                    matches!(error, RezError::RexStop(ref message) if message == "subclass stop"),
                    "{label}: {error}"
                ),
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn rex_paths_follow_active_interpreter_for_packages_post_commands_and_callbacks() {
        let repo = tempfile::tempdir().expect("repository");
        write_package(
            repo.path(),
            "path_probe",
            "1",
            "name: path_probe\nversion: '1'\n",
        );
        let base = repo.path().join("path_probe/1").canonicalize().unwrap();
        let mut context = ResolvedContext::empty();
        context.status = ResolverStatus::Solved;
        let roots = [
            r"\\?\C:\payload with spaces\package\1".to_string(),
            r"\\?\UNC\server\share\package\1".to_string(),
            format!(r"\\?\C:\{}\package\1", "long-path".repeat(40)),
            "C:/payload with spaces/package/1".to_string(),
        ];
        for root in roots {
            for interpreter in [
                Box::new(crate::shell::types::CmdShell::new()) as Box<dyn ActionInterpreter>,
                Box::new(crate::shell::types::PowerShellShell::new()),
                Box::new(crate::shell::types::BashShell::new(
                    crate::shell::types::ShellType::Bash,
                )),
                Box::new(crate::shell::rex::PythonInterpreter::new()),
            ] {
                let expected_root = interpreter.normalize_path(&root);
                let expected_base = interpreter.normalize_path(&base.display().to_string());
                context.resolved_packages = Some(vec![ResolvedPackageInfo {
                    name: "path_probe".into(),
                    version: Version::from_str("1").unwrap(),
                    variant_index: None,
                    resource_handle: None,
                    requires: Vec::new(),
                    repo_path: Some(base.clone()),
                    root: Some(PathBuf::from(&root)),
                    pre_commands: None,
                    commands: Some(
                        "env.PACKAGE_ROOT = this.root; env.PACKAGE_BASE = this.base".into(),
                    ),
                    post_commands: None,
                }]);
                let packages = context.resolved_packages.as_deref().unwrap();
                let prepared = rex_package_data(packages, &context, Some(interpreter.as_ref()))
                    .expect("interpreter-normalized metadata");
                assert_eq!(prepared[0]["root"], expected_root);
                assert_eq!(prepared[0]["base"], expected_base);

                let mut parent =
                    crate::package::Package::new("path_probe", Version::from_str("1").unwrap());
                parent.base = Some(base.clone());
                let mut variant = crate::package::Variant::from_package(parent);
                variant.root = Some(PathBuf::from(&root));
                let callback = RexExecutionCallback {
                    package_name: "path_probe".into(),
                    name: "pre_test_commands".into(),
                    code: "env.CALLBACK_ROOT = this.root; env.CALLBACK_BASE = this.base".into(),
                    package: Some(variant),
                    bindings: HashMap::new(),
                    developer: false,
                };
                let mut executor = RexExecutor::new(interpreter, Some(HashMap::new()), false);
                exec_rex_py(&mut executor, packages, &context, Some(&callback))
                    .expect("package and explicit callback paths");
                exec_rex_post_commands(
                    &mut executor,
                    &context,
                    "env.POST_ROOT = this.root; env.POST_BASE = this.base",
                    packages,
                )
                .expect("bundle post-command paths");
                for field in ["PACKAGE_ROOT", "CALLBACK_ROOT", "POST_ROOT"] {
                    assert_eq!(executor.getenv(field).unwrap(), expected_root, "{field}");
                }
                for field in ["PACKAGE_BASE", "CALLBACK_BASE", "POST_BASE"] {
                    assert_eq!(executor.getenv(field).unwrap(), expected_base, "{field}");
                }
            }
        }
    }

    #[test]
    fn test_rex_package_metadata_is_shared_with_bundle_bindings() {
        let repo = tempfile::tempdir().unwrap();
        write_package(
            repo.path(),
            "metadata_probe",
            "01.2",
            "name: metadata_probe\nversion: '01.2'\ndescription: full metadata\nauthors: [artist]\ncustom_data: {enabled: true}\n",
        );
        let mut context = ResolvedContext::empty();
        context.status = ResolverStatus::Solved;
        context.resolved_packages = Some(vec![ResolvedPackageInfo {
            name: "metadata_probe".into(),
            version: Version::from_str("01.2").unwrap(),
            variant_index: None,
            resource_handle: None,
            requires: Vec::new(),
            repo_path: Some(repo.path().join("metadata_probe").join("01.2")),
            root: Some(PathBuf::from("/resolved/payload")),
            pre_commands: None,
            commands: None,
            post_commands: None,
        }]);
        let data =
            rex_package_data(context.resolved_packages.as_ref().unwrap(), &context, None).unwrap();
        assert_eq!(data[0]["description"], "full metadata");
        assert_eq!(data[0]["custom_data"]["enabled"], true);
        assert_eq!(data[0]["root"], "/resolved/payload");
        assert_eq!(data[0]["tokens"], serde_json::json!(["01", 2]));
        let mut executor = RexExecutor::new(
            crate::shell::rex::PythonInterpreter::new(),
            Some(HashMap::new()),
            false,
        );
        exec_rex_post_commands(
            &mut executor,
            &context,
            "assert this.description == 'full metadata'; assert this.custom_data['enabled']; assert this.version.major == '01'; setenv('METADATA', this.authors[0])",
            context.resolved_packages.as_deref().unwrap_or(&[]),
        )
        .unwrap();
        assert_eq!(executor.getenv("METADATA").unwrap(), "artist");
    }

    #[test]
    fn test_rex_stop_formats_arguments_and_preserves_error_type() {
        let mut context = ResolvedContext::empty();
        context.status = ResolverStatus::Solved;
        context.resolved_packages = Some(vec![ResolvedPackageInfo {
            name: "stop_probe".into(),
            version: Version::from_str("1.0").unwrap(),
            variant_index: None,
            resource_handle: None,
            requires: Vec::new(),
            repo_path: None,
            root: None,
            pre_commands: None,
            commands: Some("stop('failed %s (%d)', 'build', 3)".into()),
            post_commands: None,
        }]);
        let error = context.get_environ(Some(HashMap::new())).unwrap_err();
        assert!(matches!(error, RezError::RexStop(ref message) if message == "failed build (3)"));
    }

    #[test]
    fn test_get_environ_propagates_rex_execution_errors() {
        let mut context = ResolvedContext::empty();
        context.status = ResolverStatus::Solved;
        context.rez_version = "0.1.0".to_string();
        context.rez_path = "/rez".to_string();
        context.timestamp = 100;
        context.resolved_packages = Some(vec![ResolvedPackageInfo {
            name: "broken".to_string(),
            version: Version::from_str("1.0").expect("parse"),
            variant_index: None,
            resource_handle: None,
            requires: Vec::new(),
            repo_path: None,
            root: None,
            pre_commands: None,
            commands: Some("raise ValueError('rex command failed')".to_string()),
            post_commands: None,
        }]);

        let error = context
            .get_environ(Some(HashMap::new()))
            .expect_err("Rex errors must be returned to callers");
        assert!(error.to_string().contains("rex command failed"), "{error}");
    }

    #[test]
    fn test_combined_filesystem_resource_handle_roundtrip() {
        let repo = tempfile::tempdir().expect("repository tempdir");
        std::fs::write(
            repo.path().join("combo.yaml"),
            "name: combo\nversions:\n  - 1.0.0\nvariants:\n  - []\n",
        )
        .expect("write combined package");

        let provider = FilesystemPackageProvider::from_path(repo.path()).unwrap();
        let context = ResolvedContext::resolve(
            vec![Requirement::new("combo").unwrap()],
            &provider,
            ResolveOptions {
                package_paths: Some(vec![repo.path().to_path_buf()]),
                add_implicit: false,
                caching: false,
                ..ResolveOptions::default()
            },
        )
        .expect("resolve combined package");

        let serialized = context.to_json().expect("serialize context");
        let handle = &serialized["resolved_packages"][0];
        assert_eq!(handle["key"], "filesystem.variant.combined");
        assert_eq!(handle["variables"]["repository_type"], "filesystem");
        assert_eq!(
            handle["variables"]["location"],
            repo.path().to_string_lossy().as_ref()
        );
        assert_eq!(handle["variables"]["name"], "combo");
        assert_eq!(handle["variables"]["version"], "1.0.0");
        assert_eq!(handle["variables"]["index"], 0);
        assert_eq!(handle["variables"]["ext"], "yaml");

        let restored = ResolvedContext::from_json(&serialized, None).expect("restore context");
        assert_eq!(
            restored.resolved_packages().unwrap()[0]
                .resource_handle
                .as_ref(),
            context.resolved_packages().unwrap()[0]
                .resource_handle
                .as_ref()
        );
    }

    #[test]
    fn test_rxt_handles_reject_missing_repository_package_and_index() {
        let repo = tempfile::tempdir().expect("repository tempdir");
        write_package(
            repo.path(),
            "variantful",
            "1.0.0",
            "name: variantful\nversion: 1.0.0\nvariants:\n  - []\n",
        );

        let mut missing_repository = handle_context(
            repo.path(),
            "filesystem.variant",
            "variantful",
            "1.0.0",
            serde_json::json!(0),
        );
        missing_repository["resolved_packages"][0]["variables"]["location"] =
            serde_json::json!(repo.path().join("missing").to_string_lossy().as_ref());
        let error = ResolvedContext::from_json(&missing_repository, None).unwrap_err();
        assert!(error.to_string().contains("resolved_packages[0]"));
        assert!(error.to_string().contains("repository location"));

        let missing_package = handle_context(
            repo.path(),
            "filesystem.variant",
            "absent",
            "1.0.0",
            serde_json::json!(0),
        );
        let error = ResolvedContext::from_json(&missing_package, None).unwrap_err();
        assert!(error.to_string().contains("resolved_packages[0]"));
        assert!(error.to_string().contains("package"));

        let mut missing_index = handle_context(
            repo.path(),
            "filesystem.variant",
            "variantful",
            "1.0.0",
            serde_json::json!(0),
        );
        missing_index["resolved_packages"][0]["variables"]
            .as_object_mut()
            .unwrap()
            .remove("index");
        let error = ResolvedContext::from_json(&missing_index, None).unwrap_err();
        assert!(error
            .to_string()
            .contains("resolved_packages[0].variables.index"));
    }

    #[test]
    fn test_rxt_handles_reject_malformed_schema_and_unsupported_version() {
        let repo = tempfile::tempdir().expect("repository tempdir");
        write_package(
            repo.path(),
            "variantful",
            "1.0.0",
            "name: variantful\nversion: 1.0.0\nvariants:\n  - []\n",
        );

        let mut malformed = handle_context(
            repo.path(),
            "filesystem.variant",
            "variantful",
            "1.0.0",
            serde_json::json!(0),
        );
        malformed["resolved_packages"][0]["variables"]["unexpected"] = serde_json::json!(true);
        let error = ResolvedContext::from_json(&malformed, None).unwrap_err();
        assert!(error.to_string().contains("resolved_packages[0]"));
        assert!(error.to_string().contains("unexpected"));

        let bad_index = handle_context(
            repo.path(),
            "filesystem.variant",
            "variantful",
            "1.0.0",
            serde_json::json!(2),
        );
        let error = ResolvedContext::from_json(&bad_index, None).unwrap_err();
        assert!(error
            .to_string()
            .contains("resolved_packages[0].variables.index"));

        let mut unsupported = handle_context(
            repo.path(),
            "filesystem.variant",
            "variantful",
            "1.0.0",
            serde_json::json!(0),
        );
        unsupported["serialize_version"] = serde_json::json!("4.10");
        let error = ResolvedContext::from_json(&unsupported, None).unwrap_err();
        assert!(error.to_string().contains("unsupported serialize_version"));
        assert!(error.to_string().contains("4.10"));
    }

    #[test]
    fn test_rxt_rejects_malformed_request_arrays_and_required_fields() {
        let repo = tempfile::tempdir().expect("repository tempdir");
        let valid = handle_context(
            repo.path(),
            "filesystem.variant",
            "not_needed",
            "1.0.0",
            serde_json::json!(0),
        );

        let mut malformed_type = valid.clone();
        malformed_type["package_requests"] = serde_json::json!(["valid", 42]);
        let error = ResolvedContext::from_json(&malformed_type, None).unwrap_err();
        assert!(error.to_string().contains("package_requests[1]"));
        assert!(error.to_string().contains("must be a string"));

        let mut malformed_requirement = valid.clone();
        malformed_requirement["implicit_packages"] = serde_json::json!(["bad-1."]);
        let error = ResolvedContext::from_json(&malformed_requirement, None).unwrap_err();
        assert!(error.to_string().contains("implicit_packages[0]"));
        assert!(error.to_string().contains("invalid requirement"));

        let mut malformed_paths = valid.clone();
        malformed_paths["package_paths"] = serde_json::json!([repo.path(), false]);
        let error = ResolvedContext::from_json(&malformed_paths, None).unwrap_err();
        assert!(error.to_string().contains("package_paths[1]"));
        assert!(error.to_string().contains("must be a string"));

        let mut missing_required = valid;
        missing_required.as_object_mut().unwrap().remove("caching");
        let error = ResolvedContext::from_json(&missing_required, None).unwrap_err();
        assert!(error.to_string().contains("caching"));
        assert!(error.to_string().contains("missing required field"));
    }

    #[test]
    fn test_variant_selection_modes_and_package_ordering() {
        use crate::package::order::PerFamilyOrder;
        use repository::provider::MemoryPackageProvider;

        let resolve_mode = |mode| {
            let mut app = Package::new("app", Version::new("1.0").unwrap());
            app.variants = vec![
                vec![Requirement::new("foo-1").unwrap()],
                vec![
                    Requirement::new("bar-1").unwrap(),
                    Requirement::new("baz-1").unwrap(),
                ],
            ];
            let mut provider = MemoryPackageProvider::new();
            provider.add(app);
            for name in ["foo", "bar", "baz"] {
                provider.add(Package::new(name, Version::new("1.0").unwrap()));
            }
            ResolvedContext::resolve(
                ["app", "foo", "bar", "baz"]
                    .into_iter()
                    .map(|name| Requirement::new(name).unwrap())
                    .collect(),
                &provider,
                ResolveOptions {
                    add_implicit: false,
                    caching: false,
                    variant_select_mode: mode,
                    ..ResolveOptions::default()
                },
            )
            .unwrap()
        };

        let version_priority = resolve_mode(VariantSelectMode::VersionPriority);
        let intersection_priority = resolve_mode(VariantSelectMode::IntersectionPriority);
        assert_eq!(
            version_priority
                .get_resolved_package("app")
                .unwrap()
                .variant_index,
            Some(0)
        );
        assert_eq!(
            intersection_priority
                .get_resolved_package("app")
                .unwrap()
                .variant_index,
            Some(1)
        );

        let mut app = Package::new("ordered_app", Version::new("1.0").unwrap());
        app.variants = vec![
            vec![Requirement::new("ordered_dep-1+").unwrap()],
            vec![Requirement::new("ordered_dep-2+").unwrap()],
        ];
        let mut provider = MemoryPackageProvider::new();
        provider.add(app);
        provider.add(Package::new("ordered_dep", Version::new("1.0").unwrap()));
        provider.add(Package::new("ordered_dep", Version::new("2.0").unwrap()));
        let mut orderers = PackageOrderList::new();
        orderers.add_orderer(Box::new(PerFamilyOrder::ascending(vec![
            "ordered_dep".into()
        ])));
        let ordered = ResolvedContext::resolve(
            ["ordered_app", "ordered_dep"]
                .into_iter()
                .map(|name| Requirement::new(name).unwrap())
                .collect(),
            &provider,
            ResolveOptions {
                add_implicit: false,
                caching: false,
                package_orderers: Some(orderers),
                ..ResolveOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            ordered
                .get_resolved_package("ordered_app")
                .unwrap()
                .variant_index,
            Some(0)
        );
    }

    #[test]
    fn test_zero_fail_limit_does_not_abort_before_a_failure() {
        use repository::provider::MemoryPackageProvider;

        let mut provider = MemoryPackageProvider::new();
        provider.add(Package::new("no_fail_app", Version::new("1.0").unwrap()));
        let context = ResolvedContext::resolve(
            vec![Requirement::new("no_fail_app").unwrap()],
            &provider,
            ResolveOptions {
                add_implicit: false,
                caching: false,
                ..ResolveOptions::default()
            }
            .with_limits(0, -1),
        )
        .unwrap();

        assert_eq!(context.status, ResolverStatus::Solved);
    }

    #[test]
    fn test_zero_time_limit_aborts_between_multiple_candidates() {
        use repository::provider::MemoryPackageProvider;

        let mut provider = MemoryPackageProvider::new();
        provider.add(Package::new(
            "time_limited_app",
            Version::new("1.0").unwrap(),
        ));
        provider.add(Package::new(
            "time_limited_app",
            Version::new("2.0").unwrap(),
        ));

        let context = ResolvedContext::resolve(
            vec![Requirement::new("time_limited_app").unwrap()],
            &provider,
            ResolveOptions {
                add_implicit: false,
                caching: false,
                ..ResolveOptions::default()
            }
            .with_limits(-1, 0),
        )
        .unwrap();

        assert_eq!(context.status, ResolverStatus::Aborted);
        assert!(context
            .failure_description
            .as_deref()
            .is_some_and(|description| description.contains("time limit exceeded")));
        assert!(context.resolved_packages().is_none());
    }

    #[test]
    fn test_variant_selection_mode_roundtrip() {
        let mut context = ResolvedContext::empty();
        context.variant_select_mode = VariantSelectMode::IntersectionPriority;
        let json = context.to_json().unwrap();
        let restored = ResolvedContext::from_json(&json, None).unwrap();
        assert_eq!(
            restored.variant_select_mode,
            VariantSelectMode::IntersectionPriority
        );
    }

    #[test]
    fn test_empty_context() {
        let ctx = ResolvedContext::empty();
        assert_eq!(ctx.status, ResolverStatus::Pending);
        assert!(!ctx.success());
        assert!(!ctx.has_graph());
        assert!(ctx.resolved_packages().is_none());
        assert!(ctx.resolved_ephemerals().is_none());
        assert!(ctx.package_requests().is_empty());
    }

    #[test]
    fn test_requested_packages() {
        let mut ctx = ResolvedContext::empty();
        let req = Requirement::from_str("foo-1.0").expect("parse");
        let imp = Requirement::from_str("platform-linux").expect("parse");
        ctx.package_requests = vec![req.clone()];
        ctx.implicit_packages = vec![imp.clone()];

        let without = ctx.requested_packages(false);
        assert_eq!(without.len(), 1);
        assert_eq!(without[0].to_string(), req.to_string());

        let with = ctx.requested_packages(true);
        assert_eq!(with.len(), 2);
    }

    #[test]
    fn test_success_flag() {
        let mut ctx = ResolvedContext::empty();
        assert!(!ctx.success());

        ctx.status = ResolverStatus::Solved;
        assert!(ctx.success());

        ctx.status = ResolverStatus::Failed;
        assert!(!ctx.success());
    }

    #[test]
    fn test_get_resolved_package() {
        let mut ctx = ResolvedContext::empty();
        ctx.status = ResolverStatus::Solved;
        ctx.resolved_packages = Some(vec![
            ResolvedPackageInfo {
                name: "foo".to_string(),
                version: Version::from_str("1.2.3").expect("parse"),
                variant_index: None,
                resource_handle: None,
                requires: Vec::new(),
                repo_path: None,
                root: None,
                commands: None,
                pre_commands: None,
                post_commands: None,
            },
            ResolvedPackageInfo {
                name: "bar".to_string(),
                version: Version::from_str("2.0").expect("parse"),
                variant_index: Some(0),
                resource_handle: None,
                requires: Vec::new(),
                repo_path: None,
                root: None,
                commands: None,
                pre_commands: None,
                post_commands: None,
            },
        ]);

        let foo = ctx.get_resolved_package("foo");
        assert!(foo.is_some());
        assert_eq!(foo.expect("foo").name, "foo");
        assert_eq!(foo.expect("foo").version.to_string(), "1.2.3");

        let bar = ctx.get_resolved_package("bar");
        assert!(bar.is_some());

        let baz = ctx.get_resolved_package("baz");
        assert!(baz.is_none());
    }

    #[test]
    fn test_display() {
        let mut ctx = ResolvedContext::empty();
        let req = Requirement::from_str("foo-1.0").expect("parse");
        ctx.package_requests = vec![req];
        ctx.status = ResolverStatus::Failed;

        let s = format!("{ctx}");
        assert!(s.contains("failed"));
        assert!(s.contains("foo"));
    }

    #[test]
    fn test_to_json_from_json_roundtrip() {
        let mut ctx = ResolvedContext::empty();
        ctx.status = ResolverStatus::Solved;
        ctx.user = "testuser".to_string();
        ctx.host = "testhost".to_string();
        ctx.building = true;
        ctx.testing = false;
        ctx.verbosity = 2;
        ctx.rez_version = "0.1.0".to_string();
        ctx.timestamp = 1700000000;
        ctx.created = 1700000000;
        ctx.solve_time = 0.5;
        ctx.load_time = 0.1;
        let repo = tempfile::tempdir().expect("repository tempdir");
        let package_dir = repo.path().join("foo").join("1.2.3");
        std::fs::create_dir_all(&package_dir).expect("create package directory");
        std::fs::write(
            package_dir.join("package.yaml"),
            "name: foo\nversion: 1.2.3\nrequires:\n  - python-3+\n  - numpy-1.0+\n",
        )
        .expect("write package definition");
        ctx.package_paths = vec![repo.path().to_path_buf(), PathBuf::from("/local")];

        let req = Requirement::from_str("foo-1.0").expect("parse");
        ctx.package_requests = vec![req];

        let imp = Requirement::from_str("platform-linux").expect("parse");
        ctx.implicit_packages = vec![imp];

        let foo_requires = vec![
            Requirement::from_str("python-3+").expect("parse"),
            Requirement::from_str("numpy-1.0+").expect("parse"),
        ];
        ctx.resolved_packages = Some(vec![ResolvedPackageInfo {
            name: "foo".to_string(),
            version: Version::from_str("1.2.3").expect("parse"),
            variant_index: None,
            resource_handle: Some(ResourceHandle {
                key: ResourceHandleKey::FilesystemVariant,
                variables: ResourceHandleVariables {
                    repository_type: "filesystem".into(),
                    location: repo.path().to_string_lossy().into_owned(),
                    name: "foo".into(),
                    version: Some("1.2.3".into()),
                    index: None,
                    ext: None,
                },
            }),
            requires: foo_requires.clone(),
            repo_path: Some(package_dir),
            root: None,
            commands: None,
            pre_commands: None,
            post_commands: None,
        }]);

        // Rez 4.9 stores package resource handles rather than package snapshots.
        let json = ctx.to_json().expect("to_json");
        assert_eq!(json["verbosity"], 2);
        assert_eq!(json["resolved_packages"][0]["key"], "filesystem.variant");
        assert_eq!(
            json["resolved_packages"][0]["variables"]["location"],
            repo.path().to_string_lossy().as_ref()
        );
        let ctx2 = ResolvedContext::from_json(&json, None).expect("from_json");

        // Verify requires round-trips (H9 fix)
        let pkgs = ctx2.resolved_packages().expect("pkgs");
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].requires.len(), 2);
        assert_eq!(pkgs[0].requires[0].to_string(), "python-3+");
        assert_eq!(pkgs[0].requires[1].to_string(), "numpy-1.0+");

        assert_eq!(ctx2.status, ResolverStatus::Solved);
        assert_eq!(ctx2.user, "testuser");
        assert_eq!(ctx2.host, "testhost");
        assert_eq!(ctx2.verbosity, 2);
        assert!(ctx2.building);
        assert!(!ctx2.testing);
        assert_eq!(ctx2.timestamp, 1700000000);
        assert_eq!(ctx2.package_paths.len(), 2);
        assert_eq!(ctx2.package_requests.len(), 1);
        assert_eq!(ctx2.implicit_packages.len(), 1);

        let pkgs = ctx2.resolved_packages().expect("resolved");
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "foo");
        assert_eq!(pkgs[0].version.to_string(), "1.2.3");
    }

    #[test]
    fn test_resolve_preserves_filter_orderers_and_cache_mode() {
        use crate::package::order::PackageOrderPod;
        use repository::provider::MemoryPackageProvider;

        let mut provider = MemoryPackageProvider::new();
        provider.add(Package::new("foo", Version::new("1.0").unwrap()));
        let filter = PackageFilterList::from_pod(&serde_json::json!([
            {"excludes": ["before(foo:1000)"]}
        ]))
        .unwrap();
        let orderers = PackageOrderList::from_pod(vec![PackageOrderPod::SortedOrder {
            descending: false,
            packages: Some(vec!["foo".into()]),
        }])
        .unwrap();

        let context = ResolvedContext::resolve(
            vec![Requirement::new("foo").unwrap()],
            &provider,
            ResolveOptions {
                package_filter: Some(filter),
                package_orderers: Some(orderers),
                package_cache_async: Some(false),
                add_implicit: false,
                ..ResolveOptions::default()
            },
        )
        .unwrap();

        assert!(context.success());
        assert_eq!(context.num_loaded_packages, 1);
        assert!(!context.package_cache_async);
        assert_eq!(
            context.package_filter.as_ref().unwrap().to_pod()[0]["excludes"][0],
            "before(foo:1000)"
        );
        assert_eq!(
            serde_json::to_value(context.package_orderers.as_ref().unwrap().to_pod().unwrap())
                .unwrap()[0]["type"],
            "sorted"
        );
    }

    #[test]
    fn test_python_rez_3_3_context_fixture_preserves_resolve_options() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../tests/fixtures/rez-3.3.0-context-4.9.rxt");
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/rez-3.3.0-context-4.9.rxt"
        ))
        .unwrap();
        let context = ResolvedContext::load(&path, None).expect("load Python Rez 3.3.0 context");

        assert_eq!(context.user, "fixture-user");
        assert!(!context.package_cache_async);
        assert_eq!(context.num_loaded_packages, 0);
        let serialized = context.to_json().expect("serialize loaded context");
        assert_eq!(serialized["package_filter"], fixture["package_filter"]);
        assert_eq!(serialized["package_orderers"], fixture["package_orderers"]);
        assert_eq!(
            serialized["package_cache_async"],
            fixture["package_cache_async"]
        );

        let restored =
            ResolvedContext::from_json(&serialized, None).expect("restore serialized context");
        assert_eq!(
            restored.package_filter.as_ref().unwrap().to_pod(),
            fixture["package_filter"]
        );
        assert_eq!(
            serde_json::to_value(
                restored
                    .package_orderers
                    .as_ref()
                    .unwrap()
                    .to_pod()
                    .unwrap()
            )
            .unwrap(),
            fixture["package_orderers"]
        );
        let mut composite_context = ResolvedContext::from_json(&fixture, None).unwrap();
        let composite_pod: crate::package::order::PackageOrderPod =
            serde_json::from_value(serde_json::json!({
                "type": "per_family",
                "orderers": [{
                    "type": "sorted",
                    "descending": false,
                    "packages": ["example"]
                }],
                "default_order": {
                    "type": "sorted",
                    "descending": true
                }
            }))
            .unwrap();
        composite_context.package_orderers =
            Some(PackageOrderList::from_pod(vec![composite_pod]).unwrap());
        let composite_json = composite_context.to_json().unwrap();
        let composite_restored = ResolvedContext::from_json(&composite_json, None).unwrap();
        assert_eq!(
            serde_json::to_value(
                composite_restored
                    .package_orderers
                    .as_ref()
                    .unwrap()
                    .to_pod()
                    .unwrap()
            )
            .unwrap(),
            composite_json["package_orderers"]
        );

        let mut legacy_defaults = fixture.clone();
        let fields = legacy_defaults.as_object_mut().unwrap();
        fields.remove("num_loaded_packages");
        fields.remove("package_cache_async");
        let legacy = ResolvedContext::from_json(&legacy_defaults, None).unwrap();
        assert_eq!(legacy.num_loaded_packages, -1);
        assert!(legacy.package_cache_async);

        let mut version_4_0 = fixture.clone();
        version_4_0["serialize_version"] = serde_json::json!("4.0");
        let fields = version_4_0.as_object_mut().unwrap();
        for field in [
            "package_filter",
            "package_orderers",
            "num_loaded_packages",
            "append_sys_path",
            "package_caching",
            "resolved_ephemerals",
            "package_cache_async",
            "testing",
        ] {
            fields.remove(field);
        }
        let restored_4_0 = ResolvedContext::from_json(&version_4_0, None).unwrap();
        assert!(restored_4_0.package_filter.unwrap().is_empty());
        assert!(restored_4_0.package_orderers.is_none());
        assert_eq!(restored_4_0.num_loaded_packages, -1);
        assert!(restored_4_0.append_sys_path);
        assert!(restored_4_0.package_caching);
        assert!(restored_4_0.package_cache_async);
        assert!(!restored_4_0.testing);

        let mut version_4_8 = fixture.clone();
        version_4_8["serialize_version"] = serde_json::json!("4.8");
        version_4_8.as_object_mut().unwrap().remove("testing");
        let restored_4_8 = ResolvedContext::from_json(&version_4_8, None).unwrap();
        assert!(!restored_4_8.testing);

        let mut pre_4_0 = fixture.clone();
        pre_4_0["serialize_version"] = serde_json::json!("3.9");
        let error = ResolvedContext::from_json(&pre_4_0, None).unwrap_err();
        assert!(error.to_string().contains("path-aware loading"));

        let mut newer = fixture.clone();
        newer["serialize_version"] = serde_json::json!("4.10");
        let error = ResolvedContext::from_json(&newer, None).unwrap_err();
        assert!(error.to_string().contains("supports up to 4.9"));
    }

    #[test]
    fn test_save_load_roundtrip() {
        let mut ctx = ResolvedContext::empty();
        ctx.status = ResolverStatus::Solved;
        ctx.user = "test".to_string();
        ctx.host = "localhost".to_string();
        ctx.rez_version = "0.1.0".to_string();
        ctx.timestamp = 1700000000;
        ctx.created = 1700000000;

        let repo = tempfile::tempdir().expect("repository tempdir");
        let package_dir = repo.path().join("mylib").join("3.1");
        std::fs::create_dir_all(&package_dir).expect("create package directory");
        std::fs::write(
            package_dir.join("package.yaml"),
            "name: mylib\nversion: '3.1'\nvariants:\n  - []\n",
        )
        .expect("write package definition");
        ctx.package_paths = vec![repo.path().to_path_buf()];
        ctx.resolved_packages = Some(vec![ResolvedPackageInfo {
            name: "mylib".to_string(),
            version: Version::from_str("3.1").expect("parse"),
            variant_index: Some(0),
            resource_handle: Some(ResourceHandle {
                key: ResourceHandleKey::FilesystemVariant,
                variables: ResourceHandleVariables {
                    repository_type: "filesystem".into(),
                    location: repo.path().to_string_lossy().into_owned(),
                    name: "mylib".into(),
                    version: Some("3.1".into()),
                    index: Some(0),
                    ext: None,
                },
            }),
            requires: Vec::new(),
            repo_path: Some(package_dir),
            root: None,
            commands: None,
            pre_commands: None,
            post_commands: None,
        }]);

        let path = repo.path().join("test_ctx.rxt");

        ctx.save(&path).expect("save");
        let loaded = ResolvedContext::load(&path, None).expect("load");

        assert_eq!(loaded.status, ResolverStatus::Solved);
        assert_eq!(loaded.user, "test");
        assert_eq!(loaded.load_path, Some(path.clone()));

        let pkgs = loaded.resolved_packages().expect("pkgs");
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "mylib");
        assert_eq!(pkgs[0].variant_index, Some(0));

        let yaml_path = repo.path().join("test_ctx_yaml.rxt");
        let yaml_doc = serde_yaml::to_string(&ctx.to_json().expect("serialize context"))
            .expect("serialize context as YAML");
        std::fs::write(&yaml_path, yaml_doc).expect("write YAML context");
        let loaded_yaml = ResolvedContext::load(&yaml_path, None).expect("load YAML context");
        assert_eq!(loaded_yaml.status, ResolverStatus::Solved);
        assert_eq!(loaded_yaml.user, "test");
        assert_eq!(loaded_yaml.load_path, Some(yaml_path.clone()));
        assert_eq!(
            loaded_yaml.resolved_packages().expect("YAML packages")[0].name,
            "mylib"
        );

        // Cleanup
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&yaml_path).ok();
    }

    #[test]
    fn test_require_success_fails_on_pending() {
        let ctx = ResolvedContext::empty();
        assert!(ctx.require_success().is_err());
    }

    #[test]
    fn test_require_success_ok_on_solved() {
        let mut ctx = ResolvedContext::empty();
        ctx.status = ResolverStatus::Solved;
        assert!(ctx.require_success().is_ok());
    }

    #[test]
    fn recipe_controls_reach_embedded_os_without_host_mutation() {
        let before = std::env::var_os("REZ_USER_PATH");
        let config = crate::config::RezConfig {
            user_path: Some("/configured-user".into()),
            sources_path: Some("/configured-sources".into()),
            offline: true,
            ..crate::config::RezConfig::default()
        };
        let mut ctx = ResolvedContext::empty();
        ctx.status = ResolverStatus::Solved;
        ctx.resolved_packages = Some(vec![ResolvedPackageInfo {
            name: "recipe_env".into(),
            version: Version::new("1.0").unwrap(),
            variant_index: None,
            resource_handle: None,
            requires: Vec::new(),
            repo_path: None,
            root: None,
            commands: Some("import os\nenv.RECIPE_USER_READ = os.environ['REZ_USER_PATH']\nenv.RECIPE_SOURCE_READ = os.getenv('REZ_SOURCES_PATH')\nos.environ['REZ_USER_PATH'] = '/interpreter-only'\nenv.RECIPE_LOCAL_READ = os.getenv('REZ_USER_PATH')\n".into()),
            pre_commands: None,
            post_commands: None,
        }]);
        let environment = ctx.get_environ(Some(config.recipe_environment())).unwrap();
        assert_eq!(environment["RECIPE_USER_READ"], "/configured-user");
        assert_eq!(environment["RECIPE_SOURCE_READ"], "/configured-sources");
        assert_eq!(environment["RECIPE_LOCAL_READ"], "/interpreter-only");
        assert_eq!(std::env::var_os("REZ_USER_PATH"), before);
        let environment = ctx.get_environ(Some(config.recipe_environment())).unwrap();
        assert_eq!(environment["RECIPE_USER_READ"], "/configured-user");
    }

    #[test]
    fn test_get_environ_on_solved() {
        let mut ctx = ResolvedContext::empty();
        ctx.status = ResolverStatus::Solved;
        ctx.rez_version = "0.1.0".to_string();
        ctx.rez_path = "/usr/bin/rez".to_string();
        ctx.timestamp = 1700000000;

        ctx.resolved_packages = Some(vec![ResolvedPackageInfo {
            name: "foo".to_string(),
            version: Version::from_str("2.3.4").expect("parse"),
            variant_index: None,
            resource_handle: None,
            requires: Vec::new(),
            repo_path: Some(PathBuf::from("/packages/foo/2.3.4")),
            root: None,
            commands: None,
            pre_commands: None,
            post_commands: None,
        }]);

        let env = ctx.get_environ(Some(HashMap::new())).expect("get_environ");

        assert_eq!(
            env.get("REZ_USED_VERSION").map(|s| s.as_str()),
            Some("0.1.0")
        );
        assert_eq!(
            env.get("REZ_USED").map(|s| s.as_str()),
            Some("/usr/bin/rez")
        );
        assert_eq!(
            env.get("REZ_FOO_VERSION").map(|s| s.as_str()),
            Some("2.3.4")
        );
        assert_eq!(
            env.get("REZ_FOO_MAJOR_VERSION").map(|s| s.as_str()),
            Some("2")
        );
        assert_eq!(
            env.get("REZ_FOO_MINOR_VERSION").map(|s| s.as_str()),
            Some("3")
        );
        assert_eq!(
            env.get("REZ_FOO_PATCH_VERSION").map(|s| s.as_str()),
            Some("4")
        );
    }

    #[test]
    fn test_get_environ_fails_on_failed() {
        let mut ctx = ResolvedContext::empty();
        ctx.status = ResolverStatus::Failed;
        assert!(ctx.get_environ(None).is_err());
    }

    #[test]
    fn test_get_actions_on_solved() {
        let mut ctx = ResolvedContext::empty();
        ctx.status = ResolverStatus::Solved;
        ctx.rez_version = "0.1.0".to_string();
        ctx.rez_path = "/rez".to_string();
        ctx.timestamp = 100;
        ctx.resolved_packages = Some(Vec::new());

        let actions = ctx.get_actions(Some(HashMap::new())).expect("get_actions");

        // Should have at least the system setup comments and env vars
        assert!(!actions.is_empty());

        // First non-comment action should be REZ_USED setenv
        let setenvs: Vec<_> = actions
            .iter()
            .filter(|a| matches!(a, Action::Setenv { .. }))
            .collect();
        assert!(!setenvs.is_empty());
    }

    #[test]
    fn rex_python_bridge_preserves_segments_and_parent_routing() {
        use crate::shell::rex::{RexSegment, RexValue};
        let mut ctx = ResolvedContext::empty();
        ctx.status = ResolverStatus::Solved;
        ctx.resolved_packages = Some(vec![ResolvedPackageInfo {
            name: "mixed".into(),
            version: Version::from_str("1.0").unwrap(),
            variant_index: None,
            resource_handle: None,
            requires: Vec::new(),
            repo_path: None,
            root: Some(std::path::PathBuf::from("/mixed/root")),
            pre_commands: None,
            post_commands: None,
            commands: Some(
                r#"
assert "USER" in env.keys()
assert "USER" in env
assert "UNSEEN" not in env
assert "UNSEEN" not in env.keys()  # Membership must not create a proxy.
assert len(env) == 1
assert list(env) == ["USER"]
assert dict(env.items())["USER"].name == "USER"
assert list(env.values())[0].get() == "alice"
assert env.get("UNSEEN", "default").name == "UNSEEN"  # Rez lookup always returns a proxy.
del env["UNSEEN"]
assert "UNSEEN" not in env
for key in ("keys", "items", "get", "update", "_PRIVATE"):
    env[key] = literal(key)
    assert env[key].get() == key
assert callable(env.keys)
env.update({"UPDATED": literal("value")})
assert env.UPDATED.get() == "value"
env.setdefault("DEFAULTED", "ignored").set("explicit")
assert env.DEFAULTED.get() == "explicit"
assert env.pop("UPDATED").name == "UPDATED"
assert "UPDATED" not in env
assert getenv("UPDATED") == "value"  # Mapping deletion does not unset an environment variable.
assert env.USER.get() == "alice"
assert env.USER.value() == "alice"
assert env.USER
assert defined("USER")
env.USER.setdefault("wrong")
assert getenv("USER") == "alice"
env.REZ_TEST_LOOKUP = "first"
assert env.REZ_TEST_LOOKUP == "first"
env["REZ_TEST_LOOKUP"] = literal("second")
assert env.REZ_TEST_LOOKUP.value() == "second"
env.REZ_TEST_MIXED = literal("${USER} {root} ").e("${USER} {this.name}").l(" end")
env.REZ_TEST_EMPTY = literal("")
env.REZ_TEST_PATH.prepend(literal("first"))
env.REZ_TEST_PATH.append(literal("second"))
"#
                .into(),
            ),
        }]);
        let parent = HashMap::from([("USER".into(), "alice".into())]);
        let env = ctx.get_environ(Some(parent.clone())).unwrap();
        assert_eq!(env["REZ_TEST_MIXED"], "${USER} {root} alice mixed end");
        assert_eq!(env["REZ_TEST_EMPTY"], "");
        assert_eq!(
            env["REZ_TEST_PATH"],
            format!("first{}second", if cfg!(windows) { ";" } else { ":" })
        );
        let actions = ctx.get_actions(Some(parent)).unwrap();
        let value = actions
            .iter()
            .find_map(|action| match action {
                Action::Setenv { key, value } if key == "REZ_TEST_MIXED" => Some(value),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            value,
            &RexValue(vec![
                RexSegment {
                    literal: true,
                    text: "${USER} {root} ".into()
                },
                RexSegment {
                    literal: false,
                    text: "${USER} mixed".into()
                },
                RexSegment {
                    literal: true,
                    text: " end".into()
                },
            ])
        );
        let mut executor = RexExecutor::new(
            crate::shell::rex::PythonInterpreter::new(),
            Some(HashMap::from([("USER".into(), "alice".into())])),
            false,
        );
        exec_rex_post_commands(
            &mut executor,
            &ctx,
            r#"env.POST_MIXED = literal("$USER ").e("$USER").l("!")"#,
            ctx.resolved_packages.as_deref().unwrap_or(&[]),
        )
        .unwrap();
        assert_eq!(executor.env()["POST_MIXED"], "$USER alice!");
    }

    #[test]
    fn rex_action_decoder_rejects_malformed_values_instead_of_empty_strings() {
        let mut executor = RexExecutor::new(
            crate::shell::rex::PythonInterpreter::new(),
            Some(HashMap::new()),
            false,
        );
        for record in [
            serde_json::json!([{"t":"set","k":"VALUE","v":42}]),
            serde_json::json!([{"t":"set","k":"VALUE","v":[{"literal":"yes","text":"bad"}]}]),
            serde_json::json!([{"t":"source","path":null}]),
            serde_json::json!([{"t":"unknown"}]),
        ] {
            assert!(apply_rex_actions(&mut executor, &record).is_err());
        }
        assert!(executor.actions().is_empty());
    }

    #[test]
    fn test_installed_command_includes_use_package_base_and_propagate_missing_module() {
        let directory = tempfile::tempdir().expect("temporary package");
        let include_dir = directory.path().join(".rez").join("include");
        std::fs::create_dir_all(&include_dir).expect("include directory");
        std::fs::write(
            directory.path().join("package.yaml"),
            "name: included\nversion: '1.0'\n",
        )
        .expect("package definition");
        let module = include_dir.join("helper.py");
        std::fs::write(&module, "VALUE = 'installed-module'\n").expect("include module");
        let mut context = ResolvedContext::empty();
        context.status = ResolverStatus::Solved;
        context.resolved_packages = Some(vec![ResolvedPackageInfo {
            name: "included".into(),
            version: Version::from_str("1.0").expect("version"),
            variant_index: None,
            resource_handle: None,
            requires: Vec::new(),
            repo_path: Some(directory.path().to_path_buf()),
            root: Some(directory.path().to_path_buf()),
            pre_commands: None,
            commands: Some(
                "@include('helper')\ndef commands():\n    env.INCLUDE_VALUE = helper.VALUE\n"
                    .into(),
            ),
            post_commands: None,
        }]);
        let environment = context
            .get_environ(Some(HashMap::new()))
            .expect("installed include execution");
        assert_eq!(
            environment.get("INCLUDE_VALUE").map(String::as_str),
            Some("installed-module")
        );
        std::fs::remove_file(module).expect("remove installed include");
        assert!(context.get_environ(Some(HashMap::new())).is_err());
    }

    #[test]
    fn test_pre_post_commands_execution() {
        // Verify pre_commands, commands, and post_commands all execute
        let mut ctx = ResolvedContext::empty();
        ctx.status = ResolverStatus::Solved;
        ctx.rez_version = "0.1.0".to_string();
        ctx.rez_path = "/rez".to_string();
        ctx.timestamp = 100;

        ctx.resolved_packages = Some(vec![ResolvedPackageInfo {
            name: "mypkg".to_string(),
            version: Version::from_str("1.0").expect("parse"),
            variant_index: None,
            resource_handle: None,
            requires: Vec::new(),
            repo_path: None,
            root: None,
            pre_commands: Some(r#"env.PRE_VAR = "pre_value""#.to_string()),
            commands: Some(r#"env.MAIN_VAR = "main_value""#.to_string()),
            post_commands: Some(r#"env.POST_VAR = "post_value""#.to_string()),
        }]);

        let env = ctx.get_environ(Some(HashMap::new())).expect("get_environ");

        // All three command phases should have set their vars
        assert_eq!(env.get("PRE_VAR").map(|s| s.as_str()), Some("pre_value"));
        assert_eq!(env.get("MAIN_VAR").map(|s| s.as_str()), Some("main_value"));
        assert_eq!(env.get("POST_VAR").map(|s| s.as_str()), Some("post_value"));
    }

    #[test]
    fn test_pre_post_commands_order() {
        // Verify execution order: pre_commands -> commands -> post_commands
        // Use prepend to PATH so we can check the order from the result
        let mut ctx = ResolvedContext::empty();
        ctx.status = ResolverStatus::Solved;
        ctx.rez_version = "0.1.0".to_string();
        ctx.rez_path = "/rez".to_string();
        ctx.timestamp = 100;

        ctx.resolved_packages = Some(vec![ResolvedPackageInfo {
            name: "mypkg".to_string(),
            version: Version::from_str("1.0").expect("parse"),
            variant_index: None,
            resource_handle: None,
            requires: Vec::new(),
            repo_path: None,
            root: None,
            pre_commands: Some("env.MY_PATH = \"/pre\"".to_string()),
            commands: Some("env.MY_PATH.prepend(\"/main\")".to_string()),
            post_commands: Some("env.MY_PATH.prepend(\"/post\")".to_string()),
        }]);

        let env = ctx.get_environ(Some(HashMap::new())).expect("get_environ");

        // Python rez order: all pre_commands, then all commands, then all post_commands
        // pre_commands sets MY_PATH="/pre", commands prepends /main, post prepends /post
        // Result: /post;/main;/pre
        let sep = if cfg!(windows) { ";" } else { ":" };
        let expected = format!("/post{sep}/main{sep}/pre");
        assert_eq!(
            env.get("MY_PATH").map(|s| s.as_str()),
            Some(expected.as_str())
        );
    }

    #[test]
    fn test_pre_post_commands_none_ok() {
        // pre_commands and post_commands can be None (most common case)
        let mut ctx = ResolvedContext::empty();
        ctx.status = ResolverStatus::Solved;
        ctx.rez_version = "0.1.0".to_string();
        ctx.rez_path = "/rez".to_string();
        ctx.timestamp = 100;

        ctx.resolved_packages = Some(vec![ResolvedPackageInfo {
            name: "mypkg".to_string(),
            version: Version::from_str("1.0").expect("parse"),
            variant_index: None,
            resource_handle: None,
            requires: Vec::new(),
            repo_path: None,
            root: None,
            pre_commands: None,
            commands: Some(r#"env.FOO = "bar""#.to_string()),
            post_commands: None,
        }]);

        let env = ctx.get_environ(Some(HashMap::new())).expect("get_environ");

        assert_eq!(env.get("FOO").map(|s| s.as_str()), Some("bar"));
    }

    #[test]
    fn test_context_changes_empty() {
        let changes = ContextChanges::default();
        assert!(changes.is_empty());
    }

    #[test]
    fn test_context_changes() {
        let mut ctx = ResolvedContext::empty();
        ctx.status = ResolverStatus::Solved;
        ctx.rez_version = "0.1.0".to_string();
        ctx.rez_path = "/rez".to_string();
        ctx.timestamp = 100;
        ctx.resolved_packages = Some(Vec::new());

        let mut parent = HashMap::new();
        parent.insert("EXISTING_VAR".to_string(), "old_value".to_string());
        parent.insert("UNTOUCHED".to_string(), "same".to_string());

        let changes = ctx.get_key_changes(Some(parent)).expect("changes");

        // REZ_USED_VERSION etc should be in 'added'
        assert!(!changes.added.is_empty());
        assert!(changes.added.contains_key("REZ_USED_VERSION"));

        // EXISTING_VAR is not set by context, so removed (it was in parent but not in result)
        // UNTOUCHED is also not set by context
    }

    #[test]
    fn test_is_current_false_by_default() {
        let ctx = ResolvedContext::empty();
        assert!(!ctx.is_current());
    }

    #[test]
    fn test_patch_lock_roundtrip() {
        let mut ctx = ResolvedContext::empty();
        ctx.default_patch_lock = PatchLock::Lock3;
        ctx.patch_locks.insert("foo".to_string(), PatchLock::Lock2);

        let json = ctx.to_json().expect("to_json");
        let ctx2 = ResolvedContext::from_json(&json, None).expect("from_json");

        assert_eq!(ctx2.default_patch_lock, PatchLock::Lock3);
        assert_eq!(ctx2.patch_locks.get("foo"), Some(&PatchLock::Lock2));
    }

    #[test]
    fn test_suite_info() {
        let mut ctx = ResolvedContext::empty();
        assert!(ctx.parent_suite_path.is_none());
        assert!(ctx.suite_context_name.is_none());

        ctx.set_parent_suite("/suites/main", "my_context");
        assert_eq!(ctx.parent_suite_path.as_deref(), Some("/suites/main"));
        assert_eq!(ctx.suite_context_name.as_deref(), Some("my_context"));
    }

    #[test]
    fn test_ephemerals_roundtrip() {
        let mut ctx = ResolvedContext::empty();
        ctx.status = ResolverStatus::Solved;
        let eph = Requirement::from_str(".foo-1.0").expect("parse eph");
        ctx.resolved_ephemerals = Some(vec![eph]);

        let json = ctx.to_json().expect("json");
        let ctx2 = ResolvedContext::from_json(&json, None).expect("from_json");

        let ephs = ctx2.resolved_ephemerals().expect("ephs");
        assert_eq!(ephs.len(), 1);
        assert!(ephs[0].to_string().contains("foo"));
    }

    #[test]
    fn test_graph_string() {
        let mut ctx = ResolvedContext::empty();
        assert!(!ctx.has_graph());

        ctx.graph_string = Some("{compact graph data}".to_string());
        assert!(ctx.has_graph());
    }

    #[test]
    fn test_rxt_extension() {
        assert_eq!(RXT_EXTENSION, ".rxt");
    }

    #[test]
    fn test_rez_rs_version() {
        assert!(!REZ_RS_VERSION.is_empty());
    }

    // =========================================================================
    // get_patched_request tests
    // =========================================================================

    fn make_solved_ctx() -> ResolvedContext {
        let mut ctx = ResolvedContext::empty();
        ctx.status = ResolverStatus::Solved;
        ctx.package_requests = vec![
            Requirement::from_str("foo-1.2").unwrap(),
            Requirement::from_str("bar-3.0").unwrap(),
        ];
        ctx.resolved_packages = Some(vec![
            ResolvedPackageInfo {
                name: "foo".to_string(),
                version: Version::from_str("1.2.3").unwrap(),
                variant_index: None,
                resource_handle: None,
                requires: Vec::new(),
                repo_path: None,
                root: None,
                commands: None,
                pre_commands: None,
                post_commands: None,
            },
            ResolvedPackageInfo {
                name: "bar".to_string(),
                version: Version::from_str("3.0.1").unwrap(),
                variant_index: None,
                resource_handle: None,
                requires: Vec::new(),
                repo_path: None,
                root: None,
                commands: None,
                pre_commands: None,
                post_commands: None,
            },
        ]);
        ctx
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn get_shell_code_renders_and_executes_file_output() {
        #[cfg(unix)]
        let shell = ShellType::Bash;
        #[cfg(windows)]
        let shell = ShellType::Cmd;

        let mut context = ResolvedContext::empty();
        context.status = ResolverStatus::Solved;
        context.resolved_packages = Some(vec![ResolvedPackageInfo {
            name: "shell_output_probe".to_string(),
            version: Version::from_str("1.0").expect("valid test version"),
            variant_index: None,
            resource_handle: None,
            requires: Vec::new(),
            repo_path: None,
            root: None,
            pre_commands: None,
            commands: Some("env.REZ_RS_SHELL_OUTPUT_PROBE = \"rendered\"".to_string()),
            post_commands: None,
        }]);

        let mut code = context
            .get_shell_code(Some(shell), Some(HashMap::new()))
            .expect("get shell code");
        assert!(code.contains("REZ_RS_SHELL_OUTPUT_PROBE"), "{code}");

        #[cfg(unix)]
        code.push_str("\nprintf '%s\\n' \"$REZ_RS_SHELL_OUTPUT_PROBE\"\n");
        #[cfg(windows)]
        code.push_str("\r\necho %REZ_RS_SHELL_OUTPUT_PROBE%\r\n");

        let tempdir = tempfile::tempdir().expect("create script directory");
        let script_path = tempdir.path().join(if cfg!(windows) {
            "context.cmd"
        } else {
            "context.sh"
        });
        std::fs::write(&script_path, code).expect("write rendered shell script");

        let output = if cfg!(windows) {
            Command::new(shell.executable())
                .args(["/d", "/c"])
                .arg(&script_path)
                .output()
                .expect("execute cmd script")
        } else {
            Command::new(shell.executable())
                .arg(&script_path)
                .output()
                .expect("execute bash script")
        };
        assert!(
            output.status.success(),
            "shell script failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "rendered");
    }

    #[test]
    fn test_patched_no_changes() {
        let ctx = make_solved_ctx();
        let result = ctx.get_patched_request(None, None, false, 0);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].name(), "foo");
        assert_eq!(result[1].name(), "bar");
    }

    #[test]
    fn test_patched_override() {
        let ctx = make_solved_ctx();
        let result = ctx.get_patched_request(Some(vec!["foo-2.0".to_string()]), None, false, 0);
        assert_eq!(result.len(), 2);
        // foo should be overridden to foo-2.0
        assert!(result[0].to_string().contains("foo"));
        assert!(result[0].to_string().contains("2.0"));
        assert_eq!(result[1].name(), "bar");
    }

    #[test]
    fn test_patched_subtraction() {
        let ctx = make_solved_ctx();
        let result = ctx.get_patched_request(None, Some(vec!["bar".to_string()]), false, 0);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name(), "foo");
    }

    #[test]
    fn test_patched_caret_subtraction() {
        let ctx = make_solved_ctx();
        let result = ctx.get_patched_request(Some(vec!["^bar".to_string()]), None, false, 0);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name(), "foo");
    }

    #[test]
    fn test_patched_append_new() {
        let ctx = make_solved_ctx();
        let result = ctx.get_patched_request(Some(vec!["baz-1.0".to_string()]), None, false, 0);
        assert_eq!(result.len(), 3);
        assert_eq!(result[2].name(), "baz");
    }

    #[test]
    fn test_patched_strict_uses_resolved() {
        let ctx = make_solved_ctx();
        let result = ctx.get_patched_request(None, None, true, 0);
        assert_eq!(result.len(), 2);
        // Strict uses qualified names (exact versions)
        assert!(result[0].to_string().contains("1.2.3"));
        assert!(result[1].to_string().contains("3.0.1"));
    }

    #[test]
    fn test_patched_rank_limiters() {
        let ctx = make_solved_ctx();
        // rank=3 means lock major+minor, only patch can increase
        let result = ctx.get_patched_request(None, None, false, 3);
        // Original 2 requests + 2 rank limiters
        assert_eq!(result.len(), 4);
        // Last two should be weak constraints (~pkg<upper)
        assert!(result[2].weak());
        assert!(result[3].weak());
    }

    #[test]
    fn test_patched_rank_skips_overridden() {
        let ctx = make_solved_ctx();
        // Override foo, rank limiters should only apply to bar
        let result = ctx.get_patched_request(Some(vec!["foo-5.0".to_string()]), None, false, 3);
        // 2 base (foo overridden + bar) + 1 rank limiter (bar only)
        assert_eq!(result.len(), 3);
        let weak_reqs: Vec<_> = result.iter().filter(|r| r.weak()).collect();
        assert_eq!(weak_reqs.len(), 1);
        assert!(weak_reqs[0].to_string().contains("bar"));
    }

    #[test]
    fn test_patched_conflict_mismatch_appends() {
        let ctx = make_solved_ctx();
        // !foo should not replace foo (conflict flag mismatch) - it gets appended
        let result = ctx.get_patched_request(Some(vec!["!foo".to_string()]), None, false, 0);
        assert_eq!(result.len(), 3);
        // Original foo still there, !foo appended
        assert_eq!(result[0].name(), "foo");
        assert!(!result[0].conflict());
        assert_eq!(result[2].name(), "foo");
        assert!(result[2].conflict());
    }

    // =========================================================================
    // get_patched_request_simple tests
    // =========================================================================

    #[test]
    fn test_patched_simple_no_changes() {
        let ctx = make_solved_ctx();
        let result = ctx
            .get_patched_request_simple(&[], false, 0)
            .expect("patch");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].name(), "foo");
        assert_eq!(result[1].name(), "bar");
    }

    #[test]
    fn test_patched_simple_override() {
        let ctx = make_solved_ctx();
        let patch = vec![Requirement::new("foo-2.0").unwrap()];
        let result = ctx
            .get_patched_request_simple(&patch, false, 0)
            .expect("patch");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].name(), "foo");
        assert!(result[0].to_string().contains("2.0"));
        assert_eq!(result[1].name(), "bar");
    }

    #[test]
    fn test_patched_simple_append_new() {
        let ctx = make_solved_ctx();
        let patch = vec![Requirement::new("baz-1.0").unwrap()];
        let result = ctx
            .get_patched_request_simple(&patch, false, 0)
            .expect("patch");
        assert_eq!(result.len(), 3);
        assert_eq!(result[2].name(), "baz");
    }

    #[test]
    fn test_patched_simple_strict_intersection() {
        let ctx = make_solved_ctx();
        // Original: foo-1.2, patch with foo-1.0+<2.0
        let patch = vec![Requirement::new("foo-1.0+<2.0").unwrap()];
        let result = ctx
            .get_patched_request_simple(&patch, true, 0)
            .expect("patch");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].name(), "foo");
        // Should have intersection of 1.2+ and 1.0+<2.0 = 1.2+<2.0
    }

    #[test]
    fn test_patched_simple_strict_conflict() {
        let ctx = make_solved_ctx();
        // Original: foo-1.2+, patch with foo<1.0 (no intersection)
        let patch = vec![Requirement::new("foo<1.0").unwrap()];
        let result = ctx.get_patched_request_simple(&patch, true, 0);
        assert!(result.is_err());
    }

    #[cfg(any(unix, windows))]
    fn platform_context(side_effects: &Path) -> ResolvedContext {
        let platform = crate::package::bind::detect_platform();
        let side_effect_path = serde_json::to_string(&side_effects.display().to_string())
            .expect("encode Python side-effect path");
        let commands = format!(
            "{}\nopen({side_effect_path}, 'a').write('package\\n')",
            platform.commands.expect("platform bind defines commands")
        );
        let mut context = ResolvedContext::empty();
        context.status = ResolverStatus::Solved;
        // System PATH entries come from post-system setup, not the platform package.
        context.append_sys_path = true;
        context.resolved_packages = Some(vec![ResolvedPackageInfo {
            name: "platform".to_string(),
            version: Version::from_str("1.0").expect("valid test version"),
            variant_index: None,
            resource_handle: None,
            requires: Vec::new(),
            repo_path: None,
            root: None,
            pre_commands: None,
            commands: Some(commands),
            post_commands: None,
        }]);
        context
    }

    #[test]
    fn deferred_rex_phases_and_callback_share_installed_include_state() {
        let repository = tempfile::tempdir().expect("repository");
        let base = repository.path();
        let includes = base.join(".rez").join("include");
        std::fs::create_dir_all(&includes).expect("include directory");
        std::fs::write(
            includes.join("counter.py"),
            "value = 0\ndef next_value():\n    global value\n    value += 1\n    return value\n",
        )
        .expect("include source");
        std::fs::write(base.join("package.yaml"), "name: demo\nversion: '1'\n")
            .expect("package definition");

        let decorated = |field: &str| {
            format!(
                "@include('counter')\ndef aliased_hook():\n    env.{field} = str(counter.next_value())"
            )
        };
        let mut context = ResolvedContext::empty();
        context.status = ResolverStatus::Solved;
        context.append_sys_path = false;
        context.resolved_packages = Some(vec![ResolvedPackageInfo {
            name: "demo".into(),
            version: Version::from_str("1").expect("version"),
            variant_index: None,
            resource_handle: None,
            requires: Vec::new(),
            repo_path: Some(base.to_path_buf()),
            root: Some(base.to_path_buf()),
            pre_commands: Some(decorated("PRE_COUNT")),
            commands: Some("env.RAW_COUNT = 'once'".into()),
            post_commands: Some(decorated("POST_COUNT")),
        }]);
        let callback = RexExecutionCallback {
            package_name: "demo".into(),
            name: "pre_test_commands".into(),
            code: "@include('counter')\ndef differently_named():\n    env.CALLBACK_COUNT = str(counter.next_value())\n    env.CALLBACK_TEST = test.name".into(),
            package: None,
            bindings: HashMap::from([("test".into(), serde_json::json!({"name": "decorated"}))]),
            developer: false,
        };
        let environment = context
            .get_environ_with_callback(Some(HashMap::new()), Some(&callback))
            .expect("deferred Rex and callback");
        assert_eq!(environment.get("PRE_COUNT").map(String::as_str), Some("1"));
        assert_eq!(
            environment.get("RAW_COUNT").map(String::as_str),
            Some("once")
        );
        assert_eq!(environment.get("POST_COUNT").map(String::as_str), Some("2"));
        assert_eq!(
            environment.get("CALLBACK_COUNT").map(String::as_str),
            Some("3")
        );
        assert_eq!(
            environment.get("CALLBACK_TEST").map(String::as_str),
            Some("decorated")
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn execute_shell_preserves_explicit_parent_for_both_inheritance_modes() {
        #[cfg(unix)]
        let shell = ShellType::Bash;
        #[cfg(windows)]
        let shell = ShellType::Cmd;

        let side_effects = tempfile::tempdir().expect("side-effect tempdir");
        let side_effect_file = side_effects.path().join("rex-count.txt");
        let side_effect_path = serde_json::to_string(&side_effect_file.display().to_string())
            .expect("encode Python side-effect path");
        let callback = RexExecutionCallback {
            package_name: "platform".to_string(),
            name: "pre_test_commands".to_string(),
            package: None,
            bindings: HashMap::from([(
                "test".into(),
                serde_json::json!({"name": "explicit-parent"}),
            )]),
            developer: false,
            code: format!(
                "env.REZ_CONTEXT_CALLBACK.append('ran')\nopen({side_effect_path}, 'a').write('callback\\n')"
            ),
        };
        let host_parent = std::env::vars().collect::<HashMap<_, _>>();
        let baseline = crate::environment::platform_environ(
            crate::platform::Platform::current(),
            &host_parent,
        );
        let host_only = host_parent
            .iter()
            .find(|(name, value)| {
                if value.is_empty() {
                    return false;
                }
                let mut chars = name.chars();
                let valid_name = chars
                    .next()
                    .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
                    && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
                valid_name
                    && !matches!(
                        name.as_str(),
                        "PWD"
                            | "OLDPWD"
                            | "SHLVL"
                            | "_"
                            | "BASH_ENV"
                            | "PS1"
                            | "UID"
                            | "EUID"
                            | "PPID"
                            | "BASH_VERSION"
                            | "BASHOPTS"
                            | "CMDCMDLINE"
                            | "CMDEXTVERSION"
                            | "PROMPT"
                            | "CD"
                    )
                    && !baseline.keys().any(|key| key.eq_ignore_ascii_case(name))
            })
            .expect("host must expose a nonempty variable outside the platform baseline")
            .0
            .clone();
        let print_vars = if cfg!(windows) {
            format!(
                "if defined REZ_PARENT_SENTINEL (echo %REZ_PARENT_SENTINEL%) else (echo MISSING)\r\necho %REZ_CONTEXT_CALLBACK%\r\necho %PATH%\r\nif defined {host_only} (echo LEAKED) else (echo MISSING)\r\nexit /b 0"
            )
        } else {
            format!(
                "printf '%s\\n%s\\n%s\\n' \"$REZ_PARENT_SENTINEL\" \"$REZ_CONTEXT_CALLBACK\" \"$PATH\"; if [ -n \"${host_only}\" ]; then printf 'LEAKED\\n'; else printf 'MISSING\\n'; fi; exit 0"
            )
        };
        let system_paths =
            crate::environment::system_paths(crate::platform::Platform::current(), &host_parent);
        let inherits_parent_path = CONFIG.all_parent_variables
            || CONFIG
                .parent_variables
                .iter()
                .any(|name| name.eq_ignore_ascii_case("PATH"));
        let rez_bin_dir = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|path| path.to_string_lossy().into_owned()));

        for inherited in [false, true] {
            for (parent, expected_sentinel) in [
                (HashMap::new(), if cfg!(windows) { "MISSING" } else { "" }),
                (
                    HashMap::from([
                        (String::from("PATH"), String::from("rez-parent-path")),
                        (
                            String::from("REZ_PARENT_SENTINEL"),
                            String::from("explicit"),
                        ),
                    ]),
                    "explicit",
                ),
            ] {
                std::fs::write(&side_effect_file, "").expect("clear Rex side-effect counter");
                let output = platform_context(&side_effect_file)
                    .execute_shell_output(
                        Some(shell),
                        &print_vars,
                        Some(parent),
                        true,
                        inherited,
                        Some(&callback),
                    )
                    .expect("execute with explicit parent");
                assert!(output.status.success());
                let stdout = String::from_utf8_lossy(&output.stdout);
                let lines: Vec<_> = stdout.lines().collect();
                assert_eq!(lines[0], expected_sentinel);
                assert_eq!(lines[1], "ran");
                assert_eq!(lines[3], "MISSING");
                let side_effect_lines = std::fs::read_to_string(&side_effect_file)
                    .expect("read Rex side-effect counter");
                let side_effect_lines: Vec<_> = side_effect_lines.lines().collect();
                assert_eq!(side_effect_lines.len(), 2);
                assert_eq!(
                    side_effect_lines
                        .iter()
                        .filter(|line| **line == "package")
                        .count(),
                    1
                );
                assert_eq!(
                    side_effect_lines
                        .iter()
                        .filter(|line| **line == "callback")
                        .count(),
                    1
                );

                if expected_sentinel == "explicit" {
                    let mut expected_paths = Vec::new();
                    if CONFIG.rez_tools_visibility == crate::constants::RezToolsVisibility::Prepend
                    {
                        if let Some(path) = &rez_bin_dir {
                            expected_paths.push(path.clone());
                        }
                    }
                    if inherits_parent_path {
                        expected_paths.push("rez-parent-path".to_string());
                    }
                    expected_paths.extend(system_paths.iter().cloned());
                    if CONFIG.rez_tools_visibility == crate::constants::RezToolsVisibility::Append {
                        if let Some(path) = &rez_bin_dir {
                            expected_paths.push(path.clone());
                        }
                    }
                    let actual_paths = std::env::split_paths(lines[2])
                        .map(|path| path.to_string_lossy().into_owned())
                        .collect::<Vec<_>>();
                    assert_eq!(actual_paths, expected_paths);
                }
            }
        }
    }
}
