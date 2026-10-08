// SPDX-License-Identifier: Apache-2.0

//! Package version ordering and prioritization.
//!
//! Ported from Python rez package_order.py.

use crate::errors::{Result, RezError};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use version::{Version, VersionRange};

/// Package ordering strategy (trait object for dynamic dispatch)
pub trait PackageOrder: Send + Sync {
    /// Package families this orderer applies to (empty or `*` means all packages)
    fn packages(&self) -> &[String];

    /// Apply ordering to mutable slice of versions
    fn reorder(&self, _name: &str, versions: &mut Vec<Version>);

    /// Convert this orderer into the Rez package-order POD representation.
    ///
    /// Custom orderers must override this method before they can be preserved in
    /// a resolved context. Unsupported implementations fail explicitly.
    fn to_pod(&self) -> Result<PackageOrderPod> {
        Err(RezError::Plugin(
            "custom package orderer does not support Rez POD serialization".into(),
        ))
    }

    /// Check if this orderer applies to given package name
    fn applies_to(&self, name: &str) -> bool {
        let packages = self.packages();
        packages.is_empty() || packages.iter().any(|p| p == name || p == "*")
    }
}

/// Sort order direction
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    /// Latest version first (default)
    Descending,
    /// Oldest version first
    Ascending,
}

/// Per-family ordering with configurable sort direction
#[derive(Debug, Clone)]
pub struct PerFamilyOrder {
    packages: Vec<String>,
    order: SortOrder,
}

impl PerFamilyOrder {
    /// Create new per-family orderer
    pub fn new(packages: Vec<String>, order: SortOrder) -> Self {
        Self { packages, order }
    }

    /// Descending order (latest first)
    pub fn descending(packages: Vec<String>) -> Self {
        Self::new(packages, SortOrder::Descending)
    }

    /// Ascending order (oldest first)
    pub fn ascending(packages: Vec<String>) -> Self {
        Self::new(packages, SortOrder::Ascending)
    }
}

impl PackageOrder for PerFamilyOrder {
    fn packages(&self) -> &[String] {
        &self.packages
    }

    fn reorder(&self, _name: &str, versions: &mut Vec<Version>) {
        match self.order {
            SortOrder::Descending => versions.sort_by(|a, b| b.cmp(a)),
            SortOrder::Ascending => versions.sort(),
        }
    }

    fn to_pod(&self) -> Result<PackageOrderPod> {
        Ok(PackageOrderPod::SortedOrder {
            descending: self.order == SortOrder::Descending,
            packages: Some(self.packages.clone()),
        })
    }
}

/// Version split ordering: different sort order before/after a pivot version
#[derive(Debug, Clone)]
pub struct VersionSplitOrder {
    packages: Vec<String>,
    first_version: Version,
    /// If true: versions >= first_version are ascending, else descending
    ascending_after: bool,
}

impl VersionSplitOrder {
    /// Create new version split orderer
    ///
    /// # Arguments
    /// * `packages` - Package families to apply this ordering to
    /// * `first_version` - Pivot version
    /// * `ascending_after` - If true, versions >= pivot are ascending, else descending
    pub fn new(packages: Vec<String>, first_version: Version, ascending_after: bool) -> Self {
        Self {
            packages,
            first_version,
            ascending_after,
        }
    }
}

impl PackageOrder for VersionSplitOrder {
    fn packages(&self) -> &[String] {
        &self.packages
    }

    fn reorder(&self, _name: &str, versions: &mut Vec<Version>) {
        // Sort with priority key: versions <= first_version come first
        versions.sort_by(|a, b| {
            let a_priority = if *a <= self.first_version { 1 } else { 0 };
            let b_priority = if *b <= self.first_version { 1 } else { 0 };

            match b_priority.cmp(&a_priority) {
                Ordering::Equal => {
                    // Within same priority group, sort by version
                    if *a <= self.first_version {
                        // Before/at pivot: descending (latest first)
                        b.cmp(a)
                    } else if self.ascending_after {
                        // After pivot with ascending_after=true: ascending
                        a.cmp(b)
                    } else {
                        // After pivot with ascending_after=false: descending
                        b.cmp(a)
                    }
                }
                other => other,
            }
        });
    }

    fn to_pod(&self) -> Result<PackageOrderPod> {
        Ok(PackageOrderPod::VersionSplit {
            first_version: self.first_version.to_string(),
            ascending_after: self.ascending_after,
            packages: Some(self.packages.clone()),
        })
    }
}

