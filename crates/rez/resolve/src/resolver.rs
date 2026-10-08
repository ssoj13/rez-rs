// SPDX-License-Identifier: Apache-2.0

//! High-level package resolver wrapping the solver.
//!
//! Ported from Python rez resolver.py.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use repository::provider::{PackageProvider, PackageVariant, ResourceHandle};

use super::solver::{Solver, SolverState};
use crate::config::CONFIG;
use crate::constants::{ResolverStatus, SolverCallbackReturn, SolverStatus, VariantSelectMode};
use crate::errors::{Result, RezError};
use crate::package::filter::PackageFilterList;
use crate::package::order::PackageOrderList;
use crate::{log_debug, log_info, log_trace};
use version::{Requirement, Version};

// ---------------------------------------------------------------------------
// ResolvedPackageInfo - resolved package metadata
// ---------------------------------------------------------------------------

/// Information about a resolved package variant.
#[derive(Clone, Debug)]
pub struct ResolvedPackageInfo {
    pub name: String,
    pub version: Version,
    pub variant_index: Option<usize>,
    /// Rez resource identity, including explicit repository and package source provenance.
    pub resource_handle: Option<ResourceHandle>,
    pub requires: Vec<Requirement>,
    /// Package base directory (e.g. /packages/maya/2024)
    pub repo_path: Option<PathBuf>,
    /// Variant install root (e.g. /packages/maya/2024/python-3.10)
    pub root: Option<PathBuf>,
    /// Rex commands from package.py (e.g. env.PATH.prepend("{root}/bin"))
    pub commands: Option<String>,
    /// Rex pre_commands - run before main commands
    pub pre_commands: Option<String>,
    /// Rex post_commands - run after main commands
    pub post_commands: Option<String>,
}

impl ResolvedPackageInfo {
    /// Reload canonical metadata from the exact resource, retaining deferred source values.
    /// Synthetic/developer contexts without handles keep their directory-data fallback.
    pub(crate) fn load_data(
        &self,
    ) -> Result<Option<std::collections::HashMap<String, serde_json::Value>>> {
        if let Some(handle) = &self.resource_handle {
            let provider = repository::provider::FilesystemPackageProvider::from_paths(&[])?;
            return Ok(Some(
                provider
                    .get_candidate_for_handle(handle)?
                    .package
                    .attributes,
            ));
        }
        self.repo_path
            .as_ref()
            .map(|path| crate::serialise::load_package_data(path).map(|(data, _)| data))
            .transpose()
    }

    /// Create from a solver PackageVariant.
    pub(super) fn from_solver_variant(pv: &PackageVariant) -> Self {
        let variant = &pv.variant;
        let root = variant.root();
        Self {
            name: variant.name().to_string(),
            version: variant.version().clone(),
            variant_index: variant.index,
            resource_handle: pv.provenance.as_ref().and_then(|provenance| {
                provenance.resource_handle(variant.name(), variant.version(), variant.index)
            }),
            requires: variant.parent.requires.clone(),
            repo_path: variant.parent.base.clone(),
            root,
            commands: variant.parent.commands.clone(),
            pre_commands: variant.parent.pre_commands.clone(),
            post_commands: variant.parent.post_commands.clone(),
        }
    }

    /// Qualified name: "name-version".
    pub fn qualified_name(&self) -> String {
        format!("{}-{}", self.name, self.version)
    }
}

impl std::fmt::Display for ResolvedPackageInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.variant_index {
            Some(idx) => write!(f, "{}-{}[{}]", self.name, self.version, idx),
            None => write!(f, "{}-{}", self.name, self.version),
        }
    }
}

// ---------------------------------------------------------------------------
// Resolver - high-level resolver wrapping Solver
// ---------------------------------------------------------------------------

/// High-level package resolver.
///
/// Wraps the Solver engine with builder-pattern configuration, status mapping,
/// and optional caching (cache key computed, backend not yet wired).
pub struct Resolver {
    // Config
    package_requests: Vec<Requirement>,
    package_paths: Vec<PathBuf>,
    package_filter: Option<PackageFilterList>,
    package_orderers: Option<PackageOrderList>,
    variant_select_mode: VariantSelectMode,
    building: bool,
    timestamp: Option<u64>,
    caching: bool,

    // Callback (Arc for safe sharing with solver without unsafe)
    #[allow(clippy::type_complexity)]
    callback:
        Option<Arc<dyn Fn(&SolverState) -> (SolverCallbackReturn, String) + Send + Sync + 'static>>,

    // Result state
    status: ResolverStatus,
    resolved_packages: Option<Vec<ResolvedPackageInfo>>,
    resolved_ephemerals: Option<Vec<Requirement>>,
    failure_description: Option<String>,
    graph: Option<String>,
    from_cache: bool,

    // Metrics
    pub solve_time: f64,
    pub load_time: f64,
    pub num_solves: u32,
    pub num_fails: u32,
    pub num_loaded_packages: i64,
}

