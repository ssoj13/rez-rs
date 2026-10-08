// SPDX-License-Identifier: Apache-2.0

//! Version bounds - lower and upper bounds for version ranges.
//!
//! Ported from Python rez _version.py.

use super::version::Version;
use foundation::errors::RezError;
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::LazyLock;

/// Lower bound of a version range.
///
/// Examples:
/// - `LowerBound::new(Version::empty(), true)` = minimum bound (>= empty)
/// - `LowerBound::new(Version::new("1.2"), true)` = "1.2+" (inclusive)
/// - `LowerBound::new(Version::new("1.2"), false)` = ">1.2" (exclusive)
#[derive(Clone, Debug)]
pub struct LowerBound {
    pub version: Version,
    pub inclusive: bool,
}

impl LowerBound {
    pub fn new(version: Version, inclusive: bool) -> Self {
        Self { version, inclusive }
    }

    /// Minimum lower bound (>= empty version)
    pub fn min() -> &'static LowerBound {
        &MIN_LOWER_BOUND
    }

    /// Check if this bound contains the given version
    pub fn contains_version(&self, version: &Version) -> bool {
        version > &self.version || (self.inclusive && version == &self.version)
    }
}

static MIN_LOWER_BOUND: LazyLock<LowerBound> =
    LazyLock::new(|| LowerBound::new(Version::empty(), true));

impl PartialEq for LowerBound {
    fn eq(&self, other: &Self) -> bool {
        self.version == other.version && self.inclusive == other.inclusive
    }
}

impl Eq for LowerBound {}

impl PartialOrd for LowerBound {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for LowerBound {
    fn cmp(&self, other: &Self) -> Ordering {
        // First compare versions
        match self.version.cmp(&other.version) {
            Ordering::Less => Ordering::Less,
            Ordering::Greater => Ordering::Greater,
            // If versions equal, inclusive < exclusive (for lower bounds)
            Ordering::Equal => {
                if self.inclusive == other.inclusive {
                    Ordering::Equal
                } else if self.inclusive && !other.inclusive {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
        }
    }
}

impl Hash for LowerBound {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.version.hash(state);
        self.inclusive.hash(state);
    }
}

impl fmt::Display for LowerBound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.version.is_truthy() {
            if self.inclusive {
                write!(f, "{}+", self.version)
            } else {
                write!(f, ">{}", self.version)
            }
        } else {
            // Empty version
            if self.inclusive {
                write!(f, "")
            } else {
                write!(f, ">")
            }
        }
    }
}

/// Upper bound of a version range.
///
/// Examples:
/// - `UpperBound::new(Version::inf(), true)` = infinity bound (<= inf)
/// - `UpperBound::new(Version::new("2.0"), true)` = "<=2.0" (inclusive)
/// - `UpperBound::new(Version::new("2.0"), false)` = "<2.0" (exclusive)
#[derive(Clone, Debug)]
pub struct UpperBound {
    pub version: Version,
    pub inclusive: bool,
}

impl UpperBound {
    pub fn new(version: Version, inclusive: bool) -> Result<Self, RezError> {
        // Invalid: empty version with exclusive bound
        if !version.is_truthy() && !inclusive {
            return Err(RezError::Version(format!(
                "Invalid upper bound: '<{}'",
                version
            )));
        }
        Ok(Self { version, inclusive })
    }

    /// Infinity upper bound (<= inf)
    pub fn inf() -> &'static UpperBound {
        &INF_UPPER_BOUND
    }

    /// Check if this bound contains the given version
    pub fn contains_version(&self, version: &Version) -> bool {
        version < &self.version || (self.inclusive && version == &self.version)
    }
}

static INF_UPPER_BOUND: LazyLock<UpperBound> =
    LazyLock::new(|| UpperBound::new(Version::inf(), true).unwrap());

impl PartialEq for UpperBound {
    fn eq(&self, other: &Self) -> bool {
        self.version == other.version && self.inclusive == other.inclusive
    }
}