/// Null orderer: no reordering (preserves original order)
#[derive(Debug, Clone)]
pub struct NullOrder {
    packages: Vec<String>,
}

impl NullOrder {
    pub fn new(packages: Vec<String>) -> Self {
        Self { packages }
    }
}

impl PackageOrder for NullOrder {
    fn packages(&self) -> &[String] {
        &self.packages
    }

    fn reorder(&self, _name: &str, _versions: &mut Vec<Version>) {
        // No-op: preserve existing order
    }

    fn to_pod(&self) -> Result<PackageOrderPod> {
        Ok(PackageOrderPod::NullOrder {
            packages: Some(self.packages.clone()),
        })
    }
}

/// Sorted version orderer (ascending or descending)
#[derive(Debug, Clone)]
pub struct SortedOrder {
    packages: Vec<String>,
    descending: bool,
}

impl SortedOrder {
    pub fn new(packages: Vec<String>, descending: bool) -> Self {
        Self {
            packages,
            descending,
        }
    }
}

impl PackageOrder for SortedOrder {
    fn packages(&self) -> &[String] {
        &self.packages
    }

    fn reorder(&self, _name: &str, versions: &mut Vec<Version>) {
        if self.descending {
            versions.sort_by(|a, b| b.cmp(a));
        } else {
            versions.sort();
        }
    }

    fn to_pod(&self) -> Result<PackageOrderPod> {
        Ok(PackageOrderPod::SortedOrder {
            descending: self.descending,
            packages: Some(self.packages.clone()),
        })
    }
}

/// Timestamp-based package order (soft_timestamp).
/// Prefers packages released before timestamp T, with optional rank flexibility.
pub struct TimestampPackageOrder {
    packages: Vec<String>,
    timestamp: i64,
    rank: Option<usize>,
    /// Cached first-version-after-T per family (set via set_first_after)
    pub(crate) first_after: Mutex<HashMap<String, Option<Version>>>,
}

impl std::fmt::Debug for TimestampPackageOrder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimestampPackageOrder")
            .field("timestamp", &self.timestamp)
            .field("rank", &self.rank)
            .field("packages", &self.packages)
            .finish()
    }
}

impl Clone for TimestampPackageOrder {
    fn clone(&self) -> Self {
        Self {
            packages: self.packages.clone(),
            timestamp: self.timestamp,
            rank: self.rank,
            first_after: Mutex::new(self.first_after.lock().unwrap().clone()),
        }
    }
}

impl TimestampPackageOrder {
    pub fn new(packages: Vec<String>, timestamp: i64, rank: usize) -> Self {
        Self::with_rank(packages, timestamp, Some(rank))
    }

    fn with_rank(packages: Vec<String>, timestamp: i64, rank: Option<usize>) -> Self {
        Self {
            packages,
            timestamp,
            rank,
            first_after: Mutex::new(HashMap::new()),
        }
    }

    /// Precompute the first-version-after-T for a package family.
    /// Call this with version+timestamp pairs (descending by version).
    pub fn set_first_after(&self, family: &str, version: Option<Version>) {
        self.first_after
            .lock()
            .unwrap()
            .insert(family.to_string(), version);
    }

    /// Compute first_after from a list of (version, timestamp) pairs.
    /// Pairs should be provided in any order; this function sorts them.
    pub fn compute_first_after(&self, family: &str, version_timestamps: &[(Version, Option<i64>)]) {
        let mut sorted: Vec<_> = version_timestamps.to_vec();
        sorted.sort_by(|a, b| b.0.cmp(&a.0)); // descending by version

        let mut first_after: Option<Version> = None;
        let mut last_before_idx = None;

        for (i, (ver, ts)) in sorted.iter().enumerate() {
            if let Some(t) = ts {
                if *t > self.timestamp {
                    first_after = Some(ver.clone());
                } else {
                    last_before_idx = Some(i);
                    break;
                }
            }
        }

        if let Some(rank) = self.rank.filter(|rank| *rank > 0) {
            if let Some(idx) = last_before_idx {
                if idx > 0 {
                    let trimmed = sorted[idx].0.trim(rank - 1);
                    first_after = None;
                    for item in sorted[..idx].iter().rev() {
                        if item.0.trim(rank - 1) != trimmed {
                            first_after = Some(item.0.clone());
                            break;
                        }
                    }
                }
            }
        }

        self.first_after
            .lock()
            .unwrap()
            .insert(family.to_string(), first_after);
    }
}