impl Resolver {
    /// Create a new resolver with required parameters.
    pub fn new(requests: Vec<Requirement>, package_paths: Vec<PathBuf>) -> Self {
        Self {
            package_requests: requests,
            package_paths,
            package_filter: None,
            package_orderers: None,
            variant_select_mode: crate::config::CONFIG.variant_select_mode,
            building: false,
            timestamp: None,
            caching: true,
            callback: None,
            status: ResolverStatus::Pending,
            resolved_packages: None,
            resolved_ephemerals: None,
            failure_description: None,
            graph: None,
            from_cache: false,
            solve_time: 0.0,
            load_time: 0.0,
            num_solves: 0,
            num_fails: 0,
            num_loaded_packages: 0,
        }
    }

    // -- Builder methods --

    /// Set package filter.
    pub fn with_filter(mut self, filter: PackageFilterList) -> Self {
        self.package_filter = Some(filter);
        self
    }

    /// Set package orderers.
    pub fn with_orderers(mut self, orderers: PackageOrderList) -> Self {
        self.package_orderers = Some(orderers);
        self
    }

    /// Set variant selection mode.
    pub fn with_variant_mode(mut self, mode: VariantSelectMode) -> Self {
        self.variant_select_mode = mode;
        self
    }

    /// Set building flag (includes build_requires in resolve).
    pub fn with_building(mut self, building: bool) -> Self {
        self.building = building;
        self
    }

    /// Set timestamp for the resolve.
    pub fn with_timestamp(mut self, ts: u64) -> Self {
        self.timestamp = Some(ts);
        self
    }

    /// Enable or disable caching.
    pub fn with_caching(mut self, caching: bool) -> Self {
        self.caching = caching;
        self
    }

    /// Set a progress callback.
    pub fn with_callback(
        mut self,
        cb: impl Fn(&SolverState) -> (SolverCallbackReturn, String) + Send + Sync + 'static,
    ) -> Self {
        self.callback = Some(Arc::new(cb));
        self
    }

    // -- Accessors --

    /// Current resolver status.
    pub fn status(&self) -> ResolverStatus {
        self.status
    }

    /// Resolved packages (None if not yet solved or failed).
    pub fn resolved_packages(&self) -> Option<&[ResolvedPackageInfo]> {
        self.resolved_packages.as_deref()
    }

    /// Resolved ephemeral requirements (None if not yet solved).
    pub fn resolved_ephemerals(&self) -> Option<&[Requirement]> {
        self.resolved_ephemerals.as_deref()
    }

    /// Failure description (None if not failed).
    pub fn failure_description(&self) -> Option<&str> {
        self.failure_description.as_deref()
    }

    /// Dependency graph in DOT format (None if not yet solved).
    pub fn graph_str(&self) -> Option<&str> {
        self.graph.as_deref()
    }

    /// Whether result came from cache.
    pub fn from_cache(&self) -> bool {
        self.from_cache
    }

    // -- Solve --

    /// Perform the resolve using a package provider.
    ///
    /// This is the main entry point. It creates a Solver, runs it, and
    /// maps the result to ResolverStatus + ResolvedPackageInfo list.
    pub fn resolve(&mut self, provider: &dyn PackageProvider) -> Result<()> {
        log_info!(
            "resolver",
            "Starting resolve for {} request(s)",
            self.package_requests.len()
        );
        log_trace!("resolver", "Requests: {:?}", self.package_requests);
        if self.status != ResolverStatus::Pending {
            return Err(RezError::Resolve("resolver has already been run".into()));
        }

        // Persistent cache is only safe when every input has a stable identity.
        let provider_cache_identity = self.caching.then(|| provider.cache_identity()).flatten();
        if let Some(cached) = self.get_cached_solve(provider_cache_identity.as_deref()) {
            self.from_cache = true;
            self.set_result_from_cache(cached);
            return Ok(());
        }

        // Run solver
        self.from_cache = false;
        let t_start = Instant::now();

        // C5 fix: pass orderers to solver
        let orderers_ref = self
            .package_orderers
            .as_ref()
            .map(|o| o as &PackageOrderList);
        let mut solver = Solver::new(
            self.package_requests.clone(),
            provider,
            self.building,
            orderers_ref,
        )?;
        solver.set_variant_select_mode(self.variant_select_mode)?;
        // Pass package filter to solver if present (C2 fix)
        if let Some(ref filter) = self.package_filter {
            solver.set_package_filter(filter.clone())?;
        }

        // Pass timestamp to solver if present (C1 fix)
        if let Some(ts) = self.timestamp {
            solver.set_timestamp(ts)?;
        }

        // Clone Arc to share callback with solver (no unsafe needed)
        if let Some(ref cb) = self.callback {
            let cb_clone = Arc::clone(cb);
            solver.set_callback(move |state: &SolverState| cb_clone(state));
        }

        log_debug!("resolver", "Running solver");
        let solve_result = solver.solve();
        let num_loaded_packages = i64::try_from(solver.num_loaded_packages()).map_err(|_| {
            RezError::Resolve("num_loaded_packages exceeds the supported range".into())
        })?;
        if let Err(error) = solve_result {
            self.num_loaded_packages = num_loaded_packages;
            return Err(error);
        }

        let elapsed = t_start.elapsed().as_secs_f64();
        log_info!(
            "resolver",
            "Resolve finished in {:.3}s, status={:?}",
            elapsed,
            solver.status()
        );

        // Extract results from solver (C5 fix: extract before mut borrow)
        let status = solver.status();
        let resolved_pkgs = solver.resolved_packages();
        let resolved_ephemerals = solver.resolved_ephemerals();
        let failure_reason = solver.failure_description();
        let abort_reason = solver.abort_reason.clone();
        let solve_time = solver.solve_time;
        let load_time = solver.load_time;
        let num_solves = solver.num_solves();
        let num_fails = solver.num_fails();
        let graph = solver.graph_as_dot();

        // Drop solver to end borrow on self.package_orderers
        drop(solver);

        // Now we can take mutable borrow on self
        self.set_result_from_extracted(
            status,
            resolved_pkgs,
            resolved_ephemerals,
            failure_reason,
            abort_reason,
        );
        self.solve_time = solve_time;
        self.load_time = load_time;
        self.num_solves = num_solves;
        self.num_fails = num_fails;
        self.num_loaded_packages = num_loaded_packages;
        self.graph = Some(graph);

        // Graph: solver.solve_time already includes its internal timing,
        // but total wall time is our elapsed
        if self.solve_time == 0.0 {
            self.solve_time = elapsed;
        }

        // Do not write a result if the provider changed during the solve.
        if let (ResolverStatus::Solved, Some(identity)) =
            (self.status, provider_cache_identity.as_deref())
        {
            if provider.cache_identity().as_deref() == Some(identity) {
                self.set_cached_solve(identity);
            }
        }

        Ok(())
    }

