// SPDX-License-Identifier: Apache-2.0

//! Package requirements with version constraints.
//!
//! Provides VersionedObject, Requirement, and RequirementList types.

use super::version::Version;
// NOTE: This depends on range.rs which will be implemented separately
// VersionRange API expected:
// - new(s: &str) -> Result<Self, RezError>
// - is_any() -> bool
// - issuperset(&self, other: &Self) -> bool
// - intersects(&self, other: &Self) -> bool
// - contains_version(&self, version: &Version) -> bool
// - BitAnd, BitOr, Not, Sub operations
// - Display trait
use super::range::VersionRange;
use foundation::errors::RezError;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;
use std::sync::{LazyLock, OnceLock};

static SEP_REGEX_VERSIONED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[-@#]").expect("Invalid separator regex"));

static SEP_REGEX_REQUIREMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[-@#=<>]").expect("Invalid separator regex"));

/// A named object with a version (e.g. "foo-1.2.3", "bar@2.0", "baz#1.0").
///
/// This represents a concrete versioned object like a resolved package.
///
/// # Examples
/// ```no_run
/// use version::VersionedObject;
///
/// let obj = VersionedObject::new("foo-1.2.3").unwrap();
/// assert_eq!(obj.name(), "foo");
/// assert_eq!(obj.version().to_string(), "1.2.3");
///
/// let req = obj.as_exact_requirement();
/// assert_eq!(req, "foo==1.2.3");
/// ```
#[derive(Clone, Debug)]
pub struct VersionedObject {
    name: String,
    version: Version,
    sep: char,
}

impl VersionedObject {
    /// Parse a versioned object from a string.
    ///
    /// # Arguments
    /// * `s` - String like "foo-1.2.3", "bar@2.0", "baz#1.0", or "foo" (no version)
    ///
    /// # Returns
    /// * `Ok(VersionedObject)` on success
    /// * `Err(RezError)` if parsing fails
    pub fn new(s: &str) -> Result<Self, RezError> {
        // Search for separator
        if let Some(mat) = SEP_REGEX_VERSIONED.find(s) {
            let i = mat.start();
            let name = s[..i].to_string();
            // Safe: i is from regex match within ASCII range of s
            let sep = s.as_bytes()[i] as char;
            let ver_str = &s[i + 1..];
            let version = Version::new(ver_str)?;
            Ok(Self { name, version, sep })
        } else {
            // No separator, just a name with empty version
            Ok(Self {
                name: s.to_string(),
                version: Version::empty(),
                sep: '-',
            })
        }
    }

    /// Construct a versioned object from parts.
    ///
    /// # Arguments
    /// * `name` - Package name
    /// * `version` - Optional version (defaults to empty)
    pub fn construct(name: impl Into<String>, version: Option<Version>) -> Self {
        Self {
            name: name.into(),
            version: version.unwrap_or_else(Version::empty),
            sep: '-',
        }
    }

    /// Get the package name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the version.
    pub fn version(&self) -> &Version {
        &self.version
    }

    /// Convert to an exact requirement string (e.g. "foo==1.2.3").
    ///
    /// If version is empty, returns just the name.
    pub fn as_exact_requirement(&self) -> String {
        if self.version.is_truthy() {
            format!("{}=={}", self.name, self.version)
        } else {
            self.name.clone()
        }
    }
}

impl PartialEq for VersionedObject {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.version == other.version
    }
}

impl Eq for VersionedObject {}

impl Hash for VersionedObject {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        self.version.hash(state);
    }
}

impl fmt::Display for VersionedObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.version.is_truthy() {
            write!(f, "{}{}{}", self.name, self.sep, self.version)
        } else {
            write!(f, "{}", self.name)
        }
    }
}

impl FromStr for VersionedObject {
    type Err = RezError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        VersionedObject::new(s)
    }
}