impl PackageOrder for TimestampPackageOrder {
    fn packages(&self) -> &[String] {
        &self.packages
    }

    fn reorder(&self, name: &str, versions: &mut Vec<Version>) {
        let cache = self.first_after.lock().unwrap();
        let first_after = cache.get(name).cloned().flatten();
        drop(cache);

        // Split into before/after
        let (mut before, mut after): (Vec<_>, Vec<_>) = match &first_after {
            None => (std::mem::take(versions), vec![]),
            Some(fa) => versions.drain(..).partition(|v: &Version| *v < *fa),
        };

        // Before T: descending
        before.sort_by(|a, b| b.cmp(a));

        if let Some(rank) = self
            .rank
            .filter(|rank| *rank > 0)
            .filter(|_| !after.is_empty())
        {
            // After T with rank: group by trimmed version (ascending across groups),
            // descending within each group
            after.sort_by(|a, b| {
                let a_trim = a.trim(rank - 1);
                let b_trim = b.trim(rank - 1);
                match a_trim.cmp(&b_trim) {
                    Ordering::Equal => b.cmp(a), // descending within group
                    other => other,              // ascending across groups
                }
            });
        } else {
            // After T without rank: ascending
            after.sort();
        }

        versions.clear();
        versions.extend(before);
        versions.extend(after);
    }

    fn to_pod(&self) -> Result<PackageOrderPod> {
        Ok(PackageOrderPod::Timestamp {
            timestamp: self.timestamp,
            rank: self.rank,
            packages: Some(self.packages.clone()),
        })
    }
}

/// Rez's nested per-family orderer: each family selects its own orderer,
/// with an optional fallback for all other families.
#[derive(Clone)]
pub struct PerFamilyPackageOrder {
    dispatch_packages: Vec<String>,
    orderers: HashMap<String, Arc<dyn PackageOrder>>,
    default_order: Option<Arc<dyn PackageOrder>>,
}

impl PerFamilyPackageOrder {
    fn from_pod(
        orderer_pods: Vec<serde_json::Value>,
        default_pod: Option<serde_json::Value>,
    ) -> Result<Self> {
        let mut orderers = HashMap::new();
        for (index, value) in orderer_pods.into_iter().enumerate() {
            let family_names: Vec<String> = value
                .get("packages")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| {
                    RezError::Config(format!(
                        "per_family orderers[{index}].packages must be an array"
                    ))
                })?
                .iter()
                .enumerate()
                .map(|(family_index, family)| {
                    family.as_str().map(str::to_owned).ok_or_else(|| {
                        RezError::Config(format!(
                            "per_family orderers[{index}].packages[{family_index}] must be a string"
                        ))
                    })
                })
                .collect::<Result<_>>()?;
            let pod: PackageOrderPod = serde_json::from_value(value).map_err(|error| {
                RezError::Config(format!("invalid per_family orderers[{index}]: {error}"))
            })?;
            let orderer: Arc<dyn PackageOrder> = Arc::from(pod.into_orderer()?);
            for family in family_names {
                orderers.insert(family, Arc::clone(&orderer));
            }
        }

        let default_order = default_pod
            .map(|value| {
                let pod: PackageOrderPod = serde_json::from_value(value).map_err(|error| {
                    RezError::Config(format!("invalid per_family default_order: {error}"))
                })?;
                pod.into_orderer().map(Arc::from)
            })
            .transpose()?;

        let mut dispatch_packages: Vec<_> = orderers.keys().cloned().collect();
        dispatch_packages.sort();

        Ok(Self {
            dispatch_packages,
            orderers,
            default_order,
        })
    }
}

impl PackageOrder for PerFamilyPackageOrder {
    fn packages(&self) -> &[String] {
        &self.dispatch_packages
    }

    fn reorder(&self, name: &str, versions: &mut Vec<Version>) {
        if let Some(orderer) = self.orderers.get(name).or(self.default_order.as_ref()) {
            orderer.reorder(name, versions);
        }
    }

