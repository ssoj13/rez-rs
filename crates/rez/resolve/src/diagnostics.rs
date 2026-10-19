// SPDX-License-Identifier: Apache-2.0

//! Conflict Diagnostics ("Why-Not" / Unsat Core Analyzer) and What-If Analysis.
//!
//! Analyzes failed or unsatisfiable package resolution requests against a
//! `PackageProvider`, identifying:
//! 1. Direct contradictions within the request list.
//! 2. Missing package families or version ranges with no matching repository versions
//!    (reporting the nearest available versions).
//! 3. Platform / architecture / OS variant mismatches.
//! 4. Pairwise and transitive dependency conflicts across candidate variants.
//! 5. Verified "What-If" upgrade/downgrade suggestions that produce a valid solve.

use std::collections::{BTreeSet, HashMap, HashSet};

use repository::provider::PackageProvider;
use serde::{Deserialize, Serialize};
use version::{Requirement, Version, VersionRange, VersionedObject};

use crate::constants::SolverStatus;
use crate::errors::Result;
use crate::package::Package;
use crate::solver::Solver;

/// Direct contradiction between two requirements in the user/implicit request list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectContradictionDiagnostic {
    pub package_name: String,
    pub first_requirement: String,
    pub second_requirement: String,
    pub explanation: String,
}

/// A requested package family that does not exist in any configured repository path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingPackageDiagnostic {
    pub requested: String,
    pub package_name: String,
    pub required_by: Option<String>,
}

/// A requested version range that has no matching versions in the repository,
/// even though the package family itself exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionRangeDiagnostic {
    pub requested: String,
    pub package_name: String,
    pub available_versions: Vec<String>,
    pub nearest_version: Option<String>,
    pub suggestion: String,
}

/// Conflict between two packages' dependencies or between a package's dependency
/// and a top-level/implicit request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageConflictDiagnostic {
    pub source_package: String,
    pub source_versions: Vec<String>,
    pub source_dependency: String,
    pub conflicting_target: String,
    pub conflicting_requirement: String,
    pub explanation: String,
}

/// Verified "What-If" relaxation (upgrade or downgrade of a requested package)
/// that makes the entire request solvable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhatIfSuggestion {
    /// Original request string that was relaxed (e.g. `"rig_tools-2.0"`).
    pub original_request: String,
    /// Suggested replacement requirement (e.g. `"rig_tools==2.1.0"` or `"rig_tools==1.8.0"`).
    pub suggested_requirement: String,
    /// Direction of change (`"upgrade"`, `"downgrade"`, or `"alternative"`).
    pub direction: String,
    /// Human-readable explanation of why this resolves the conflict.
    pub summary: String,
    /// Complete resolved package list (`name-version`) achieved by this What-If scenario.
    pub resulting_resolve: Vec<String>,
}

/// Complete structured conflict diagnostic and What-If report.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConflictDiagnosticReport {
    pub requested_packages: Vec<String>,
    pub implicit_packages: Vec<String>,
    pub solver_failure_description: Option<String>,
    pub direct_contradictions: Vec<DirectContradictionDiagnostic>,
    pub missing_packages: Vec<MissingPackageDiagnostic>,
    pub unsatisfied_ranges: Vec<VersionRangeDiagnostic>,
    pub dependency_conflicts: Vec<PackageConflictDiagnostic>,
    pub what_if_suggestions: Vec<WhatIfSuggestion>,
}

impl ConflictDiagnosticReport {
    /// Returns true if at least one structured finding or suggestion was produced.
    pub fn has_findings(&self) -> bool {
        !self.direct_contradictions.is_empty()
            || !self.missing_packages.is_empty()
            || !self.unsatisfied_ranges.is_empty()
            || !self.dependency_conflicts.is_empty()
            || !self.what_if_suggestions.is_empty()
    }

