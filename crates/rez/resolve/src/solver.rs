// SPDX-License-Identifier: Apache-2.0

//! Dependency resolution engine with phase-based backtracking.
//!
//! Ported from Python rez solver.py. Takes requests → loads variants via PackageProvider →
//! applies orderers/filter → resolves graph. `Solver::solve` returns `SolverStatus`.

use crate::constants::{SolverCallbackReturn, SolverStatus, VariantSelectMode};
use crate::errors::{Result, RezError};
use crate::package::filter::PackageFilterList;
use crate::package::order::PackageOrderList;
use crate::package::Variant;
use crate::{log_debug, log_info, log_trace};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::time::Instant;
use version::{Requirement, RequirementList, Version, VersionRange, VersionedObject};

// ---------------------------------------------------------------------------
// Failure types
// ---------------------------------------------------------------------------

/// A variant was removed because its dependency conflicted with another scope.
#[derive(Clone, Debug)]
pub struct Reduction {
    pub name: String,
    pub version: Version,
    pub variant_index: Option<usize>,
    pub dependency: Requirement,
    pub conflicting_request: Requirement,
}

impl Reduction {
    /// Get requirements involved in this reduction.
    pub fn involved_reqs(&self) -> Vec<Requirement> {
        let range = exact_range(&self.version);
        let req = Requirement::construct(&self.name, range);
        vec![
            req,
            self.dependency.clone(),
            self.conflicting_request.clone(),
        ]
    }
}

impl fmt::Display for Reduction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let idx = match self.variant_index {
            Some(i) => format!("[{}]", i),
            None => "[]".to_string(),
        };
        write!(
            f,
            "{}-{}{} (dep({}) <--!--> {})",
            self.name, self.version, idx, self.dependency, self.conflicting_request
        )
    }
}

/// A common dependency shared by all variants conflicted with another scope.
#[derive(Clone, Debug)]
pub struct DependencyConflict {
    pub dependency: Requirement,
    pub conflicting_request: Requirement,
}

impl fmt::Display for DependencyConflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} <--!--> {}",
            self.dependency, self.conflicting_request
        )
    }
}

/// Reason a solve phase failed.
#[derive(Clone, Debug)]
pub enum FailureReason {
    /// All variants in a scope were reduced away.
    TotalReduction(Vec<Reduction>),
    /// Common dependencies conflicted between scopes.
    DependencyConflicts(Vec<DependencyConflict>),
    /// Cyclic dependency detected.
    Cycle(Vec<VersionedObject>),
}

impl FailureReason {
    /// Human-readable description of the failure.
    pub fn description(&self) -> String {
        match self {
            Self::TotalReduction(reds) => {
                let s: Vec<String> = reds.iter().map(|r| format!("({})", r)).collect();
                format!("A package was completely reduced: {}", s.join(" "))
            }
            Self::DependencyConflicts(conflicts) => {
                let s: Vec<String> = conflicts.iter().map(|c| format!("({})", c)).collect();
                format!("The following package conflicts occurred: {}", s.join(" "))
            }
            Self::Cycle(pkgs) => {
                let mut stmts: Vec<String> = pkgs.iter().map(|p| p.to_string()).collect();
                if let Some(first) = pkgs.first() {
                    stmts.push(first.to_string());
                }
                format!("A cyclic dependency was detected: {}", stmts.join(" --> "))
            }
        }
    }

    /// Get all requirements involved in the failure.
    pub fn involved_reqs(&self) -> Vec<Requirement> {
        match self {
            Self::TotalReduction(reds) => reds.iter().flat_map(|r| r.involved_reqs()).collect(),
            Self::DependencyConflicts(conflicts) => {
                let mut out = Vec::new();
                for c in conflicts {
                    out.push(c.dependency.clone());
                    out.push(c.conflicting_request.clone());
                }
                out
            }
            Self::Cycle(pkgs) => pkgs
                .iter()
                .map(|p| {
                    let range = exact_range(p.version());
                    Requirement::construct(p.name(), range)
                })
                .collect(),
        }
    }
}

impl fmt::Display for FailureReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.description())
    }
}

// ---------------------------------------------------------------------------
// SolverState - callback info
// ---------------------------------------------------------------------------

/// Current state of the solver, passed to callbacks.
#[derive(Debug)]
pub struct SolverState {
    pub num_solves: u32,
    pub num_fails: u32,
    pub phase_str: String,
}

impl fmt::Display for SolverState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "solve #{} ({} fails so far): {}",
            self.num_solves, self.num_fails, self.phase_str
        )
    }
}

// ---------------------------------------------------------------------------
// PackageProvider - trait for supplying packages to solver
// ---------------------------------------------------------------------------

use repository::provider::{PackageProvider, PackageVariant};

// ---------------------------------------------------------------------------
// PackageEntry - variants grouped by package version
// ---------------------------------------------------------------------------

/// Variants of a single package version, with sort state.
#[derive(Clone, Debug)]
struct PackageEntry {
    version: Version,
    variants: Vec<PackageVariant>,
    sorted: bool,
}

impl PackageEntry {
    fn new(version: Version, variants: Vec<PackageVariant>) -> Self {
        Self {
            version,
            variants,
            sorted: false,
        }
    }

    fn len(&self) -> usize {
        self.variants.len()
    }

    /// Sort variants by index for deterministic ordering.
    fn sort(&mut self) {
        if self.sorted {
            return;
        }
        self.variants.sort_by_key(|v| v.index().unwrap_or(0));
        self.sorted = true;
    }

    /// Split entry at n variants. Returns (first_n, rest) or None if n >= len.
    fn split(&mut self, n: usize) -> Option<(PackageEntry, PackageEntry)> {
        if n >= self.variants.len() {
            return None;
        }
        self.sort();
        let rest = self.variants.split_off(n);
        let first = PackageEntry {
            version: self.version.clone(),
            variants: self.variants.clone(),
            sorted: true,
        };
        let second = PackageEntry {
            version: self.version.clone(),
            variants: rest,
            sorted: true,
        };
        // Restore self
        self.variants.extend(second.variants.clone());
        Some((first, second))
    }
}

#[derive(Clone)]
struct VariantSelectionKey {
    requested: Vec<(usize, String, Option<VersionRange>)>,
    additional: Vec<(String, Option<VersionRange>)>,
    index: usize,
}

impl VariantSelectionKey {
    fn new(variant: &mut PackageVariant, request_list: &RequirementList) -> Self {
        let requires = variant.requires_list().clone();
        let mut requested = Vec::new();
        let mut names = HashSet::new();
        for (i, request) in request_list.requirements().iter().enumerate() {
            if request.conflict() {
                continue;
            }
            if let Some(requirement) = requires.get(request.name()) {
                requested.push((
                    i,
                    requirement.name().to_string(),
                    requirement.range().cloned(),
                ));
                names.insert(requirement.name().to_string());
            }
        }

        let additional = requires
            .iter()
            .filter(|requirement| !requirement.conflict() && !names.contains(requirement.name()))
            .map(|requirement| (requirement.name().to_string(), requirement.range().cloned()))
            .collect();

        Self {
            requested,
            additional,
            index: variant.index().unwrap_or(0),
        }
    }
}

fn compare_range_keys(
    family: &str,
    left: Option<&VersionRange>,
    right: Option<&VersionRange>,
    orderers: Option<&PackageOrderList>,
) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => match orderers {
            Some(orderers) => orderers.compare_ranges(family, left, right),
            None => left.cmp_by(right, |a, b| a.cmp(b)),
        },
        // Rez maps an unbounded requirement to the same neutral key for every
        // candidate, preserving stable variant order.
        (None, None) | (None, Some(_)) | (Some(_), None) => Ordering::Equal,
    }
}

fn compare_variant_keys(
    left: &VariantSelectionKey,
    right: &VariantSelectionKey,
    mode: VariantSelectMode,
    orderers: Option<&PackageOrderList>,
) -> Ordering {
    let requested_cmp = || {
        for ((left_i, left_name, left_range), (right_i, right_name, right_range)) in
            left.requested.iter().zip(&right.requested)
        {
            let index = right_i.cmp(left_i);
            if index != Ordering::Equal {
                return index;
            }
            let family = left_name;
            let range =
                compare_range_keys(family, left_range.as_ref(), right_range.as_ref(), orderers);
            if range != Ordering::Equal {
                return range;
            }
            debug_assert_eq!(left_name, right_name);
        }
        left.requested.len().cmp(&right.requested.len())
    };
    let additional_cmp = || {
        for ((left_name, left_range), (right_name, right_range)) in
            left.additional.iter().zip(&right.additional)
        {
            let range = compare_range_keys(
                left_name,
                left_range.as_ref(),
                right_range.as_ref(),
                orderers,
            );
            if range != Ordering::Equal {
                return range;
            }
            let name = left_name.cmp(right_name);
            if name != Ordering::Equal {
                return name;
            }
        }
        left.additional.len().cmp(&right.additional.len())
    };

    let version_priority = || {
        requested_cmp()
            .reverse()
            .then_with(|| left.additional.len().cmp(&right.additional.len()))
            .then_with(|| additional_cmp().reverse())
            .then_with(|| right.index.cmp(&left.index))
    };

    match mode {
        VariantSelectMode::VersionPriority => version_priority(),
        VariantSelectMode::IntersectionPriority => right
            .requested
            .len()
            .cmp(&left.requested.len())
            .then_with(version_priority),
    }
}

// ---------------------------------------------------------------------------
// VariantSlice - subset of variants with dependency tracking
// ---------------------------------------------------------------------------

/// A subset of package variants with dependency-related tracking.
/// Supports intersection, reduction, extraction, and splitting operations.
#[derive(Clone, Debug)]
struct VariantSlice {
    package_name: String,
    entries: Vec<PackageEntry>,
    extracted_fams: HashSet<String>,
    sorted: bool,
    // Cached info
    common_fams: Option<HashSet<String>>,
    fam_requires: Option<HashSet<String>>,
}

impl VariantSlice {
    fn new(package_name: String, entries: Vec<PackageEntry>) -> Self {
        Self {
            package_name,
            entries,
            extracted_fams: HashSet::new(),
            sorted: false,
            common_fams: None,
            fam_requires: None,
        }
    }

    /// Total number of variants across all entries.
    fn len(&self) -> usize {
        self.entries.iter().map(|e| e.len()).sum()
    }

    /// Version range spanned by entries.
    fn range(&self) -> VersionRange {
        let versions: Vec<&Version> = self.entries.iter().map(|e| &e.version).collect();
        if versions.is_empty() {
            return VersionRange::any();
        }
        union_range(&versions)
    }

    /// Whether there are extractable common families remaining.
    fn extractable(&mut self) -> bool {
        let common = self.common_fams().clone();
        !self.extracted_fams.is_superset(&common)
    }

    /// Compute common and all-required family info.
    fn update_fam_info(&mut self) {
        if self.common_fams.is_some() {
            return;
        }
        if self.entries.is_empty() || self.entries[0].variants.is_empty() {
            self.common_fams = Some(HashSet::new());
            self.fam_requires = Some(HashSet::new());
            return;
        }
        let first_fams = {
            self.entries[0].sort();
            self.entries[0].variants[0].request_fams()
        };
        let mut common = first_fams;
        let mut all_fams = HashSet::new();

        for entry in &mut self.entries {
            for variant in &mut entry.variants {
                let req_fams = variant.request_fams();
                let conflict_fams = variant.conflict_request_fams();
                common = common.intersection(&req_fams).cloned().collect();
                all_fams.extend(req_fams);
                all_fams.extend(conflict_fams);
            }
        }
        self.common_fams = Some(common);
        self.fam_requires = Some(all_fams);
    }

    fn common_fams(&mut self) -> &HashSet<String> {
        self.update_fam_info();
        self.common_fams.as_ref().expect("just computed")
    }

    fn fam_requires(&mut self) -> &HashSet<String> {
        self.update_fam_info();
        self.fam_requires.as_ref().expect("just computed")
    }

    /// Remove entries whose version falls outside the range.
    fn intersect(&self, range: &VersionRange) -> Option<VariantSlice> {
        if range.is_any() {
            return Some(self.clone());
        }

        let entries: Vec<PackageEntry> = self
            .entries
            .iter()
            .filter(|e| range.contains_version(&e.version))
            .cloned()
            .collect();

        if entries.is_empty() {
            None
        } else if entries.len() < self.entries.len() {
            Some(self.copy_with(entries))
        } else {
            Some(self.clone())
        }
    }