    /// Map extracted solver results to resolver state (C5 fix: avoids borrow conflict).
    fn set_result_from_extracted(
        &mut self,
        status: SolverStatus,
        resolved_pkgs: Option<Vec<PackageVariant>>,
        resolved_ephemerals: Vec<Requirement>,
        failure_desc: Option<String>,
        abort_reason: Option<String>,
    ) {
        match status {
            SolverStatus::Solved => {
                self.status = ResolverStatus::Solved;
                if let Some(variants) = resolved_pkgs {
                    self.resolved_packages = Some(
                        variants
                            .iter()
                            .map(ResolvedPackageInfo::from_solver_variant)
                            .collect(),
                    );
                }
                self.resolved_ephemerals = Some(resolved_ephemerals);
            }
            SolverStatus::Failed | SolverStatus::Cyclic => {
                self.status = ResolverStatus::Failed;
                self.failure_description = failure_desc;
            }
            SolverStatus::Unsolved => {
                self.status = ResolverStatus::Aborted;
                self.failure_description = abort_reason;
            }
            _ => {
                self.status = ResolverStatus::Failed;
                self.failure_description =
                    Some(format!("Unexpected solver status after solve: {}", status,));
            }
        }
    }

    // -- Caching (disk: ~/.rez/resolve_cache/) --

    fn cache_key(
        &self,
        provider_identity: &str,
        error_on_missing_variant_requires: bool,
    ) -> Option<String> {
        // These inputs affect candidate selection or callback behavior but do
        // not yet have a stable, complete persistent representation.
        if self.package_filter.is_some()
            || self.package_orderers.is_some()
            || self.callback.is_some()
        {
            return None;
        }

        let requests: Vec<String> = self
            .package_requests
            .iter()
            .map(ToString::to_string)
            .collect();
        let package_paths: Vec<String> = self
            .package_paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        serde_json::to_string(&(
            "rez-rs-resolve-cache-v3",
            requests,
            package_paths,
            self.building,
            self.timestamp,
            format!("{:?}", self.variant_select_mode),
            error_on_missing_variant_requires,
            provider_identity,
        ))
        .ok()
    }

    fn resolve_cache_dir() -> Option<PathBuf> {
        let home = crate::platform::SystemInfo::home()?;
        Some(PathBuf::from(home).join(".rez").join("resolve_cache"))
    }

    fn get_cached_solve(&self, provider_identity: Option<&str>) -> Option<CachedSolveResult> {
        if !CONFIG.resolve_caching {
            return None;
        }
        let key = self.cache_key(provider_identity?, CONFIG.error_on_missing_variant_requires)?;
        let dir = Self::resolve_cache_dir()?;
        let path = dir.join(format!(
            "{:016x}.json",
            crate::util::stable_hash(key.as_bytes())
        ));
        let data = std::fs::read_to_string(&path).ok()?;
        let ser: CachedSolveResultSer = serde_json::from_str(&data).ok()?;
        if ser.cache_key != key {
            return None;
        }
        ser.to_cached_result().ok()
    }

    fn set_cached_solve(&self, provider_identity: &str) {
        if !CONFIG.resolve_caching {
            return;
        }
        let Some(key) = self.cache_key(provider_identity, CONFIG.error_on_missing_variant_requires)
        else {
            return;
        };
        let Some(dir) = Self::resolve_cache_dir() else {
            return;
        };
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let path = dir.join(format!(
            "{:016x}.json",
            crate::util::stable_hash(key.as_bytes())
        ));
        let ser = CachedSolveResultSer::from_resolver(self, key);
        if let Ok(json) = serde_json::to_string_pretty(&ser) {
            if std::fs::write(&path, json).is_ok() {
                log_debug!("resolver", "Cached solve result to {}", path.display());
            }
        }
    }