impl Eq for UpperBound {}

impl PartialOrd for UpperBound {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for UpperBound {
    fn cmp(&self, other: &Self) -> Ordering {
        // First compare versions
        match self.version.cmp(&other.version) {
            Ordering::Less => Ordering::Less,
            Ordering::Greater => Ordering::Greater,
            // If versions equal, exclusive < inclusive (for upper bounds)
            Ordering::Equal => {
                if self.inclusive == other.inclusive {
                    Ordering::Equal
                } else if !self.inclusive && other.inclusive {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
        }
    }
}

impl Hash for UpperBound {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.version.hash(state);
        self.inclusive.hash(state);
    }
}

impl fmt::Display for UpperBound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.inclusive {
            write!(f, "<={}", self.version)
        } else {
            write!(f, "<{}", self.version)
        }
    }
}

/// A version bound with both lower and upper limits.
///
/// Examples:
/// - `Bound::any()` = any version (>= empty, <= inf)
/// - `Bound::new(Some(lower), Some(upper), true)` = bounded range
/// - `Bound::new(None, None, true)` = any version (uses defaults)
#[derive(Clone, Debug)]
pub struct Bound {
    pub lower: LowerBound,
    pub upper: UpperBound,
}

impl Bound {
    /// Create a new bound with optional lower/upper bounds.
    ///
    /// # Arguments
    /// * `lower` - Lower bound (None = minimum)
    /// * `upper` - Upper bound (None = infinity)
    /// * `invalid_bound_error` - If true, validate bound consistency
    ///
    /// # Returns
    /// * `Ok(Bound)` if valid
    /// * `Err(RezError)` if invalid_bound_error=true and bounds are inconsistent
    pub fn new(
        lower: Option<LowerBound>,
        upper: Option<UpperBound>,
        invalid_bound_error: bool,
    ) -> Result<Self, RezError> {
        let lower = lower.unwrap_or_else(|| LowerBound::min().clone());
        let upper = upper.unwrap_or_else(|| UpperBound::inf().clone());

        // Validate bound consistency if requested
        if invalid_bound_error {
            let invalid = lower.version > upper.version
                || (lower.version == upper.version && !(lower.inclusive && upper.inclusive));

            if invalid {
                return Err(RezError::Version("Invalid bound".to_string()));
            }
        }

        Ok(Self { lower, upper })
    }

    /// Any version bound (no restrictions)
    pub fn any() -> &'static Bound {
        &ANY_BOUND
    }

    /// Check if lower bound is not minimum
    pub fn lower_bounded(&self) -> bool {
        &self.lower != LowerBound::min()
    }

    /// Check if upper bound is not infinity
    pub fn upper_bounded(&self) -> bool {
        &self.upper != UpperBound::inf()
    }

    /// Check if this bound contains the given version
    pub fn contains_version(&self, version: &Version) -> bool {
        self.version_containment(version) == 0
    }

    /// Check version containment with directional information.
    ///
    /// # Returns
    /// * `-1` if version is below lower bound
    /// * `0` if version is within bounds
    /// * `1` if version is above upper bound
    pub fn version_containment(&self, version: &Version) -> i8 {
        if !self.lower.contains_version(version) {
            return -1;
        }
        if !self.upper.contains_version(version) {
            return 1;
        }
        0
    }

    /// Check if this bound contains another bound entirely
    pub fn contains_bound(&self, bound: &Bound) -> bool {
        self.lower <= bound.lower && self.upper >= bound.upper
    }

    /// Check if this bound intersects with another
    pub fn intersects(&self, other: &Bound) -> bool {
        let lower = std::cmp::max(&self.lower, &other.lower);
        let upper = std::cmp::min(&self.upper, &other.upper);

        lower.version < upper.version
            || (lower.version == upper.version && lower.inclusive && upper.inclusive)
    }

    /// Compute intersection of two bounds.
    ///
    /// # Returns
    /// * `Some(Bound)` if bounds intersect
    /// * `None` if bounds do not intersect
    pub fn intersection(&self, other: &Bound) -> Option<Bound> {
        let lower = std::cmp::max(&self.lower, &other.lower).clone();
        let upper = std::cmp::min(&self.upper, &other.upper).clone();

        if lower.version < upper.version
            || (lower.version == upper.version && lower.inclusive && upper.inclusive)
        {
            // Use invalid_bound_error=false since we already validated
            Bound::new(Some(lower), Some(upper), false).ok()
        } else {
            None
        }
    }
}