    /// Remove variants whose dependencies conflict with the given request.
    fn reduce_by(
        &mut self,
        package_request: &Requirement,
    ) -> (Option<VariantSlice>, Vec<Reduction>) {
        // No range or not in our required families -> no reduction
        if package_request.range().is_none() {
            return (Some(self.clone()), vec![]);
        }

        let fam_reqs = self.fam_requires().clone();
        if !fam_reqs.contains(package_request.name()) {
            return (Some(self.clone()), vec![]);
        }

        let mut new_entries = Vec::new();
        let mut reductions = Vec::new();

        for entry in &mut self.entries {
            let mut new_variants = Vec::new();

            for variant in &mut entry.variants {
                let req = variant.get_req(package_request.name());
                if let Some(ref req) = req {
                    if req.conflicts_with_req(package_request) {
                        reductions.push(Reduction {
                            name: variant.name().to_string(),
                            version: variant.version().clone(),
                            variant_index: variant.index(),
                            dependency: req.clone(),
                            conflicting_request: package_request.clone(),
                        });
                        continue;
                    }
                }
                new_variants.push(variant.clone());
            }

            if !new_variants.is_empty() {
                new_entries.push(PackageEntry {
                    version: entry.version.clone(),
                    variants: new_variants,
                    sorted: entry.sorted,
                });
            }
        }

        if new_entries.is_empty() {
            (None, reductions)
        } else if !reductions.is_empty() {
            (Some(self.copy_with(new_entries)), reductions)
        } else {
            (Some(self.clone()), vec![])
        }
    }

    /// Extract a common dependency shared by all variants.
    fn extract(&mut self) -> (VariantSlice, Option<Requirement>) {
        if !self.extractable() {
            return (self.clone(), None);
        }

        let extractable: HashSet<String> = {
            let common = self.common_fams().clone();
            common.difference(&self.extracted_fams).cloned().collect()
        };

        // Sort for determinism
        let mut sorted_extractable: Vec<&String> = extractable.iter().collect();
        sorted_extractable.sort();
        let fam = sorted_extractable[0].clone();

        // Collect all ranges for this family across variants
        let mut ranges = Vec::new();
        for entry in &mut self.entries {
            for variant in &mut entry.variants {
                if let Some(req) = variant.get_req(&fam) {
                    if let Some(range) = req.range() {
                        if !ranges.contains(range) {
                            ranges.push(range.clone());
                        }
                    }
                }
            }
        }

        let mut new_slice = self.clone();
        new_slice.extracted_fams.insert(fam.clone());
        // Reset cached fam info since extracted_fams changed
        new_slice.common_fams = None;
        new_slice.fam_requires = None;

        if ranges.is_empty() {
            return (new_slice, None);
        }

        // Union all ranges
        let mut combined = ranges[0].clone();
        for r in &ranges[1..] {
            combined = &combined | r;
        }
        let common_req = Requirement::construct(&fam, Some(combined));

        (new_slice, Some(common_req))
    }

    /// Split the slice into two halves for backtracking.
    fn split_slice(&mut self) -> Option<(VariantSlice, VariantSlice)> {
        // Sort versions descending
        self.sort_versions();

        // Trivial case: split on first variant
        if self.len() <= 1 {
            return None;
        }

        self.entries[0].sort();

        // Split at first entry boundary or first variant
        if self.entries[0].len() > 1 {
            if let Some((first, rest)) = self.entries[0].split(1) {
                let entries_a = vec![first];
                let entries_b = {
                    let mut e = vec![rest];
                    e.extend(self.entries[1..].to_vec());
                    e
                };
                return Some((self.copy_with(entries_a), self.copy_with(entries_b)));
            }
        }

        if self.entries.len() > 1 {
            let entries_a = vec![self.entries[0].clone()];
            let entries_b = self.entries[1..].to_vec();
            Some((self.copy_with(entries_a), self.copy_with(entries_b)))
        } else {
            None
        }
    }

    /// Sort entries by version descending.
    fn sort_versions(&mut self) {
        if self.sorted {
            return;
        }
        self.entries.sort_by(|a, b| {
            b.version
                .partial_cmp(&a.version)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        self.sorted = true;
    }

    /// Create a copy with new entries, preserving sort/extracted state.
    fn copy_with(&self, entries: Vec<PackageEntry>) -> VariantSlice {
        VariantSlice {
            package_name: self.package_name.clone(),
            entries,
            extracted_fams: self.extracted_fams.clone(),
            sorted: self.sorted,
            common_fams: None,
            fam_requires: None,
        }
    }
}

impl fmt::Display for VariantSlice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let nvariants = self.len();
        let nversions = self.entries.len();
        if nvariants == 1 {
            let v = &self.entries[0].variants[0];
            let idx = match v.index() {
                Some(i) => format!("[{}]", i),
                None => String::new(),
            };
            write!(f, "[{}=={}{}]", self.package_name, v.version(), idx)
        } else {
            write!(
                f,
                "{}[({})..({} versions)]",
                self.package_name, nvariants, nversions
            )
        }
    }
}

// ---------------------------------------------------------------------------
// VariantCache - caches loaded variants per package name
// ---------------------------------------------------------------------------

/// Cache of loaded package variants, keyed by package name.
struct VariantCache<'a> {
    provider: &'a dyn PackageProvider,
    building: bool,
    package_filter: Option<&'a PackageFilterList>,
    timestamp: Option<u64>,
    orderers: Option<&'a PackageOrderList>,
    request_list: RequirementList,
    variant_select_mode: VariantSelectMode,
    cache: HashMap<String, Vec<PackageEntry>>,
    eligible_versions: HashMap<String, Vec<Version>>,
    loaded_packages: HashSet<(String, String)>,
}

impl<'a> VariantCache<'a> {
    fn new(
        provider: &'a dyn PackageProvider,
        building: bool,
        package_filter: Option<&'a PackageFilterList>,
        timestamp: Option<u64>,
        orderers: Option<&'a PackageOrderList>,
        request_list: RequirementList,
        variant_select_mode: VariantSelectMode,
    ) -> Self {
        Self {
            provider,
            building,
            package_filter,
            timestamp, // C1 fix
            orderers,  // C5 fix
            request_list,
            variant_select_mode,
            cache: HashMap::new(),
            eligible_versions: HashMap::new(),
            loaded_packages: HashSet::new(),
        }
    }

    /// Get a variant slice for packages matching name and range.
    fn get_slice(&mut self, name: &str, range: &VersionRange) -> Result<Option<VariantSlice>> {
        // Load all packages for this name if not cached
        if !self.cache.contains_key(name) {
            log_trace!("solver", "loading packages for {}", name);
            let candidates = self.provider.get_all_candidates(name)?;
            let mut entries = Vec::new();
            let mut eligible_versions = Vec::new();
            for candidate in candidates {
                let pkg = candidate.package;
                let provenance = candidate.provenance;
                // Apply package filter if present
                if let Some(filter) = self.package_filter {
                    if filter.is_excluded_package(&pkg) {
                        continue; // skip filtered package
                    }
                }

                // C1 fix: Apply timestamp filter - skip packages released AFTER timestamp
                if let Some(ts) = self.timestamp {
                    if let Some(pkg_ts) = pkg.timestamp {
                        if pkg_ts > i128::from(ts) {
                            continue; // skip package released after timestamp
                        }
                    }
                }

                // Rez calls package_load_callback after range/filter eligibility and before variant expansion.
                eligible_versions.push(pkg.version.clone());

                let mut variants = Vec::new();
                if pkg.variants.is_empty() {
                    // Non-variant package: single variant with index=None
                    let v = Variant::from_package(pkg.clone());
                    variants.push(PackageVariant::new(v, self.building, provenance.clone()));
                } else {
                    for var in pkg.iter_variants() {
                        variants.push(PackageVariant::new(var, self.building, provenance.clone()));
                    }
                }
                if !variants.is_empty() {
                    let mut entry = PackageEntry::new(pkg.version.clone(), variants);
                    let mut ranked: Vec<_> = entry
                        .variants
                        .drain(..)
                        .map(|mut variant| {
                            let key = VariantSelectionKey::new(&mut variant, &self.request_list);
                            (key, variant)
                        })
                        .collect();
                    ranked.sort_by(|(left, _), (right, _)| {
                        compare_variant_keys(left, right, self.variant_select_mode, self.orderers)
                    });
                    entry.variants = ranked.into_iter().map(|(_, variant)| variant).collect();
                    entry.sorted = true;
                    entries.push(entry);
                }
            }
            self.eligible_versions
                .insert(name.to_owned(), eligible_versions);

            // C5: Apply package orderers to reorder versions
            if let Some(orderers) = self.orderers {
                let mut versions: Vec<Version> =
                    entries.iter().map(|e| e.version.clone()).collect();
                orderers.reorder(name, &mut versions);
                let mut ordered = Vec::with_capacity(entries.len());
                for v in &versions {
                    if let Some(pos) = entries.iter().position(|e| &e.version == v) {
                        ordered.push(entries.remove(pos));
                    }
                }
                entries = ordered;
            }
            log_debug!(
                "solver",
                "loaded {} version(s) for {} ({} variant(s) total)",
                entries.len(),
                name,
                entries.iter().map(|e| e.len()).sum::<usize>()
            );
            self.cache.insert(name.to_string(), entries);
        }

        let Some(all_entries) = self.cache.get(name) else {
            return Ok(None);
        };

        // Filter by range
        let mut entries: Vec<PackageEntry> = all_entries
            .iter()
            .filter(|e| range.contains_version(&e.version))
            .cloned()
            .collect();
        if let Some(eligible_versions) = self.eligible_versions.get(name) {
            self.loaded_packages.extend(
                eligible_versions
                    .iter()
                    .filter(|version| range.contains_version(version))
                    .map(|version| (name.to_owned(), version.to_string())),
            );
        }

        // C5 fix: Apply package orderers if present
        if let Some(orderers) = self.orderers {
            // Extract versions, reorder them, then reorder entries accordingly
            let mut versions: Vec<Version> = entries.iter().map(|e| e.version.clone()).collect();
            orderers.reorder(name, &mut versions);

            // Create a map of version->index in reordered list
            let version_order: HashMap<&Version, usize> =
                versions.iter().enumerate().map(|(i, v)| (v, i)).collect();

            // Sort entries by reordered version indices
            entries.sort_by_key(|e| version_order.get(&e.version).copied().unwrap_or(usize::MAX));
        }

        Ok(if entries.is_empty() {
            None
        } else {
            Some(VariantSlice::new(name.to_string(), entries))
        })
    }
}

// ---------------------------------------------------------------------------
// PackageScope - wraps a variant slice or conflict requirement
// ---------------------------------------------------------------------------

/// Contains possible solutions for a package. Can be a variant slice (normal)
/// or a conflict/ephemeral requirement (no variants).
#[derive(Clone, Debug)]
struct PackageScope {
    package_name: String,
    package_request: Option<Requirement>,
    variant_slice: Option<VariantSlice>,
    is_ephemeral: bool,
}

impl PackageScope {
    /// Create scope from a variant slice (normal package).
    fn from_slice(name: &str, slice: VariantSlice) -> Self {
        let range = slice.range();
        let req = Requirement::construct(name, Some(range));
        Self {
            package_name: name.to_string(),
            package_request: Some(req),
            variant_slice: Some(slice),
            is_ephemeral: false,
        }
    }

    /// Create scope for a conflict or ephemeral requirement.
    fn from_request(req: &Requirement) -> Self {
        Self {
            package_name: req.name().to_string(),
            package_request: Some(req.clone()),
            variant_slice: None,
            is_ephemeral: req.name().starts_with('.'),
        }
    }

    fn is_conflict(&self) -> bool {
        self.package_request
            .as_ref()
            .map(|r| r.conflict())
            .unwrap_or(false)
    }

    /// Intersect this scope with a version range.
    fn intersect(&self, range: &VersionRange, cache: &mut VariantCache) -> Result<ScopeResult> {
        // Ephemerals: range intersection
        if self.is_ephemeral {
            if let Some(req) = &self.package_request {
                if let Some(req_range) = req.range() {
                    let new_range = if self.is_conflict() {
                        req_range - range
                    } else {
                        req_range & range
                    };
                    match new_range {
                        None => return Ok(ScopeResult::Empty),
                        Some(r) if r == *req_range => return Ok(ScopeResult::Unchanged),
                        Some(r) => {
                            let mut scope = self.clone();
                            scope.package_request =
                                Some(Requirement::construct(&self.package_name, Some(r)));
                            return Ok(ScopeResult::Changed(Box::new(scope)));
                        }
                    }
                }
            }
            return Ok(ScopeResult::Unchanged);
        }

        // Conflict scope: need to get fresh slice
        if self.is_conflict() {
            if let Some(req) = &self.package_request {
                let new_range = if let Some(req_range) = req.range() {
                    range - req_range
                } else {
                    Some(range.clone())
                };
                if let Some(nr) = new_range {
                    if let Some(slice) = cache.get_slice(&self.package_name, &nr)? {
                        let scope = PackageScope::from_slice(&self.package_name, slice);
                        return Ok(ScopeResult::Changed(Box::new(scope)));
                    }
                }
                return Ok(ScopeResult::Empty);
            }
            return Ok(ScopeResult::Empty);
        }

        // Normal scope: intersect variant slice
        Ok(if let Some(slice) = &self.variant_slice {
            match slice.intersect(range) {
                None => ScopeResult::Empty,
                Some(new_slice) => {
                    if new_slice.len() == slice.len() {
                        ScopeResult::Unchanged
                    } else {
                        ScopeResult::Changed(Box::new(PackageScope::from_slice(
                            &self.package_name,
                            new_slice,
                        )))
                    }
                }
            }
        } else {
            ScopeResult::Unchanged
        })
    }