    /// Apply cached result to resolver state.
    fn set_result_from_cache(&mut self, cached: CachedSolveResult) {
        self.status = cached.status;
        self.resolved_packages = cached.resolved_packages;
        self.resolved_ephemerals = cached.resolved_ephemerals;
        self.failure_description = cached.failure_description;
        self.graph = cached.graph;
        self.solve_time = cached.solve_time;
        self.load_time = cached.load_time;
    }
}

/// Cached solve result.
#[derive(Clone, Debug)]
struct CachedSolveResult {
    status: ResolverStatus,
    resolved_packages: Option<Vec<ResolvedPackageInfo>>,
    resolved_ephemerals: Option<Vec<Requirement>>,
    failure_description: Option<String>,
    graph: Option<String>,
    solve_time: f64,
    load_time: f64,
}

/// Serializable cache entry (uses Strings for paths/versions).
#[derive(serde::Serialize, serde::Deserialize)]
struct CachedSolveResultSer {
    cache_key: String,
    status: ResolverStatus,
    resolved_packages: Option<Vec<ResolvedPackageInfoSer>>,
    resolved_ephemerals: Option<Vec<String>>,
    failure_description: Option<String>,
    graph: Option<String>,
    solve_time: f64,
    load_time: f64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ResolvedPackageInfoSer {
    name: String,
    version: String,
    variant_index: Option<usize>,
    #[serde(default)]
    resource_handle: Option<ResourceHandle>,
    requires: Vec<String>,
    repo_path: Option<String>,
    root: Option<String>,
    commands: Option<String>,
    pre_commands: Option<String>,
    post_commands: Option<String>,
}

impl CachedSolveResultSer {
    fn from_resolver(r: &Resolver, cache_key: String) -> Self {
        let resolved_packages = r.resolved_packages.as_ref().map(|v| {
            v.iter()
                .map(|p| ResolvedPackageInfoSer {
                    name: p.name.clone(),
                    version: p.version.to_string(),
                    variant_index: p.variant_index,
                    resource_handle: p.resource_handle.clone(),
                    requires: p.requires.iter().map(|x| x.to_string()).collect(),
                    repo_path: p
                        .repo_path
                        .as_ref()
                        .map(|x| x.to_string_lossy().to_string()),
                    root: p.root.as_ref().map(|x| x.to_string_lossy().to_string()),
                    commands: p.commands.clone(),
                    pre_commands: p.pre_commands.clone(),
                    post_commands: p.post_commands.clone(),
                })
                .collect()
        });
        let resolved_ephemerals = r
            .resolved_ephemerals
            .as_ref()
            .map(|v| v.iter().map(|x| x.to_string()).collect());
        Self {
            cache_key,
            status: r.status,
            resolved_packages,
            resolved_ephemerals,
            failure_description: r.failure_description.clone(),
            graph: r.graph.clone(),
            solve_time: r.solve_time,
            load_time: r.load_time,
        }
    }