    fn to_pod(&self) -> Result<PackageOrderPod> {
        let mut families: Vec<_> = self.orderers.iter().collect();
        families.sort_by_key(|(family, _)| family.as_str());

        let mut grouped: Vec<(Arc<dyn PackageOrder>, Vec<String>)> = Vec::new();
        for (family, orderer) in families {
            if let Some((_, package_names)) = grouped
                .iter_mut()
                .find(|(existing, _)| Arc::ptr_eq(existing, orderer))
            {
                package_names.push(family.clone());
            } else {
                grouped.push((Arc::clone(orderer), vec![family.clone()]));
            }
        }

        let orderers = grouped
            .into_iter()
            .map(|(orderer, mut packages)| {
                packages.sort();
                let mut value = serde_json::to_value(orderer.to_pod()?).map_err(|error| {
                    RezError::Config(format!("cannot encode nested package orderer: {error}"))
                })?;
                let object = value.as_object_mut().ok_or_else(|| {
                    RezError::Config("nested package orderer POD must be a mapping".into())
                })?;
                object.insert("packages".into(), serde_json::json!(packages));
                Ok(value)
            })
            .collect::<Result<Vec<_>>>()?;

        let default_order = self
            .default_order
            .as_ref()
            .map(|orderer| {
                serde_json::to_value(orderer.to_pod()?).map_err(|error| {
                    RezError::Config(format!("cannot encode per_family default_order: {error}"))
                })
            })
            .transpose()?;

        Ok(PackageOrderPod::PerFamily {
            orderers,
            default_order,
        })
    }
}

/// List of package orderers with per-family dispatch
#[derive(Clone)]
pub struct PackageOrderList {
    orderers: Vec<Arc<dyn PackageOrder>>,
    /// Fast lookup: package name -> orderer index
    by_package: HashMap<String, usize>,
}

impl std::fmt::Debug for PackageOrderList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackageOrderList")
            .field("len", &self.orderers.len())
            .field("by_package", &self.by_package)
            .finish()
    }
}

impl PackageOrderList {
    /// Create empty orderer list
    pub fn new() -> Self {
        Self {
            orderers: Vec::new(),
            by_package: HashMap::new(),
        }
    }

    /// Build the runtime list from Rez package-order POD values.
    pub fn from_pod(pods: Vec<PackageOrderPod>) -> Result<Self> {
        let mut list = Self::new();
        for pod in pods {
            list.add_orderer(pod.into_orderer()?);
        }
        Ok(list)
    }

    /// Convert the runtime list into Rez package-order POD values.
    pub fn to_pod(&self) -> Result<Vec<PackageOrderPod>> {
        self.orderers
            .iter()
            .map(|orderer| orderer.to_pod())
            .collect()
    }

    /// Add orderer to list
    pub fn add_orderer(&mut self, orderer: Box<dyn PackageOrder>) {
        let idx = self.orderers.len();
        for pkg in orderer.packages() {
            // First orderer for package wins
            self.by_package.entry(pkg.clone()).or_insert(idx);
        }
        self.orderers.push(orderer.into());
    }

    /// Reorder versions for given package name
    pub fn reorder(&self, name: &str, versions: &mut Vec<Version>) {
        // Try exact package name first
        if let Some(&idx) = self.by_package.get(name) {
            self.orderers[idx].reorder(name, versions);
            return;
        }

        // Try wildcard "*"
        if let Some(&idx) = self.by_package.get("*") {
            self.orderers[idx].reorder(name, versions);
            return;
        }

        // Default: descending order (latest first)
        versions.sort_by(|a, b| b.cmp(a));
    }

    /// Compare versions in the order used for this package family.
    ///
    /// Custom orderers are queried through their existing `reorder` behavior;
    /// equal custom keys fall back to Rez's version comparison, matching
    /// `FallbackComparable` in the Python implementation.
    pub fn compare_versions(&self, name: &str, left: &Version, right: &Version) -> Ordering {
        if left == right {
            return Ordering::Equal;
        }
        let Some(orderer) = self.get_orderer(name) else {
            return left.cmp(right);
        };

        let first = |versions: &mut Vec<Version>| {
            orderer.reorder(name, versions);
            versions.first().cloned()
        };
        let forward = first(&mut vec![left.clone(), right.clone()]);
        let reverse = first(&mut vec![right.clone(), left.clone()]);
        match (forward.as_ref(), reverse.as_ref()) {
            (Some(a), Some(b)) if a == left && b == right => left.cmp(right),
            (Some(a), Some(b)) if a == right && b == right => Ordering::Less,
            (Some(a), Some(b)) if a == left && b == left => Ordering::Greater,
            _ => left.cmp(right),
        }
    }

    /// Compare version ranges using this family's package ordering.
    pub fn compare_ranges(
        &self,
        name: &str,
        left: &VersionRange,
        right: &VersionRange,
    ) -> Ordering {
        left.cmp_by(right, |a, b| self.compare_versions(name, a, b))
    }