    /// Reduce this scope against a package request.
    fn reduce_by(&mut self, package_request: &Requirement) -> (ScopeAction, Vec<Reduction>) {
        if self.is_conflict() || self.is_ephemeral {
            return (ScopeAction::Unchanged, vec![]);
        }

        if let Some(slice) = &mut self.variant_slice {
            let (new_slice, reductions) = slice.reduce_by(package_request);
            match new_slice {
                None => (ScopeAction::Removed, reductions),
                Some(ns) => {
                    if reductions.is_empty() {
                        (ScopeAction::Unchanged, vec![])
                    } else {
                        let scope = PackageScope::from_slice(&self.package_name, ns);
                        (ScopeAction::Replaced(Box::new(scope)), reductions)
                    }
                }
            }
        } else {
            (ScopeAction::Unchanged, vec![])
        }
    }

    /// Extract a common dependency from this scope.
    fn extract(&mut self) -> (PackageScope, Option<Requirement>) {
        if self.is_conflict() || self.is_ephemeral {
            return (self.clone(), None);
        }

        if let Some(slice) = &mut self.variant_slice {
            let (new_slice, req) = slice.extract();
            if req.is_some() {
                let mut scope = self.clone();
                scope.variant_slice = Some(new_slice);
                scope.update_request();
                (scope, req)
            } else {
                (self.clone(), None)
            }
        } else {
            (self.clone(), None)
        }
    }

