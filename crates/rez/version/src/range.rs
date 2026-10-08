// SPDX-License-Identifier: Apache-2.0

//! Version ranges - version constraint expressions.
//!
//! Ported from Python rez _version.py.

use super::bound::{Bound, LowerBound, UpperBound};
use super::version::Version;
use foundation::errors::RezError;
use regex::Regex;
use std::cell::OnceCell;
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::{Add, BitAnd, BitOr, Not, Sub};
use std::sync::LazyLock;

// Version group pattern
const VERSION_GROUP: &str = r"([0-9a-zA-Z_]+(?:[.-][0-9a-zA-Z_]+)*)";

// Simplified regex patterns without conditionals - we'll use multiple regex patterns instead
static VERSION_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"^{}$", VERSION_GROUP)).expect("valid VERSION_REGEX pattern")
});

static EXACT_VERSION_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"^==({vg})?$", vg = VERSION_GROUP))
        .expect("valid EXACT_VERSION_REGEX pattern")
});

static INCLUSIVE_RANGE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    // Match version..version where .. is exactly two dots
    // We need to be careful: VERSION_GROUP can contain dots, so we match greedily up to ".."
    Regex::new(r"^(.+?)\.\.(.+?)$|^\.\.(.+)$|^(.+)\.\.$|^\.\.$")
        .expect("valid INCLUSIVE_RANGE_REGEX pattern")
});

static LOWER_BOUND_PREFIX_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"^(>|>=)({vg})?$", vg = VERSION_GROUP))
        .expect("valid LOWER_BOUND_PREFIX_REGEX pattern")
});

static LOWER_BOUND_PLUS_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"^({vg})\+$", vg = VERSION_GROUP))
        .expect("valid LOWER_BOUND_PLUS_REGEX pattern")
});

static UPPER_BOUND_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"^(<|<=)({vg})?$", vg = VERSION_GROUP))
        .expect("valid UPPER_BOUND_REGEX pattern")
});

/// Parser for version range strings.
/// Port of Python _VersionRangeParser class.
struct VersionRangeParser;

impl VersionRangeParser {
    /// Parse a version range string into a list of bounds.
    ///
    /// # Arguments
    /// * `range_str` - Version range string like "1.2+", "1.0..2.0", ">=1.0,<2.0"
    ///
    /// # Returns
    /// * `Ok(Vec<Bound>)` - List of bounds (empty string returns [Bound::any()])
    /// * `Err(RezError)` - If parsing fails
    fn parse(range_str: &str) -> Result<Vec<Bound>, RezError> {
        if range_str.is_empty() {
            return Ok(vec![Bound::any().clone()]);
        }

        let mut bounds = Vec::new();

        // Split by "|" for OR ranges
        for part in range_str.split('|') {
            let part = part.trim();

            if part.is_empty() {
                bounds.push(Bound::any().clone());
                continue;
            }

            // Try each pattern in order of specificity
            // Order is important: most specific first

            // Composite range: with comma (">=1.0,<2.0") or without ("1.2+<2.0")
            let bound = if part.contains(',') {
                Self::parse_composite_range(part)?
            } else if let Some(split) = Self::find_composite_split(part) {
                // No comma but has lower+upper joined: "1.2+<2.0", ">=1.0<2.0"
                Self::parse_composite_no_comma(part, split)?
            } else if let Some(caps) = EXACT_VERSION_REGEX.captures(part) {
                Self::act_exact_version(&caps)?
            } else if let Some(caps) = INCLUSIVE_RANGE_REGEX.captures(part) {
                Self::act_inclusive_range(&caps)?
            } else if let Some(caps) = UPPER_BOUND_REGEX.captures(part) {
                Self::act_upper_bound(&caps)?
            } else if let Some(caps) = LOWER_BOUND_PREFIX_REGEX.captures(part) {
                Self::act_lower_bound_prefix(&caps)?
            } else if let Some(caps) = LOWER_BOUND_PLUS_REGEX.captures(part) {
                Self::act_lower_bound_plus(&caps)?
            } else if let Some(caps) = VERSION_REGEX.captures(part) {
                Self::act_version(&caps)?
            } else {
                return Err(RezError::Version(format!(
                    "Invalid version range syntax: '{}'",
                    part
                )));
            };

            bounds.push(bound);
        }

        Ok(bounds)
    }

    /// Action for a version prefix (e.g., "1.2" -> >=1.2,<1.2_).
    fn act_version(captures: &regex::Captures) -> Result<Bound, RezError> {
        let version_str = captures.get(0).unwrap().as_str();
        let version = Version::new(version_str)?;
        // The version factory always creates one bound; share its prefix policy.
        VersionRange::from_version(&version, None).map(|mut range| range.bounds.remove(0))
    }

    /// Action for exact version (e.g., "==1.2")
    fn act_exact_version(captures: &regex::Captures) -> Result<Bound, RezError> {
        let version_str = captures
            .get(1)
            .ok_or_else(|| RezError::Version("Missing exact version".to_string()))?
            .as_str();

        let version = Version::new(version_str)?;

        let lower = LowerBound::new(version.clone(), true);
        let upper = UpperBound::new(version, true)?;
        Bound::new(Some(lower), Some(upper), true)
    }

    /// Action for inclusive range (e.g., "1.0..2.0")
    fn act_inclusive_range(captures: &regex::Captures) -> Result<Bound, RezError> {
        // Groups: 1,2 = both present, 3 = only upper (..X), 4 = only lower (X..), no groups = ..
        let lower_str = captures
            .get(1)
            .or_else(|| captures.get(4))
            .map(|m| m.as_str());
        let upper_str = captures
            .get(2)
            .or_else(|| captures.get(3))
            .map(|m| m.as_str());

        let lower = if let Some(s) = lower_str {
            if !s.is_empty() {
                let version = Version::new(s)?;
                Some(LowerBound::new(version, true))
            } else {
                None
            }
        } else {
            None
        };

        let upper = if let Some(s) = upper_str {
            if !s.is_empty() {
                let version = Version::new(s)?;
                Some(UpperBound::new(version, true)?)
            } else {
                None
            }
        } else {
            None
        };

        Bound::new(lower, upper, true)
    }

    /// Action for lower bound with prefix (e.g., ">1.2", ">=1.2")
    fn act_lower_bound_prefix(captures: &regex::Captures) -> Result<Bound, RezError> {
        let prefix = captures.get(1).unwrap().as_str();
        let version_str = captures.get(2).map(|m| m.as_str());

        let version = if let Some(v) = version_str {
            if !v.is_empty() {
                Version::new(v)?
            } else {
                Version::empty()
            }
        } else {
            Version::empty()
        };

        let inclusive = prefix == ">=";
        let lower = LowerBound::new(version, inclusive);
        Bound::new(Some(lower), None, true)
    }