/// A package requirement with version range (e.g. "foo-1.2+", "bar<2.0", "!baz").
///
/// Supports:
/// - Normal requirements: "foo-1.2+" (any version >= 1.2)
/// - Conflict requirements: "!foo" (conflicts with foo)
/// - Weak requirements: "~foo" (weak reference, doesn't conflict but inverts range)
///
/// # Examples
/// ```no_run
/// use version::Requirement;
///
/// let req = Requirement::new("foo-1.2+<2.0").unwrap();
/// assert_eq!(req.name(), "foo");
/// assert!(!req.conflict());
///
/// let conflict = Requirement::new("!bar").unwrap();
/// assert!(conflict.conflict());
/// ```
#[derive(Clone, Debug)]
pub struct Requirement {
    name: String,
    range: Option<VersionRange>,
    negate: bool,
    conflict: bool,
    sep: char,
    cached_str: OnceLock<String>,
}

impl Requirement {
    /// Parse a requirement from a string.
    ///
    /// # Arguments
    /// * `s` - Requirement string like "foo-1.2+", "!bar", "~baz<2.0"
    /// * `invalid_bound_error` - If true, raise error on invalid version bounds
    ///
    /// # Returns
    /// * `Ok(Requirement)` on success
    /// * `Err(RezError)` if parsing fails
    pub fn new(s: &str) -> Result<Self, RezError> {
        let mut s = s;
        let mut conflict = false;
        let mut negate = false;

        // Check for conflict prefix
        if s.starts_with('!') {
            conflict = true;
            s = &s[1..];
        } else if s.starts_with('~') {
            s = &s[1..];
            negate = true;
            conflict = true;
        }

        // Search for separator
        if let Some(mat) = SEP_REGEX_REQUIREMENT.find(s) {
            let i = mat.start();
            let name = s[..i].to_string();
            let mut req_str = &s[i..];
            let mut sep = '-';

            // Check if separator is one of the special chars
            if let Some(first_char) = req_str.chars().next() {
                if first_char == '-' || first_char == '@' || first_char == '#' {
                    sep = first_char;
                    req_str = &req_str[1..];
                }
            }

            let range = VersionRange::new(req_str)?;
            let range = if negate {
                range.inverse() // inverse for negate (~) prefix
            } else {
                Some(range)
            };

            Ok(Self {
                name,
                range,
                negate,
                conflict,
                sep,
                cached_str: OnceLock::new(),
            })
        } else if negate {
            // ~foo with no range = no effect, range is None
            Ok(Self {
                name: s.to_string(),
                range: None,
                negate,
                conflict,
                sep: '-',
                cached_str: OnceLock::new(),
            })
        } else {
            // No separator, just a name with any range
            Ok(Self {
                name: s.to_string(),
                range: Some(VersionRange::any()),
                negate,
                conflict,
                sep: '-',
                cached_str: OnceLock::new(),
            })
        }
    }

    /// Construct a requirement from parts.
    ///
    /// # Arguments
    /// * `name` - Package name
    /// * `range` - Optional version range (defaults to any range)
    pub fn construct(name: impl Into<String>, range: Option<VersionRange>) -> Self {
        Self {
            name: name.into(),
            range: Some(range.unwrap_or_else(VersionRange::any)),
            negate: false,
            conflict: false,
            sep: '-',
            cached_str: OnceLock::new(),
        }
    }

    /// Get the package name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the version range.
    pub fn range(&self) -> Option<&VersionRange> {
        self.range.as_ref()
    }

    /// Check if this is a conflict requirement.
    pub fn conflict(&self) -> bool {
        self.conflict
    }

    /// Check if this is a weak requirement.
    pub fn weak(&self) -> bool {
        self.negate
    }

    /// Safe string representation (same as Display).
    pub fn safe_str(&self) -> String {
        self.to_string()
    }