    /// Get orderer for package name (None if no match)
    pub fn get_orderer(&self, name: &str) -> Option<&dyn PackageOrder> {
        if let Some(&idx) = self.by_package.get(name) {
            return Some(&*self.orderers[idx]);
        }

        if let Some(&idx) = self.by_package.get("*") {
            return Some(&*self.orderers[idx]);
        }

        None
    }
}

impl Default for PackageOrderList {
    fn default() -> Self {
        Self::new()
    }
}

fn default_timestamp_rank() -> Option<usize> {
    Some(0)
}

/// Serializable representation of a Rez package orderer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PackageOrderPod {
    #[serde(rename = "no_order")]
    NullOrder {
        #[serde(default)]
        packages: Option<Vec<String>>,
    },

    #[serde(rename = "sorted")]
    SortedOrder {
        descending: bool,
        #[serde(default)]
        packages: Option<Vec<String>>,
    },

    #[serde(rename = "version_split")]
    VersionSplit {
        first_version: String,
        /// Rust extension; Rez's native behavior is `false` (descending after pivot).
        #[serde(default)]
        ascending_after: bool,
        #[serde(default)]
        packages: Option<Vec<String>>,
    },

    #[serde(rename = "soft_timestamp")]
    Timestamp {
        timestamp: i64,
        #[serde(default = "default_timestamp_rank")]
        rank: Option<usize>,
        #[serde(default)]
        packages: Option<Vec<String>>,
    },

    /// Nested family-specific orderers used by Rez's composite orderer.
    #[serde(rename = "per_family")]
    PerFamily {
        orderers: Vec<serde_json::Value>,
        #[serde(default)]
        default_order: Option<serde_json::Value>,
    },
}