    /// Analyze a set of user and implicit requests against `provider`.
    pub fn analyze(
        user_requests: &[Requirement],
        implicit_requests: &[Requirement],
        provider: &dyn PackageProvider,
        building: bool,
        solver_failure_description: Option<String>,
    ) -> Result<Self> {
        let mut report = Self {
            requested_packages: user_requests.iter().map(ToString::to_string).collect(),
            implicit_packages: implicit_requests.iter().map(ToString::to_string).collect(),
            solver_failure_description,
            ..Default::default()
        };

        let mut all_requests: Vec<(Requirement, bool)> = Vec::new();
        for r in user_requests {
            all_requests.push((r.clone(), false));
        }
        for r in implicit_requests {
            all_requests.push((r.clone(), true));
        }

        // 1. Check direct contradictions among requested + implicit requirements
        for i in 0..all_requests.len() {
            for j in (i + 1)..all_requests.len() {
                let (req_a, imp_a) = &all_requests[i];
                let (req_b, imp_b) = &all_requests[j];
                if req_a.name() == req_b.name() && req_a.conflicts_with_req(req_b) {
                    let label_a = if *imp_a {
                        format!("{req_a} (implicit)")
                    } else {
                        req_a.to_string()
                    };
                    let label_b = if *imp_b {
                        format!("{req_b} (implicit)")
                    } else {
                        req_b.to_string()
                    };
                    report
                        .direct_contradictions
                        .push(DirectContradictionDiagnostic {
                            package_name: req_a.name().to_string(),
                            first_requirement: label_a.clone(),
                            second_requirement: label_b.clone(),
                            explanation: format!(
                                "Request '{label_a}' directly contradicts '{label_b}' (disjoint version ranges)"
                            ),
                        });
                }
            }
        }

        // 2. Check missing package families and unsatisfiable direct version ranges
        let mut candidate_packages_by_req: Vec<(Requirement, bool, Vec<Package>)> = Vec::new();
        for (req, is_implicit) in &all_requests {
            if req.conflict() || req.weak() || req.name().starts_with('.') {
                continue;
            }
            let all_versions = provider.get_packages(req.name(), &VersionRange::any())?;
            if all_versions.is_empty() {
                report.missing_packages.push(MissingPackageDiagnostic {
                    requested: req.to_string(),
                    package_name: req.name().to_string(),
                    required_by: is_implicit.then(|| "implicit_packages".to_string()),
                });
                continue;
            }

            let matching: Vec<Package> = all_versions
                .iter()
                .filter(|pkg| {
                    let obj = VersionedObject::construct(&pkg.name, Some(pkg.version.clone()));
                    !req.conflicts_with_obj(&obj)
                })
                .cloned()
                .collect();

            if matching.is_empty() {
                let mut sorted_versions: Vec<Version> =
                    all_versions.iter().map(|p| p.version.clone()).collect();
                sorted_versions.sort();
                sorted_versions.dedup();
                let nearest = sorted_versions.last().map(ToString::to_string);
                let avail_strs: Vec<String> =
                    sorted_versions.iter().map(ToString::to_string).collect();
                let suggestion = match &nearest {
                    Some(v) => format!(
                        "No version of '{}' satisfies '{}'; available versions: [{}]. Nearest available: {}-{}",
                        req.name(),
                        req,
                        avail_strs.join(", "),
                        req.name(),
                        v
                    ),
                    None => format!("No version of '{}' satisfies '{}'", req.name(), req),
                };
                report.unsatisfied_ranges.push(VersionRangeDiagnostic {
                    requested: req.to_string(),
                    package_name: req.name().to_string(),
                    available_versions: avail_strs,
                    nearest_version: nearest,
                    suggestion,
                });
            } else {
                candidate_packages_by_req.push((req.clone(), *is_implicit, matching));
            }
        }

        // 3. Analyze candidate variant dependencies for missing transitive families and pairwise conflicts
        // Collect effective requirement map per package family from top-level + common dependencies.
        let mut seen_conflicts: HashSet<(String, String, String)> = HashSet::new();
        let mut seen_missing_deps: HashSet<(String, String)> = HashSet::new();

        // Build per-request effective requirements summary:
        // For a request R, if ALL matching versions/variants require package D with combined range Rd,
        // then R effectively enforces D in Rd.
        let mut enforced_by_req: Vec<(String, HashMap<String, Requirement>)> = Vec::new();

        for (req, is_implicit, pkgs) in &candidate_packages_by_req {
            let req_label = if *is_implicit {
                format!("{req} (implicit)")
            } else {
                req.to_string()
            };
            let mut family_union_ranges: HashMap<String, Option<VersionRange>> = HashMap::new();
            let mut family_present_in_all = true;
            let mut common_families: Option<HashSet<String>> = None;

            for pkg in pkgs {
                let variant_req_lists = collect_package_variant_requires(pkg, building);
                for vreqs in &variant_req_lists {
                    // Check if any variant requirement references a completely missing package family
                    for dreq in vreqs {
                        if dreq.conflict() || dreq.weak() || dreq.name().starts_with('.') {
                            continue;
                        }
                        if provider
                            .get_packages(dreq.name(), &VersionRange::any())?
                            .is_empty()
                            && seen_missing_deps
                                .insert((req.name().to_string(), dreq.name().to_string()))
                        {
                            report.missing_packages.push(MissingPackageDiagnostic {
                                requested: dreq.to_string(),
                                package_name: dreq.name().to_string(),
                                required_by: Some(format!("{}-{}", pkg.name, pkg.version)),
                            });
                        }
                    }

                    let mut this_variant_families = HashSet::new();
                    for dreq in vreqs {
                        if dreq.conflict() || dreq.weak() || dreq.name().starts_with('.') {
                            continue;
                        }
                        this_variant_families.insert(dreq.name().to_string());
                        if let Some(r) = dreq.range() {
                            let entry = family_union_ranges
                                .entry(dreq.name().to_string())
                                .or_insert(None);
                            *entry = Some(match entry.take() {
                                Some(existing) => &existing | r,
                                None => r.clone(),
                            });
                        }
                    }
                    common_families = Some(match common_families.take() {
                        Some(prev) => prev.intersection(&this_variant_families).cloned().collect(),
                        None => this_variant_families,
                    });
                }
            }

            let mut enforced_map = HashMap::new();
            // Always enforces itself
            enforced_map.insert(req.name().to_string(), req.clone());

            if let Some(common) = common_families {
                if common.is_empty() {
                    family_present_in_all = false;
                }
                if family_present_in_all {
                    for fam in common {
                        if let Some(Some(union_range)) = family_union_ranges.get(&fam) {
                            enforced_map.insert(
                                fam.clone(),
                                Requirement::construct(fam, Some(union_range.clone())),
                            );
                        }
                    }
                }
            }
            enforced_by_req.push((req_label, enforced_map));
        }

        // Compare enforced requirements across all pairs of requests
        for i in 0..enforced_by_req.len() {
            for j in (i + 1)..enforced_by_req.len() {
                let (label_a, map_a) = &enforced_by_req[i];
                let (label_b, map_b) = &enforced_by_req[j];
                for (dep_family, req_from_a) in map_a {
                    if let Some(req_from_b) = map_b.get(dep_family) {
                        if req_from_a.conflicts_with_req(req_from_b) {
                            let key = (label_a.clone(), label_b.clone(), dep_family.clone());
                            if seen_conflicts.insert(key) {
                                let versions_a: Vec<String> = candidate_packages_by_req
                                    .get(i)
                                    .map(|(_, _, pkgs)| {
                                        pkgs.iter()
                                            .map(|p| p.version.to_string())
                                            .collect::<BTreeSet<_>>()
                                            .into_iter()
                                            .collect()
                                    })
                                    .unwrap_or_default();
                                report.dependency_conflicts.push(PackageConflictDiagnostic {
                                    source_package: label_a.clone(),
                                    source_versions: versions_a,
                                    source_dependency: req_from_a.to_string(),
                                    conflicting_target: label_b.clone(),
                                    conflicting_requirement: req_from_b.to_string(),
                                    explanation: format!(
                                        "'{label_a}' requires '{req_from_a}', which conflicts with '{req_from_b}' required by '{label_b}'"
                                    ),
                                });
                            }
                        }
                    }
                }
            }
        }

        // 4. What-If Analysis: test alternative versions of each user-requested package
        if report.missing_packages.is_empty() {
            report.what_if_suggestions =
                compute_what_if_suggestions(user_requests, implicit_requests, provider, building)?;
        }

        Ok(report)
    }