    /// Action for lower bound with + suffix (e.g., "1.2+")
    fn act_lower_bound_plus(captures: &regex::Captures) -> Result<Bound, RezError> {
        let version_str = captures.get(1).unwrap().as_str();
        let version = Version::new(version_str)?;
        let lower = LowerBound::new(version, true);
        Bound::new(Some(lower), None, true)
    }

    /// Action for upper bound (e.g., "<2.0", "<=2.0")
    fn act_upper_bound(captures: &regex::Captures) -> Result<Bound, RezError> {
        let prefix = captures.get(1).unwrap().as_str();
        let version_str = captures.get(2).map(|m| m.as_str());

        let (version, inclusive) = match version_str {
            Some(v) if !v.is_empty() => {
                let version = Version::new(v)?;
                let inclusive = prefix == "<=";
                (version, inclusive)
            }
            _ => {
                let inclusive = prefix == "<=";
                (Version::empty(), inclusive)
            }
        };

        let upper = UpperBound::new(version, inclusive)?;
        Bound::new(None, Some(upper), true)
    }

    /// Find split point for composite range without comma.
    /// Patterns: "1.2+<2.0", ">=1.0<2.0", ">1.0<=2.0", "1.2+<=2.0"
    /// Returns the byte index where upper bound starts (at '<').
    fn find_composite_split(part: &str) -> Option<usize> {
        // Look for '<' that is NOT at position 0 and is preceded by
        // either '+' or a digit (end of lower version)
        let bytes = part.as_bytes();
        for i in 1..bytes.len() {
            if bytes[i] == b'<' {
                let prev = bytes[i - 1];
                // Valid split: previous char is '+', digit, or '=' (from ">=")
                if prev == b'+' || prev.is_ascii_digit() {
                    return Some(i);
                }
            }
        }
        None
    }

    /// Parse composite range without comma (e.g., "1.2+<2.0", ">=1.0<2.0")
    fn parse_composite_no_comma(part: &str, split: usize) -> Result<Bound, RezError> {
        let lower_part = &part[..split];
        let upper_part = &part[split..];
        Self::parse_lower_upper(lower_part, upper_part)
    }

    /// Parse composite range with comma (e.g., ">=1.0,<2.0", "<2.0,>=1.0")
    fn parse_composite_range(part: &str) -> Result<Bound, RezError> {
        let parts: Vec<&str> = part.split(',').map(|s| s.trim()).collect();
        if parts.len() != 2 {
            return Err(RezError::Version(format!(
                "Invalid composite range: '{}'",
                part
            )));
        }

        // Determine which part is lower and which is upper
        let first_is_lower = parts[0].starts_with('>')
            || parts[0].ends_with('+')
            || (!parts[0].starts_with('<') && !parts[0].starts_with("<="));

        let (lower_part, upper_part) = if first_is_lower {
            (parts[0], parts[1])
        } else {
            (parts[1], parts[0])
        };

        Self::parse_lower_upper(lower_part, upper_part)
    }

    /// Parse a lower+upper bound pair into a single Bound
    fn parse_lower_upper(lower_part: &str, upper_part: &str) -> Result<Bound, RezError> {
        // Parse lower bound
        let lower = if let Some(caps) = LOWER_BOUND_PREFIX_REGEX.captures(lower_part) {
            Some(Self::parse_lower_from_captures(&caps)?)
        } else if let Some(caps) = LOWER_BOUND_PLUS_REGEX.captures(lower_part) {
            Some(Self::parse_lower_from_plus_captures(&caps)?)
        } else {
            return Err(RezError::Version(format!(
                "Invalid lower bound in composite range: '{}'",
                lower_part
            )));
        };

        // Parse upper bound
        let upper = if let Some(caps) = UPPER_BOUND_REGEX.captures(upper_part) {
            Some(Self::parse_upper_from_captures(&caps)?)
        } else {
            return Err(RezError::Version(format!(
                "Invalid upper bound in composite range: '{}'",
                upper_part
            )));
        };

        Bound::new(lower, upper, true)
    }

    fn parse_lower_from_captures(captures: &regex::Captures) -> Result<LowerBound, RezError> {
        let prefix = captures.get(1).unwrap().as_str();
        let version_str = captures.get(2).map(|m| m.as_str());

        let version = if let Some(v) = version_str {
            if !v.is_empty() {
                Version::new(v)?
            } else {
                Version::empty()
            }
        } else {
            Version::empty()
        };

        let inclusive = prefix == ">=";
        Ok(LowerBound::new(version, inclusive))
    }

    fn parse_lower_from_plus_captures(captures: &regex::Captures) -> Result<LowerBound, RezError> {
        let version_str = captures.get(1).unwrap().as_str();
        let version = Version::new(version_str)?;
        Ok(LowerBound::new(version, true))
    }

    fn parse_upper_from_captures(captures: &regex::Captures) -> Result<UpperBound, RezError> {
        let prefix = captures.get(1).unwrap().as_str();
        let version_str = captures.get(2).map(|m| m.as_str());

        let version = if let Some(v) = version_str {
            if !v.is_empty() {
                Version::new(v)?
            } else {
                Version::empty()
            }
        } else {
            Version::empty()
        };

        let inclusive = prefix == "<=";
        UpperBound::new(version, inclusive)
    }
}

/// Version range representing a union of bounds.
///
/// Examples:
/// - `VersionRange::new("")` - any version
/// - `VersionRange::new("1.2+")` - >= 1.2
/// - `VersionRange::new("1.0..2.0")` - [1.0, 2.0]
/// - `VersionRange::new("1.2+|3.0+")` - >= 1.2 OR >= 3.0
#[derive(Clone, Debug)]
pub struct VersionRange {
    bounds: Vec<Bound>,
    str_cache: OnceCell<String>,
}

impl VersionRange {
    /// Create a new version range from a string.
    ///
    /// # Arguments
    /// * `range_str` - Range string like "1.2+", ">=1.0,<2.0", "1.0|2.0|3.0"
    ///
    /// # Returns
    /// * `Ok(VersionRange)` on success
    /// * `Err(RezError)` if parsing fails
    pub fn new(range_str: &str) -> Result<Self, RezError> {
        let mut bounds = VersionRangeParser::parse(range_str)?;

        // Union and sort bounds
        Self::union_bounds(&mut bounds);

        Ok(Self {
            bounds,
            str_cache: OnceCell::new(),
        })
    }

    /// Create a version range from a list of bounds.
    ///
    /// # Arguments
    /// * `bounds` - List of bounds
    ///
    /// # Returns
    /// New VersionRange with unified bounds
    pub fn from_bounds(mut bounds: Vec<Bound>) -> Self {
        Self::union_bounds(&mut bounds);

        Self {
            bounds,
            str_cache: OnceCell::new(),
        }
    }