impl PackageOrderPod {
    /// Convert a Rez POD value to a live orderer without discarding invalid data.
    pub fn into_orderer(self) -> Result<Box<dyn PackageOrder>> {
        let packages =
            |packages: Option<Vec<String>>| packages.unwrap_or_else(|| vec!["*".to_string()]);

        match self {
            Self::NullOrder { packages: names } => Ok(Box::new(NullOrder::new(packages(names)))),
            Self::SortedOrder {
                descending,
                packages: names,
            } => Ok(Box::new(SortedOrder::new(packages(names), descending))),
            Self::VersionSplit {
                first_version,
                ascending_after,
                packages: names,
            } => {
                let version = Version::new(&first_version).map_err(|error| {
                    RezError::Config(format!(
                        "invalid package order first_version {first_version:?}: {error}"
                    ))
                })?;
                Ok(Box::new(VersionSplitOrder::new(
                    packages(names),
                    version,
                    ascending_after,
                )))
            }
            Self::Timestamp {
                timestamp,
                rank,
                packages: names,
            } => Ok(Box::new(TimestampPackageOrder::with_rank(
                packages(names),
                timestamp,
                rank,
            ))),
            Self::PerFamily {
                orderers,
                default_order,
            } => Ok(Box::new(PerFamilyPackageOrder::from_pod(
                orderers,
                default_order,
            )?)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    #[test]
    fn test_per_family_descending() {
        let orderer = PerFamilyOrder::descending(vec!["maya".to_string()]);
        let mut versions = vec![v("1.0"), v("2.0"), v("1.5")];
        orderer.reorder("maya", &mut versions);

        assert_eq!(versions, vec![v("2.0"), v("1.5"), v("1.0")]);
    }

    #[test]
    fn test_per_family_ascending() {
        let orderer = PerFamilyOrder::ascending(vec!["maya".to_string()]);
        let mut versions = vec![v("2.0"), v("1.0"), v("1.5")];
        orderer.reorder("maya", &mut versions);

        assert_eq!(versions, vec![v("1.0"), v("1.5"), v("2.0")]);
    }

    #[test]
    fn test_version_split_order() {
        // Split at version 3.0: prefer <=3.0, then >3.0
        let orderer = VersionSplitOrder::new(
            vec!["python".to_string()],
            v("3.0"),
            false, // descending after pivot
        );

        let mut versions = vec![v("2.7"), v("3.6"), v("3.0"), v("3.8"), v("2.6")];
        orderer.reorder("python", &mut versions);

        // Expected: [3.0, 2.7, 2.6] (before pivot, desc), then [3.8, 3.6] (after pivot, desc)
        assert_eq!(
            versions,
            vec![v("3.0"), v("2.7"), v("2.6"), v("3.8"), v("3.6")]
        );
    }

    #[test]
    fn test_version_split_ascending_after() {
        let orderer = VersionSplitOrder::new(
            vec!["python".to_string()],
            v("3.0"),
            true, // ascending after pivot
        );

        let mut versions = vec![v("2.7"), v("3.6"), v("3.0"), v("3.8"), v("2.6")];
        orderer.reorder("python", &mut versions);

        // Expected: [3.0, 2.7, 2.6] (before pivot, desc), then [3.6, 3.8] (after pivot, asc)
        assert_eq!(
            versions,
            vec![v("3.0"), v("2.7"), v("2.6"), v("3.6"), v("3.8")]
        );
    }

    #[test]
    fn test_null_order() {
        let orderer = NullOrder::new(vec!["test".to_string()]);
        let mut versions = vec![v("3.0"), v("1.0"), v("2.0")];
        let original = versions.clone();

        orderer.reorder("test", &mut versions);
        assert_eq!(versions, original); // No change
    }

    #[test]
    fn test_package_order_list() {
        let mut list = PackageOrderList::new();

        // Maya: descending
        list.add_orderer(Box::new(PerFamilyOrder::descending(vec![
            "maya".to_string()
        ])));

        // Python: split at 3.0
        list.add_orderer(Box::new(VersionSplitOrder::new(
            vec!["python".to_string()],
            v("3.0"),
            false,
        )));

        // Default for all: ascending
        list.add_orderer(Box::new(PerFamilyOrder::ascending(vec!["*".to_string()])));

        // Test maya (descending)
        let mut maya_versions = vec![v("2018"), v("2020"), v("2019")];
        list.reorder("maya", &mut maya_versions);
        assert_eq!(maya_versions, vec![v("2020"), v("2019"), v("2018")]);

        // Test python (split)
        let mut py_versions = vec![v("2.7"), v("3.6"), v("3.0")];
        list.reorder("python", &mut py_versions);
        assert_eq!(py_versions, vec![v("3.0"), v("2.7"), v("3.6")]);

        // Test houdini (default wildcard = ascending)
        let mut hou_versions = vec![v("18.5"), v("18.0"), v("19.0")];
        list.reorder("houdini", &mut hou_versions);
        assert_eq!(hou_versions, vec![v("18.0"), v("18.5"), v("19.0")]);
    }

    #[test]
    fn test_applies_to() {
        let orderer = PerFamilyOrder::descending(vec!["maya".to_string(), "houdini".to_string()]);
        assert!(orderer.applies_to("maya"));
        assert!(orderer.applies_to("houdini"));
        assert!(!orderer.applies_to("python"));

        let wildcard = PerFamilyOrder::descending(vec!["*".to_string()]);
        assert!(wildcard.applies_to("anything"));
    }

    #[test]
    fn test_first_orderer_wins() {
        let mut list = PackageOrderList::new();

        // First orderer for maya: ascending
        list.add_orderer(Box::new(PerFamilyOrder::ascending(
            vec!["maya".to_string()],
        )));

        // Second orderer for maya: descending (should be ignored)
        list.add_orderer(Box::new(PerFamilyOrder::descending(vec![
            "maya".to_string()
        ])));

        let mut versions = vec![v("2.0"), v("1.0"), v("1.5")];
        list.reorder("maya", &mut versions);

        // Should use first orderer (ascending)
        assert_eq!(versions, vec![v("1.0"), v("1.5"), v("2.0")]);
    }

    #[test]
    fn test_sorted_order_descending() {
        let orderer = SortedOrder::new(vec!["test".into()], true);
        let mut versions = vec![v("1.0"), v("3.0"), v("2.0")];
        orderer.reorder("test", &mut versions);
        assert_eq!(versions, vec![v("3.0"), v("2.0"), v("1.0")]);
    }

    #[test]
    fn test_sorted_order_ascending() {
        let orderer = SortedOrder::new(vec!["test".into()], false);
        let mut versions = vec![v("3.0"), v("1.0"), v("2.0")];
        orderer.reorder("test", &mut versions);
        assert_eq!(versions, vec![v("1.0"), v("2.0"), v("3.0")]);
    }

    #[test]
    fn test_timestamp_order_basic() {
        let orderer = TimestampPackageOrder::new(vec!["foo".into()], 1000, 0);
        // Set first_after to 2.0.5 (meaning 2.0.5+ are after timestamp)
        orderer.set_first_after("foo", Some(v("2.0.5")));

        let mut versions = vec![v("2.0.0"), v("1.9.0"), v("2.0.5"), v("2.0.6"), v("2.1.0")];
        orderer.reorder("foo", &mut versions);

        // Before T (< 2.0.5): descending, then After T (>= 2.0.5): ascending
        assert_eq!(
            versions,
            vec![v("2.0.0"), v("1.9.0"), v("2.0.5"), v("2.0.6"), v("2.1.0")]
        );
    }

    #[test]
    fn test_timestamp_order_with_rank() {
        let orderer = TimestampPackageOrder::new(vec!["foo".into()], 1000, 3);
        // With rank=3, patches are allowed over the timestamp
        // first_after = 2.1.0 (first version with different minor after T)
        orderer.set_first_after("foo", Some(v("2.1.0")));

        let mut versions = vec![
            v("2.2.1"),
            v("2.2.0"),
            v("2.1.1"),
            v("2.1.0"),
            v("2.0.6"),
            v("2.0.5"),
            v("2.0.0"),
            v("1.9.0"),
        ];
        orderer.reorder("foo", &mut versions);

        // Before T: descending
        // After T with rank=3: group by minor (ascending), descending within
        assert_eq!(
            versions,
            vec![
                v("2.0.6"),
                v("2.0.5"),
                v("2.0.0"),
                v("1.9.0"),
                v("2.1.1"),
                v("2.1.0"),
                v("2.2.1"),
                v("2.2.0"),
            ]
        );
    }

    #[test]
    fn test_timestamp_order_all_before() {
        let orderer = TimestampPackageOrder::new(vec!["foo".into()], 1000, 0);
        orderer.set_first_after("foo", None); // all before T

        let mut versions = vec![v("1.0"), v("3.0"), v("2.0")];
        orderer.reorder("foo", &mut versions);
        assert_eq!(versions, vec![v("3.0"), v("2.0"), v("1.0")]); // descending
    }

    #[test]
    fn test_compute_first_after() {
        let orderer = TimestampPackageOrder::new(vec!["foo".into()], 1000, 0);
        orderer.compute_first_after(
            "foo",
            &[
                (v("1.0"), Some(500)),
                (v("2.0"), Some(800)),
                (v("3.0"), Some(1200)),
                (v("4.0"), Some(1500)),
            ],
        );

        let cache = orderer.first_after.lock().unwrap();
        assert_eq!(cache.get("foo"), Some(&Some(v("3.0"))));
    }

    #[test]
    fn pod_round_trip_preserves_builtin_parameters() {
        let input = serde_json::json!([
            {"type": "no_order", "packages": ["maya"]},
            {"type": "sorted", "descending": false, "packages": ["houdini"]},
            {"type": "version_split", "first_version": "3.0", "ascending_after": true, "packages": ["python"]},
            {"type": "soft_timestamp", "timestamp": 1234, "rank": 3, "packages": ["foo"]}
        ]);
        let pods: Vec<PackageOrderPod> = serde_json::from_value(input.clone()).unwrap();
        let orderers = PackageOrderList::from_pod(pods).unwrap();
        let result = serde_json::to_value(orderers.to_pod().unwrap()).unwrap();

        assert_eq!(result, input);
    }

    #[test]
    fn pod_null_packages_means_all_packages() {
        let pod: PackageOrderPod = serde_json::from_value(serde_json::json!({
            "type": "sorted",
            "descending": false,
            "packages": null
        }))
        .unwrap();
        let list = PackageOrderList::from_pod(vec![pod]).unwrap();
        let mut versions = vec![v("3.0"), v("1.0"), v("2.0")];

        list.reorder("any_family", &mut versions);

        assert_eq!(versions, vec![v("1.0"), v("2.0"), v("3.0")]);
        assert_eq!(list.get_orderer("any_family").unwrap().packages(), &["*"]);
        assert_eq!(
            serde_json::to_value(list.to_pod().unwrap()).unwrap(),
            serde_json::json!([{"type": "sorted", "descending": false, "packages": ["*"]}])
        );
    }

    #[test]
    fn pod_missing_rank_uses_rez_default_zero() {
        let pod: PackageOrderPod = serde_json::from_value(serde_json::json!({
            "type": "soft_timestamp",
            "timestamp": 1000,
            "packages": ["foo"]
        }))
        .unwrap();
        let list = PackageOrderList::from_pod(vec![pod]).unwrap();

        assert_eq!(
            serde_json::to_value(list.to_pod().unwrap()).unwrap(),
            serde_json::json!([{"type": "soft_timestamp", "timestamp": 1000, "rank": 0, "packages": ["foo"]}])
        );
    }

    #[test]
    fn pod_null_rank_is_preserved() {
        let pod: PackageOrderPod = serde_json::from_value(serde_json::json!({
            "type": "soft_timestamp",
            "timestamp": 1000,
            "rank": null,
            "packages": ["foo"]
        }))
        .unwrap();
        let list = PackageOrderList::from_pod(vec![pod]).unwrap();

        assert_eq!(
            serde_json::to_value(list.to_pod().unwrap()).unwrap(),
            serde_json::json!([{"type": "soft_timestamp", "timestamp": 1000, "rank": null, "packages": ["foo"]}])
        );
    }

    #[test]
    fn rust_per_family_order_uses_rez_sorted_pod() {
        let mut list = PackageOrderList::new();
        list.add_orderer(Box::new(PerFamilyOrder::ascending(vec!["maya".into()])));

        assert_eq!(
            serde_json::to_value(list.to_pod().unwrap()).unwrap(),
            serde_json::json!([{"type": "sorted", "descending": false, "packages": ["maya"]}])
        );
    }

    #[test]
    fn rez_composite_per_family_pod_dispatches_and_round_trips() {
        let input = serde_json::json!({
            "type": "per_family",
            "packages": ["ignored-by-rez"],
            "orderers": [
                {
                    "type": "sorted",
                    "descending": false,
                    "packages": ["*", "foo", "bar"]
                }
            ],
            "default_order": {
                "type": "sorted",
                "descending": false
            }
        });
        let pod: PackageOrderPod = serde_json::from_value(input).unwrap();
        let mut list = PackageOrderList::new();
        list.add_orderer(pod.into_orderer().unwrap());

        let mut foo_versions = vec![v("2.0"), v("1.0")];
        list.reorder("foo", &mut foo_versions);
        assert_eq!(foo_versions, vec![v("1.0"), v("2.0")]);

        let mut bar_versions = vec![v("2.0"), v("1.0")];
        list.reorder("bar", &mut bar_versions);
        assert_eq!(bar_versions, vec![v("1.0"), v("2.0")]);

        // The wildcard selects this per-family orderer for other families, then
        // its nested default order applies when no exact family orderer exists.
        for family in ["other", "unlisted"] {
            let mut versions = vec![v("2.0"), v("1.0")];
            list.reorder(family, &mut versions);
            assert_eq!(versions, vec![v("1.0"), v("2.0")]);
        }

        assert_eq!(
            serde_json::to_value(list.to_pod().unwrap()).unwrap(),
            serde_json::json!([{
                "type": "per_family",
                "orderers": [{
                    "type": "sorted",
                    "descending": false,
                    "packages": ["*", "bar", "foo"]
                }],
                "default_order": {
                    "type": "sorted",
                    "descending": false,
                    "packages": ["*"]
                }
            }])
        );
    }

    #[test]
    fn rez_per_family_pod_requires_orderers_field() {
        let missing_orderers = serde_json::from_value::<PackageOrderPod>(serde_json::json!({
            "type": "per_family",
            "default_order": { "type": "sorted", "descending": true }
        }));
        assert!(missing_orderers.is_err());

        let pod: PackageOrderPod = serde_json::from_value(serde_json::json!({
            "type": "per_family",
            "orderers": [{"type": "sorted", "descending": false}]
        }))
        .unwrap();

        assert!(matches!(pod.into_orderer(), Err(RezError::Config(_))));
    }

    #[test]
    fn invalid_version_split_pod_returns_error() {
        let pod = PackageOrderPod::VersionSplit {
            first_version: "..invalid".into(),
            ascending_after: false,
            packages: Some(vec!["python".into()]),
        };

        assert!(matches!(pod.into_orderer(), Err(RezError::Config(_))));
    }

    #[test]
    fn custom_runtime_orderer_cannot_be_silently_serialized() {
        struct CustomOrderer(Vec<String>);

        impl PackageOrder for CustomOrderer {
            fn packages(&self) -> &[String] {
                &self.0
            }

            fn reorder(&self, _name: &str, _versions: &mut Vec<Version>) {}
        }

        let mut list = PackageOrderList::new();
        list.add_orderer(Box::new(CustomOrderer(vec!["custom".into()])));

        assert!(matches!(list.to_pod(), Err(RezError::Plugin(_))));
    }
}