    /// Check if this requirement conflicts with another requirement.
    ///
    /// # Arguments
    /// * `other` - Another requirement to check against
    ///
    /// # Returns
    /// * `true` if requirements conflict
    /// * `false` otherwise
    pub fn conflicts_with_req(&self, other: &Requirement) -> bool {
        if self.name != other.name {
            return false;
        }

        match (&self.range, &other.range) {
            (None, _) | (_, None) => false,
            (Some(self_range), Some(other_range)) => {
                if self.conflict {
                    if other.conflict {
                        false
                    } else {
                        self_range.issuperset(other_range)
                    }
                } else if other.conflict {
                    other_range.issuperset(self_range)
                } else {
                    !self_range.intersects(other_range)
                }
            }
        }
    }

    /// Check if this requirement conflicts with a versioned object.
    ///
    /// # Arguments
    /// * `obj` - A versioned object to check against
    ///
    /// # Returns
    /// * `true` if requirement conflicts with the object
    /// * `false` otherwise
    pub fn conflicts_with_obj(&self, obj: &VersionedObject) -> bool {
        if self.name != obj.name {
            return false;
        }

        match &self.range {
            None => false,
            Some(range) => {
                if self.conflict {
                    range.contains_version(obj.version())
                } else {
                    !range.contains_version(obj.version())
                }
            }
        }
    }

    /// Merge this requirement with another requirement.
    ///
    /// # Arguments
    /// * `other` - Another requirement to merge with
    ///
    /// # Returns
    /// * `Some(Requirement)` if merge succeeds
    /// * `None` if requirements cannot be merged (conflict)
    pub fn merged(&self, other: &Requirement) -> Option<Requirement> {
        if self.name != other.name {
            return None;
        }

        // Helper to clone requirement structure
        let clone_from = |r: &Requirement| -> Requirement {
            Requirement {
                name: r.name.clone(),
                range: None,
                negate: r.negate,
                conflict: r.conflict,
                sep: r.sep,
                cached_str: OnceLock::new(),
            }
        };

        match (&self.range, &other.range) {
            (None, _) => Some(other.clone()),
            (_, None) => Some(self.clone()),
            (Some(self_range), Some(other_range)) => {
                if self.conflict {
                    if other.conflict {
                        // Both conflicts: union of ranges
                        let mut r = clone_from(self);
                        let union_range = self_range | other_range;
                        r.negate = self.negate && other.negate && !union_range.is_any();
                        r.range = Some(union_range);
                        Some(r)
                    } else {
                        // self conflict, other normal: subtract
                        let range = other_range - self_range;
                        range.map(|rng| {
                            let mut r = clone_from(other);
                            r.range = Some(rng);
                            r
                        })
                    }
                } else if other.conflict {
                    // self normal, other conflict: subtract
                    let range = self_range - other_range;
                    range.map(|rng| {
                        let mut r = clone_from(self);
                        r.range = Some(rng);
                        r
                    })
                } else {
                    // Both normal: intersection
                    let range = self_range & other_range;
                    range.map(|rng| {
                        let mut r = clone_from(self);
                        r.range = Some(rng);
                        r
                    })
                }
            }
        }
    }
}

impl PartialEq for Requirement {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.range == other.range && self.conflict == other.conflict
    }
}

impl Eq for Requirement {}

impl Hash for Requirement {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        self.range.hash(state);
        self.conflict.hash(state);
    }
}

impl fmt::Display for Requirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Use cached string if available
        let s = self.cached_str.get_or_init(|| {
            let pre_str = if self.negate {
                "~"
            } else if self.conflict {
                "!"
            } else {
                ""
            };

            let mut range_str = String::new();
            let mut sep_str = String::new();

            if let Some(range) = &self.range {
                // Negate inverts the range; inverse() returns None for "any" (= empty inverse)
                let range_to_use = if self.negate {
                    match range.inverse() {
                        Some(inv) => inv,
                        None => VersionRange::any(), // inverse of "any" is conceptually empty
                    }
                } else {
                    range.clone()
                };

                if !range_to_use.is_any() {
                    let rs = range_to_use.to_string();
                    if !rs.starts_with('=') && !rs.starts_with('<') && !rs.starts_with('>') {
                        sep_str = self.sep.to_string();
                    }
                    range_str = rs;
                }
            }

            format!("{}{}{}{}", pre_str, self.name, sep_str, range_str)
        });

        write!(f, "{}", s)
    }
}