    /// Split this scope for backtracking.
    fn split_scope(&mut self) -> Option<(PackageScope, PackageScope)> {
        if self.is_conflict() || self.is_ephemeral {
            return None;
        }

        if let Some(slice) = &mut self.variant_slice {
            if slice.len() <= 1 {
                return None;
            }
            if let Some((a, b)) = slice.split_slice() {
                Some((
                    PackageScope::from_slice(&self.package_name, a),
                    PackageScope::from_slice(&self.package_name, b),
                ))
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Check if this scope is solved (single variant, all deps extracted).
    fn is_solved(&mut self) -> bool {
        if self.is_conflict() || self.is_ephemeral {
            return true;
        }
        if let Some(slice) = &mut self.variant_slice {
            slice.len() == 1 && !slice.extractable()
        } else {
            true
        }
    }

    /// Get the solved variant, if scope is solved.
    fn solved_variant(&mut self) -> Option<PackageVariant> {
        if let Some(slice) = &mut self.variant_slice {
            if slice.len() == 1 && !slice.extractable() {
                slice.entries[0].sort();
                Some(slice.entries[0].variants[0].clone())
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Update package_request from current variant slice range.
    fn update_request(&mut self) {
        if let Some(slice) = &self.variant_slice {
            let range = slice.range();
            self.package_request = Some(Requirement::construct(&self.package_name, Some(range)));
        }
    }
}

impl fmt::Display for PackageScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(slice) = &self.variant_slice {
            write!(f, "{}", slice)
        } else if let Some(req) = &self.package_request {
            write!(f, "{}", req)
        } else {
            write!(f, "{}", self.package_name)
        }
    }
}

/// Result of scope intersection.
enum ScopeResult {
    Unchanged,
    Changed(Box<PackageScope>),
    Empty,
}

/// Result of scope reduction.
enum ScopeAction {
    Unchanged,
    Replaced(Box<PackageScope>),
    Removed,
}

// ---------------------------------------------------------------------------
// ResolvePhase - full snapshot of resolve state
// ---------------------------------------------------------------------------

/// A resolve phase contains a full copy of the resolve state. Runs the
/// extract -> intersect -> reduce loop until no further progress, then
/// reports solved/exhausted/failed.
#[derive(Clone, Debug)]
struct ResolvePhase {
    scopes: Vec<PackageScope>,
    failure_reason: Option<FailureReason>,
    status: SolverStatus,
    extractions: HashMap<(String, String), Requirement>,
    changed_scopes_i: HashSet<usize>,
}

impl ResolvePhase {
    fn new(request_list: &RequirementList, cache: &mut VariantCache) -> Result<Self> {
        let mut scopes = Vec::new();

        for req in request_list.requirements() {
            if req.conflict() || req.name().starts_with('.') {
                scopes.push(PackageScope::from_request(req));
            } else {
                let range = req.range().cloned().unwrap_or_else(VersionRange::any);
                match cache.get_slice(req.name(), &range)? {
                    Some(slice) => scopes.push(PackageScope::from_slice(req.name(), slice)),
                    None => {
                        return Err(RezError::PackageNotFound(format!(
                            "Package could not be found: {}",
                            req
                        )));
                    }
                }
            }
        }

        let changed = (0..scopes.len()).collect();
        Ok(Self {
            scopes,
            failure_reason: None,
            status: SolverStatus::Pending,
            extractions: HashMap::new(),
            changed_scopes_i: changed,
        })
    }

    /// Attempt to solve this phase.
    fn solve(&self, cache: &mut VariantCache) -> Result<ResolvePhase> {
        if self.status != SolverStatus::Pending {
            return Ok(self.clone());
        }

        let mut scopes = self.scopes.clone();
        let mut failure_reason = None;
        let mut extractions = self.extractions.clone();
        let mut changed_scopes_i = self.changed_scopes_i.clone();

        // Main loop: extract -> intersect -> reduce
        loop {
            let prev_num_scopes = scopes.len();
            let mut widened_scopes_i: HashSet<usize> = HashSet::new();

            // Inner loop: extract until no more extractions
            loop {
                let mut extracted_requests = Vec::new();

                // Perform all possible extractions
                for scope in scopes.iter_mut() {
                    loop {
                        let (new_scope, extracted_req) = scope.extract();
                        if let Some(req) = extracted_req {
                            extracted_requests.push(req.clone());
                            let key = (scope.package_name.clone(), req.name().to_string());
                            extractions.insert(key, req);
                            *scope = new_scope;
                        } else {
                            break;
                        }
                    }
                }

                if extracted_requests.is_empty() {
                    break;
                }

                // Merge extractions
                let merged = RequirementList::new(extracted_requests);
                if let Some((req1, req2)) = merged.conflict() {
                    let conflict = DependencyConflict {
                        dependency: req1.clone(),
                        conflicting_request: req2.clone(),
                    };
                    failure_reason = Some(FailureReason::DependencyConflicts(vec![conflict]));
                    return Ok(self.create_phase(
                        scopes,
                        failure_reason,
                        extractions,
                        Some(SolverStatus::Failed),
                    ));
                }

                // Intersect extractions with existing scopes
                let mut req_fams = Vec::new();
                let scope_names: Vec<String> =
                    scopes.iter().map(|s| s.package_name.clone()).collect();

                for i in 0..scopes.len() {
                    if let Some(extracted_req) = merged.get(&scope_names[i]) {
                        if let Some(range) = extracted_req.range() {
                            let result = scopes[i].intersect(range, cache)?;
                            req_fams.push(extracted_req.name().to_string());

                            match result {
                                ScopeResult::Empty => {
                                    let conflict = DependencyConflict {
                                        dependency: extracted_req.clone(),
                                        conflicting_request: scopes[i]
                                            .package_request
                                            .clone()
                                            .unwrap_or_else(|| {
                                                Requirement::construct(&scope_names[i], None)
                                            }),
                                    };
                                    failure_reason =
                                        Some(FailureReason::DependencyConflicts(vec![conflict]));
                                    return Ok(self.create_phase(
                                        scopes,
                                        failure_reason,
                                        extractions,
                                        Some(SolverStatus::Failed),
                                    ));
                                }
                                ScopeResult::Changed(new_scope) => {
                                    let was_conflict = scopes[i].is_conflict();
                                    scopes[i] = *new_scope;
                                    changed_scopes_i.insert(i);
                                    if was_conflict && !scopes[i].is_conflict() {
                                        widened_scopes_i.insert(i);
                                    }
                                }
                                ScopeResult::Unchanged => {}
                            }
                        }
                    }
                }

                // Add new scopes for extracted requirements not yet in scope list
                let new_reqs: Vec<Requirement> = merged
                    .requirements()
                    .iter()
                    .filter(|r| !req_fams.contains(&r.name().to_string()))
                    .cloned()
                    .collect();

                for req in new_reqs {
                    if req.conflict() || req.name().starts_with('.') {
                        scopes.push(PackageScope::from_request(&req));
                    } else {
                        let range = req.range().cloned().unwrap_or_else(VersionRange::any);
                        let slice = match cache.get_slice(req.name(), &range) {
                            Ok(Some(slice)) => slice,
                            Ok(None) => {
                                return Err(RezError::PackageNotFound(format!(
                                    "Package could not be found: {}",
                                    req
                                )));
                            }
                            Err(RezError::PackageFamilyNotFound(_)) => {
                                let mut requesters: Vec<String> = extractions
                                    .iter()
                                    .filter(|(_, extracted)| extracted.name() == req.name())
                                    .map(|((requester, _), _)| requester.clone())
                                    .collect();
                                requesters.sort();
                                requesters.dedup();
                                let message = if requesters.is_empty() {
                                    format!("package family not found: {}", req.name())
                                } else {
                                    format!(
                                        "package family not found: {}, was required by: {}",
                                        req.name(),
                                        requesters.join(", ")
                                    )
                                };

                                if crate::config::CONFIG.error_on_missing_variant_requires {
                                    return Err(RezError::PackageFamilyNotFound(message));
                                }

                                failure_reason = Some(FailureReason::DependencyConflicts(vec![
                                    DependencyConflict {
                                        dependency: req.clone(),
                                        conflicting_request: Requirement::construct(
                                            req.name(),
                                            None,
                                        ),
                                    },
                                ]));
                                return Ok(self.create_phase(
                                    scopes,
                                    failure_reason,
                                    extractions,
                                    Some(SolverStatus::Failed),
                                ));
                            }
                            Err(error) => return Err(error),
                        };
                        scopes.push(PackageScope::from_slice(req.name(), slice));
                    }
                }
            } // end extraction loop

            let num_scopes = scopes.len();

            // Check if we need to reduce
            if num_scopes == prev_num_scopes
                && changed_scopes_i.is_empty()
                && widened_scopes_i.is_empty()
            {
                break;
            }

            // Build pending reduction pairs
            let all_i: Vec<usize> = (0..num_scopes).collect();
            let prev_i: Vec<usize> = (0..prev_num_scopes).collect();
            let added_i: Vec<usize> = (prev_num_scopes..num_scopes).collect();

            let mut pending: Vec<(usize, usize)> = Vec::new();

            // Existing vs changed
            for &x in &prev_i {
                for &y in &changed_scopes_i {
                    if x < num_scopes && y < num_scopes {
                        pending.push((x, y));
                    }
                }
            }
            // Existing vs added
            for &x in &prev_i {
                for &y in &added_i {
                    pending.push((x, y));
                }
            }
            // Added vs all
            for &x in &added_i {
                for &y in &all_i {
                    pending.push((x, y));
                }
            }
            // Widened vs all
            for &x in &widened_scopes_i {
                for &y in &all_i {
                    if x < num_scopes {
                        pending.push((x, y));
                    }
                }
            }

            pending.sort();
            pending.dedup();

            // Iteratively reduce
            while let Some((x, y)) = pending.pop() {
                if x == y || x >= scopes.len() || y >= scopes.len() {
                    continue;
                }

                let req = match &scopes[y].package_request {
                    Some(r) => r.clone(),
                    None => continue,
                };

                let (action, reductions) = scopes[x].reduce_by(&req);

                match action {
                    ScopeAction::Removed => {
                        failure_reason = Some(FailureReason::TotalReduction(reductions));
                        return Ok(self.create_phase(
                            scopes,
                            failure_reason,
                            extractions,
                            Some(SolverStatus::Failed),
                        ));
                    }
                    ScopeAction::Replaced(new_scope) => {
                        scopes[x] = *new_scope;
                        // Other scopes need to reduce against x again
                        for j in 0..scopes.len() {
                            if j != x {
                                pending.push((j, x));
                            }
                        }
                    }
                    ScopeAction::Unchanged => {}
                }
            }

            changed_scopes_i.clear();
        } // end main loop

        // Determine final status
        Ok(self.create_phase(scopes, failure_reason, extractions, None))
    }

    /// Create a new phase with given state.
    fn create_phase(
        &self,
        scopes: Vec<PackageScope>,
        failure_reason: Option<FailureReason>,
        extractions: HashMap<(String, String), Requirement>,
        status: Option<SolverStatus>,
    ) -> ResolvePhase {
        let status = status.unwrap_or_else(|| {
            let mut s = scopes.clone();
            if s.iter_mut().all(|scope| scope.is_solved()) {
                SolverStatus::Solved
            } else {
                SolverStatus::Exhausted
            }
        });
        ResolvePhase {
            scopes,
            failure_reason,
            status,
            extractions,
            changed_scopes_i: HashSet::new(),
        }
    }

    /// Get solved variants from all scopes.
    fn solved_variants(&mut self) -> Vec<PackageVariant> {
        self.scopes
            .iter_mut()
            .filter_map(|s| s.solved_variant())
            .collect()
    }

    /// Finalise: detect cycles, reorder by dependency.
    fn finalise(&mut self) -> ResolvePhase {
        // Build dependency graph
        let mut graph = DepGraph::new();
        let scope_map: HashMap<String, usize> = self
            .scopes
            .iter()
            .enumerate()
            .map(|(i, s)| (s.package_name.clone(), i))
            .collect();

        for scope in &mut self.scopes {
            if scope.is_conflict() {
                continue;
            }
            graph.add_node(&scope.package_name);

            if let Some(variant) = scope.solved_variant() {
                let mut v = variant;
                for req in v.requires_list().requirements() {
                    if !req.conflict() {
                        graph.add_edge(&scope.package_name, req.name());
                    }
                }
            }
        }

        // Check for cycles
        if let Some(cycle_nodes) = graph.find_cycle() {
            let cycle: Vec<VersionedObject> = cycle_nodes
                .iter()
                .filter_map(|name| {
                    if let Some(&idx) = scope_map.get(name) {
                        let variant = self.scopes[idx].solved_variant()?;
                        Some(VersionedObject::construct(
                            name,
                            Some(variant.version().clone()),
                        ))
                    } else {
                        None
                    }
                })
                .collect();

            let mut phase = self.clone();
            phase.failure_reason = Some(FailureReason::Cycle(cycle));
            phase.status = SolverStatus::Cyclic;
            return phase;
        }

        // Reorder: dependencies first
        let ordered = graph.topo_order();
        let mut reordered_scopes = Vec::new();
        for name in &ordered {
            if let Some(&idx) = scope_map.get(name) {
                if !self.scopes[idx].is_conflict() {
                    reordered_scopes.push(self.scopes[idx].clone());
                }
            }
        }
        // Add any scopes not in graph
        for scope in &self.scopes {
            if !scope.is_conflict() && !ordered.contains(&scope.package_name) {
                reordered_scopes.push(scope.clone());
            }
        }

        let mut phase = self.clone();
        phase.scopes = reordered_scopes;
        phase
    }

    /// Split the phase for backtracking.
    fn split_phase(&mut self) -> Option<(ResolvePhase, ResolvePhase)> {
        let mut scopes_a = Vec::new();
        let mut scopes_b = Vec::new();
        let mut split_i = None;

        for (i, scope) in self.scopes.iter_mut().enumerate() {
            if split_i.is_none() {
                if let Some((a, b)) = scope.split_scope() {
                    scopes_a.push(a);
                    scopes_b.push(b);
                    split_i = Some(i);
                    continue;
                }
            }
            scopes_a.push(scope.clone());
            scopes_b.push(scope.clone());
        }

        let split_i = split_i?;

        let mut phase_a = self.clone();
        phase_a.scopes = scopes_a;
        phase_a.status = SolverStatus::Pending;
        phase_a.changed_scopes_i = [split_i].into_iter().collect();

        let mut phase_b = self.clone();
        phase_b.scopes = scopes_b;
        phase_b.status = SolverStatus::Pending;
        phase_b.changed_scopes_i = [split_i].into_iter().collect();

        Some((phase_a, phase_b))
    }
}

impl ResolvePhase {
    /// Render the selected phase with its requests and failure edges as DOT.
    fn graph_as_dot(&self, initial_requests: &RequirementList, status: SolverStatus) -> String {
        let mut graph = DotGraph::default();
        let mut scopes = self.scopes.clone();
        let mut scope_nodes = HashMap::new();
        let mut scope_requests = HashMap::new();
        let mut request_nodes = HashMap::new();

        for request in initial_requests.requirements() {
            let id = graph.add_node(
                format!("request:{}", request),
                &request.to_string(),
                "#FFFFAA",
                "filled,dashed",
            );
            request_nodes.insert(request.to_string(), id);
        }

        for scope in &mut scopes {
            let request = scope.package_request.clone();
            let request_key = request.as_ref().map(ToString::to_string);
            let id = if scope.is_conflict() {
                request_key
                    .as_ref()
                    .and_then(|key| request_nodes.get(key).cloned())
                    .unwrap_or_else(|| {
                        graph.add_node(
                            format!("scope:{}", scope.package_name),
                            &scope.to_string(),
                            "#F6F6F6",
                            "filled,dashed",
                        )
                    })
            } else if let Some(variant) = scope.solved_variant() {
                graph.add_node(
                    format!("scope:{}", scope.package_name),
                    &variant.to_string(),
                    "#AAFFAA",
                    "filled",
                )
            } else {
                graph.add_node(
                    format!("scope:{}", scope.package_name),
                    &scope.to_string(),
                    "#F6F6F6",
                    "filled",
                )
            };
            scope_nodes.insert(scope.package_name.clone(), id.clone());
            scope_requests.insert(scope.package_name.clone(), request);
        }

        for request in initial_requests.requirements() {
            if let (Some(from), Some(to)) = (
                request_nodes.get(&request.to_string()),
                scope_nodes.get(request.name()),
            ) {
                if from != to {
                    graph.add_edge(from, to, "");
                }
            }
        }

        for scope in &mut scopes {
            if let Some(variant) = scope.solved_variant() {
                let from = &scope_nodes[&scope.package_name];
                let mut variant = variant;
                for request in variant.requires_list().requirements() {
                    let to = graph.add_node(
                        format!("request:{}", request),
                        &request.to_string(),
                        "#F6F6F6",
                        "filled,dashed",
                    );
                    request_nodes.insert(request.to_string(), to.clone());
                    graph.add_edge(from, &to, "");

                    if let (Some(dependency_scope), Some(scope_request)) = (
                        scope_nodes.get(request.name()),
                        scope_requests.get(request.name()).and_then(Option::as_ref),
                    ) {
                        if !request.conflicts_with_req(scope_request) && to != *dependency_scope {
                            graph.add_edge(&to, dependency_scope, "");
                        }
                    }
                }
            }
        }

        let mut extractions: Vec<_> = self.extractions.iter().collect();
        extractions.sort_by(
            |((left_family, left_name), _), ((right_family, right_name), _)| {
                left_family
                    .cmp(right_family)
                    .then_with(|| left_name.cmp(right_name))
            },
        );
        for ((source_family, _), destination) in &extractions {
            if let Some(from) = scope_nodes.get(source_family) {
                let to = graph.add_node(
                    format!("request:{}", destination),
                    &destination.to_string(),
                    "#F6F6F6",
                    "filled,dashed",
                );
                request_nodes.insert(destination.to_string(), to.clone());
                graph.add_edge(from, &to, "");
            }
        }

        let mut extracted_by_family: BTreeMap<&str, Vec<&Requirement>> = BTreeMap::new();
        for ((_, family), request) in &extractions {
            extracted_by_family
                .entry(family.as_str())
                .or_default()
                .push(request);
        }
        for (family, requests) in extracted_by_family {
            if requests.len() < 2 {
                continue;
            }
            let merged =
                RequirementList::new(requests.iter().map(|request| (*request).clone()).collect());
            if merged.conflict().is_none() {
                if let Some(merged_request) = merged.get(family) {
                    for request in requests {
                        if request != merged_request {
                            let from = graph.add_node(
                                format!("request:{}", request),
                                &request.to_string(),
                                "#F6F6F6",
                                "filled,dashed",
                            );
                            let to = graph.add_node(
                                format!("request:{}", merged_request),
                                &merged_request.to_string(),
                                "#F6F6F6",
                                "filled,dashed",
                            );
                            graph.add_edge(&from, &to, "arrowhead=odot");
                        }
                    }
                }
            }
        }

        match self.failure_reason.as_ref() {
            Some(FailureReason::DependencyConflicts(conflicts)) => {
                for conflict in conflicts {
                    let conflicting = &conflict.conflicting_request;
                    let scope_id = scope_nodes.get(conflicting.name());
                    let scope_request = scope_requests
                        .get(conflicting.name())
                        .and_then(Option::as_ref);
                    let (from, to) =
                        if let (Some(scope_id), Some(scope_request)) = (scope_id, scope_request) {
                            if scope_request.conflicts_with_req(conflicting) {
                                (
                                    graph.add_node(
                                        format!("request:{}", conflicting),
                                        &conflicting.to_string(),
                                        "#F6F6F6",
                                        "filled,dashed",
                                    ),
                                    scope_id.clone(),
                                )
                            } else {
                                (
                                    graph.add_node(
                                        format!("request:{}", conflict.dependency),
                                        &conflict.dependency.to_string(),
                                        "#F6F6F6",
                                        "filled,dashed",
                                    ),
                                    scope_id.clone(),
                                )
                            }
                        } else if let Some(scope_id) = scope_id {
                            (
                                scope_id.clone(),
                                graph.add_node(
                                    format!("request:{}", conflict.dependency),
                                    &conflict.dependency.to_string(),
                                    "#F6F6F6",
                                    "filled,dashed",
                                ),
                            )
                        } else {
                            (
                                graph.add_node(
                                    format!("request:{}", conflict.dependency),
                                    &conflict.dependency.to_string(),
                                    "#F6F6F6",
                                    "filled,dashed",
                                ),
                                graph.add_node(
                                    format!("request:{}", conflicting),
                                    &conflicting.to_string(),
                                    "#F6F6F6",
                                    "filled,dashed",
                                ),
                            )
                        };
                    graph.add_edge(
                        &from,
                        &to,
                        "label=\"CONFLICT\", color=red, fontcolor=red, style=bold",
                    );
                }
            }
            Some(FailureReason::TotalReduction(reductions)) => {
                for (index, reduction) in reductions.iter().enumerate() {
                    let reduced_scope =
                        scope_nodes
                            .get(&reduction.name)
                            .cloned()
                            .unwrap_or_else(|| {
                                graph.add_node(
                                    format!("scope:{}", reduction.name),
                                    &reduction.name,
                                    "#F6F6F6",
                                    "filled",
                                )
                            });
                    let conflicting_scope = scope_nodes
                        .get(reduction.conflicting_request.name())
                        .cloned()
                        .unwrap_or_else(|| {
                            graph.add_node(
                                format!("scope:{}", reduction.conflicting_request.name()),
                                reduction.conflicting_request.name(),
                                "#F6F6F6",
                                "filled",
                            )
                        });
                    if reductions.len() == 1 {
                        let dependency = graph.add_node(
                            format!("request:{}", reduction.dependency),
                            &reduction.dependency.to_string(),
                            "#F6F6F6",
                            "filled,dashed",
                        );
                        graph.add_edge(&reduced_scope, &dependency, "");
                        graph.add_edge(
                            &dependency,
                            &conflicting_scope,
                            "label=\"CONFLICT\", color=red, fontcolor=red, style=bold",
                        );
                    } else {
                        let variant_index = reduction
                            .variant_index
                            .map(|index| format!("[{index}]"))
                            .unwrap_or_else(|| "[]".to_string());
                        let reducee =
                            format!("{}-{}{}", reduction.name, reduction.version, variant_index);
                        let dependency = graph.add_node(
                            format!("reduction:{index}"),
                            &reduction.dependency.to_string(),
                            "#F6F6F6",
                            "filled,dashed",
                        );
                        graph.add_edge(
                            &reduced_scope,
                            &dependency,
                            &format!("label={}, fontsize=10", dot_quote(&reducee)),
                        );
                        graph.add_edge(
                            &dependency,
                            &conflicting_scope,
                            "label=\"CONFLICT\", color=red, fontcolor=red, style=bold",
                        );
                    }
                }
            }
            Some(FailureReason::Cycle(packages)) if !packages.is_empty() => {
                for (index, package) in packages.iter().enumerate() {
                    let next = &packages[(index + 1) % packages.len()];
                    let from = scope_nodes.get(package.name()).cloned().unwrap_or_else(|| {
                        graph.add_node(
                            format!("scope:{}", package.name()),
                            &package.to_string(),
                            "#F6F6F6",
                            "filled",
                        )
                    });
                    let to = scope_nodes.get(next.name()).cloned().unwrap_or_else(|| {
                        graph.add_node(
                            format!("scope:{}", next.name()),
                            &next.to_string(),
                            "#F6F6F6",
                            "filled",
                        )
                    });
                    graph.add_edge(
                        &from,
                        &to,
                        "label=\"CYCLE\", color=red, fontcolor=red, style=bold",
                    );
                }
            }
            Some(FailureReason::Cycle(_)) | None => {}
        }

        graph.finish(status)
    }
}

/// DOT graph assembled from phase snapshots. Stable node ids keep output
/// deterministic even when phase maps are hash-based.
#[derive(Default)]
struct DotGraph {
    nodes: Vec<String>,
    edges: Vec<String>,
    node_ids: HashMap<String, String>,
    edge_positions: HashMap<(String, String), usize>,
    next_node: usize,
}

impl DotGraph {
    fn add_node(&mut self, key: String, label: &str, color: &str, style: &str) -> String {
        if let Some(id) = self.node_ids.get(&key) {
            return id.clone();
        }
        let id = format!("n{}", self.next_node);
        self.next_node += 1;
        self.node_ids.insert(key, id.clone());
        self.nodes.push(format!(
            "  {} [label={}, fillcolor={}, style={}];",
            id,
            dot_quote(label),
            dot_quote(color),
            dot_quote(style),
        ));
        id
    }

    fn add_edge(&mut self, from: &str, to: &str, attributes: &str) {
        let key = (from.to_string(), to.to_string());
        let line = if attributes.is_empty() {
            format!("  {from} -> {to};")
        } else {
            format!("  {from} -> {to} [{attributes}];")
        };
        if let Some(index) = self.edge_positions.get(&key).copied() {
            self.edges[index] = line;
        } else {
            self.edge_positions.insert(key, self.edges.len());
            self.edges.push(line);
        }
    }

    fn finish(self, status: SolverStatus) -> String {
        let mut dot = format!(
            "digraph rez_resolve {{\n  label={};\n  node [shape=box, style=filled, fontsize=10];\n",
            dot_quote(&format!("Resolve: {status}")),
        );
        for node in self.nodes {
            dot.push_str(&node);
            dot.push('\n');
        }
        for edge in self.edges {
            dot.push_str(&edge);
            dot.push('\n');
        }
        dot.push_str("}\n");
        dot
    }
}

fn dot_quote(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r");
    format!("\"{escaped}\"")
}

impl fmt::Display for ResolvePhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let strs: Vec<String> = self.scopes.iter().map(|s| s.to_string()).collect();
        write!(f, "{}", strs.join(" "))
    }
}

// ---------------------------------------------------------------------------
// DepGraph - simple directed graph for cycle detection
// ---------------------------------------------------------------------------

/// Simple directed graph for cycle detection and topological ordering.
#[derive(Clone, Debug, Default)]
struct DepGraph {
    nodes: HashSet<String>,
    edges: HashMap<String, HashSet<String>>,
}

impl DepGraph {
    fn new() -> Self {
        Self::default()
    }

    fn add_node(&mut self, name: &str) {
        self.nodes.insert(name.to_string());
    }

    fn add_edge(&mut self, from: &str, to: &str) {
        self.nodes.insert(from.to_string());
        self.nodes.insert(to.to_string());
        self.edges
            .entry(from.to_string())
            .or_default()
            .insert(to.to_string());
    }

    /// Find a cycle using DFS. Returns cycle nodes or None.
    fn find_cycle(&self) -> Option<Vec<String>> {
        let mut visited = HashSet::new();
        let mut rec_stack = HashSet::new();
        let mut path = Vec::new();

        for node in &self.nodes {
            if !visited.contains(node) {
                if let Some(cycle) = self.dfs_cycle(node, &mut visited, &mut rec_stack, &mut path) {
                    return Some(cycle);
                }
            }
        }
        None
    }

    fn dfs_cycle(
        &self,
        node: &str,
        visited: &mut HashSet<String>,
        rec_stack: &mut HashSet<String>,
        path: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        visited.insert(node.to_string());
        rec_stack.insert(node.to_string());
        path.push(node.to_string());

        if let Some(neighbors) = self.edges.get(node) {
            // Sort neighbors for determinism
            let mut sorted: Vec<&String> = neighbors.iter().collect();
            sorted.sort();
            for neighbor in sorted {
                if !self.nodes.contains(neighbor) {
                    continue;
                }
                if !visited.contains(neighbor.as_str()) {
                    if let Some(cycle) = self.dfs_cycle(neighbor, visited, rec_stack, path) {
                        return Some(cycle);
                    }
                } else if rec_stack.contains(neighbor.as_str()) {
                    // Found cycle - extract it from path
                    let start = path.iter().position(|n| n == neighbor).unwrap_or(0);
                    return Some(path[start..].to_vec());
                }
            }
        }

        path.pop();
        rec_stack.remove(node);
        None
    }

    /// Topological sort (Kahn's algorithm). Returns nodes in dependency order.
    fn topo_order(&self) -> Vec<String> {
        let mut in_degree: HashMap<String, usize> = HashMap::new();
        for node in &self.nodes {
            in_degree.entry(node.clone()).or_insert(0);
        }
        for neighbors in self.edges.values() {
            for n in neighbors {
                if self.nodes.contains(n) {
                    *in_degree.entry(n.clone()).or_insert(0) += 1;
                }
            }
        }

        let mut queue: Vec<String> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(n, _)| n.clone())
            .collect();
        queue.sort(); // deterministic

        let mut result = Vec::new();
        while let Some(node) = queue.pop() {
            result.push(node.clone());
            if let Some(neighbors) = self.edges.get(&node) {
                let mut sorted_n: Vec<&String> = neighbors.iter().collect();
                sorted_n.sort();
                for n in sorted_n {
                    if let Some(deg) = in_degree.get_mut(n) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push(n.clone());
                            queue.sort();
                        }
                    }
                }
            }
        }