    fn to_cached_result(&self) -> Result<CachedSolveResult> {
        let resolved_packages = self
            .resolved_packages
            .as_ref()
            .map(|packages| -> Result<Vec<ResolvedPackageInfo>> {
                packages
                    .iter()
                    .map(|package| {
                        let version = Version::new(&package.version)?;
                        let requires = package
                            .requires
                            .iter()
                            .map(|requirement| Requirement::new(requirement))
                            .collect::<Result<Vec<_>>>()?;

                        if let Some(handle) = &package.resource_handle {
                            handle.validate()?;
                            if handle.variables.name != package.name
                                || handle.variables.version.as_deref().unwrap_or("")
                                    != package.version
                                || handle.variables.index != package.variant_index
                            {
                                return Err(RezError::Resolve(
                                    "Cached package identity does not match its resource handle"
                                        .into(),
                                ));
                            }
                            let provider =
                                repository::provider::FilesystemPackageProvider::from_paths(&[])?;
                            let variant = provider
                                .get_candidate_for_handle(handle)?
                                .into_variant(handle.variables.index, false)?;
                            return Ok(ResolvedPackageInfo::from_solver_variant(&variant));
                        }
                        Ok(ResolvedPackageInfo {
                            name: package.name.clone(),
                            version,
                            variant_index: package.variant_index,
                            resource_handle: package.resource_handle.clone(),
                            requires,
                            repo_path: package.repo_path.as_ref().map(PathBuf::from),
                            root: package.root.as_ref().map(PathBuf::from),
                            commands: package.commands.clone(),
                            pre_commands: package.pre_commands.clone(),
                            post_commands: package.post_commands.clone(),
                        })
                    })
                    .collect()
            })
            .transpose()?;
        let resolved_ephemerals = self
            .resolved_ephemerals
            .as_ref()
            .map(|requirements| {
                requirements
                    .iter()
                    .map(|requirement| Requirement::new(requirement))
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?;
        Ok(CachedSolveResult {
            status: self.status,
            resolved_packages,
            resolved_ephemerals,
            failure_description: self.failure_description.clone(),
            graph: self.graph.clone(),
            solve_time: self.solve_time,
            load_time: self.load_time,
        })
    }
}

// ---------------------------------------------------------------------------
// Convenience function
// ---------------------------------------------------------------------------

/// Convenience: resolve packages in one call.
///
/// Creates a Resolver, runs it, and returns resolved packages on success.
pub fn resolve(
    requests: Vec<Requirement>,
    paths: Vec<PathBuf>,
    provider: &dyn PackageProvider,
    building: bool,
) -> Result<Vec<ResolvedPackageInfo>> {
    let mut resolver = Resolver::new(requests, paths).with_building(building);
    resolver.resolve(provider)?;

    match resolver.status() {
        ResolverStatus::Solved => Ok(resolver.resolved_packages.unwrap_or_default()),
        ResolverStatus::Failed => Err(RezError::Resolve(
            resolver
                .failure_description
                .unwrap_or_else(|| "resolve failed".into()),
        )),
        ResolverStatus::Aborted => Err(RezError::Resolve(
            resolver
                .failure_description
                .unwrap_or_else(|| "resolve aborted".into()),
        )),
        ResolverStatus::Pending => Err(RezError::Resolve("resolve did not complete".into())),
    }
}

/// Convenience: resolve with a filter.
pub fn resolve_filtered(
    requests: Vec<Requirement>,
    paths: Vec<PathBuf>,
    provider: &dyn PackageProvider,
    filter: PackageFilterList,
    building: bool,
) -> Result<Vec<ResolvedPackageInfo>> {
    let mut resolver = Resolver::new(requests, paths)
        .with_building(building)
        .with_filter(filter);
    resolver.resolve(provider)?;

    match resolver.status() {
        ResolverStatus::Solved => Ok(resolver.resolved_packages.unwrap_or_default()),
        _ => Err(RezError::Resolve(
            resolver
                .failure_description
                .unwrap_or_else(|| "resolve failed".into()),
        )),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::Package;
    use repository::provider::MemoryPackageProvider;
    use version::Version;

    /// Helper: parse a Requirement from string.
    fn req(s: &str) -> Requirement {
        s.parse().expect("valid requirement")
    }

    /// Helper: parse a Version from string.
    fn ver(s: &str) -> Version {
        s.parse().expect("valid version")
    }

    /// Helper: create a simple package with no dependencies.
    fn simple_pkg(name: &str, version: &str) -> Package {
        Package {
            name: name.into(),
            version: ver(version),
            ..Package::default()
        }
    }

    /// Helper: create a package with requires.
    fn pkg_with_requires(name: &str, version: &str, requires: &[&str]) -> Package {
        Package {
            name: name.into(),
            version: ver(version),
            requires: requires.iter().map(|s| req(s)).collect(),
            ..Package::default()
        }
    }

    /// Build a provider with the given packages.
    fn make_provider(packages: Vec<Package>) -> MemoryPackageProvider {
        let mut provider = MemoryPackageProvider::new();
        for pkg in packages {
            provider.add(pkg);
        }
        provider
    }

    fn cached_solve_result_ser() -> CachedSolveResultSer {
        CachedSolveResultSer {
            cache_key: "cache-key".into(),
            status: ResolverStatus::Solved,
            resolved_packages: Some(vec![ResolvedPackageInfoSer {
                name: "foo".into(),
                version: "1.0".into(),
                variant_index: None,
                resource_handle: None,
                requires: vec!["bar".into()],
                repo_path: None,
                root: None,
                commands: None,
                pre_commands: None,
                post_commands: None,
            }]),
            resolved_ephemerals: Some(vec!["baz".into()]),
            failure_description: None,
            graph: None,
            solve_time: 0.25,
            load_time: 0.1,
        }
    }

    // -- ResolvedPackageInfo tests --

    #[test]
    fn test_resolved_pkg_info_display() {
        let info = ResolvedPackageInfo {
            name: "foo".into(),
            version: ver("1.2.3"),
            variant_index: None,
            resource_handle: None,
            requires: vec![],
            repo_path: None,
            root: None,
            commands: None,
            pre_commands: None,
            post_commands: None,
        };
        assert_eq!(info.to_string(), "foo-1.2.3");
        assert_eq!(info.qualified_name(), "foo-1.2.3");
    }

    #[test]
    fn test_resolved_pkg_info_display_with_variant() {
        let info = ResolvedPackageInfo {
            name: "bar".into(),
            version: ver("2.0"),
            variant_index: Some(1),
            resource_handle: None,
            requires: vec![],
            repo_path: None,
            root: None,
            commands: None,
            pre_commands: None,
            post_commands: None,
        };
        assert_eq!(info.to_string(), "bar-2.0[1]");
    }

    // -- Resolver creation tests --

    #[test]
    fn test_resolver_new_defaults() {
        let r = Resolver::new(vec![req("foo-1+")], vec![PathBuf::from("/pkgs")]);
        assert_eq!(r.status(), ResolverStatus::Pending);
        assert!(r.resolved_packages().is_none());
        assert!(r.failure_description().is_none());
        assert!(r.graph_str().is_none());
        assert!(!r.from_cache());
        assert_eq!(r.solve_time, 0.0);
        assert_eq!(r.num_solves, 0);
    }

    #[test]
    fn test_resolver_builder_chain() {
        let r = Resolver::new(vec![req("foo")], vec![])
            .with_building(true)
            .with_timestamp(12345)
            .with_caching(false)
            .with_variant_mode(VariantSelectMode::IntersectionPriority);

        assert!(r.building);
        assert_eq!(r.timestamp, Some(12345));
        assert!(!r.caching);
        assert_eq!(
            r.variant_select_mode,
            VariantSelectMode::IntersectionPriority
        );
    }

    // -- Simple resolve tests --

    #[test]
    fn test_resolve_single_package() {
        let provider = make_provider(vec![
            simple_pkg("foo", "1.0.0"),
            simple_pkg("foo", "1.1.0"),
            simple_pkg("foo", "2.0.0"),
        ]);

        let mut resolver = Resolver::new(vec![req("foo-1+<2")], vec![]);
        resolver.resolve(&provider).expect("resolve should succeed");

        assert_eq!(resolver.status(), ResolverStatus::Solved);
        let pkgs = resolver.resolved_packages().expect("should have packages");
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "foo");
        assert_eq!(pkgs[0].version, ver("1.1.0"));
        assert_eq!(
            resolver.num_loaded_packages, 2,
            "count both eligible versions, not only the selected package"
        );
    }

    #[test]
    fn test_resolve_multiple_packages() {
        let provider = make_provider(vec![
            simple_pkg("foo", "1.0"),
            pkg_with_requires("bar", "1.0", &["foo-1"]),
        ]);

        let mut resolver = Resolver::new(vec![req("bar-1")], vec![]);
        resolver.resolve(&provider).expect("resolve should succeed");

        assert_eq!(resolver.status(), ResolverStatus::Solved);
        let pkgs = resolver.resolved_packages().expect("should have packages");
        assert_eq!(pkgs.len(), 2);
        let names: Vec<&str> = pkgs.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"bar"));
    }