impl FromStr for Requirement {
    type Err = RezError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Requirement::new(s)
    }
}

/// A list of requirements with conflict detection and automatic merging.
///
/// When requirements with the same package name are added, they are automatically
/// merged. If merging fails (conflicting requirements), the conflict is stored.
///
/// # Examples
/// ```no_run
/// use version::{Requirement, RequirementList};
///
/// let reqs = vec![
///     Requirement::new("foo-1.2+").unwrap(),
///     Requirement::new("foo<2.0").unwrap(),
/// ];
///
/// let req_list = RequirementList::new(reqs);
/// assert!(req_list.conflict().is_none());
/// ```
#[derive(Clone, Debug)]
pub struct RequirementList {
    requirements: Vec<Requirement>,
    conflict: Option<(Requirement, Requirement)>,
    requirements_dict: HashMap<String, Requirement>,
    names: HashSet<String>,
    conflict_names: HashSet<String>,
}

impl RequirementList {
    /// Create a requirement list from a vector of requirements.
    ///
    /// Requirements with the same package name will be automatically merged.
    /// If merging fails, a conflict is recorded.
    ///
    /// # Arguments
    /// * `requirements` - Vector of requirements to process
    pub fn new(requirements: Vec<Requirement>) -> Self {
        let mut requirements_dict: HashMap<String, Requirement> = HashMap::new();
        let mut conflict = None;

        // Merge requirements with the same name
        for req in &requirements {
            if let Some(existing_req) = requirements_dict.get(req.name()) {
                if let Some(merged_req) = existing_req.merged(req) {
                    requirements_dict.insert(req.name().to_string(), merged_req);
                } else {
                    conflict = Some((existing_req.clone(), req.clone()));
                    break;
                }
            } else {
                requirements_dict.insert(req.name().to_string(), req.clone());
            }
        }

        // Build final requirements list preserving input order
        let mut final_requirements = Vec::new();
        let mut names = HashSet::new();
        let mut conflict_names = HashSet::new();
        let mut seen = HashSet::new();

        for req in &requirements {
            if !seen.contains(req.name()) {
                seen.insert(req.name().to_string());
                if let Some(final_req) = requirements_dict.get(req.name()) {
                    final_requirements.push(final_req.clone());
                    if final_req.conflict() {
                        conflict_names.insert(req.name().to_string());
                    } else {
                        names.insert(req.name().to_string());
                    }
                }
            }
        }

        Self {
            requirements: final_requirements,
            conflict,
            requirements_dict,
            names,
            conflict_names,
        }
    }

    /// Get the list of merged requirements.
    pub fn requirements(&self) -> &[Requirement] {
        &self.requirements
    }

    /// Get the conflict if one exists.
    ///
    /// Returns a tuple of the two conflicting requirements.
    pub fn conflict(&self) -> Option<&(Requirement, Requirement)> {
        self.conflict.as_ref()
    }

    /// Get set of non-conflict package names.
    pub fn names(&self) -> &HashSet<String> {
        &self.names
    }

    /// Get set of conflict package names.
    pub fn conflict_names(&self) -> &HashSet<String> {
        &self.conflict_names
    }

    /// Get a requirement by package name.
    ///
    /// # Arguments
    /// * `name` - Package name to look up
    ///
    /// # Returns
    /// * `Some(&Requirement)` if found
    /// * `None` if not found
    pub fn get(&self, name: &str) -> Option<&Requirement> {
        self.requirements_dict.get(name)
    }

    /// Iterate over all requirements.
    pub fn iter(&self) -> impl Iterator<Item = &Requirement> {
        self.requirements.iter()
    }
}

impl PartialEq for RequirementList {
    fn eq(&self, other: &Self) -> bool {
        self.requirements == other.requirements && self.conflict == other.conflict
    }
}

impl Eq for RequirementList {}