        // Reverse so dependencies come first
        result.reverse();
        result
    }
}

// ---------------------------------------------------------------------------
// Solver - main solving engine
// ---------------------------------------------------------------------------

/// The dependency resolver. Takes a list of package requests and resolves
/// them into a non-conflicting set of packages including all dependencies.
pub struct Solver<'a> {
    // Config
    provider: &'a dyn PackageProvider,
    request_list: RequirementList,
    building: bool,
    optimised: bool,
    variant_select_mode: VariantSelectMode,
    package_filter: Option<PackageFilterList>,
    timestamp: Option<u64>, // C1 fix: timestamp for filtering packages
    package_orderers: Option<&'a PackageOrderList>, // C5 fix: stored for set_package_filter
    #[allow(clippy::type_complexity)]
    callback: Option<Box<dyn Fn(&SolverState) -> (SolverCallbackReturn, String) + 'a>>,

    // State
    phase_stack: Vec<ResolvePhase>,
    failed_phase_list: Vec<ResolvePhase>,
    pub abort_reason: Option<String>,
    callback_return: Option<SolverCallbackReturn>,
    solve_begun: bool,

    // Metrics
    pub solve_count: u32,
    pub solve_time: f64,
    pub load_time: f64,
    loaded_packages: HashSet<(String, String)>,
}

impl<'a> Solver<'a> {
    /// Create a new solver.
    ///
    /// # Arguments
    /// * `requests` - Package requirements to resolve
    /// * `provider` - Package provider for loading packages
    /// * `building` - True if resolving for a build (includes build_requires)
    pub fn new(
        requests: Vec<Requirement>,
        provider: &'a dyn PackageProvider,
        building: bool,
        orderers: Option<&'a PackageOrderList>, // C5 fix
    ) -> Result<Self> {
        let request_list = RequirementList::new(requests);
        let mut solver = Self {
            provider,
            request_list,
            building,
            optimised: true,
            variant_select_mode: crate::config::CONFIG.variant_select_mode,
            package_filter: None,
            timestamp: None, // C1 fix
            package_orderers: orderers,
            callback: None,
            phase_stack: Vec::new(),
            failed_phase_list: Vec::new(),
            abort_reason: None,
            callback_return: None,
            solve_begun: false,
            solve_count: 0,
            solve_time: 0.0,
            load_time: 0.0,
            loaded_packages: HashSet::new(),
        };

        // Check for conflicts in request list itself
        if let Some((req1, req2)) = solver.request_list.conflict() {
            let conflict = DependencyConflict {
                dependency: req1.clone(),
                conflicting_request: req2.clone(),
            };
            let phase = ResolvePhase {
                scopes: Vec::new(),
                failure_reason: Some(FailureReason::DependencyConflicts(vec![conflict])),
                status: SolverStatus::Failed,
                extractions: HashMap::new(),
                changed_scopes_i: HashSet::new(),
            };
            solver.phase_stack.push(phase);
            return Ok(solver);
        }

        // Create initial phase
        let filter_ref = solver.package_filter.as_ref();
        let mut cache = VariantCache::new(
            provider,
            building,
            filter_ref,
            solver.timestamp,
            orderers,
            solver.request_list.clone(),
            solver.variant_select_mode,
        );
        let phase = ResolvePhase::new(&solver.request_list, &mut cache)?;
        solver.loaded_packages = cache.loaded_packages;
        solver.phase_stack.push(phase);

        Ok(solver)
    }

    /// Set the callback function called after each solve step.
    pub fn set_callback(
        &mut self,
        cb: impl Fn(&SolverState) -> (SolverCallbackReturn, String) + 'a,
    ) {
        self.callback = Some(Box::new(cb));
    }

    /// Set whether to use optimised solving mode.
    pub fn set_optimised(&mut self, optimised: bool) {
        self.optimised = optimised;
    }

    /// Set variant selection mode.
    pub fn set_variant_select_mode(&mut self, mode: VariantSelectMode) -> Result<()> {
        let previous = std::mem::replace(&mut self.variant_select_mode, mode);
        if let Err(error) = self.rebuild_initial_phase() {
            self.variant_select_mode = previous;
            return Err(error);
        }
        Ok(())
    }

    /// Set timestamp for filtering packages (C1 fix).
    pub fn set_timestamp(&mut self, ts: u64) -> Result<()> {
        // Rez treats timestamp zero as unset, so it must not exclude packages
        // with positive release timestamps.
        let timestamp = (ts != 0).then_some(ts);
        let previous = std::mem::replace(&mut self.timestamp, timestamp);
        if let Err(error) = self.rebuild_initial_phase() {
            self.timestamp = previous;
            return Err(error);
        }
        Ok(())
    }

    /// Set package filter.
    pub fn set_package_filter(&mut self, filter: PackageFilterList) -> Result<()> {
        let previous = self.package_filter.replace(filter);
        if let Err(error) = self.rebuild_initial_phase() {
            self.package_filter = previous;
            return Err(error);
        }
        Ok(())
    }

    fn rebuild_initial_phase(&mut self) -> Result<()> {
        if self.solve_begun {
            return Ok(());
        }

        let (phase, loaded_packages) = if let Some((req1, req2)) = self.request_list.conflict() {
            (
                ResolvePhase {
                    scopes: Vec::new(),
                    failure_reason: Some(FailureReason::DependencyConflicts(vec![
                        DependencyConflict {
                            dependency: req1.clone(),
                            conflicting_request: req2.clone(),
                        },
                    ])),
                    status: SolverStatus::Failed,
                    extractions: HashMap::new(),
                    changed_scopes_i: HashSet::new(),
                },
                HashSet::new(),
            )
        } else {
            let mut cache = VariantCache::new(
                self.provider,
                self.building,
                self.package_filter.as_ref(),
                self.timestamp,
                self.package_orderers,
                self.request_list.clone(),
                self.variant_select_mode,
            );
            let phase = ResolvePhase::new(&self.request_list, &mut cache)?;
            (phase, cache.loaded_packages)
        };

        self.loaded_packages = loaded_packages;
        self.phase_stack.clear();
        self.phase_stack.push(phase);
        self.failed_phase_list.clear();
        Ok(())
    }

    /// Get current solver status.
    pub fn status(&self) -> SolverStatus {
        if self.request_list.conflict().is_some() {
            return SolverStatus::Failed;
        }
        if self.callback_return == Some(SolverCallbackReturn::Fail) {
            return SolverStatus::Failed;
        }

        let top_status = self
            .phase_stack
            .last()
            .map(|p| p.status)
            .unwrap_or(SolverStatus::Pending);

        match top_status {
            SolverStatus::Cyclic => SolverStatus::Failed,
            SolverStatus::Solved => SolverStatus::Solved,
            SolverStatus::Pending | SolverStatus::Exhausted => SolverStatus::Unsolved,
            // A failed top phase is backtrackable while another phase remains.
            SolverStatus::Failed if self.phase_stack.len() > 1 => SolverStatus::Unsolved,
            other => other,
        }
    }

    /// Number of solve steps executed.
    pub fn num_solves(&self) -> u32 {
        self.solve_count
    }

    /// Number of unique package family/version entries eligible for variant expansion.
    pub fn num_loaded_packages(&self) -> usize {
        self.loaded_packages.len()
    }

    /// Number of failed solve steps.
    pub fn num_fails(&self) -> u32 {
        let mut n = self.failed_phase_list.len() as u32;
        if let Some(top) = self.phase_stack.last() {
            if top.status == SolverStatus::Failed || top.status == SolverStatus::Cyclic {
                n += 1;
            }
        }
        n
    }

    /// Run the full solve. Iterates solve steps until solved, failed, or aborted.
    pub fn solve(&mut self) -> Result<()> {
        if self.solve_begun {
            return Err(RezError::Resolve(
                "cannot run solve() on a solve already started".into(),
            ));
        }

        let t1 = Instant::now();

        // Extract immutable config into locals so VariantCache doesn't borrow self,
        // allowing &mut self in solve_step_inner. Packages don't change during a
        // solve, so the cache stays valid across all backtracking steps.
        let provider = self.provider;
        let building = self.building;
        let timestamp = self.timestamp;
        let orderers = self.package_orderers;
        let request_list = self.request_list.clone();
        let variant_select_mode = self.variant_select_mode;
        let pkg_filter = self.package_filter.take();
        let filter_ref = pkg_filter.as_ref();
        let mut cache = VariantCache::new(
            provider,
            building,
            filter_ref,
            timestamp,
            orderers,
            request_list,
            variant_select_mode,
        );
        cache.loaded_packages.clone_from(&self.loaded_packages);

        let result = (|| {
            while self.status() == SolverStatus::Unsolved {
                self.solve_step_inner(&mut cache)?;
                if self.status() == SolverStatus::Unsolved && !self.do_callback() {
                    break;
                }
            }
            Ok(())
        })();

        self.loaded_packages = cache.loaded_packages.clone();
        // Restore package_filter
        self.package_filter = pkg_filter;
        self.solve_time = t1.elapsed().as_secs_f64();
        log_info!(
            "solver",
            "solve finished: status={:?} steps={} time={:.3}s",
            self.status(),
            self.solve_count,
            self.solve_time
        );
        result
    }