    #[test]
    fn test_resolve_conflict_fails_and_preserves_dot_graph() {
        let provider = make_provider(vec![simple_pkg("foo", "1.0"), simple_pkg("foo", "2.0")]);
        let mut resolver = Resolver::new(vec![req("foo-==1.0.0"), req("foo-==2.0.0")], vec![]);

        resolver
            .resolve(&provider)
            .expect("solver should report failure as state");

        assert_eq!(resolver.status(), ResolverStatus::Failed);
        let graph = resolver
            .graph_str()
            .expect("failed solve should retain graph");
        assert!(graph.contains("digraph rez_resolve"));
        assert!(graph.contains("CONFLICT"));
        assert!(graph.contains("foo==1.0.0"));
        assert!(graph.contains("foo==2.0.0"));
        assert_eq!(
            resolver.num_loaded_packages, 0,
            "conflicting requests are rejected before any package entry is loaded"
        );
    }

    #[test]
    fn test_resolve_not_found_fails() {
        let provider = make_provider(vec![simple_pkg("foo", "1.0")]);

        // Disable caching so we don't hit a stale disk cache from a previous run
        let mut resolver = Resolver::new(vec![req("bar-1")], vec![]).with_caching(false);
        let result = resolver.resolve(&provider);
        // Not found should either error or set Failed status
        let failed = result.is_err() || resolver.status() == ResolverStatus::Failed;
        assert!(failed, "non-existent package should fail");
    }

    #[test]
    fn test_resolve_double_call_errors() {
        let provider = make_provider(vec![simple_pkg("foo", "1.0")]);

        let mut resolver = Resolver::new(vec![req("foo")], vec![]);
        resolver.resolve(&provider).expect("first resolve ok");

        let err = resolver.resolve(&provider);
        assert!(err.is_err(), "second resolve should fail");
    }

    // -- Convenience function tests --

    #[test]
    fn test_convenience_resolve() {
        let provider = make_provider(vec![simple_pkg("foo", "1.0"), simple_pkg("foo", "2.0")]);

        let result = resolve(vec![req("foo-1+")], vec![], &provider, false)
            .expect("convenience resolve should succeed");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "foo");
        assert_eq!(result[0].version, ver("2.0"));
    }

    #[test]
    fn test_convenience_resolve_conflict() {
        let provider = make_provider(vec![simple_pkg("foo", "1.0")]);

        let result = resolve(vec![req("foo-1"), req("foo-2")], vec![], &provider, false);

        assert!(result.is_err());
    }

    // -- Metrics tests --