    /// Create an "any" version range (no restrictions).
    pub fn any() -> Self {
        Self {
            bounds: vec![Bound::any().clone()],
            str_cache: OnceCell::new(),
        }
    }

    /// Check if this is the "any" range (no restrictions).
    pub fn is_any(&self) -> bool {
        self.bounds.len() == 1 && &self.bounds[0] == Bound::any()
    }

    /// Check if range has a lower bound (not infinite lower).
    pub fn lower_bounded(&self) -> bool {
        self.bounds.iter().any(|b| b.lower_bounded())
    }

    /// Check if range has an upper bound (not infinite upper).
    pub fn upper_bounded(&self) -> bool {
        self.bounds.iter().any(|b| b.upper_bounded())
    }

    /// Check if range has both lower and upper bounds.
    pub fn bounded(&self) -> bool {
        self.lower_bounded() && self.upper_bounded()
    }

    /// Check if this range is a superset of another range.
    ///
    /// # Arguments
    /// * `other` - Range to check
    ///
    /// # Returns
    /// `true` if self contains all versions in other
    pub fn issuperset(&self, other: &VersionRange) -> bool {
        Self::is_superset_bounds(&self.bounds, &other.bounds)
    }

    /// Check if this range is a subset of another range.
    ///
    /// # Arguments
    /// * `other` - Range to check
    ///
    /// # Returns
    /// `true` if other contains all versions in self
    pub fn issubset(&self, other: &VersionRange) -> bool {
        other.issuperset(self)
    }

    /// Create union of this range with another range or list of ranges.
    ///
    /// # Arguments
    /// * `other` - Single range or vector of ranges
    ///
    /// # Returns
    /// New VersionRange representing the union
    pub fn union(&self, other: &VersionRange) -> VersionRange {
        let mut new_bounds = self.bounds.clone();
        new_bounds.extend(other.bounds.clone());
        Self::from_bounds(new_bounds)
    }

    /// Create intersection of this range with another range.
    ///
    /// # Arguments
    /// * `other` - Range to intersect with
    ///
    /// # Returns
    /// * `Some(VersionRange)` if ranges intersect
    /// * `None` if ranges do not intersect
    pub fn intersection(&self, other: &VersionRange) -> Option<VersionRange> {
        let new_bounds = Self::intersection_bounds(&self.bounds, &other.bounds);

        if new_bounds.is_empty() {
            None
        } else {
            Some(Self::from_bounds(new_bounds))
        }
    }

    /// Create the inverse (NOT) of this range.
    ///
    /// # Returns
    /// * `Some(VersionRange)` - inverted range
    /// * `None` - if range is "any" (inverse of any is empty, which is invalid)
    pub fn inverse(&self) -> Option<VersionRange> {
        if self.is_any() {
            return None;
        }

        let inverted_bounds = Self::inverse_bounds(&self.bounds);

        if inverted_bounds.is_empty() {
            None
        } else {
            Some(Self::from_bounds(inverted_bounds))
        }
    }

    /// Check if this range intersects with another range.
    ///
    /// # Arguments
    /// * `other` - Range to check
    ///
    /// # Returns
    /// `true` if ranges have any common versions
    pub fn intersects(&self, other: &VersionRange) -> bool {
        Self::intersects_bounds(&self.bounds, &other.bounds)
    }

    /// Compare ranges using the supplied version ordering.
    ///
    /// This follows Rez package-order keys: bounds compare lexicographically,
    /// lower bounds precede upper bounds, and inclusivity breaks equal-version
    /// ties in the same direction as the corresponding bound types.
    pub fn cmp_by(
        &self,
        other: &Self,
        mut compare_versions: impl FnMut(&Version, &Version) -> Ordering,
    ) -> Ordering {
        for (left, right) in self.bounds.iter().zip(&other.bounds) {
            let lower = compare_versions(&left.lower.version, &right.lower.version)
                .then_with(|| right.lower.inclusive.cmp(&left.lower.inclusive));
            if lower != Ordering::Equal {
                return lower;
            }
            let upper = compare_versions(&left.upper.version, &right.upper.version)
                .then_with(|| left.upper.inclusive.cmp(&right.upper.inclusive));
            if upper != Ordering::Equal {
                return upper;
            }
        }
        self.bounds.len().cmp(&other.bounds.len())
    }

    /// Split this range into individual contiguous sub-ranges.
    ///
    /// # Returns
    /// Vector of VersionRange, each representing a single bound
    pub fn split(&self) -> Vec<VersionRange> {
        self.bounds
            .iter()
            .map(|b| VersionRange {
                bounds: vec![b.clone()],
                str_cache: OnceCell::new(),
            })
            .collect()
    }

    /// Create a version range from a span of versions.
    ///
    /// # Arguments
    /// * `lower_version` - Lower bound version (None = empty)
    /// * `upper_version` - Upper bound version (None = infinity)
    /// * `lower_inclusive` - Include lower version
    /// * `upper_inclusive` - Include upper version
    ///
    /// # Returns
    /// New VersionRange
    pub fn as_span(
        lower_version: Option<Version>,
        upper_version: Option<Version>,
        lower_inclusive: bool,
        upper_inclusive: bool,
    ) -> Result<VersionRange, RezError> {
        let lower = lower_version.map(|v| LowerBound::new(v, lower_inclusive));
        let upper = upper_version
            .map(|v| UpperBound::new(v, upper_inclusive))
            .transpose()?;

        let bound = Bound::new(lower, upper, true)?;
        Ok(VersionRange {
            bounds: vec![bound],
            str_cache: OnceCell::new(),
        })
    }

    /// Create a version range from a single version and operator.
    ///
    /// # Arguments
    /// * `version` - Version to use
    /// * `op` - Comparison operator; None selects the version prefix and its descendants.
    ///
    /// # Returns
    /// * `Ok(VersionRange)` on success
    /// * `Err(RezError)` for invalid operator
    pub fn from_version(version: &Version, op: Option<&str>) -> Result<VersionRange, RezError> {
        let bound = match op {
            None => {
                let lower = LowerBound::new(version.clone(), true);
                let upper = UpperBound::new(version.next(), false)?;
                Bound::new(Some(lower), Some(upper), true)?
            }
            Some("gt" | ">") => {
                let lower = LowerBound::new(version.clone(), false);
                Bound::new(Some(lower), None, true)?
            }
            Some("gte" | ">=") => {
                let lower = LowerBound::new(version.clone(), true);
                Bound::new(Some(lower), None, true)?
            }
            Some("lt" | "<") => {
                let upper = UpperBound::new(version.clone(), false)?;
                Bound::new(None, Some(upper), true)?
            }
            Some("lte" | "<=") => {
                let upper = UpperBound::new(version.clone(), true)?;
                Bound::new(None, Some(upper), true)?
            }
            Some("eq" | "==") => {
                let lower = LowerBound::new(version.clone(), true);
                let upper = UpperBound::new(version.clone(), true)?;
                Bound::new(Some(lower), Some(upper), true)?
            }
            Some(op) => {
                return Err(RezError::Version(format!("Invalid operator: '{}'", op)));
            }
        };

        Ok(VersionRange {
            bounds: vec![bound],
            str_cache: OnceCell::new(),
        })
    }