    /// Perform a single solve step (public entry, creates its own cache).
    pub fn solve_step(&mut self) -> Result<()> {
        let provider = self.provider;
        let building = self.building;
        let timestamp = self.timestamp;
        let orderers = self.package_orderers;
        let request_list = self.request_list.clone();
        let variant_select_mode = self.variant_select_mode;
        let pkg_filter = self.package_filter.take();
        let filter_ref = pkg_filter.as_ref();
        let mut cache = VariantCache::new(
            provider,
            building,
            filter_ref,
            timestamp,
            orderers,
            request_list,
            variant_select_mode,
        );
        cache.loaded_packages.clone_from(&self.loaded_packages);
        let result = self.solve_step_inner(&mut cache);
        self.loaded_packages = cache.loaded_packages.clone();
        self.package_filter = pkg_filter;
        result
    }

    /// Inner solve step using a shared cache.
    fn solve_step_inner(&mut self, cache: &mut VariantCache) -> Result<()> {
        self.solve_begun = true;
        if self.status() != SolverStatus::Unsolved {
            return Ok(());
        }

        // Pop phase
        let mut phase = match self.phase_stack.pop() {
            Some(p) => p,
            None => return Ok(()),
        };

        // Discard failed phase, get previous
        if phase.status == SolverStatus::Failed {
            self.failed_phase_list.push(phase);
            phase = match self.phase_stack.pop() {
                Some(p) => p,
                None => return Ok(()),
            };
        }

        // Split exhausted phase
        let mut retry_phase = None;
        let mut alternate_phase = None;
        if phase.status == SolverStatus::Exhausted {
            log_debug!(
                "solver",
                "backtrack: splitting exhausted phase (stack={})",
                self.phase_stack.len()
            );
            if let Some((phase_a, phase_b)) = phase.split_phase() {
                retry_phase = Some(phase.clone());
                alternate_phase = Some(phase_b);
                phase = phase_a;
            }
        }

        // Solve the phase
        log_trace!(
            "solver",
            "solve step #{} scopes={}",
            self.solve_count + 1,
            phase.scopes.len()
        );
        let new_phase = match phase.solve(cache) {
            Ok(new_phase) => new_phase,
            Err(error) => {
                self.phase_stack.push(retry_phase.unwrap_or(phase));
                return Err(error);
            }
        };
        if let Some(alternate) = alternate_phase {
            self.phase_stack.push(alternate);
        }
        self.solve_count += 1;

        match new_phase.status {
            SolverStatus::Failed => {
                self.phase_stack.push(new_phase);
            }
            SolverStatus::Solved => {
                let mut final_phase = new_phase.clone();
                let finalised = final_phase.finalise();
                self.phase_stack.push(finalised);
            }
            SolverStatus::Exhausted => {
                self.phase_stack.push(new_phase);
            }
            _ => {
                self.phase_stack.push(new_phase);
            }
        }
        Ok(())
    }

    /// Get resolved packages if solve succeeded.
    pub fn resolved_packages(&mut self) -> Option<Vec<PackageVariant>> {
        if self.status() != SolverStatus::Solved {
            return None;
        }
        self.phase_stack.last_mut().map(|p| p.solved_variants())
    }

    /// Get resolved ephemeral requirements (C3 fix).
    /// Ephemerals are conflict requirements (starting with "!").
    pub fn resolved_ephemerals(&mut self) -> Vec<Requirement> {
        if self.status() != SolverStatus::Solved {
            return Vec::new();
        }

        let mut ephemerals = Vec::new();
        if let Some(phase) = self.phase_stack.last_mut() {
            for variant in phase.solved_variants() {
                // Get requirements from variant
                if let Some(req_list) = variant.cached_requirements() {
                    for req in req_list.requirements() {
                        if req.conflict() {
                            ephemerals.push(req.clone());
                        }
                    }
                }
            }
        }

        ephemerals
    }

    /// Get the failure reason if solve failed.
    pub fn failure_reason(&self) -> Option<&FailureReason> {
        // Check most recent failure
        if let Some(top) = self.phase_stack.last() {
            if top.status == SolverStatus::Failed || top.status == SolverStatus::Cyclic {
                return top.failure_reason.as_ref();
            }
        }
        // Check first failed phase
        self.failed_phase_list
            .first()
            .and_then(|p| p.failure_reason.as_ref())
    }

    /// Get human-readable failure description.
    pub fn failure_description(&self) -> Option<String> {
        self.failure_reason().map(|fr| {
            let desc = fr.description();
            if let Some(reason) = &self.abort_reason {
                format!("{}:\n{}", reason, desc)
            } else {
                desc
            }
        })
    }

    /// Return a DOT graph for the most relevant solve phase.
    ///
    /// Solved and in-progress solves use the latest non-failed phase. Failed
    /// solves use the first failure, except cyclic solves and callback failures,
    /// which use the most recent failure, matching Rez's solver graph selection.
    pub fn graph_as_dot(&self) -> String {
        let status = self.status();
        let phase = self.graph_phase();
        match phase {
            Some(phase) => phase.graph_as_dot(&self.request_list, status),
            None => DotGraph::default().finish(status),
        }
    }

    fn graph_phase(&self) -> Option<&ResolvePhase> {
        let current = self.phase_stack.last();
        let current_phase_status = current.map(|phase| phase.status);

        if matches!(
            self.status(),
            SolverStatus::Solved | SolverStatus::Unsolved | SolverStatus::Pending
        ) {
            return self
                .phase_stack
                .iter()
                .rev()
                .find(|phase| !matches!(phase.status, SolverStatus::Failed | SolverStatus::Cyclic))
                .or(current);
        }

        let current_failure = current
            .filter(|phase| matches!(phase.status, SolverStatus::Failed | SolverStatus::Cyclic));
        if current_phase_status == Some(SolverStatus::Cyclic) {
            return current_failure.or_else(|| self.failed_phase_list.last());
        }
        if self.callback_return == Some(SolverCallbackReturn::Fail) {
            return current_failure.or_else(|| self.failed_phase_list.last());
        }

        self.failed_phase_list.first().or(current_failure)
    }

    /// Execute callback if set.
    fn do_callback(&mut self) -> bool {
        if let Some(ref cb) = self.callback {
            let state = SolverState {
                num_solves: self.num_solves(),
                num_fails: self.num_fails(),
                phase_str: self
                    .phase_stack
                    .last()
                    .map(|p| p.to_string())
                    .unwrap_or_default(),
            };
            let (ret, reason) = cb(&state);
            match ret {
                SolverCallbackReturn::Abort => {
                    self.abort_reason = Some(reason);
                    return false;
                }
                SolverCallbackReturn::Fail => {
                    if self.num_fails() > 0 {
                        self.abort_reason = Some(reason);
                        self.callback_return = Some(SolverCallbackReturn::Fail);
                        return false;
                    }
                }
                SolverCallbackReturn::KeepGoing => {}
            }
        }
        true
    }
}

impl<'a> fmt::Display for Solver<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {}",
            self.status(),
            self.phase_stack
                .last()
                .map(|p| p.to_string())
                .unwrap_or_default()
        )
    }
}

// ---------------------------------------------------------------------------
// Helper: exact version range from a version
// ---------------------------------------------------------------------------

/// Create an exact (==) version range from a single version, returning None on error.
fn exact_range(version: &Version) -> Option<VersionRange> {
    VersionRange::from_version(version, Some("==")).ok()
}

/// Create a range spanning all given versions (union of exact versions).
fn union_range(versions: &[&Version]) -> VersionRange {
    if versions.is_empty() {
        return VersionRange::any();
    }
    let owned: Vec<Version> = versions.iter().map(|v| (*v).clone()).collect();
    VersionRange::from_versions(&owned).unwrap_or_else(|_| VersionRange::any())
}