    #[test]
    fn test_metrics_populated() {
        let provider = make_provider(vec![
            simple_pkg("foo", "1.0"),
            pkg_with_requires("bar", "1.0", &["foo"]),
        ]);

        let mut resolver = Resolver::new(vec![req("bar")], vec![]).with_caching(false);
        resolver.resolve(&provider).expect("resolve ok");

        assert_eq!(resolver.status(), ResolverStatus::Solved);
        assert!(resolver.solve_time >= 0.0);
        assert!(resolver.num_solves > 0);
    }

    // -- Cache entry validation tests --

    #[test]
    fn cached_filesystem_handles_rehydrate_current_exact_metadata() {
        let owned = tempfile::tempdir().unwrap();
        let package_path = owned.path().join("foo/1.0");
        std::fs::create_dir_all(&package_path).unwrap();
        std::fs::write(
            package_path.join("package.yaml"),
            "name: foo\nversion: '1.0'\ncommands: |\n  env.FRESH = 'source'\n",
        )
        .unwrap();
        let mut cached = cached_solve_result_ser();
        let package = &mut cached.resolved_packages.as_mut().unwrap()[0];
        package.commands = Some("stale serialized command".into());
        package.resource_handle = Some(repository::provider::ResourceHandle {
            key: repository::provider::ResourceHandleKey::FilesystemVariant,
            variables: repository::provider::ResourceHandleVariables {
                repository_type: "filesystem".into(),
                location: owned.path().to_string_lossy().into_owned(),
                name: "foo".into(),
                version: Some("1.0".into()),
                index: None,
                ext: None,
            },
        });
        let result = cached.to_cached_result().unwrap();
        assert!(result.resolved_packages.unwrap()[0]
            .commands
            .as_deref()
            .unwrap()
            .contains("env.FRESH"));
        cached.resolved_packages.as_mut().unwrap()[0]
            .resource_handle
            .as_mut()
            .unwrap()
            .variables
            .name = "different".into();
        assert!(cached.to_cached_result().is_err());
        cached.resolved_packages.as_mut().unwrap()[0]
            .resource_handle
            .as_mut()
            .unwrap()
            .variables
            .name = "foo".into();
        std::fs::remove_file(package_path.join("package.yaml")).unwrap();
        assert!(
            cached.to_cached_result().is_err(),
            "missing exact source becomes a resolve-cache miss"
        );
    }