    /// Create a version range from a list of versions (union of exact matches).
    ///
    /// # Arguments
    /// * `versions` - List of versions
    ///
    /// # Returns
    /// VersionRange representing "==v1|==v2|..."
    pub fn from_versions(versions: &[Version]) -> Result<VersionRange, RezError> {
        if versions.is_empty() {
            return Ok(Self::any());
        }

        let mut bounds = Vec::new();
        for version in versions {
            let lower = LowerBound::new(version.clone(), true);
            let upper = UpperBound::new(version.clone(), true)?;
            let bound = Bound::new(Some(lower), Some(upper), true)?;
            bounds.push(bound);
        }

        Ok(Self::from_bounds(bounds))
    }

    /// Extract exact versions if this range is a union of exact version matches.
    ///
    /// # Returns
    /// * `Some(Vec<Version>)` if range is "==v1|==v2|..."
    /// * `None` if range contains non-exact bounds
    pub fn to_versions(&self) -> Option<Vec<Version>> {
        let mut versions = Vec::new();

        for bound in &self.bounds {
            // Check if bound is exact (lower == upper, both inclusive)
            if bound.lower.version == bound.upper.version
                && bound.lower.inclusive
                && bound.upper.inclusive
            {
                versions.push(bound.lower.version.clone());
            } else {
                // Not an exact bound
                return None;
            }
        }

        Some(versions)
    }

    /// Check if this range contains a specific version.
    ///
    /// # Arguments
    /// * `version` - Version to check
    ///
    /// # Returns
    /// `true` if version is in range
    pub fn contains_version(&self, version: &Version) -> bool {
        Self::contains_version_bounds(&self.bounds, version)
    }

    /// Get the smallest contiguous range that contains this range.
    ///
    /// # Returns
    /// VersionRange representing the span from min to max
    pub fn span(&self) -> VersionRange {
        if self.bounds.is_empty() || self.is_any() {
            return Self::any();
        }

        // Find min lower and max upper
        let min_lower = self.bounds.iter().map(|b| &b.lower).min().unwrap();
        let max_upper = self.bounds.iter().map(|b| &b.upper).max().unwrap();

        let bound = Bound::new(Some(min_lower.clone()), Some(max_upper.clone()), false)
            .expect("Span bound should be valid");

        VersionRange {
            bounds: vec![bound],
            str_cache: OnceCell::new(),
        }
    }

    /// Apply a function to all versions in bounds.
    ///
    /// # Arguments
    /// * `func` - Function to apply to each version
    ///
    /// # Returns
    /// New VersionRange with transformed versions
    pub fn visit_versions<F>(&self, func: F) -> Result<VersionRange, RezError>
    where
        F: Fn(&Version) -> Result<Version, RezError>,
    {
        let mut new_bounds = Vec::new();

        for bound in &self.bounds {
            let new_lower_version = func(&bound.lower.version)?;
            let new_upper_version = func(&bound.upper.version)?;

            let new_lower = LowerBound::new(new_lower_version, bound.lower.inclusive);
            let new_upper = UpperBound::new(new_upper_version, bound.upper.inclusive)?;
            let new_bound = Bound::new(Some(new_lower), Some(new_upper), true)?;

            new_bounds.push(new_bound);
        }

        Ok(Self::from_bounds(new_bounds))
    }

    /// Get number of bounds in this range.
    pub fn len(&self) -> usize {
        self.bounds.len()
    }

    /// Check if range is empty (should not happen for valid ranges).
    pub fn is_empty(&self) -> bool {
        self.bounds.is_empty()
    }