    /// Format a clear, studio-friendly explanation of why the resolve failed and how to fix it.
    pub fn format_human(&self) -> String {
        let mut lines = Vec::new();
        lines.push(
            "================================================================================"
                .to_string(),
        );
        lines.push("Rez Conflict Diagnostics & What-If Analysis".to_string());
        lines.push(
            "================================================================================"
                .to_string(),
        );

        if !self.direct_contradictions.is_empty() {
            lines.push("\n[1] Direct Request Contradictions:".to_string());
            for item in &self.direct_contradictions {
                lines.push(format!("  - {}", item.explanation));
            }
        }

        if !self.missing_packages.is_empty() {
            lines.push("\n[2] Missing Packages in Repository:".to_string());
            for item in &self.missing_packages {
                match &item.required_by {
                    Some(parent) => lines.push(format!(
                        "  - Package '{}' (requested as '{}' by {}) was not found in any search path",
                        item.package_name, item.requested, parent
                    )),
                    None => lines.push(format!(
                        "  - Package '{}' (requested as '{}') was not found in any search path",
                        item.package_name, item.requested
                    )),
                }
            }
        }

        if !self.unsatisfied_ranges.is_empty() {
            lines.push("\n[3] Unsatisfied Version Ranges:".to_string());
            for item in &self.unsatisfied_ranges {
                lines.push(format!("  - {}", item.suggestion));
            }
        }

        if !self.dependency_conflicts.is_empty() {
            lines.push("\n[4] Root-Cause Dependency Conflicts:".to_string());
            for item in &self.dependency_conflicts {
                lines.push(format!("  - {}", item.explanation));
            }
        }

        if !self.what_if_suggestions.is_empty() {
            lines.push("\n[5] Verified 'What-If' Solutions:".to_string());
            for s in &self.what_if_suggestions {
                lines.push(format!(
                    "  * [{}] Replace '{}' with '{}' -> {}",
                    s.direction.to_uppercase(),
                    s.original_request,
                    s.suggested_requirement,
                    s.summary
                ));
                lines.push(format!(
                    "    Resolves to: {}",
                    s.resulting_resolve.join(" ")
                ));
            }
        } else if !self.has_findings() {
            if let Some(ref desc) = self.solver_failure_description {
                lines.push(format!("\nSolver Failure Details:\n  {desc}"));
            }
        }

        lines.push(
            "================================================================================"
                .to_string(),
        );
        lines.join("\n")
    }
}