    #[test]
    fn test_cached_solve_result_round_trips_valid_package_data() {
        let result = cached_solve_result_ser()
            .to_cached_result()
            .expect("valid cache entry should deserialize");

        assert_eq!(result.status, ResolverStatus::Solved);
        let packages = result.resolved_packages.expect("cached packages");
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "foo");
        assert_eq!(packages[0].version, ver("1.0"));
        assert_eq!(
            packages[0]
                .requires
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec!["bar"]
        );
        assert_eq!(
            result
                .resolved_ephemerals
                .expect("cached ephemerals")
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec!["baz"]
        );
    }

    #[test]
    fn test_cached_solve_result_rejects_malformed_package_version() {
        assert!(Version::new("..").is_err());
        let mut cached = cached_solve_result_ser();
        cached.resolved_packages.as_mut().unwrap()[0].version = "..".into();

        assert!(cached.to_cached_result().is_err());
    }

    #[test]
    fn test_cached_solve_result_rejects_malformed_package_requirement() {
        assert!(Requirement::new("bar-1..0").is_err());
        let mut cached = cached_solve_result_ser();
        cached.resolved_packages.as_mut().unwrap()[0].requires =
            vec!["bar".into(), "bar-1..0".into()];

        assert!(cached.to_cached_result().is_err());
    }

    #[test]
    fn test_cached_solve_result_rejects_malformed_ephemeral_requirement() {
        assert!(Requirement::new("baz-1..0").is_err());
        let mut cached = cached_solve_result_ser();
        cached.resolved_ephemerals = Some(vec!["baz".into(), "baz-1..0".into()]);

        assert!(cached.to_cached_result().is_err());
    }

    // -- Cache key tests --

    #[test]
    fn test_cache_key_is_deterministic_and_covers_resolve_inputs() {
        let base = Resolver::new(vec![req("foo-1")], vec![PathBuf::from("/a")]);
        let same = Resolver::new(vec![req("foo-1")], vec![PathBuf::from("/a")]);
        let key = base.cache_key("provider-v1", false).unwrap();
        assert_eq!(
            Some(key.as_str()),
            same.cache_key("provider-v1", false).as_deref()
        );
        assert_ne!(
            Some(key.as_str()),
            base.cache_key("provider-v1", true).as_deref()
        );

        assert_ne!(
            Some(key.as_str()),
            base.cache_key("provider-v2", false).as_deref()
        );
        assert_ne!(
            Some(key.as_str()),
            Resolver::new(vec![req("foo-2")], vec![PathBuf::from("/a")])
                .cache_key("provider-v1", false)
                .as_deref()
        );
        assert_ne!(
            Some(key.as_str()),
            Resolver::new(vec![req("foo-1")], vec![PathBuf::from("/b")])
                .cache_key("provider-v1", false)
                .as_deref()
        );
        assert_ne!(
            Some(key.as_str()),
            Resolver::new(vec![req("foo-1")], vec![PathBuf::from("/a")])
                .with_building(true)
                .cache_key("provider-v1", false)
                .as_deref()
        );
        assert_ne!(
            Some(key.as_str()),
            Resolver::new(vec![req("foo-1")], vec![PathBuf::from("/a")])
                .with_timestamp(100)
                .cache_key("provider-v1", false)
                .as_deref()
        );
        assert_ne!(
            Some(key.as_str()),
            Resolver::new(vec![req("foo-1")], vec![PathBuf::from("/a")])
                .with_variant_mode(VariantSelectMode::IntersectionPriority)
                .cache_key("provider-v1", false)
                .as_deref()
        );
    }

    #[test]
    fn test_persistent_cache_is_disabled_for_unfingerprinted_or_unrepresented_inputs() {
        let provider = make_provider(vec![simple_pkg("foo", "1")]);
        assert!(provider.cache_identity().is_none());
        assert!(Resolver::new(vec![req("foo")], vec![])
            .with_filter(PackageFilterList::new())
            .cache_key("provider-v1", false)
            .is_none());
        assert!(Resolver::new(vec![req("foo")], vec![])
            .with_orderers(PackageOrderList::new())
            .cache_key("provider-v1", false)
            .is_none());
        assert!(Resolver::new(vec![req("foo")], vec![])
            .with_callback(|_| (SolverCallbackReturn::KeepGoing, String::new()))
            .cache_key("provider-v1", false)
            .is_none());
    }

    #[test]
    fn test_timestamp_filters_initial_request_candidates() {
        let mut future_pkg = simple_pkg("timestamp_probe", "1.0");
        future_pkg.timestamp = Some(200);
        let provider = make_provider(vec![future_pkg]);

        let mut resolver = Resolver::new(vec![req("timestamp_probe")], vec![])
            .with_timestamp(100)
            .with_caching(false);
        let error = resolver
            .resolve(&provider)
            .expect_err("an existing family with no timestamp-eligible package is missing");

        assert!(matches!(error, RezError::PackageNotFound(_)));
    }

    #[test]
    fn test_timestamp_filters_transitive_candidates_returns_package_not_found() {
        let mut root = pkg_with_requires("timestamp_root", "1.0", &["timestamp_dep"]);
        root.timestamp = Some(50);
        let mut future_dependency = simple_pkg("timestamp_dep", "1.0");
        future_dependency.timestamp = Some(200);
        let provider = make_provider(vec![root, future_dependency]);

        let mut resolver = Resolver::new(vec![req("timestamp_root")], vec![])
            .with_timestamp(100)
            .with_caching(false);
        let error = resolver
            .resolve(&provider)
            .expect_err("existing family without timestamp-eligible versions is missing");

        assert!(matches!(error, RezError::PackageNotFound(_)));
    }

    // -- Callback / abort tests --

    #[test]
    fn test_resolve_with_callback_wiring() {
        // Verify callback is properly wired - simple solves complete before
        // callback fires, so we test that the callback mechanism doesn't break
        // normal resolution
        let provider = make_provider(vec![
            simple_pkg("foo", "1.0"),
            pkg_with_requires("bar", "1.0", &["foo"]),
        ]);

        let mut resolver = Resolver::new(vec![req("bar")], vec![])
            .with_callback(|_state| (SolverCallbackReturn::KeepGoing, String::new()));

        resolver
            .resolve(&provider)
            .expect("resolve with callback should work");
        // Simple solve completes regardless of callback
        assert_eq!(resolver.status(), ResolverStatus::Solved);
        // Callback is preserved after resolve
        assert!(resolver.callback.is_some());
    }

    // -- Building mode test --

    #[test]
    fn test_resolve_with_building() {
        let mut build_pkg = simple_pkg("app", "1.0");
        build_pkg.build_requires = vec![req("cmake-3")];

        let provider = make_provider(vec![build_pkg, simple_pkg("cmake", "3.20")]);

        let mut resolver = Resolver::new(vec![req("app")], vec![]).with_building(true);
        resolver.resolve(&provider).expect("resolve ok");

        assert_eq!(resolver.status(), ResolverStatus::Solved);
        let pkgs = resolver.resolved_packages().expect("packages");
        let names: Vec<&str> = pkgs.iter().map(|p| p.name.as_str()).collect();
        assert!(
            names.contains(&"cmake"),
            "build_requires should be included"
        );
    }

    #[test]
    fn test_resolve_without_building_excludes_build_requires() {
        let mut build_pkg = simple_pkg("app", "1.0");
        build_pkg.build_requires = vec![req("cmake-3")];

        let provider = make_provider(vec![build_pkg, simple_pkg("cmake", "3.20")]);

        let mut resolver = Resolver::new(vec![req("app")], vec![]).with_building(false);
        resolver.resolve(&provider).expect("resolve ok");

        assert_eq!(resolver.status(), ResolverStatus::Solved);
        let pkgs = resolver.resolved_packages().expect("packages");
        let names: Vec<&str> = pkgs.iter().map(|p| p.name.as_str()).collect();
        assert!(
            !names.contains(&"cmake"),
            "build_requires should be excluded"
        );
    }
}