    /// Create an iterator for testing version containment over sorted items.
    ///
    /// # Arguments
    /// * `iterable` - Sorted list of items
    /// * `key` - Function to extract version from item
    /// * `descending` - True if items are sorted descending
    /// * `mode` - INTERSECTING, NON_INTERSECTING, or ALL
    ///
    /// # Returns
    /// Iterator yielding items based on containment mode
    pub fn iter_intersect_test<'a, T, F>(
        &'a self,
        iterable: &'a [T],
        key: F,
        descending: bool,
        mode: ContainmentMode,
    ) -> ContainsVersionIter<'a, T, F>
    where
        F: Fn(&T) -> &Version,
    {
        ContainsVersionIter::new(&self.bounds, iterable, key, descending, mode)
    }

    /// Iterate over items that intersect this range.
    pub fn iter_intersecting<'a, T, F>(
        &'a self,
        iterable: &'a [T],
        key: F,
        descending: bool,
    ) -> ContainsVersionIter<'a, T, F>
    where
        F: Fn(&T) -> &Version,
    {
        self.iter_intersect_test(iterable, key, descending, ContainmentMode::Intersecting)
    }

    /// Iterate over items that do not intersect this range.
    pub fn iter_non_intersecting<'a, T, F>(
        &'a self,
        iterable: &'a [T],
        key: F,
        descending: bool,
    ) -> ContainsVersionIter<'a, T, F>
    where
        F: Fn(&T) -> &Version,
    {
        self.iter_intersect_test(iterable, key, descending, ContainmentMode::NonIntersecting)
    }

    // -- Internal helper methods --

    /// Union bounds (merge overlapping, sort).
    fn union_bounds(bounds: &mut Vec<Bound>) {
        if bounds.is_empty() {
            return;
        }

        // Sort bounds
        bounds.sort();

        // Merge overlapping bounds
        let mut merged = Vec::new();
        let mut current = bounds[0].clone();

        for next in bounds.iter().skip(1) {
            // Check if current and next overlap or are adjacent
            if current.intersects(next)
                || (current.upper.version == next.lower.version
                    && (current.upper.inclusive || next.lower.inclusive))
            {
                // Merge: take min lower, max upper
                let new_lower = std::cmp::min(&current.lower, &next.lower).clone();
                let new_upper = std::cmp::max(&current.upper, &next.upper).clone();
                current = Bound::new(Some(new_lower), Some(new_upper), false)
                    .expect("Merged bound should be valid");
            } else {
                // No overlap: save current and start new
                merged.push(current);
                current = next.clone();
            }
        }

        merged.push(current);
        *bounds = merged;
    }

    /// Intersection of two bound lists.
    fn intersection_bounds(bounds1: &[Bound], bounds2: &[Bound]) -> Vec<Bound> {
        let mut result = Vec::new();

        for b1 in bounds1 {
            for b2 in bounds2 {
                if let Some(intersection) = b1.intersection(b2) {
                    result.push(intersection);
                }
            }
        }

        // Sort and merge overlapping
        if !result.is_empty() {
            Self::union_bounds(&mut result);
        }

        result
    }

    /// Inverse of bound list.
    fn inverse_bounds(bounds: &[Bound]) -> Vec<Bound> {
        if bounds.is_empty() {
            return vec![Bound::any().clone()];
        }

        let mut result = Vec::new();

        // Sorted bounds assumed
        let first_bound = &bounds[0];

        // Gap before first bound (if lower is not minimum)
        if first_bound.lower.version > Version::empty() || !first_bound.lower.inclusive {
            let new_version = first_bound.lower.version.clone();
            let new_inclusive = !first_bound.lower.inclusive;
            let upper = UpperBound::new(new_version, new_inclusive).ok();
            if let Some(upper) = upper {
                let bound = Bound::new(None, Some(upper), false)
                    .expect("Inverse lower gap bound should be valid");
                result.push(bound);
            }
        }

        // Gaps between consecutive bounds
        for i in 0..bounds.len() - 1 {
            let current = &bounds[i];
            let next = &bounds[i + 1];

            // Gap from current.upper to next.lower
            let gap_lower_version = current.upper.version.clone();
            let gap_lower_inclusive = !current.upper.inclusive;

            let gap_upper_version = next.lower.version.clone();
            let gap_upper_inclusive = !next.lower.inclusive;

            // Only create gap if there's space between bounds
            if gap_lower_version < gap_upper_version
                || (gap_lower_version == gap_upper_version
                    && gap_lower_inclusive
                    && gap_upper_inclusive)
            {
                let lower = LowerBound::new(gap_lower_version, gap_lower_inclusive);
                let upper = UpperBound::new(gap_upper_version, gap_upper_inclusive).ok();
                if let Some(upper) = upper {
                    let bound = Bound::new(Some(lower), Some(upper), false)
                        .expect("Inverse gap bound should be valid");
                    result.push(bound);
                }
            }
        }

        // Gap after last bound (if upper is not infinity)
        let last_bound = &bounds[bounds.len() - 1];
        if !last_bound.upper.version.is_inf() {
            let new_version = last_bound.upper.version.clone();
            let new_inclusive = !last_bound.upper.inclusive;
            let lower = LowerBound::new(new_version, new_inclusive);
            let bound = Bound::new(Some(lower), None, false)
                .expect("Inverse upper gap bound should be valid");
            result.push(bound);
        }

        result
    }

    /// Check if bounds1 is a superset of bounds2.
    fn is_superset_bounds(bounds1: &[Bound], bounds2: &[Bound]) -> bool {
        // For each bound in bounds2, check if it's contained in some bound in bounds1
        for b2 in bounds2 {
            let mut contained = false;
            for b1 in bounds1 {
                if b1.contains_bound(b2) {
                    contained = true;
                    break;
                }
            }
            if !contained {
                return false;
            }
        }
        true
    }

    /// Check if two bound lists intersect.
    fn intersects_bounds(bounds1: &[Bound], bounds2: &[Bound]) -> bool {
        // Optimization: use binary search for large lists
        const BINARY_SEARCH_THRESHOLD: usize = 5;

        if bounds1.len() >= BINARY_SEARCH_THRESHOLD && bounds2.len() >= BINARY_SEARCH_THRESHOLD {
            // Use binary search approach
            for b1 in bounds1 {
                // Find potential intersecting bounds in bounds2
                for b2 in bounds2 {
                    if b1.intersects(b2) {
                        return true;
                    }
                }
            }
        } else {
            // Linear search for small lists
            for b1 in bounds1 {
                for b2 in bounds2 {
                    if b1.intersects(b2) {
                        return true;
                    }
                }
            }
        }

        false
    }

    /// Check if version is contained in bounds.
    fn contains_version_bounds(bounds: &[Bound], version: &Version) -> bool {
        // Optimization: use binary search for >=5 bounds
        if bounds.len() >= 5 {
            // Binary search: find first bound where lower <= version
            let idx = bounds.partition_point(|b| b.lower.version < *version);

            // Check bounds at and before the partition point
            if idx > 0 && bounds[idx - 1].contains_version(version) {
                return true;
            }
            if idx < bounds.len() && bounds[idx].contains_version(version) {
                return true;
            }

            false
        } else {
            // Linear search for small lists
            bounds.iter().any(|b| b.contains_version(version))
        }
    }
}

// -- Trait implementations for VersionRange --

impl PartialEq for VersionRange {
    fn eq(&self, other: &Self) -> bool {
        self.bounds == other.bounds
    }
}

impl Eq for VersionRange {}

impl PartialOrd for VersionRange {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for VersionRange {
    fn cmp(&self, other: &Self) -> Ordering {
        // Compare bounds lexicographically
        self.bounds.cmp(&other.bounds)
    }
}

impl Hash for VersionRange {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for bound in &self.bounds {
            bound.hash(state);
        }
    }
}

impl fmt::Display for VersionRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Use cached string if available
        let s = self.str_cache.get_or_init(|| {
            if self.is_any() {
                String::new()
            } else {
                self.bounds
                    .iter()
                    .map(|b| b.to_string())
                    .collect::<Vec<_>>()
                    .join("|")
            }
        });

        write!(f, "{}", s)
    }
}

// -- Operator overloads --

impl Not for VersionRange {
    type Output = Option<VersionRange>;

    fn not(self) -> Self::Output {
        self.inverse()
    }
}

impl BitAnd for VersionRange {
    type Output = Option<VersionRange>;

    fn bitand(self, rhs: Self) -> Self::Output {
        self.intersection(&rhs)
    }
}

impl BitOr for VersionRange {
    type Output = VersionRange;

    fn bitor(self, rhs: Self) -> Self::Output {
        self.union(&rhs)
    }
}

impl Add for VersionRange {
    type Output = VersionRange;

    fn add(self, rhs: Self) -> Self::Output {
        self.union(&rhs)
    }
}

impl Sub for VersionRange {
    type Output = Option<VersionRange>;

    fn sub(self, rhs: Self) -> Self::Output {
        if let Some(inv) = rhs.inverse() {
            self.intersection(&inv)
        } else {
            None
        }
    }
}

impl Not for &VersionRange {
    type Output = Option<VersionRange>;

    fn not(self) -> Self::Output {
        self.inverse()
    }
}

impl BitAnd for &VersionRange {
    type Output = Option<VersionRange>;