impl fmt::Display for RequirementList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some((req1, req2)) = &self.conflict {
            write!(f, "{} <--!--> {}", req1, req2)
        } else {
            let strs: Vec<String> = self.requirements.iter().map(|r| r.to_string()).collect();
            write!(f, "{}", strs.join(" "))
        }
    }
}

impl<'a> IntoIterator for &'a RequirementList {
    type Item = &'a Requirement;
    type IntoIter = std::slice::Iter<'a, Requirement>;

    fn into_iter(self) -> Self::IntoIter {
        self.requirements.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_versioned_object_parse() {
        let obj = VersionedObject::new("foo-1.2.3").unwrap();
        assert_eq!(obj.name(), "foo");
        assert_eq!(obj.version().to_string(), "1.2.3");
        assert_eq!(obj.sep, '-');
    }

    #[test]
    fn test_versioned_object_parse_no_version() {
        let obj = VersionedObject::new("foo").unwrap();
        assert_eq!(obj.name(), "foo");
        assert!(obj.version().is_empty());
    }

    #[test]
    fn test_versioned_object_parse_different_seps() {
        let obj1 = VersionedObject::new("foo@1.2").unwrap();
        assert_eq!(obj1.sep, '@');

        let obj2 = VersionedObject::new("bar#2.0").unwrap();
        assert_eq!(obj2.sep, '#');
    }

    #[test]
    fn test_versioned_object_construct() {
        let v = Version::new("1.2.3").unwrap();
        let obj = VersionedObject::construct("foo", Some(v));
        assert_eq!(obj.name(), "foo");
        assert_eq!(obj.version().to_string(), "1.2.3");
    }

    #[test]
    fn test_versioned_object_as_exact_requirement() {
        let obj = VersionedObject::new("foo-1.2.3").unwrap();
        assert_eq!(obj.as_exact_requirement(), "foo==1.2.3");

        let obj_no_ver = VersionedObject::new("foo").unwrap();
        assert_eq!(obj_no_ver.as_exact_requirement(), "foo");
    }

    #[test]
    fn test_versioned_object_display() {
        let obj = VersionedObject::new("foo-1.2.3").unwrap();
        assert_eq!(obj.to_string(), "foo-1.2.3");

        let obj_no_ver = VersionedObject::new("foo").unwrap();
        assert_eq!(obj_no_ver.to_string(), "foo");
    }

    #[test]
    fn test_versioned_object_equality() {
        let obj1 = VersionedObject::new("foo-1.2.3").unwrap();
        let obj2 = VersionedObject::new("foo-1.2.3").unwrap();
        let obj3 = VersionedObject::new("foo-1.2.4").unwrap();

        assert_eq!(obj1, obj2);
        assert_ne!(obj1, obj3);
    }

    #[test]
    fn test_versioned_object_hash() {
        use std::collections::HashMap;

        let obj = VersionedObject::new("foo-1.2.3").unwrap();
        let mut map = HashMap::new();
        map.insert(obj.clone(), "test");
        assert_eq!(map.get(&obj), Some(&"test"));
    }

    // Requirement tests would require VersionRange to be implemented
    // Placeholder tests structure:

    #[test]

    fn test_requirement_parse_simple() {
        let req = Requirement::new("foo-1.2+").unwrap();
        assert_eq!(req.name(), "foo");
        assert!(!req.conflict());
        assert!(!req.weak());
    }

    #[test]

    fn test_requirement_parse_conflict() {
        let req = Requirement::new("!foo").unwrap();
        assert_eq!(req.name(), "foo");
        assert!(req.conflict());
        assert!(!req.weak());
    }

    #[test]

    fn test_requirement_parse_weak() {
        let req = Requirement::new("~foo<2.0").unwrap();
        assert_eq!(req.name(), "foo");
        assert!(req.conflict());
        assert!(req.weak());
    }

    #[test]
    fn test_requirement_hash_matches_semantic_equality() {
        use std::collections::hash_map::DefaultHasher;

        let hyphen = Requirement::new("foo-1+").unwrap();
        let at = Requirement::new("foo@1+").unwrap();
        assert_eq!(hyphen, at);

        let hash = |req: &Requirement| {
            let mut hasher = DefaultHasher::new();
            req.hash(&mut hasher);
            hasher.finish()
        };
        assert_eq!(hash(&hyphen), hash(&at));

        let requirements = HashMap::from([(hash(&hyphen), vec![hyphen])]);
        assert!(requirements[&hash(&at)]
            .iter()
            .any(|requirement| requirement == &at));
    }

    #[test]

    fn test_requirement_construct() {
        let req = Requirement::construct("foo", None);
        assert_eq!(req.name(), "foo");
        assert!(!req.conflict());
    }

    #[test]

    fn test_requirement_conflicts_with_req() {
        let req1 = Requirement::new("foo-1.2+<2.0").unwrap();
        let req2 = Requirement::new("foo-2.0+").unwrap();
        assert!(req1.conflicts_with_req(&req2));
    }

    #[test]

    fn test_requirement_conflicts_with_obj() {
        let req = Requirement::new("foo<2.0").unwrap();
        let obj = VersionedObject::new("foo-2.5").unwrap();
        assert!(req.conflicts_with_obj(&obj));
    }

    #[test]

    fn test_requirement_merged() {
        let req1 = Requirement::new("foo-1.2+").unwrap();
        let req2 = Requirement::new("foo<2.0").unwrap();
        let merged = req1.merged(&req2);
        assert!(merged.is_some());
    }

    #[test]

    fn test_requirement_list_new() {
        let reqs = vec![
            Requirement::new("foo-1.2+").unwrap(),
            Requirement::new("bar<2.0").unwrap(),
        ];
        let req_list = RequirementList::new(reqs);
        assert_eq!(req_list.requirements().len(), 2);
        assert!(req_list.conflict().is_none());
    }

    #[test]

    fn test_requirement_list_merge() {
        let reqs = vec![
            Requirement::new("foo-1.2+").unwrap(),
            Requirement::new("foo<2.0").unwrap(),
        ];
        let req_list = RequirementList::new(reqs);
        assert_eq!(req_list.requirements().len(), 1);
        assert!(req_list.conflict().is_none());
    }

    #[test]

    fn test_requirement_list_conflict() {
        let reqs = vec![
            Requirement::new("foo-1.2+").unwrap(),
            Requirement::new("foo<1.0").unwrap(),
        ];
        let req_list = RequirementList::new(reqs);
        assert!(req_list.conflict().is_some());
    }

    #[test]

    fn test_requirement_list_display() {
        let reqs = vec![
            Requirement::new("foo-1.2+").unwrap(),
            Requirement::new("bar<2.0").unwrap(),
        ];
        let req_list = RequirementList::new(reqs);
        let display = req_list.to_string();
        assert!(display.contains("foo"));
        assert!(display.contains("bar"));
    }

    #[test]

    fn test_requirement_list_display_conflict() {
        let reqs = vec![
            Requirement::new("foo-1.2+").unwrap(),
            Requirement::new("foo<1.0").unwrap(),
        ];
        let req_list = RequirementList::new(reqs);
        let display = req_list.to_string();
        assert!(display.contains("<--!-->"));
    }

    #[test]

    fn test_requirement_list_get() {
        let reqs = vec![
            Requirement::new("foo-1.2+").unwrap(),
            Requirement::new("bar<2.0").unwrap(),
        ];
        let req_list = RequirementList::new(reqs);
        assert!(req_list.get("foo").is_some());
        assert!(req_list.get("bar").is_some());
        assert!(req_list.get("baz").is_none());
    }

    #[test]

    fn test_requirement_list_iter() {
        let reqs = vec![
            Requirement::new("foo-1.2+").unwrap(),
            Requirement::new("bar<2.0").unwrap(),
        ];
        let req_list = RequirementList::new(reqs);
        let count = req_list.iter().count();
        assert_eq!(count, 2);
    }
}