fn collect_package_variant_requires(pkg: &Package, building: bool) -> Vec<Vec<Requirement>> {
    let mut base_reqs = pkg.requires.clone();
    if building {
        base_reqs.extend(pkg.build_requires.clone());
    }
    if pkg.variants.is_empty() {
        vec![base_reqs]
    } else {
        pkg.variants
            .iter()
            .map(|v| {
                let mut combined = base_reqs.clone();
                combined.extend(v.iter().cloned());
                combined
            })
            .collect()
    }
}

/// Search for single-package version adjustments in `user_requests` that make the
/// full request list solvable.
fn compute_what_if_suggestions(
    user_requests: &[Requirement],
    implicit_requests: &[Requirement],
    provider: &dyn PackageProvider,
    building: bool,
) -> Result<Vec<WhatIfSuggestion>> {
    let mut suggestions = Vec::new();
    if user_requests.is_empty() {
        return Ok(suggestions);
    }

    for (idx, target_req) in user_requests.iter().enumerate() {
        if target_req.conflict() || target_req.weak() || target_req.name().starts_with('.') {
            continue;
        }
        let all_pkgs = provider.get_packages(target_req.name(), &VersionRange::any())?;
        if all_pkgs.is_empty() {
            continue;
        }

        // Collect unique versions outside the originally requested range
        let mut versions_in_range = BTreeSet::new();
        let mut versions_outside_range = BTreeSet::new();
        for p in &all_pkgs {
            let obj = VersionedObject::construct(&p.name, Some(p.version.clone()));
            if target_req.conflicts_with_obj(&obj) {
                versions_outside_range.insert(p.version.clone());
            } else {
                versions_in_range.insert(p.version.clone());
            }
        }

        if versions_outside_range.is_empty() {
            continue;
        }

        let min_requested = versions_in_range.iter().next().cloned();
        let max_requested = versions_in_range.iter().next_back().cloned();

        // Partition outside versions into upgrades (higher than requested) and downgrades (lower than requested),
        // ordered from closest to furthest.
        let mut upgrades: Vec<Version> = Vec::new();
        let mut downgrades: Vec<Version> = Vec::new();
        for v in versions_outside_range {
            if let Some(ref max_v) = max_requested {
                if &v > max_v {
                    upgrades.push(v);
                    continue;
                }
            }
            if let Some(ref min_v) = min_requested {
                if &v < min_v {
                    downgrades.push(v);
                    continue;
                }
            }
            upgrades.push(v);
        }
        // Closest upgrade is smallest version > max_requested; closest downgrade is largest version < min_requested
        upgrades.sort();
        downgrades.sort_by(|a, b| b.cmp(a));

        // Test closest upgrade (up to 4 candidates) and closest downgrade (up to 4 candidates)
        for (direction, candidates) in [
            ("upgrade", upgrades.iter().take(4)),
            ("downgrade", downgrades.iter().take(4)),
        ] {
            for cand_ver in candidates {
                let exact_req_str = if cand_ver.is_empty() {
                    target_req.name().to_string()
                } else {
                    format!("{}-{}", target_req.name(), cand_ver)
                };
                let Ok(replacement_req) = Requirement::new(&exact_req_str) else {
                    continue;
                };

                let mut trial_reqs =
                    Vec::with_capacity(user_requests.len() + implicit_requests.len());
                for (j, r) in user_requests.iter().enumerate() {
                    if j == idx {
                        trial_reqs.push(replacement_req.clone());
                    } else {
                        trial_reqs.push(r.clone());
                    }
                }
                trial_reqs.extend(implicit_requests.iter().cloned());

                let Ok(mut solver) = Solver::new(trial_reqs, provider, building, None) else {
                    continue;
                };
                if solver.solve().is_ok() && solver.status() == SolverStatus::Solved {
                    let resolved: Vec<String> = solver
                        .resolved_packages()
                        .unwrap_or_default()
                        .iter()
                        .map(|pv| format!("{}-{}", pv.name(), pv.version()))
                        .collect();
                    let dir_label = if versions_in_range.is_empty() {
                        "alternative"
                    } else {
                        direction
                    };
                    suggestions.push(WhatIfSuggestion {
                        original_request: target_req.to_string(),
                        suggested_requirement: exact_req_str.clone(),
                        direction: dir_label.to_string(),
                        summary: format!(
                            "Changing '{}' to '{}' resolves all dependency constraints",
                            target_req, exact_req_str
                        ),
                        resulting_resolve: resolved,
                    });
                    break;
                }
            }
        }
    }

    Ok(suggestions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use repository::provider::MemoryPackageProvider;

    fn make_pkg(name: &str, version: &str, requires: &[&str]) -> Package {
        let mut pkg = Package::new(name.to_string(), Version::new(version).unwrap());
        pkg.requires = requires
            .iter()
            .map(|r| Requirement::new(r).unwrap())
            .collect();
        pkg
    }

    #[test]
    fn test_conflict_diagnostics_and_what_if_suggestions() {
        let mut provider = MemoryPackageProvider::new();
        // Python versions
        provider.add(make_pkg("python", "3.9.18", &[]));
        provider.add(make_pkg("python", "3.10.13", &[]));

        // Maya 2024 requires python-3.10
        provider.add(make_pkg("maya", "2024.0", &["python-3.10+<3.11"]));

        // Studio rig_tools:
        // - 1.8.0 requires python-3.9
        // - 2.0.0 requires python-3.9 (conflicts with Maya 2024!)
        // - 2.1.0 requires python-3.10 (compatible with Maya 2024!)
        provider.add(make_pkg("rig_tools", "1.8.0", &["python-3.10+<3.11"]));
        provider.add(make_pkg("rig_tools", "2.0.0", &["python-3.9+<3.10"]));
        provider.add(make_pkg("rig_tools", "2.1.0", &["python-3.10+<3.11"]));

        let user_reqs = vec![
            Requirement::new("maya-2024.0").unwrap(),
            Requirement::new("rig_tools-2.0.0").unwrap(),
        ];

        let report = ConflictDiagnosticReport::analyze(&user_reqs, &[], &provider, false, None)
            .expect("diagnostics should succeed");

        assert!(report.has_findings());
        assert_eq!(report.dependency_conflicts.len(), 1);
        assert!(report.dependency_conflicts[0]
            .explanation
            .contains("python"));

        // Should suggest both upgrading rig_tools to 2.1.0 and downgrading to 1.8.0!
        assert_eq!(report.what_if_suggestions.len(), 2);
        let suggested_reqs: Vec<&str> = report
            .what_if_suggestions
            .iter()
            .map(|s| s.suggested_requirement.as_str())
            .collect();
        assert!(suggested_reqs.contains(&"rig_tools-2.1.0"));
        assert!(suggested_reqs.contains(&"rig_tools-1.8.0"));
    }

    #[test]
    fn test_missing_package_and_unsatisfied_range_diagnostics() {
        let mut provider = MemoryPackageProvider::new();
        provider.add(make_pkg("usd", "22.11", &[]));
        provider.add(make_pkg("usd", "23.05", &[]));

        let user_reqs = vec![
            Requirement::new("nonexistent_pkg-1.0").unwrap(),
            Requirement::new("usd-24.0+").unwrap(),
        ];

        let report = ConflictDiagnosticReport::analyze(&user_reqs, &[], &provider, false, None)
            .expect("diagnostics should succeed");

        assert_eq!(report.missing_packages.len(), 1);
        assert_eq!(report.missing_packages[0].package_name, "nonexistent_pkg");

        assert_eq!(report.unsatisfied_ranges.len(), 1);
        assert_eq!(
            report.unsatisfied_ranges[0].nearest_version.as_deref(),
            Some("23.05")
        );
    }
}