    fn bitand(self, rhs: Self) -> Self::Output {
        self.intersection(rhs)
    }
}

impl BitOr for &VersionRange {
    type Output = VersionRange;

    fn bitor(self, rhs: Self) -> Self::Output {
        self.union(rhs)
    }
}

impl Add for &VersionRange {
    type Output = VersionRange;

    fn add(self, rhs: Self) -> Self::Output {
        self.union(rhs)
    }
}

impl Sub for &VersionRange {
    type Output = Option<VersionRange>;

    fn sub(self, rhs: Self) -> Self::Output {
        // Subtraction: self & !rhs
        if let Some(inv) = rhs.inverse() {
            self.intersection(&inv)
        } else {
            // rhs is "any", so self - any = None
            None
        }
    }
}

// -- Containment mode for iterator --

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainmentMode {
    Intersecting,
    NonIntersecting,
    All,
}

/// Iterator for testing version containment over sorted items.
/// Port of Python _ContainsVersionIterator class.
pub struct ContainsVersionIter<'a, T, F>
where
    F: Fn(&T) -> &Version,
{
    bounds: &'a [Bound],
    items: &'a [T],
    key_fn: F,
    descending: bool,
    mode: ContainmentMode,
    index: usize,
    constant_result: Option<bool>,
}

impl<'a, T, F> ContainsVersionIter<'a, T, F>
where
    F: Fn(&T) -> &Version,
{
    fn new(
        bounds: &'a [Bound],
        items: &'a [T],
        key_fn: F,
        descending: bool,
        mode: ContainmentMode,
    ) -> Self {
        // Check for constant result optimization
        let constant_result = if bounds.len() == 1 {
            let bound = &bounds[0];
            if bound == Bound::any() {
                // Any range: all versions match
                Some(mode != ContainmentMode::NonIntersecting)
            } else if !bound.lower_bounded() && !bound.upper_bounded() {
                // Unbounded: all versions match
                Some(mode != ContainmentMode::NonIntersecting)
            } else {
                None
            }
        } else {
            None
        };

        Self {
            bounds,
            items,
            key_fn,
            descending,
            mode,
            index: 0,
            constant_result,
        }
    }

    fn ascending_next(&mut self) -> Option<&'a T> {
        while self.index < self.items.len() {
            let item = &self.items[self.index];
            let version = (self.key_fn)(item);
            self.index += 1;

            // Check containment
            let contained = self.check_version_contained(version);

            match self.mode {
                ContainmentMode::All => return Some(item),
                ContainmentMode::Intersecting if contained => return Some(item),
                ContainmentMode::NonIntersecting if !contained => return Some(item),
                _ => continue,
            }
        }

        None
    }

    fn descending_next(&mut self) -> Option<&'a T> {
        while self.index < self.items.len() {
            let item = &self.items[self.index];
            let version = (self.key_fn)(item);
            self.index += 1;

            // Check containment (same as ascending)
            let contained = self.check_version_contained(version);

            match self.mode {
                ContainmentMode::All => return Some(item),
                ContainmentMode::Intersecting if contained => return Some(item),
                ContainmentMode::NonIntersecting if !contained => return Some(item),
                _ => continue,
            }
        }

        None
    }

    fn check_version_contained(&self, version: &Version) -> bool {
        // Fast path: constant result
        if let Some(result) = self.constant_result {
            return result;
        }

        // Linear search through bounds (could optimize with binary search)
        for bound in self.bounds {
            if bound.contains_version(version) {
                return true;
            }
        }

        false
    }
}