static ANY_BOUND: LazyLock<Bound> = LazyLock::new(|| Bound::new(None, None, false).unwrap());

impl PartialEq for Bound {
    fn eq(&self, other: &Self) -> bool {
        self.lower == other.lower && self.upper == other.upper
    }
}

impl Eq for Bound {}

impl PartialOrd for Bound {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Bound {
    fn cmp(&self, other: &Self) -> Ordering {
        // Compare as tuple (lower, upper)
        match self.lower.cmp(&other.lower) {
            Ordering::Equal => self.upper.cmp(&other.upper),
            other => other,
        }
    }
}

impl Hash for Bound {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.lower.hash(state);
        self.upper.hash(state);
    }
}

impl fmt::Display for Bound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Port of Python __str__ logic exactly

        if self.upper.version == Version::inf() {
            // Only lower bound matters
            write!(f, "{}", self.lower)
        } else if self.lower.version == self.upper.version {
            // Exact version (both inclusive)
            write!(f, "=={}", self.lower.version)
        } else if self.lower.inclusive && self.upper.inclusive {
            // Range with both inclusive
            if self.lower.version.is_truthy() {
                write!(f, "{}..{}", self.lower.version, self.upper.version)
            } else {
                // Empty lower, just show upper
                write!(f, "<={}", self.upper.version)
            }
        } else if self.lower.inclusive
            && !self.upper.inclusive
            && self.lower.version.next() == self.upper.version
        {
            // A prefix range: "1.2" is bounded above by the exclusive "1.2_".
            write!(f, "{}", self.lower.version)
        } else {
            // General case: show both bounds
            write!(f, "{}{}", self.lower, self.upper)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lower_bound_min() {
        let min = LowerBound::min();
        assert_eq!(min.version, Version::empty());
        assert!(min.inclusive);
    }

    #[test]
    fn test_lower_bound_display() {
        let v1 = Version::new("1.2").unwrap();
        let lb1 = LowerBound::new(v1.clone(), true);
        assert_eq!(lb1.to_string(), "1.2+");

        let lb2 = LowerBound::new(v1.clone(), false);
        assert_eq!(lb2.to_string(), ">1.2");

        let empty = LowerBound::new(Version::empty(), true);
        assert_eq!(empty.to_string(), "");

        let empty_ex = LowerBound::new(Version::empty(), false);
        assert_eq!(empty_ex.to_string(), ">");
    }

    #[test]
    fn test_lower_bound_ordering() {
        let v1 = Version::new("1.0").unwrap();
        let v2 = Version::new("2.0").unwrap();

        let lb1_inc = LowerBound::new(v1.clone(), true);
        let lb1_exc = LowerBound::new(v1.clone(), false);
        let lb2_inc = LowerBound::new(v2.clone(), true);

        assert!(lb1_inc < lb1_exc); // inclusive < exclusive for same version
        assert!(lb1_inc < lb2_inc); // 1.0+ < 2.0+
        assert!(lb1_exc < lb2_inc); // >1.0 < 2.0+
    }

    #[test]
    fn test_lower_bound_contains_version() {
        let v1 = Version::new("1.0").unwrap();
        let v15 = Version::new("1.5").unwrap();
        let v2 = Version::new("2.0").unwrap();

        let lb_inc = LowerBound::new(v1.clone(), true);
        assert!(lb_inc.contains_version(&v1)); // 1.0 >= 1.0+
        assert!(lb_inc.contains_version(&v15)); // 1.5 >= 1.0+
        assert!(lb_inc.contains_version(&v2)); // 2.0 >= 1.0+

        let lb_exc = LowerBound::new(v1.clone(), false);
        assert!(!lb_exc.contains_version(&v1)); // 1.0 not > 1.0
        assert!(lb_exc.contains_version(&v15)); // 1.5 > 1.0
        assert!(lb_exc.contains_version(&v2)); // 2.0 > 1.0
    }

    #[test]
    fn test_upper_bound_inf() {
        let inf = UpperBound::inf();
        assert!(inf.version.is_inf());
        assert!(inf.inclusive);
    }

    #[test]
    fn test_upper_bound_invalid() {
        // Empty version with exclusive bound is invalid
        let result = UpperBound::new(Version::empty(), false);
        assert!(result.is_err());
    }

    #[test]
    fn test_upper_bound_display() {
        let v1 = Version::new("1.2").unwrap();
        let ub1 = UpperBound::new(v1.clone(), true).unwrap();
        assert_eq!(ub1.to_string(), "<=1.2");

        let ub2 = UpperBound::new(v1.clone(), false).unwrap();
        assert_eq!(ub2.to_string(), "<1.2");
    }

    #[test]
    fn test_upper_bound_ordering() {
        let v1 = Version::new("1.0").unwrap();
        let v2 = Version::new("2.0").unwrap();

        let ub1_inc = UpperBound::new(v1.clone(), true).unwrap();
        let ub1_exc = UpperBound::new(v1.clone(), false).unwrap();
        let ub2_inc = UpperBound::new(v2.clone(), true).unwrap();

        assert!(ub1_exc < ub1_inc); // exclusive < inclusive for same version
        assert!(ub1_inc < ub2_inc); // <=1.0 < <=2.0
        assert!(ub1_exc < ub2_inc); // <1.0 < <=2.0
    }

    #[test]
    fn test_upper_bound_contains_version() {
        let v1 = Version::new("1.0").unwrap();
        let v05 = Version::new("0.5").unwrap();
        let v2 = Version::new("2.0").unwrap();

        let ub_inc = UpperBound::new(v1.clone(), true).unwrap();
        assert!(ub_inc.contains_version(&v05)); // 0.5 <= 1.0
        assert!(ub_inc.contains_version(&v1)); // 1.0 <= 1.0
        assert!(!ub_inc.contains_version(&v2)); // 2.0 not <= 1.0

        let ub_exc = UpperBound::new(v1.clone(), false).unwrap();
        assert!(ub_exc.contains_version(&v05)); // 0.5 < 1.0
        assert!(!ub_exc.contains_version(&v1)); // 1.0 not < 1.0
        assert!(!ub_exc.contains_version(&v2)); // 2.0 not < 1.0
    }

    #[test]
    fn test_bound_any() {
        let any = Bound::any();
        assert_eq!(any.lower.version, Version::empty());
        assert!(any.upper.version.is_inf());
    }

    #[test]
    fn test_bound_invalid() {
        let v1 = Version::new("2.0").unwrap();
        let v2 = Version::new("1.0").unwrap();

        let lower = LowerBound::new(v1, true);
        let upper = UpperBound::new(v2, true).unwrap();

        // Lower > upper is invalid
        let result = Bound::new(Some(lower), Some(upper), true);
        assert!(result.is_err());
    }

    #[test]
    fn test_bound_display() {
        // Only lower bound (upper = inf)
        let v1 = Version::new("1.0").unwrap();
        let lower = LowerBound::new(v1.clone(), true);
        let bound1 = Bound::new(Some(lower), None, false).unwrap();
        assert_eq!(bound1.to_string(), "1.0+");

        // Exact version
        let lower2 = LowerBound::new(v1.clone(), true);
        let upper2 = UpperBound::new(v1.clone(), true).unwrap();
        let bound2 = Bound::new(Some(lower2), Some(upper2), false).unwrap();
        assert_eq!(bound2.to_string(), "==1.0");

        // Range
        let v2 = Version::new("2.0").unwrap();
        let lower3 = LowerBound::new(v1.clone(), true);
        let upper3 = UpperBound::new(v2.clone(), true).unwrap();
        let bound3 = Bound::new(Some(lower3), Some(upper3), false).unwrap();
        assert_eq!(bound3.to_string(), "1.0..2.0");

        // Only upper bound (lower = empty)
        let upper4 = UpperBound::new(v2.clone(), true).unwrap();
        let bound4 = Bound::new(None, Some(upper4), false).unwrap();
        assert_eq!(bound4.to_string(), "<=2.0");
    }

    #[test]
    fn test_bound_lower_upper_bounded() {
        let v1 = Version::new("1.0").unwrap();
        let lower = LowerBound::new(v1.clone(), true);

        let bound1 = Bound::new(Some(lower), None, false).unwrap();
        assert!(bound1.lower_bounded());
        assert!(!bound1.upper_bounded());

        let any = Bound::any();
        assert!(!any.lower_bounded());
        assert!(!any.upper_bounded());
    }

    #[test]
    fn test_bound_contains_version() {
        let v1 = Version::new("1.0").unwrap();
        let v2 = Version::new("2.0").unwrap();
        let v15 = Version::new("1.5").unwrap();

        let lower = LowerBound::new(v1.clone(), true);
        let upper = UpperBound::new(v2.clone(), true).unwrap();
        let bound = Bound::new(Some(lower), Some(upper), false).unwrap();

        assert!(bound.contains_version(&v1)); // 1.0 in [1.0, 2.0]
        assert!(bound.contains_version(&v15)); // 1.5 in [1.0, 2.0]
        assert!(bound.contains_version(&v2)); // 2.0 in [1.0, 2.0]

        let v05 = Version::new("0.5").unwrap();
        let v3 = Version::new("3.0").unwrap();
        assert!(!bound.contains_version(&v05)); // 0.5 not in [1.0, 2.0]
        assert!(!bound.contains_version(&v3)); // 3.0 not in [1.0, 2.0]
    }

    #[test]
    fn test_bound_version_containment() {
        let v1 = Version::new("1.0").unwrap();
        let v2 = Version::new("2.0").unwrap();
        let v15 = Version::new("1.5").unwrap();
        let v05 = Version::new("0.5").unwrap();
        let v3 = Version::new("3.0").unwrap();

        let lower = LowerBound::new(v1.clone(), true);
        let upper = UpperBound::new(v2.clone(), true).unwrap();
        let bound = Bound::new(Some(lower), Some(upper), false).unwrap();

        assert_eq!(bound.version_containment(&v05), -1); // below
        assert_eq!(bound.version_containment(&v1), 0); // within
        assert_eq!(bound.version_containment(&v15), 0); // within
        assert_eq!(bound.version_containment(&v2), 0); // within
        assert_eq!(bound.version_containment(&v3), 1); // above
    }

    #[test]
    fn test_bound_contains_bound() {
        let v1 = Version::new("1.0").unwrap();
        let v2 = Version::new("2.0").unwrap();
        let v15 = Version::new("1.5").unwrap();

        let lower1 = LowerBound::new(v1.clone(), true);
        let upper1 = UpperBound::new(v2.clone(), true).unwrap();
        let bound1 = Bound::new(Some(lower1), Some(upper1), false).unwrap();

        let lower2 = LowerBound::new(v15.clone(), true);
        let upper2 = UpperBound::new(v2.clone(), true).unwrap();
        let bound2 = Bound::new(Some(lower2), Some(upper2), false).unwrap();

        assert!(bound1.contains_bound(&bound2)); // [1.0, 2.0] contains [1.5, 2.0]
        assert!(!bound2.contains_bound(&bound1)); // [1.5, 2.0] does not contain [1.0, 2.0]
    }

    #[test]
    fn test_bound_intersects() {
        let v1 = Version::new("1.0").unwrap();
        let v2 = Version::new("2.0").unwrap();
        let v3 = Version::new("3.0").unwrap();

        let lower1 = LowerBound::new(v1.clone(), true);
        let upper1 = UpperBound::new(v2.clone(), true).unwrap();
        let bound1 = Bound::new(Some(lower1), Some(upper1), false).unwrap();

        let lower2 = LowerBound::new(v2.clone(), true);
        let upper2 = UpperBound::new(v3.clone(), true).unwrap();
        let bound2 = Bound::new(Some(lower2), Some(upper2), false).unwrap();

        assert!(bound1.intersects(&bound2)); // [1.0, 2.0] intersects [2.0, 3.0] at 2.0

        let lower3 = LowerBound::new(v2.clone(), false);
        let upper3 = UpperBound::new(v3.clone(), true).unwrap();
        let bound3 = Bound::new(Some(lower3), Some(upper3), false).unwrap();

        let upper4 = UpperBound::new(v2.clone(), false).unwrap();
        let bound4 = Bound::new(None, Some(upper4), false).unwrap();

        assert!(!bound3.intersects(&bound4)); // (2.0, 3.0] does not intersect (empty, 2.0)
    }

    #[test]
    fn test_bound_intersection() {
        let v1 = Version::new("1.0").unwrap();
        let v2 = Version::new("2.0").unwrap();
        let v3 = Version::new("3.0").unwrap();

        let lower1 = LowerBound::new(v1.clone(), true);
        let upper1 = UpperBound::new(v2.clone(), true).unwrap();
        let bound1 = Bound::new(Some(lower1), Some(upper1), false).unwrap();

        let lower2 = LowerBound::new(v2.clone(), true);
        let upper2 = UpperBound::new(v3.clone(), true).unwrap();
        let bound2 = Bound::new(Some(lower2), Some(upper2), false).unwrap();

        let intersection = bound1.intersection(&bound2);
        assert!(intersection.is_some());
        let inter = intersection.unwrap();
        assert_eq!(inter.lower.version, v2);
        assert_eq!(inter.upper.version, v2);

        // Non-intersecting bounds
        let lower3 = LowerBound::new(v2.clone(), false);
        let upper3 = UpperBound::new(v3.clone(), true).unwrap();
        let bound3 = Bound::new(Some(lower3), Some(upper3), false).unwrap();

        let upper4 = UpperBound::new(v2.clone(), false).unwrap();
        let bound4 = Bound::new(None, Some(upper4), false).unwrap();

        assert!(bound3.intersection(&bound4).is_none());
    }

    #[test]
    fn test_bound_hash() {
        use std::collections::HashSet;

        let v1 = Version::new("1.0").unwrap();
        let lower = LowerBound::new(v1.clone(), true);
        let upper = UpperBound::new(v1.clone(), true).unwrap();
        let bound1 = Bound::new(Some(lower.clone()), Some(upper.clone()), false).unwrap();
        let bound2 = Bound::new(Some(lower), Some(upper), false).unwrap();

        let mut set = HashSet::new();
        set.insert(bound1);
        assert!(set.contains(&bound2));
    }

    #[test]
    fn test_bound_ordering() {
        let v1 = Version::new("1.0").unwrap();
        let v2 = Version::new("2.0").unwrap();

        let lower1 = LowerBound::new(v1.clone(), true);
        let upper1 = UpperBound::new(v2.clone(), true).unwrap();
        let bound1 = Bound::new(Some(lower1), Some(upper1), false).unwrap();

        let lower2 = LowerBound::new(v2.clone(), true);
        let upper2 = UpperBound::new(v2.clone(), true).unwrap();
        let bound2 = Bound::new(Some(lower2), Some(upper2), false).unwrap();

        assert!(bound1 < bound2); // [1.0, 2.0] < [2.0, 2.0]
    }
}