// ---------------------------------------------------------------------------
// FilesystemPackageProvider - bridges repository system with solver
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::Package;
    use repository::provider::{FilesystemPackageProvider, MemoryPackageProvider};

    // Helper: create a simple package with name, version, and requires
    fn make_pkg(name: &str, ver: &str, requires: &[&str]) -> Package {
        let mut pkg = Package::new(name, Version::new(ver).unwrap());
        pkg.requires = requires
            .iter()
            .map(|r| Requirement::new(r).unwrap())
            .collect();
        pkg
    }

    fn make_provider(packages: Vec<Package>) -> MemoryPackageProvider {
        let mut provider = MemoryPackageProvider::new();
        for pkg in packages {
            provider.add(pkg);
        }
        provider
    }

    struct FailingPackageProvider {
        packages: MemoryPackageProvider,
        failing_name: &'static str,
    }

    impl PackageProvider for FailingPackageProvider {
        fn get_packages(&self, name: &str, range: &VersionRange) -> Result<Vec<Package>> {
            if name == self.failing_name {
                return Err(RezError::PackageRepository(
                    "fixture provider failure".into(),
                ));
            }
            self.packages.get_packages(name, range)
        }
    }

    #[test]
    fn test_provider_errors_propagate_from_initial_and_transitive_package_loads() {
        let provider = FailingPackageProvider {
            packages: make_provider(vec![make_pkg("app", "1.0", &["broken_dep"])]),
            failing_name: "broken_dep",
        };
        let app_request = Requirement::new("app").unwrap();
        let mut solver = Solver::new(vec![app_request], &provider, false, None).unwrap();
        let error = match solver.solve() {
            Err(error) => error,
            Ok(()) => panic!("transitive provider error must propagate"),
        };
        assert!(matches!(
            error,
            RezError::PackageRepository(message) if message == "fixture provider failure"
        ));
        assert_eq!(
            solver.num_loaded_packages(),
            1,
            "retain entries expanded before a provider error"
        );

        let missing_from_provider = Requirement::new("broken_dep").unwrap();
        let error = match Solver::new(vec![missing_from_provider], &provider, false, None) {
            Err(error) => error,
            Ok(_) => panic!("provider error must not become an empty package list"),
        };
        assert!(matches!(
            error,
            RezError::PackageRepository(message) if message == "fixture provider failure"
        ));
    }

    #[test]
    fn test_dep_graph_no_cycle() {
        let mut g = DepGraph::new();
        g.add_edge("a", "b");
        g.add_edge("b", "c");
        assert!(g.find_cycle().is_none());
    }

    #[test]
    fn test_dep_graph_with_cycle() {
        let mut g = DepGraph::new();
        g.add_edge("a", "b");
        g.add_edge("b", "c");
        g.add_edge("c", "a");
        assert!(g.find_cycle().is_some());
    }

    #[test]
    fn test_dep_graph_topo_order() {
        let mut g = DepGraph::new();
        g.add_edge("app", "lib");
        g.add_edge("lib", "base");
        let order = g.topo_order();
        // base should come before lib, lib before app
        let pos_base = order.iter().position(|n| n == "base").unwrap();
        let pos_lib = order.iter().position(|n| n == "lib").unwrap();
        let pos_app = order.iter().position(|n| n == "app").unwrap();
        assert!(pos_base < pos_lib);
        assert!(pos_lib < pos_app);
    }

    #[test]
    fn test_failed_phase_with_fallback_remains_unsolved() {
        let provider = make_provider(vec![make_pkg("foo", "1.0.0", &[])]);
        let request = Requirement::new("foo").unwrap();
        let mut solver = Solver::new(vec![request], &provider, false, None).unwrap();

        let mut fallback = solver.phase_stack[0].clone();
        fallback.status = SolverStatus::Pending;
        let mut failed = fallback.clone();
        failed.status = SolverStatus::Failed;

        solver.phase_stack = vec![fallback.clone(), failed.clone()];
        assert_eq!(solver.status(), SolverStatus::Unsolved);

        solver.phase_stack = vec![failed.clone()];
        assert_eq!(solver.status(), SolverStatus::Failed);

        failed.status = SolverStatus::Cyclic;
        solver.phase_stack = vec![fallback, failed];
        assert_eq!(solver.status(), SolverStatus::Failed);
    }

    #[test]
    fn test_graph_as_dot_for_solved_dependencies() {
        let provider = make_provider(vec![
            make_pkg("app", "1.0.0", &["lib"]),
            make_pkg("lib", "2.0.0", &[]),
        ]);
        let mut solver = Solver::new(
            vec![Requirement::new("app").unwrap()],
            &provider,
            false,
            None,
        )
        .unwrap();
        solver.solve().unwrap();

        let dot = solver.graph_as_dot();
        assert!(dot.contains("digraph rez_resolve"));
        assert!(dot.contains("Resolve: solved"));
        assert!(dot.contains("app-1.0.0"));
        assert!(dot.contains("lib-2.0.0"));
        assert!(dot.contains(" -> "));
    }

    #[test]
    fn test_graph_as_dot_for_failed_requests_contains_conflict_edge() {
        let provider = make_provider(vec![
            make_pkg("foo", "1.0.0", &[]),
            make_pkg("foo", "2.0.0", &[]),
        ]);
        let requests = vec![
            Requirement::new("foo-==1.0.0").unwrap(),
            Requirement::new("foo-==2.0.0").unwrap(),
        ];
        let mut solver = Solver::new(requests, &provider, false, None).unwrap();
        solver.solve().unwrap();

        let dot = solver.graph_as_dot();
        assert_eq!(solver.status(), SolverStatus::Failed);
        assert!(dot.contains("CONFLICT"));
        assert!(dot.contains("foo==1.0.0"));
        assert!(dot.contains("foo==2.0.0"));
    }

    #[test]
    fn test_graph_as_dot_for_cyclic_resolve_marks_cycle_edges() {
        let provider = make_provider(vec![
            make_pkg("alpha", "1.0.0", &["beta"]),
            make_pkg("beta", "1.0.0", &["alpha"]),
        ]);
        let mut solver = Solver::new(
            vec![Requirement::new("alpha").unwrap()],
            &provider,
            false,
            None,
        )
        .unwrap();
        solver.solve().unwrap();

        let dot = solver.graph_as_dot();
        assert!(matches!(
            solver.phase_stack.last().map(|phase| phase.status),
            Some(SolverStatus::Cyclic)
        ));
        assert!(dot.contains("CYCLE"));
        assert!(dot.contains("alpha-1.0.0"));
        assert!(dot.contains("beta-1.0.0"));
    }

    #[test]
    fn test_graph_as_dot_backtracking_uses_latest_nonfailed_phase() {
        let provider = make_provider(vec![make_pkg("foo", "1.0.0", &[])]);
        let mut solver = Solver::new(
            vec![Requirement::new("foo").unwrap()],
            &provider,
            false,
            None,
        )
        .unwrap();

        let fallback = solver.phase_stack[0].clone();
        let mut failed = fallback.clone();
        failed.status = SolverStatus::Failed;
        failed.failure_reason = Some(FailureReason::DependencyConflicts(vec![
            DependencyConflict {
                dependency: Requirement::new("ignored_dependency").unwrap(),
                conflicting_request: Requirement::new("ignored_conflict").unwrap(),
            },
        ]));
        solver.phase_stack = vec![fallback, failed];

        let dot = solver.graph_as_dot();
        assert_eq!(solver.status(), SolverStatus::Unsolved);
        assert!(dot.contains("foo"));
        assert!(!dot.contains("CONFLICT"));
        assert!(!dot.contains("ignored_dependency"));
    }

    #[test]
    fn test_graph_as_dot_failure_selection_matches_rez() {
        let provider = make_provider(vec![make_pkg("foo", "1.0.0", &[])]);
        let mut solver = Solver::new(
            vec![Requirement::new("foo").unwrap()],
            &provider,
            false,
            None,
        )
        .unwrap();
        let base = solver.phase_stack[0].clone();

        let failure_phase = |name: &str| {
            let mut phase = base.clone();
            phase.status = SolverStatus::Failed;
            phase.failure_reason = Some(FailureReason::DependencyConflicts(vec![
                DependencyConflict {
                    dependency: Requirement::new(name).unwrap(),
                    conflicting_request: Requirement::new(&format!("{}_conflict", name)).unwrap(),
                },
            ]));
            phase
        };

        solver.failed_phase_list = vec![
            failure_phase("first_failure"),
            failure_phase("last_failure"),
        ];
        solver.phase_stack = vec![failure_phase("current_failure")];
        let first_failure_dot = solver.graph_as_dot();
        assert!(first_failure_dot.contains("first_failure"));
        assert!(!first_failure_dot.contains("last_failure"));
        assert!(!first_failure_dot.contains("current_failure"));

        solver.phase_stack = vec![base];
        solver.callback_return = Some(SolverCallbackReturn::Fail);
        let callback_failure_dot = solver.graph_as_dot();
        assert!(callback_failure_dot.contains("last_failure"));
        assert!(!callback_failure_dot.contains("first_failure"));
    }

    #[test]
    fn test_missing_package_family_is_distinct_from_empty_candidate_set() {
        let provider = make_provider(vec![make_pkg("other", "1.0.0", &[])]);

        let missing_family = Solver::new(
            vec![Requirement::new("missing").unwrap()],
            &provider,
            false,
            None,
        )
        .err()
        .expect("missing package family must fail");
        assert!(matches!(missing_family, RezError::PackageFamilyNotFound(_)));

        let foo_provider = make_provider(vec![make_pkg("foo", "1.0.0", &[])]);
        let missing_version = Solver::new(
            vec![Requirement::new("foo-2+").unwrap()],
            &foo_provider,
            false,
            None,
        )
        .err()
        .expect("a family with no matching version must fail");
        assert!(matches!(missing_version, RezError::PackageNotFound(_)));
    }

    #[test]
    fn test_simple_resolve() {
        let provider = make_provider(vec![
            make_pkg("foo", "1.0.0", &[]),
            make_pkg("foo", "2.0.0", &[]),
        ]);

        let reqs = vec![Requirement::new("foo").unwrap()];
        let mut solver = Solver::new(reqs, &provider, false, None).unwrap();
        solver.solve().unwrap();

        assert_eq!(solver.status(), SolverStatus::Solved);
        let resolved = solver.resolved_packages().unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].name(), "foo");
    }

    #[test]
    fn test_resolve_with_dependency() {
        let provider = make_provider(vec![
            make_pkg("app", "1.0.0", &["lib"]),
            make_pkg("lib", "1.0.0", &[]),
            make_pkg("lib", "2.0.0", &[]),
        ]);

        let reqs = vec![Requirement::new("app").unwrap()];
        let mut solver = Solver::new(reqs, &provider, false, None).unwrap();
        solver.solve().unwrap();

        assert_eq!(solver.status(), SolverStatus::Solved);
        let resolved = solver.resolved_packages().unwrap();
        assert_eq!(resolved.len(), 2);
        // Both app and lib should be resolved
        let names: HashSet<&str> = resolved.iter().map(|r| r.name()).collect();
        assert!(names.contains("app"));
        assert!(names.contains("lib"));
    }

    #[test]
    fn test_resolve_with_version_constraint() {
        let provider = make_provider(vec![
            make_pkg("app", "1.0.0", &["lib-1+"]),
            make_pkg("lib", "0.9.0", &[]),
            make_pkg("lib", "1.0.0", &[]),
            make_pkg("lib", "2.0.0", &[]),
        ]);

        let reqs = vec![Requirement::new("app").unwrap()];
        let mut solver = Solver::new(reqs, &provider, false, None).unwrap();
        solver.solve().unwrap();

        assert_eq!(solver.status(), SolverStatus::Solved);
        let resolved = solver.resolved_packages().unwrap();
        let lib = resolved.iter().find(|r| r.name() == "lib").unwrap();
        // lib should be >= 1.0.0
        assert!(lib.version() >= &Version::new("1.0.0").unwrap());
    }

    #[test]
    fn test_resolve_conflicting_requests() {
        let provider = make_provider(vec![
            make_pkg("foo", "1.0.0", &[]),
            make_pkg("foo", "2.0.0", &[]),
        ]);

        let reqs = vec![
            Requirement::new("foo-==1.0.0").unwrap(),
            Requirement::new("foo-==2.0.0").unwrap(),
        ];
        let mut solver = Solver::new(reqs, &provider, false, None).unwrap();
        solver.solve().unwrap();

        // These exact ranges don't intersect so it should fail
        assert_eq!(solver.status(), SolverStatus::Failed);
    }

    #[test]
    fn test_resolve_package_not_found() {
        let provider = make_provider(vec![]);

        let reqs = vec![Requirement::new("nonexistent").unwrap()];
        let result = Solver::new(reqs, &provider, false, None);

        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_dependency_conflict() {
        let provider = make_provider(vec![
            make_pkg("app", "1.0.0", &["lib<2"]),
            make_pkg("plugin", "1.0.0", &["lib-2+"]),
            make_pkg("lib", "1.0.0", &[]),
            make_pkg("lib", "2.0.0", &[]),
            make_pkg("lib", "3.0.0", &[]),
        ]);

        let reqs = vec![
            Requirement::new("app").unwrap(),
            Requirement::new("plugin").unwrap(),
        ];
        let mut solver = Solver::new(reqs, &provider, false, None).unwrap();
        solver.solve().unwrap();

        // app wants lib<2, plugin wants lib>=2 - conflict
        assert_eq!(solver.status(), SolverStatus::Failed);
    }

    #[test]
    fn test_resolve_diamond_dependency() {
        // Diamond: app -> (libA, libB), libA -> core, libB -> core
        let provider = make_provider(vec![
            make_pkg("app", "1.0.0", &["libA", "libB"]),
            make_pkg("libA", "1.0.0", &["core"]),
            make_pkg("libB", "1.0.0", &["core"]),
            make_pkg("core", "1.0.0", &[]),
        ]);

        let reqs = vec![Requirement::new("app").unwrap()];
        let mut solver = Solver::new(reqs, &provider, false, None).unwrap();
        solver.solve().unwrap();

        assert_eq!(solver.status(), SolverStatus::Solved);
        let resolved = solver.resolved_packages().unwrap();
        assert_eq!(resolved.len(), 4);
        let names: HashSet<&str> = resolved.iter().map(|r| r.name()).collect();
        assert!(names.contains("app"));
        assert!(names.contains("libA"));
        assert!(names.contains("libB"));
        assert!(names.contains("core"));
    }

    #[test]
    fn test_resolve_multiple_versions() {
        let provider = make_provider(vec![
            make_pkg("app", "1.0.0", &["lib-1+<3"]),
            make_pkg("lib", "1.0.0", &[]),
            make_pkg("lib", "2.0.0", &[]),
            make_pkg("lib", "3.0.0", &[]),
        ]);

        let reqs = vec![Requirement::new("app").unwrap()];
        let mut solver = Solver::new(reqs, &provider, false, None).unwrap();
        solver.solve().unwrap();

        assert_eq!(solver.status(), SolverStatus::Solved);
        let resolved = solver.resolved_packages().unwrap();
        let lib = resolved.iter().find(|r| r.name() == "lib").unwrap();
        // Should pick one of 1.0, 2.0 (not 3.0 which is excluded by <3)
        let ver = lib.version();
        assert!(ver >= &Version::new("1.0.0").unwrap());
        assert!(ver < &Version::new("3.0.0").unwrap());
    }

    #[test]
    fn test_solver_state_display() {
        let state = SolverState {
            num_solves: 5,
            num_fails: 2,
            phase_str: "foo bar".to_string(),
        };
        assert_eq!(state.to_string(), "solve #5 (2 fails so far): foo bar");
    }

    #[test]
    fn test_failure_reason_description() {
        let fr = FailureReason::TotalReduction(vec![Reduction {
            name: "foo".to_string(),
            version: Version::new("1.0").unwrap(),
            variant_index: None,
            dependency: Requirement::new("bar-2+").unwrap(),
            conflicting_request: Requirement::new("bar<2").unwrap(),
        }]);
        let desc = fr.description();
        assert!(desc.contains("completely reduced"));
    }

    #[test]
    fn test_memory_provider() {
        let mut provider = MemoryPackageProvider::new();
        provider.add(make_pkg("foo", "1.0.0", &[]));
        provider.add(make_pkg("foo", "2.0.0", &[]));
        provider.add(make_pkg("bar", "1.0.0", &[]));

        let all_foo = provider.get_all_packages("foo").unwrap();
        assert_eq!(all_foo.len(), 2);

        let range = VersionRange::new("1+<2").unwrap();
        let filtered = provider.get_packages("foo", &range).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].version.to_string(), "1.0.0");
    }

    #[test]
    fn test_callback_mechanism() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let counter = AtomicU32::new(0);
        let provider = make_provider(vec![
            make_pkg("foo", "1.0.0", &[]),
            make_pkg("foo", "2.0.0", &[]),
        ]);

        let reqs = vec![Requirement::new("foo").unwrap()];
        let mut solver = Solver::new(reqs, &provider, false, None).unwrap();
        solver.set_callback(|state| {
            assert!(state.num_solves > 0);
            counter.fetch_add(1, Ordering::SeqCst);
            (SolverCallbackReturn::KeepGoing, String::new())
        });
        solver.solve().unwrap();

        assert_eq!(solver.status(), SolverStatus::Solved);
        assert!(counter.load(Ordering::SeqCst) > 0);
    }

    #[test]
    fn test_conflict_requirement() {
        // Request !foo should not actually require foo to exist
        let provider = make_provider(vec![make_pkg("bar", "1.0.0", &[])]);

        let reqs = vec![
            Requirement::new("bar").unwrap(),
            Requirement::new("!foo").unwrap(),
        ];
        let mut solver = Solver::new(reqs, &provider, false, None).unwrap();
        solver.solve().unwrap();

        assert_eq!(solver.status(), SolverStatus::Solved);
    }

    #[test]
    fn test_reduction_display() {
        let red = Reduction {
            name: "foo".to_string(),
            version: Version::new("1.2").unwrap(),
            variant_index: Some(0),
            dependency: Requirement::new("bar-2+").unwrap(),
            conflicting_request: Requirement::new("bar<2").unwrap(),
        };
        let s = red.to_string();
        assert!(s.contains("foo"));
        assert!(s.contains("1.2"));
    }

    #[test]
    fn test_transitive_deps() {
        // a -> b -> c -> d
        let provider = make_provider(vec![
            make_pkg("a", "1.0.0", &["b"]),
            make_pkg("b", "1.0.0", &["c"]),
            make_pkg("c", "1.0.0", &["d"]),
            make_pkg("d", "1.0.0", &[]),
        ]);

        let reqs = vec![Requirement::new("a").unwrap()];
        let mut solver = Solver::new(reqs, &provider, false, None).unwrap();
        solver.solve().unwrap();

        assert_eq!(solver.status(), SolverStatus::Solved);
        let resolved = solver.resolved_packages().unwrap();
        assert_eq!(resolved.len(), 4);
    }

    // -----------------------------------------------------------------------
    // FilesystemPackageProvider tests
    // -----------------------------------------------------------------------

    /// Helper: create a temp repo dir with package.yaml files
    fn create_test_repo(base: &std::path::Path, packages: &[(&str, &str, &str)]) {
        for (name, version, yaml) in packages {
            let dir = base.join(name).join(version);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("package.yaml"), yaml).unwrap();
        }
    }

    #[test]
    fn test_fs_provider_basic() {
        let tmp = tempfile::tempdir().unwrap();
        create_test_repo(
            tmp.path(),
            &[
                ("foo", "1.0.0", "name: foo\nversion: \"1.0.0\""),
                ("foo", "2.0.0", "name: foo\nversion: \"2.0.0\""),
                ("bar", "1.0.0", "name: bar\nversion: \"1.0.0\""),
            ],
        );

        let provider = FilesystemPackageProvider::from_path(tmp.path()).unwrap();
        let all_foo = provider.get_all_packages("foo").unwrap();
        assert_eq!(all_foo.len(), 2);

        let range = VersionRange::new("1+<2").unwrap();
        let filtered = provider.get_packages("foo", &range).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].version.to_string(), "1.0.0");
    }

    #[test]
    fn test_fs_provider_with_requires() {
        let tmp = tempfile::tempdir().unwrap();
        create_test_repo(
            tmp.path(),
            &[
                (
                    "app",
                    "1.0.0",
                    "name: app\nversion: \"1.0.0\"\nrequires:\n  - lib-1+",
                ),
                ("lib", "1.0.0", "name: lib\nversion: \"1.0.0\""),
            ],
        );

        let provider = FilesystemPackageProvider::from_path(tmp.path()).unwrap();
        let pkgs = provider.get_all_packages("app").unwrap();
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].requires.len(), 1);
        assert_eq!(pkgs[0].requires[0].name(), "lib");
    }

    #[test]
    fn test_fs_provider_invalid_configured_path_is_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let invalid = temp.path().join("not-a-repository");
        std::fs::write(&invalid, "file").unwrap();
        let result = FilesystemPackageProvider::from_path(&invalid);
        let error = match result {
            Ok(_) => panic!("An existing file must not be silently skipped as a repository"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("Failed to open configured repository"),
            "{error}"
        );
        assert!(error.contains("not-a-repository"), "{error}");
    }

    #[test]
    fn test_fs_provider_multi_repo_dedup() {
        // Two repos with overlapping packages - first repo wins
        let tmp1 = tempfile::tempdir().unwrap();
        let tmp2 = tempfile::tempdir().unwrap();
        create_test_repo(
            tmp1.path(),
            &[(
                "foo",
                "1.0.0",
                "name: foo\nversion: \"1.0.0\"\ndescription: from_repo1",
            )],
        );
        create_test_repo(
            tmp2.path(),
            &[
                (
                    "foo",
                    "1.0.0",
                    "name: foo\nversion: \"1.0.0\"\ndescription: from_repo2",
                ),
                ("foo", "2.0.0", "name: foo\nversion: \"2.0.0\""),
            ],
        );

        let paths = vec![tmp1.path().to_path_buf(), tmp2.path().to_path_buf()];
        let provider = FilesystemPackageProvider::from_paths(&paths).unwrap();
        let pkgs = provider.get_all_packages("foo").unwrap();
        assert_eq!(pkgs.len(), 2);

        // Version 1.0.0 should come from repo1
        let v1 = pkgs
            .iter()
            .find(|p| p.version.to_string() == "1.0.0")
            .unwrap();
        assert_eq!(v1.description.as_deref(), Some("from_repo1"));
    }

    // -----------------------------------------------------------------------
    // VariantSelectMode tests
    // -----------------------------------------------------------------------

    #[test]
    fn variant_cache_counts_unique_range_filter_and_timestamp_eligible_packages() {
        let mut foo_1 = make_pkg("foo", "1.0", &[]);
        foo_1.timestamp = Some(100);
        foo_1.variants = vec![vec![], vec![]];
        let mut foo_2 = make_pkg("foo", "2.0", &[]);
        foo_2.timestamp = Some(200);
        let mut foo_3 = make_pkg("foo", "3.0", &[]);
        foo_3.timestamp = Some(300);
        let provider = make_provider(vec![foo_1, foo_2, foo_3]);
        let filter = PackageFilterList::from_pod(&serde_json::json!([{
            "excludes": ["range(foo-2)"],
            "includes": []
        }]))
        .unwrap();
        let mut cache = VariantCache::new(
            &provider,
            false,
            Some(&filter),
            Some(250),
            None,
            RequirementList::new(Vec::new()),
            VariantSelectMode::VersionPriority,
        );

        let range = VersionRange::new("1+<3").unwrap();
        let slice = cache.get_slice("foo", &range).unwrap().unwrap();
        assert_eq!(slice.len(), 2, "the eligible package has two variants");
        assert_eq!(
            cache.loaded_packages,
            HashSet::from([("foo".to_owned(), "1.0".to_owned())]),
            "count package entries after range, filter and timestamp checks"
        );

        let excluded_range = VersionRange::new("2+").unwrap();
        assert!(cache.get_slice("foo", &excluded_range).unwrap().is_none());
        assert_eq!(
            cache.loaded_packages.len(),
            1,
            "subsequent lookups must not count excluded or duplicate entries"
        );
    }

    #[test]
    fn test_variant_select_mode_default() {
        let mode = VariantSelectMode::default();
        assert_eq!(mode, VariantSelectMode::VersionPriority);
    }

    #[test]
    fn test_variant_select_mode_setter() {
        let provider = make_provider(vec![
            make_pkg("foo", "1.0.0", &[]),
            make_pkg("foo", "2.0.0", &[]),
        ]);

        let reqs = vec![Requirement::new("foo").unwrap()];
        let mut solver = Solver::new(reqs, &provider, false, None).unwrap();

        // Default is VersionPriority
        assert_eq!(
            solver.variant_select_mode,
            VariantSelectMode::VersionPriority
        );

        // Set to IntersectionPriority
        solver
            .set_variant_select_mode(VariantSelectMode::IntersectionPriority)
            .unwrap();
        assert_eq!(
            solver.variant_select_mode,
            VariantSelectMode::IntersectionPriority
        );
    }

    // -----------------------------------------------------------------------
    // PackageFilter integration tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_timestamp_zero_is_unset_and_nonzero_filters_candidates() {
        let mut timestamped = make_pkg("foo", "1.0.0", &[]);
        timestamped.timestamp = Some(100);
        let provider = make_provider(vec![timestamped]);

        let mut zero_timestamp_solver = Solver::new(
            vec![Requirement::new("foo").unwrap()],
            &provider,
            false,
            None,
        )
        .unwrap();
        zero_timestamp_solver.set_timestamp(0).unwrap();
        zero_timestamp_solver.solve().unwrap();
        assert_eq!(zero_timestamp_solver.status(), SolverStatus::Solved);

        let mut cutoff_solver = Solver::new(
            vec![Requirement::new("foo").unwrap()],
            &provider,
            false,
            None,
        )
        .unwrap();
        let error = cutoff_solver
            .set_timestamp(50)
            .expect_err("packages released after the cutoff must be excluded");
        assert!(matches!(error, RezError::PackageNotFound(_)));

        let untimestamped_provider = make_provider(vec![make_pkg("bar", "1.0.0", &[])]);
        let mut untimestamped_solver = Solver::new(
            vec![Requirement::new("bar").unwrap()],
            &untimestamped_provider,
            false,
            None,
        )
        .unwrap();
        untimestamped_solver.set_timestamp(50).unwrap();
        untimestamped_solver.solve().unwrap();
        assert_eq!(untimestamped_solver.status(), SolverStatus::Solved);
    }

    #[test]
    fn test_package_filter_excludes_package() {
        use crate::package::filter::{PackageFilter, PackageFilterList, Rule};

        let provider = make_provider(vec![
            make_pkg("foo", "1.0.0", &[]),
            make_pkg("foo", "2.0.0", &[]),
            make_pkg("foo", "3.0.0", &[]),
        ]);

        let reqs = vec![Requirement::new("foo").unwrap()];
        let mut solver = Solver::new(reqs, &provider, false, None).unwrap();

        // Create filter that excludes foo-<2.0
        let mut filter = PackageFilter::new();
        let rule = Rule::parse("foo-<2.0").unwrap();
        filter.add_exclusion(rule, Some("foo"));

        let mut filter_list = PackageFilterList::new();
        filter_list.add_filter(filter);
        solver.set_package_filter(filter_list).unwrap();

        solver.solve().unwrap();

        assert_eq!(solver.status(), SolverStatus::Solved);
        let resolved = solver.resolved_packages().unwrap();
        assert_eq!(resolved.len(), 1);

        // Should have picked version >= 2.0 (not 1.0.0 which is excluded)
        let foo = &resolved[0];
        assert_eq!(foo.name(), "foo");
        assert!(foo.version() >= &Version::new("2.0.0").unwrap());
    }

    #[test]
    fn test_timestamp_package_filter_is_applied_to_full_package() {
        use crate::package::filter::{PackageFilter, PackageFilterList, Rule};

        let mut older = make_pkg("foo", "1.0.0", &[]);
        older.timestamp = Some(10);
        let mut newer = make_pkg("foo", "2.0.0", &[]);
        newer.timestamp = Some(20);
        let provider = make_provider(vec![older, newer]);
        let mut solver = Solver::new(
            vec![Requirement::new("foo").unwrap()],
            &provider,
            false,
            None,
        )
        .unwrap();

        let mut filter = PackageFilter::new();
        filter.add_exclusion(Rule::parse("before(15)").unwrap(), None);
        let mut filters = PackageFilterList::new();
        filters.add_filter(filter);
        solver.set_package_filter(filters).unwrap();
        solver.solve().unwrap();

        let resolved = solver.resolved_packages().unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].version(), &Version::new("2.0.0").unwrap());
    }

    #[test]
    fn test_package_filter_excludes_all_versions_returns_package_not_found() {
        use crate::package::filter::{PackageFilter, PackageFilterList, Rule};

        let provider = make_provider(vec![
            make_pkg("foo", "1.0.0", &[]),
            make_pkg("foo", "2.0.0", &[]),
        ]);

        let reqs = vec![Requirement::new("foo").unwrap()];
        let mut solver = Solver::new(reqs, &provider, false, None).unwrap();

        // Create filter that excludes all foo versions
        let mut filter = PackageFilter::new();
        let rule = Rule::parse("foo").unwrap();
        filter.add_exclusion(rule, Some("foo"));

        let mut filter_list = PackageFilterList::new();
        filter_list.add_filter(filter);
        let error = solver
            .set_package_filter(filter_list)
            .expect_err("all packages in an existing family were excluded");

        assert!(matches!(error, RezError::PackageNotFound(_)));
    }

    #[test]
    fn test_package_filter_multiple_packages() {
        use crate::package::filter::{PackageFilter, PackageFilterList, Rule};

        let provider = make_provider(vec![
            make_pkg("app", "1.0.0", &["lib"]),
            make_pkg("lib", "1.0.0", &[]),
            make_pkg("lib", "2.0.0", &[]),
        ]);

        let reqs = vec![Requirement::new("app").unwrap()];
        let mut solver = Solver::new(reqs, &provider, false, None).unwrap();

        // Filter excludes lib-<2.0
        let mut filter = PackageFilter::new();
        let rule = Rule::parse("lib-<2.0").unwrap();
        filter.add_exclusion(rule, Some("lib"));

        let mut filter_list = PackageFilterList::new();
        filter_list.add_filter(filter);
        solver.set_package_filter(filter_list).unwrap();

        solver.solve().unwrap();

        assert_eq!(solver.status(), SolverStatus::Solved);
        let resolved = solver.resolved_packages().unwrap();
        assert_eq!(resolved.len(), 2);

        // lib should be version 2.0.0 (1.0.0 filtered out)
        let lib = resolved.iter().find(|p| p.name() == "lib").unwrap();
        assert_eq!(lib.version().to_string(), "2.0.0");
    }
}