impl<'a, T, F> Iterator for ContainsVersionIter<'a, T, F>
where
    F: Fn(&T) -> &Version,
{
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.descending {
            self.descending_next()
        } else {
            self.ascending_next()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty() {
        let range = VersionRange::new("").unwrap();
        assert!(range.is_any());
    }

    #[test]
    fn test_parse_simple_version() {
        let range = VersionRange::new("1.2").unwrap();
        assert_eq!(range.to_string(), "1.2");
        assert!(range.contains_version(&Version::new("1.2").unwrap()));
        assert!(range.contains_version(&Version::new("1.2.99").unwrap()));
        assert!(!range.contains_version(&Version::new("1.2_").unwrap()));
        assert!(!range.contains_version(&Version::new("1.3").unwrap()));
        assert!(!range.contains_version(&Version::new("2.0").unwrap()));
        assert!(!range.contains_version(&Version::new("1.1").unwrap()));
    }

    #[test]
    fn test_prefix_ranges_match_rez_source_cases() {
        // Rez's _act_version uses [version, version.next()), not an open lower bound.
        // Source: upstream Rez 3.3.0, src/rez/version/_version.py:737-742.
        for (expression, accepted, rejected) in [
            (
                "1.2",
                vec!["1.2", "1.2.0", "1.2.99"],
                vec!["1.1", "1.2_", "1.3", "2.0"],
            ),
            ("3", vec!["3", "3.0", "3.99"], vec!["2.9", "3a", "3_", "4"]),
            ("_", vec!["_", "_.1"], vec!["__", "a"]),
            (
                "1.2|2.0",
                vec!["1.2", "1.2.8", "2.0", "2.0.5"],
                vec!["1.3", "2.1"],
            ),
        ] {
            let range = VersionRange::new(expression).unwrap();
            assert_eq!(range.to_string(), expression);
            assert_eq!(VersionRange::new(&range.to_string()).unwrap(), range);
            for version in accepted {
                assert!(
                    range.contains_version(&Version::new(version).unwrap()),
                    "{expression} excludes {version}"
                );
            }
            for version in rejected {
                assert!(
                    !range.contains_version(&Version::new(version).unwrap()),
                    "{expression} includes {version}"
                );
            }
        }
        for (prefix, equivalent) in [("3", "3+<3_"), ("_", "_+<__"), ("1.2", "1.2+<1.2_")] {
            assert_eq!(
                VersionRange::new(prefix).unwrap(),
                VersionRange::new(equivalent).unwrap()
            );
        }
    }

    #[test]
    fn test_prefix_factory_distinguishes_prefix_lower_bound_and_exact_version() {
        let version = Version::new("1.2").unwrap();
        let prefix = VersionRange::from_version(&version, None).unwrap();
        let lower = VersionRange::from_version(&version, Some(">=")).unwrap();
        let exact = VersionRange::from_version(&version, Some("==")).unwrap();
        assert_eq!(prefix, VersionRange::new("1.2").unwrap());
        assert_eq!(prefix.to_string(), "1.2");
        assert_eq!(lower.to_string(), "1.2+");
        assert_eq!(exact.to_string(), "==1.2");
        assert!(prefix.contains_version(&Version::new("1.2.1").unwrap()));
        assert!(!exact.contains_version(&Version::new("1.2.1").unwrap()));
        assert!(lower.contains_version(&Version::new("2.0").unwrap()));
        assert!(!prefix.contains_version(&Version::new("2.0").unwrap()));
    }

    #[test]
    fn test_parse_exact_version() {
        let range = VersionRange::new("==1.2").unwrap();
        assert_eq!(range.to_string(), "==1.2");
        assert!(range.contains_version(&Version::new("1.2").unwrap()));
        assert!(!range.contains_version(&Version::new("1.3").unwrap()));
        assert!(!range.contains_version(&Version::new("1.1").unwrap()));
    }

    #[test]
    fn test_parse_inclusive_range() {
        let range = VersionRange::new("1.0..2.0").unwrap();
        assert_eq!(range.to_string(), "1.0..2.0");
        assert!(range.contains_version(&Version::new("1.0").unwrap()));
        assert!(range.contains_version(&Version::new("1.5").unwrap()));
        assert!(range.contains_version(&Version::new("2.0").unwrap()));
        assert!(!range.contains_version(&Version::new("0.9").unwrap()));
        assert!(!range.contains_version(&Version::new("2.1").unwrap()));
    }

    #[test]
    fn test_parse_lower_bound() {
        let range1 = VersionRange::new("1.2+").unwrap();
        assert_eq!(range1.to_string(), "1.2+");

        let range2 = VersionRange::new(">1.2").unwrap();
        assert!(!range2.contains_version(&Version::new("1.2").unwrap()));
        assert!(range2.contains_version(&Version::new("1.3").unwrap()));

        let range3 = VersionRange::new(">=1.2").unwrap();
        assert!(range3.contains_version(&Version::new("1.2").unwrap()));
    }

    #[test]
    fn test_parse_upper_bound() {
        let range1 = VersionRange::new("<2.0").unwrap();
        assert!(!range1.contains_version(&Version::new("2.0").unwrap()));
        assert!(range1.contains_version(&Version::new("1.9").unwrap()));

        let range2 = VersionRange::new("<=2.0").unwrap();
        assert!(range2.contains_version(&Version::new("2.0").unwrap()));
    }

    #[test]
    fn test_parse_range_asc() {
        let range = VersionRange::new(">=1.0,<2.0").unwrap();
        assert!(range.contains_version(&Version::new("1.0").unwrap()));
        assert!(range.contains_version(&Version::new("1.5").unwrap()));
        assert!(!range.contains_version(&Version::new("2.0").unwrap()));
    }

    #[test]
    fn test_parse_range_desc() {
        let range = VersionRange::new("<2.0,>=1.0").unwrap();
        assert!(range.contains_version(&Version::new("1.0").unwrap()));
        assert!(range.contains_version(&Version::new("1.5").unwrap()));
        assert!(!range.contains_version(&Version::new("2.0").unwrap()));
    }

    #[test]
    fn test_parse_union() {
        let range = VersionRange::new("==1.0|==2.0|==3.0").unwrap();
        assert!(range.contains_version(&Version::new("1.0").unwrap()));
        assert!(!range.contains_version(&Version::new("1.5").unwrap()));
        assert!(range.contains_version(&Version::new("2.0").unwrap()));
        assert!(range.contains_version(&Version::new("3.0").unwrap()));
    }

    #[test]
    fn test_is_any() {
        let range1 = VersionRange::new("").unwrap();
        assert!(range1.is_any());

        let range2 = VersionRange::new("1.0+").unwrap();
        assert!(!range2.is_any());
    }

    #[test]
    fn test_bounded() {
        let range1 = VersionRange::new("1.0+").unwrap();
        assert!(range1.lower_bounded());
        assert!(!range1.upper_bounded());
        assert!(!range1.bounded());

        let range2 = VersionRange::new("1.0..2.0").unwrap();
        assert!(range2.lower_bounded());
        assert!(range2.upper_bounded());
        assert!(range2.bounded());
    }

    #[test]
    fn test_union() {
        let range1 = VersionRange::new("1.0..2.0").unwrap();
        let range2 = VersionRange::new("3.0..4.0").unwrap();
        let union = range1.union(&range2);

        assert!(union.contains_version(&Version::new("1.5").unwrap()));
        assert!(!union.contains_version(&Version::new("2.5").unwrap()));
        assert!(union.contains_version(&Version::new("3.5").unwrap()));
    }

    #[test]
    fn test_union_overlapping() {
        let range1 = VersionRange::new("1.0..3.0").unwrap();
        let range2 = VersionRange::new("2.0..4.0").unwrap();
        let union = range1.union(&range2);

        // Should merge into single bound [1.0, 4.0]
        assert_eq!(union.len(), 1);
        assert!(union.contains_version(&Version::new("1.0").unwrap()));
        assert!(union.contains_version(&Version::new("2.5").unwrap()));
        assert!(union.contains_version(&Version::new("4.0").unwrap()));
    }

    #[test]
    fn test_intersection() {
        let range1 = VersionRange::new("1.0..3.0").unwrap();
        let range2 = VersionRange::new("2.0..4.0").unwrap();
        let inter = range1.intersection(&range2).unwrap();

        assert_eq!(inter.to_string(), "2.0..3.0");
        assert!(!inter.contains_version(&Version::new("1.5").unwrap()));
        assert!(inter.contains_version(&Version::new("2.5").unwrap()));
        assert!(!inter.contains_version(&Version::new("3.5").unwrap()));
    }

    #[test]
    fn test_intersection_none() {
        let range1 = VersionRange::new("1.0..2.0").unwrap();
        let range2 = VersionRange::new("3.0..4.0").unwrap();
        let inter = range1.intersection(&range2);

        assert!(inter.is_none());
    }

    #[test]
    fn test_inverse() {
        let range = VersionRange::new("1.0..2.0").unwrap();
        let inv = range.inverse().unwrap();

        assert!(!inv.contains_version(&Version::new("1.5").unwrap()));
        assert!(inv.contains_version(&Version::new("0.5").unwrap()));
        assert!(inv.contains_version(&Version::new("2.5").unwrap()));
    }

    #[test]
    fn test_inverse_any() {
        let range = VersionRange::new("").unwrap();
        let inv = range.inverse();

        assert!(inv.is_none());
    }

    #[test]
    fn test_intersects() {
        let range1 = VersionRange::new("1.0..3.0").unwrap();
        let range2 = VersionRange::new("2.0..4.0").unwrap();
        assert!(range1.intersects(&range2));

        let range3 = VersionRange::new("5.0..6.0").unwrap();
        assert!(!range1.intersects(&range3));
    }

    #[test]
    fn test_split() {
        let range = VersionRange::new("==1.0|==2.0|==3.0").unwrap();
        let split = range.split();

        assert_eq!(split.len(), 3);
        assert_eq!(split[0].to_string(), "==1.0");
        assert_eq!(split[1].to_string(), "==2.0");
        assert_eq!(split[2].to_string(), "==3.0");
    }

    #[test]
    fn test_as_span() {
        let range = VersionRange::as_span(
            Some(Version::new("1.0").unwrap()),
            Some(Version::new("2.0").unwrap()),
            true,
            true,
        )
        .unwrap();

        assert_eq!(range.to_string(), "1.0..2.0");
    }

    #[test]
    fn test_from_version() {
        let v = Version::new("1.2").unwrap();

        let r1 = VersionRange::from_version(&v, Some(">=")).unwrap();
        assert_eq!(r1.to_string(), "1.2+");

        let r2 = VersionRange::from_version(&v, Some(">")).unwrap();
        assert!(!r2.contains_version(&v));
        assert!(r2.contains_version(&Version::new("1.3").unwrap()));

        let r3 = VersionRange::from_version(&v, Some("==")).unwrap();
        assert_eq!(r3.to_string(), "==1.2");
    }

    #[test]
    fn test_from_versions() {
        let versions = vec![
            Version::new("1.0").unwrap(),
            Version::new("2.0").unwrap(),
            Version::new("3.0").unwrap(),
        ];

        let range = VersionRange::from_versions(&versions).unwrap();
        assert!(range.contains_version(&Version::new("1.0").unwrap()));
        assert!(!range.contains_version(&Version::new("1.5").unwrap()));
        assert!(range.contains_version(&Version::new("2.0").unwrap()));
    }

    #[test]
    fn test_to_versions() {
        let range = VersionRange::new("==1.0|==2.0").unwrap();
        let versions = range.to_versions().unwrap();

        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0], Version::new("1.0").unwrap());
        assert_eq!(versions[1], Version::new("2.0").unwrap());

        let range2 = VersionRange::new("1.0..2.0").unwrap();
        assert!(range2.to_versions().is_none());
    }

    #[test]
    fn test_span() {
        let range = VersionRange::new("==1.0|==3.0|==5.0").unwrap();
        let span = range.span();

        assert_eq!(span.len(), 1);
        assert!(span.contains_version(&Version::new("1.0").unwrap()));
        assert!(span.contains_version(&Version::new("2.0").unwrap()));
        assert!(span.contains_version(&Version::new("5.0").unwrap()));
    }

    #[test]
    fn test_visit_versions() {
        let range = VersionRange::new("1.0..2.0").unwrap();
        let new_range = range
            .visit_versions(|v| {
                if v.is_inf() {
                    Ok(v.clone())
                } else {
                    Version::new(&format!("{}.0", v))
                }
            })
            .unwrap();

        // Should transform bounds
        assert!(new_range.contains_version(&Version::new("1.0.0").unwrap()));
    }

    #[test]
    fn test_superset_subset() {
        let range1 = VersionRange::new("1.0..3.0").unwrap();
        let range2 = VersionRange::new("1.5..2.5").unwrap();

        assert!(range1.issuperset(&range2));
        assert!(!range2.issuperset(&range1));
        assert!(range2.issubset(&range1));
    }

    #[test]
    fn test_operators() {
        let r1 = VersionRange::new("1.0..2.0").unwrap();
        let r2 = VersionRange::new("1.5..3.0").unwrap();

        // Union (|)
        let union = &r1 | &r2;
        assert_eq!(union.len(), 1);

        // Intersection (&)
        let inter = (&r1 & &r2).unwrap();
        assert_eq!(inter.to_string(), "1.5..2.0");

        // Inverse (!)
        let inv = !&r1;
        assert!(inv.is_some());

        // Subtraction (-)
        let sub = &r1 - &r2;
        assert!(sub.is_some());
    }

    #[test]
    fn test_contains_version_optimization() {
        // Test with < 5 bounds (linear search)
        let range1 = VersionRange::new("==1.0|==2.0|==3.0").unwrap();
        assert!(range1.contains_version(&Version::new("2.0").unwrap()));
        assert!(!range1.contains_version(&Version::new("2.5").unwrap()));

        // Test with >= 5 bounds (binary search)
        let range2 = VersionRange::new("==1.0|==2.0|==3.0|==4.0|==5.0").unwrap();
        assert!(range2.contains_version(&Version::new("3.0").unwrap()));
        assert!(!range2.contains_version(&Version::new("3.5").unwrap()));
    }

    #[test]
    fn test_iterator_intersecting() {
        let range = VersionRange::new("2.0..4.0").unwrap();
        let versions = vec![
            Version::new("1.0").unwrap(),
            Version::new("2.0").unwrap(),
            Version::new("3.0").unwrap(),
            Version::new("4.0").unwrap(),
            Version::new("5.0").unwrap(),
        ];

        let intersecting: Vec<_> = range.iter_intersecting(&versions, |v| v, false).collect();

        assert_eq!(intersecting.len(), 3);
        assert_eq!(intersecting[0], &versions[1]); // 2.0
        assert_eq!(intersecting[1], &versions[2]); // 3.0
        assert_eq!(intersecting[2], &versions[3]); // 4.0
    }

    #[test]
    fn test_iterator_non_intersecting() {
        let range = VersionRange::new("2.0..4.0").unwrap();
        let versions = vec![
            Version::new("1.0").unwrap(),
            Version::new("2.0").unwrap(),
            Version::new("3.0").unwrap(),
            Version::new("4.0").unwrap(),
            Version::new("5.0").unwrap(),
        ];

        let non_intersecting: Vec<_> = range
            .iter_non_intersecting(&versions, |v| v, false)
            .collect();

        assert_eq!(non_intersecting.len(), 2);
        assert_eq!(non_intersecting[0], &versions[0]); // 1.0
        assert_eq!(non_intersecting[1], &versions[4]); // 5.0
    }

    #[test]
    #[allow(clippy::mutable_key_type)]
    fn test_hash() {
        use std::collections::HashSet;

        let r1 = VersionRange::new("1.0..2.0").unwrap();
        let r2 = VersionRange::new("1.0..2.0").unwrap();

        let mut set = HashSet::new();
        set.insert(r1);
        assert!(set.contains(&r2));
    }

    #[test]
    fn test_ordering() {
        let r1 = VersionRange::new("1.0..2.0").unwrap();
        let r2 = VersionRange::new("2.0..3.0").unwrap();

        assert!(r1 < r2);
    }

    #[test]
    fn test_display_caching() {
        let range = VersionRange::new("1.0..2.0").unwrap();
        let s1 = range.to_string();
        let s2 = range.to_string();

        // Both should return the same cached string
        assert_eq!(s1, s2);
        assert_eq!(s1, "1.0..2.0");
    }
}
