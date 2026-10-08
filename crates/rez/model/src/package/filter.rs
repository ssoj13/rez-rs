// SPDX-License-Identifier: Apache-2.0

//! Package filtering with exclude and include rules.
//!
//! Ported from Python rez package_filter.py.

use crate::errors::RezError;
use crate::package::Package;
use regex::Regex;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::LazyLock;
use version::{Version, VersionRange};

/// Filter rule matching packages by name and version criteria
#[derive(Debug, Clone)]
pub enum Rule {
    /// Match by glob pattern on package name (e.g., "maya*", "*-beta")
    Glob { pattern: String, regex: Regex },
    /// Match by exact package family name
    Family { name: String },
    /// Match by package name + version range
    Range { name: String, range: VersionRange },
    /// Match by regex on full package string
    Regex { pattern: String, regex: Regex },
    /// Match by package timestamp (before/after given epoch time)
    Timestamp {
        timestamp: i128,
        family: Option<String>,
        reverse: bool,
        match_untimestamped: bool,
    },
}

impl Rule {
    /// Check if rule matches given package (full, including timestamp)
    pub fn matches(&self, package: &Package) -> bool {
        match self {
            Rule::Timestamp {
                timestamp,
                family,
                reverse,
                match_untimestamped,
            } => {
                if family
                    .as_deref()
                    .is_some_and(|rule_family| rule_family != package.name.as_str())
                {
                    return false;
                }
                if let Some(pkg_ts) = package.timestamp.filter(|value| *value != 0) {
                    if *reverse {
                        pkg_ts > *timestamp
                    } else {
                        pkg_ts <= *timestamp
                    }
                } else {
                    *match_untimestamped
                }
            }
            _ => self.matches_nv(&package.name, &package.version),
        }
    }

    /// Check if rule matches by name+version (Timestamp rules always match)
    pub fn matches_nv(&self, name: &str, version: &Version) -> bool {
        match self {
            Rule::Glob { regex, .. } => {
                let qualified = format!("{}-{}", name, version);
                regex.is_match(&qualified)
            }
            Rule::Family { name: family } => name == family,
            Rule::Range {
                name: req_name,
                range,
            } => {
                if name != req_name {
                    return false;
                }
                range.contains_version(version)
            }
            Rule::Regex { regex, .. } => {
                let qualified = format!("{}-{}", name, version);
                regex.is_match(&qualified)
            }
            Rule::Timestamp { .. } => true, // timestamp needs Package, not name+version
        }
    }

    /// Parse rule from string (e.g., "maya*", "maya-1.2+<2.0", "regex(.*\\.beta)")
    pub fn parse(txt: &str) -> Result<Self, RezError> {
        // Check for labeled form: "glob(...)", "regex(...)", "range(...)"
        if let Some(captures) = LABEL_RE.captures(txt) {
            let label = &captures[1];
            let content = &captures[2];

            match label {
                "before" | "after" => parse_timestamp_rule(label, content),
                "glob" => {
                    let pattern = content.to_string();
                    let regex = glob_to_regex(&pattern)?;
                    Ok(Rule::Glob { pattern, regex })
                }
                "regex" => {
                    let pattern = content.to_string();
                    let regex = Regex::new(&pattern).map_err(|e| {
                        RezError::Parse(format!("Invalid regex '{}': {}", pattern, e))
                    })?;
                    Ok(Rule::Regex { pattern, regex })
                }
                "range" => parse_range_rule(content),
                _ => Err(RezError::Parse(format!(
                    "'{}' is not a valid package filter type",
                    label
                ))),
            }
        } else {
            // No label: heuristic parsing
            if txt.contains('*') || txt.contains('?') {
                // Glob pattern
                let pattern = txt.to_string();
                let regex = glob_to_regex(&pattern)?;
                Ok(Rule::Glob { pattern, regex })
            } else {
                // Try parsing as range (family or family-version_range)
                parse_range_rule(txt)
            }
        }
    }

    /// Convert the rule to Rez's canonical package-filter POD string.
    fn to_pod_string(&self) -> String {
        match self {
            Rule::Glob { pattern, .. } => format!("glob({pattern})"),
            Rule::Family { name } => format!("range({name})"),
            Rule::Range { name, range } => format!("range({name}-{range})"),
            Rule::Regex { pattern, .. } => format!("regex({pattern})"),
            Rule::Timestamp {
                timestamp,
                family,
                reverse,
                ..
            } => {
                let label = if *reverse { "after" } else { "before" };
                match family.as_deref().filter(|family| !family.is_empty()) {
                    Some(family) => format!("{label}({family}:{timestamp})"),
                    None => format!("{label}({timestamp})"),
                }
            }
        }
    }

    /// Get package family name if rule applies to specific family only
    pub fn family(&self) -> Option<&str> {
        match self {
            Rule::Glob { pattern, .. } => extract_family(pattern),
            Rule::Family { name } => Some(name.as_str()),
            Rule::Range { name, .. } => Some(name.as_str()),
            Rule::Regex { pattern, .. } => extract_family(pattern),
            Rule::Timestamp { family, .. } => family.as_deref(),
        }
    }

    /// Relative cost of filter (cheaper filters applied first)
    pub fn cost(&self) -> u32 {
        match self {
            Rule::Family { .. } => 1,
            Rule::Range { .. } => 5,
            Rule::Glob { .. } => 10,
            Rule::Regex { .. } => 10,
            Rule::Timestamp { .. } => 1000, // expensive: requires package load
        }
    }
}

/// Parse Rez timestamp filter syntax: `before([family:]epoch)` or `after([family:]epoch)`.
fn parse_timestamp_rule(label: &str, content: &str) -> Result<Rule, RezError> {
    let (family, timestamp) = match content.split_once(':') {
        Some((family, timestamp)) => (Some(family.to_string()), timestamp),
        None => (None, content),
    };
    let timestamp = timestamp.trim().parse::<i128>().map_err(|error| {
        RezError::Parse(format!(
            "Invalid timestamp filter '{}({})': {}",
            label, content, error
        ))
    })?;

    Ok(Rule::Timestamp {
        timestamp,
        family,
        reverse: label == "after",
        match_untimestamped: false,
    })
}

/// Parse range rule: "family" or "family-version_range"
fn parse_range_rule(txt: &str) -> Result<Rule, RezError> {
    // Try parsing as full requirement (family-version_range)
    if let Some(sep_pos) = txt.find('-') {
        let name = &txt[..sep_pos];
        let range_str = &txt[sep_pos + 1..];

        // Attempt to parse version range
        if let Ok(range) = VersionRange::new(range_str) {
            return Ok(Rule::Range {
                name: name.to_string(),
                range,
            });
        }
    }

    // No version range, treat as family name
    Ok(Rule::Family {
        name: txt.to_string(),
    })
}

/// Convert glob pattern to regex
fn glob_to_regex(pattern: &str) -> Result<Regex, RezError> {
    let mut regex_str = String::new();
    regex_str.push('^');

    for ch in pattern.chars() {
        match ch {
            '*' => regex_str.push_str(".*"),
            '?' => regex_str.push('.'),
            '.' | '+' | '^' | '$' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\' => {
                regex_str.push('\\');
                regex_str.push(ch);
            }
            _ => regex_str.push(ch),
        }
    }

    regex_str.push('$');

    Regex::new(&regex_str).map_err(|e| {
        RezError::Parse(format!(
            "Failed to compile glob pattern '{}': {}",
            pattern, e
        ))
    })
}

/// Extract family name from pattern if deterministic (e.g., "maya-*" -> "maya")
fn extract_family(pattern: &str) -> Option<&str> {
    if let Some(pos) = pattern.find(['*', '?', '-']) {
        if pattern[pos..].starts_with('-') {
            return Some(&pattern[..pos]);
        }
    }
    None
}

static LABEL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([^(]+)\(([^()]+)\)$").unwrap());

/// Package filter with exclusion and inclusion rules
#[derive(Debug, Clone, Default)]
pub struct PackageFilter {
    /// Family-specific exclusion rules: family_name -> [rules]
    excludes: HashMap<String, Vec<Rule>>,
    /// Global exclusion rules (family = None)
    global_excludes: Vec<Rule>,
    /// Family-specific inclusion rules
    includes: HashMap<String, Vec<Rule>>,
    /// Global inclusion rules
    global_includes: Vec<Rule>,
}

impl PackageFilter {
    /// Decode a Rez `PackageFilter` POD mapping.
    ///
    /// Rez reads `excludes` and `includes` as either a single rule string or a
    /// list of rule strings, and ignores additional mapping keys.
    pub fn from_pod(value: &Value) -> Result<Self, RezError> {
        let object = value
            .as_object()
            .ok_or_else(|| RezError::Parse("package filter POD entry must be an object".into()))?;
        let mut filter = Self::new();
        for (field, add_rule) in [("excludes", true), ("includes", false)] {
            let Some(value) = object.get(field) else {
                continue;
            };
            let rule_strings: Vec<&str> = match value {
                Value::String(rule) => vec![rule],
                Value::Array(rules) => rules
                    .iter()
                    .enumerate()
                    .map(|(index, rule)| {
                        rule.as_str().ok_or_else(|| {
                            RezError::Parse(format!(
                                "package filter {field}[{index}] must be a string"
                            ))
                        })
                    })
                    .collect::<std::result::Result<Vec<_>, _>>()?,
                _ => {
                    return Err(RezError::Parse(format!(
                        "package filter {field} must be a string or array of strings"
                    )));
                }
            };
            for rule_string in rule_strings {
                let rule = Rule::parse(rule_string).map_err(|error| {
                    RezError::Parse(format!(
                        "invalid package filter {field} rule {rule_string:?}: {error}"
                    ))
                })?;
                let family = rule.family().map(str::to_owned);
                if add_rule {
                    filter.add_exclusion(rule, family.as_deref());
                } else {
                    filter.add_inclusion(rule, family.as_deref());
                }
            }
        }
        Ok(filter)
    }

    /// Encode this filter as Rez's canonical POD mapping.
    pub fn to_pod(&self) -> Value {
        let mut object = Map::new();
        for (field, family_rules, global_rules) in [
            ("excludes", &self.excludes, &self.global_excludes),
            ("includes", &self.includes, &self.global_includes),
        ] {
            let mut rules: Vec<(&Rule, String)> = family_rules
                .values()
                .chain(std::iter::once(global_rules))
                .flat_map(|rules| rules.iter())
                .map(|rule| (rule, rule.to_pod_string()))
                .collect();
            rules.sort_by(|(left_rule, left), (right_rule, right)| {
                left_rule
                    .cost()
                    .cmp(&right_rule.cost())
                    .then_with(|| left.cmp(right))
            });
            if !rules.is_empty() {
                object.insert(
                    field.to_string(),
                    Value::Array(
                        rules
                            .into_iter()
                            .map(|(_, rule)| Value::String(rule))
                            .collect(),
                    ),
                );
            }
        }
        Value::Object(object)
    }

    /// Create empty filter
    pub fn new() -> Self {
        Self::default()
    }

    /// Add exclusion rule (family=None for global)
    pub fn add_exclusion(&mut self, rule: Rule, family: Option<&str>) {
        if let Some(fam) = family {
            self.excludes.entry(fam.to_string()).or_default().push(rule);
        } else {
            self.global_excludes.push(rule);
        }
        self.sort_rules();
    }

    /// Add inclusion rule (family=None for global)
    pub fn add_inclusion(&mut self, rule: Rule, family: Option<&str>) {
        if let Some(fam) = family {
            self.includes.entry(fam.to_string()).or_default().push(rule);
        } else {
            self.global_includes.push(rule);
        }
        self.sort_rules();
    }

    /// Check if package is excluded, return matching rule
    pub fn excludes(&self, name: &str, version: &Version) -> Option<&Rule> {
        // Check family-specific exclusions
        if let Some(rules) = self.excludes.get(name) {
            if let Some(rule) = rules.iter().find(|r| r.matches_nv(name, version)) {
                // Check if inclusion overrides
                if self.is_included(name, version) {
                    return None;
                }
                return Some(rule);
            }
        }

        // Check global exclusions
        if let Some(rule) = self
            .global_excludes
            .iter()
            .find(|r| r.matches_nv(name, version))
        {
            if self.is_included(name, version) {
                return None;
            }
            return Some(rule);
        }

        None
    }

    /// Check if a full package is excluded, including timestamp rules.
    pub fn excludes_package(&self, package: &Package) -> Option<&Rule> {
        let name = package.name.as_str();
        let excluded = self
            .excludes
            .get(name)
            .and_then(|rules| rules.iter().find(|rule| rule.matches(package)))
            .or_else(|| {
                self.global_excludes
                    .iter()
                    .find(|rule| rule.matches(package))
            });

        excluded.filter(|_| !self.is_included_package(package))
    }

    /// Simple boolean check for exclusion
    pub fn is_excluded(&self, name: &str, version: &Version) -> bool {
        self.excludes(name, version).is_some()
    }

    /// Simple boolean check for exclusion using the complete package metadata.
    pub fn is_excluded_package(&self, package: &Package) -> bool {
        self.excludes_package(package).is_some()
    }

    /// Check if package matches any inclusion rule
    fn is_included_package(&self, package: &Package) -> bool {
        self.includes
            .get(&package.name)
            .is_some_and(|rules| rules.iter().any(|rule| rule.matches(package)))
            || self
                .global_includes
                .iter()
                .any(|rule| rule.matches(package))
    }

    /// Check if package matches any inclusion rule by name and version.
    fn is_included(&self, name: &str, version: &Version) -> bool {
        // Check family-specific inclusions
        if let Some(rules) = self.includes.get(name) {
            if rules.iter().any(|r| r.matches_nv(name, version)) {
                return true;
            }
        }

        // Check global inclusions
        self.global_includes
            .iter()
            .any(|r| r.matches_nv(name, version))
    }

    /// Sort rules by cost (cheaper first)
    fn sort_rules(&mut self) {
        for rules in self.excludes.values_mut() {
            rules.sort_by_key(|r| r.cost());
        }
        self.global_excludes.sort_by_key(|r| r.cost());

        for rules in self.includes.values_mut() {
            rules.sort_by_key(|r| r.cost());
        }
        self.global_includes.sort_by_key(|r| r.cost());
    }

    /// Calculate total cost of filter
    pub fn cost(&self) -> f64 {
        let mut total = 0.0;

        for rules in self.excludes.values() {
            total += rules.iter().map(|r| r.cost() as f64 / 10.0).sum::<f64>();
        }
        total += self
            .global_excludes
            .iter()
            .map(|r| r.cost() as f64)
            .sum::<f64>();

        total
    }
}

/// List of package filters (package excluded if ANY filter excludes it)
#[derive(Debug, Default, Clone)]
pub struct PackageFilterList {
    filters: Vec<PackageFilter>,
}

impl PackageFilterList {
    /// Decode a Rez package-filter POD list.
    pub fn from_pod(value: &Value) -> Result<Self, RezError> {
        let filters = value
            .as_array()
            .ok_or_else(|| RezError::Parse("package_filter POD must be an array".into()))?;
        let mut list = Self::new();
        for (index, filter) in filters.iter().enumerate() {
            let filter = PackageFilter::from_pod(filter).map_err(|error| {
                RezError::Parse(format!("package_filter[{index}] is invalid: {error}"))
            })?;
            list.add_filter(filter);
        }
        Ok(list)
    }

    /// Encode this list as Rez's package-filter POD list.
    pub fn to_pod(&self) -> Value {
        Value::Array(self.filters.iter().map(PackageFilter::to_pod).collect())
    }

    /// Create empty filter list
    pub fn new() -> Self {
        Self::default()
    }

    /// Add filter to list (sorted by cost)
    pub fn add_filter(&mut self, filter: PackageFilter) {
        self.filters.push(filter);
        self.filters.sort_by(|a, b| {
            a.cost()
                .partial_cmp(&b.cost())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    /// Check if package is excluded by any filter
    pub fn excludes(&self, name: &str, version: &Version) -> Option<&Rule> {
        for filter in &self.filters {
            if let Some(rule) = filter.excludes(name, version) {
                return Some(rule);
            }
        }
        None
    }

    /// Check if a full package is excluded by any filter.
    pub fn excludes_package(&self, package: &Package) -> Option<&Rule> {
        self.filters
            .iter()
            .find_map(|filter| filter.excludes_package(package))
    }

    /// Simple boolean check for exclusion
    pub fn is_excluded(&self, name: &str, version: &Version) -> bool {
        self.excludes(name, version).is_some()
    }

    /// Simple boolean check for exclusion using the complete package metadata.
    pub fn is_excluded_package(&self, package: &Package) -> bool {
        self.excludes_package(package).is_some()
    }

    /// Check if list has any filters
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    #[test]
    fn test_rule_glob() {
        let rule = Rule::parse("maya*").unwrap();
        assert!(rule.matches_nv("maya", &v("2018.1")));
        assert!(rule.matches_nv("maya_tools", &v("1.0.0")));
        assert!(!rule.matches_nv("houdini", &v("18.0")));
    }

    #[test]
    fn test_rule_family() {
        let rule = Rule::parse("maya").unwrap();
        assert!(rule.matches_nv("maya", &v("2018.1")));
        assert!(!rule.matches_nv("maya_tools", &v("1.0.0")));
    }

    #[test]
    fn test_rule_range() {
        let rule = Rule::parse("maya-1.0+<2.0").unwrap();
        assert!(rule.matches_nv("maya", &v("1.5")));
        assert!(!rule.matches_nv("maya", &v("2.5")));
        assert!(!rule.matches_nv("houdini", &v("1.5")));
    }

    #[test]
    fn test_rule_regex() {
        let rule = Rule::parse("regex(.*\\.beta$)").unwrap();
        assert!(rule.matches_nv("foo", &v("1.0.beta")));
        assert!(!rule.matches_nv("foo", &v("1.0.alpha")));
    }

    #[test]
    fn test_package_filter_exclude() {
        let mut filter = PackageFilter::new();
        filter.add_exclusion(Rule::parse("maya-<2.0").unwrap(), Some("maya"));

        assert!(filter.is_excluded("maya", &v("1.5")));
        assert!(!filter.is_excluded("maya", &v("2.5")));
    }

    #[test]
    fn test_package_filter_include_override() {
        let mut filter = PackageFilter::new();
        filter.add_exclusion(Rule::parse("*-*.beta").unwrap(), None);
        filter.add_inclusion(Rule::parse("maya-*.beta").unwrap(), Some("maya"));

        // Global exclusion of all .beta versions
        assert!(filter.is_excluded("houdini", &v("1.0.beta")));
        // But maya .beta versions are included
        assert!(!filter.is_excluded("maya", &v("1.0.beta")));
    }

    #[test]
    fn test_package_filter_list() {
        let mut list = PackageFilterList::new();

        let mut filter1 = PackageFilter::new();
        filter1.add_exclusion(Rule::parse("maya-<2.0").unwrap(), Some("maya"));

        let mut filter2 = PackageFilter::new();
        filter2.add_exclusion(Rule::parse("houdini-<18.0").unwrap(), Some("houdini"));

        list.add_filter(filter1);
        list.add_filter(filter2);

        assert!(list.is_excluded("maya", &v("1.5")));
        assert!(list.is_excluded("houdini", &v("17.0")));
        assert!(!list.is_excluded("maya", &v("2.5")));
        assert!(!list.is_excluded("houdini", &v("18.5")));
    }

    #[test]
    fn test_rule_cost() {
        let family = Rule::parse("maya").unwrap();
        let range = Rule::parse("maya-1.0+").unwrap();
        let glob = Rule::parse("maya*").unwrap();

        assert_eq!(family.cost(), 1);
        assert_eq!(range.cost(), 5);
        assert_eq!(glob.cost(), 10);
    }

    #[test]
    fn test_glob_patterns() {
        let rule = Rule::parse("glob(*-*.beta)").unwrap();
        assert!(rule.matches_nv("foo", &v("1.0.beta")));
        assert!(!rule.matches_nv("foo", &v("1.0.alpha")));

        let rule = Rule::parse("maya-*").unwrap();
        assert!(rule.matches_nv("maya", &v("2018.1")));
        assert!(!rule.matches_nv("houdini", &v("18.0")));
    }

    #[test]
    fn test_package_filter_pod_round_trip_uses_rez_shape() {
        let pod = serde_json::json!([
            {
                "excludes": ["before(maya:1700000000)", "glob(maya-*.beta)"],
                "includes": "after(houdini:1600000000)",
                "upstream_extension": "ignored by Rez"
            }
        ]);

        let filters = PackageFilterList::from_pod(&pod).unwrap();
        let round_trip = filters.to_pod();
        assert_eq!(
            round_trip,
            serde_json::json!([
                {
                    "excludes": ["glob(maya-*.beta)", "before(maya:1700000000)"],
                    "includes": ["after(houdini:1600000000)"]
                }
            ])
        );

        let filter = &filters.filters[0];
        assert!(filter.is_excluded("maya", &v("1.0.beta")));
    }

    #[test]
    fn test_empty_package_filter_pod() {
        let filters = PackageFilterList::from_pod(&serde_json::json!([])).unwrap();
        assert!(filters.is_empty());
        assert_eq!(filters.to_pod(), serde_json::json!([]));

        let filter = PackageFilter::from_pod(&serde_json::json!({})).unwrap();
        assert_eq!(filter.to_pod(), serde_json::json!({}));
    }

    #[test]
    fn test_timestamp_filter_grammar_and_behavior() {
        let before = Rule::parse("before(maya:100)").unwrap();
        let after = Rule::parse("after(100)").unwrap();
        let mut package = Package::new("maya", v("1.0"));

        package.timestamp = Some(100);
        assert!(before.matches(&package));
        assert!(!after.matches(&package));

        package.timestamp = Some(101);
        assert!(!before.matches(&package));
        assert!(after.matches(&package));

        package.timestamp = None;
        assert!(!before.matches(&package));
        assert!(!after.matches(&package));
        package.timestamp = Some(0);
        assert!(!before.matches(&package));

        assert_eq!(before.to_pod_string(), "before(maya:100)");
        assert_eq!(after.to_pod_string(), "after(100)");
        assert!(Rule::parse("before(-5)").is_ok());
        assert!(Rule::parse("after(maya:not-an-epoch)").is_err());
    }

    #[test]
    fn test_package_aware_timestamp_filters_are_family_scoped() {
        let before = Rule::parse("before(maya:100)").unwrap();
        let after = Rule::parse("after(houdini:100)").unwrap();
        let mut maya = Package::new("maya", v("1.0"));
        maya.timestamp = Some(50);
        let mut houdini = Package::new("houdini", v("1.0"));
        houdini.timestamp = Some(101);
        let mut other = Package::new("other", v("1.0"));
        other.timestamp = Some(101);

        assert!(before.matches(&maya));
        assert!(!before.matches(&houdini));
        assert!(after.matches(&houdini));
        assert!(!after.matches(&maya));
        assert!(!after.matches(&other));

        let mut filter = PackageFilter::new();
        filter.add_exclusion(before, Some("maya"));
        filter.add_exclusion(after, Some("houdini"));
        assert!(filter.is_excluded_package(&maya));
        assert!(filter.is_excluded_package(&houdini));
        assert!(!filter.is_excluded_package(&other));
        assert!(!PackageFilterList::new().is_excluded_package(&maya));
    }

    #[test]
    fn test_package_aware_timestamp_inclusion_overrides_exclusion() {
        let mut filter = PackageFilter::new();
        filter.add_exclusion(Rule::parse("before(maya:100)").unwrap(), Some("maya"));
        filter.add_inclusion(Rule::parse("before(maya:100)").unwrap(), Some("maya"));
        let mut package = Package::new("maya", v("1.0"));
        package.timestamp = Some(50);

        assert!(!filter.is_excluded_package(&package));
        assert!(!filter.is_excluded("maya", &package.version));
    }

    #[test]
    fn test_package_aware_timestamp_rules_ignore_untimestamped_and_zero() {
        let mut filter = PackageFilter::new();
        filter.add_exclusion(Rule::parse("before(100)").unwrap(), None);
        let package = Package::new("maya", v("1.0"));

        assert!(!filter.is_excluded_package(&package));
        let mut zero_timestamp = package.clone();
        zero_timestamp.timestamp = Some(0);
        assert!(!filter.is_excluded_package(&zero_timestamp));
        let mut matching_timestamp = package;
        matching_timestamp.timestamp = Some(100);
        assert!(filter.is_excluded_package(&matching_timestamp));
    }

    #[test]
    fn test_package_filter_pod_rejects_malformed_values() {
        assert!(PackageFilterList::from_pod(&serde_json::json!({})).is_err());
        assert!(PackageFilterList::from_pod(&serde_json::json!([null])).is_err());
        assert!(PackageFilterList::from_pod(&serde_json::json!([
            {"excludes": null}
        ]))
        .is_err());
        assert!(PackageFilterList::from_pod(&serde_json::json!([
            {"excludes": ["maya", 7]}
        ]))
        .is_err());
        assert!(PackageFilterList::from_pod(&serde_json::json!([
            {"includes": "before(maya:today)"}
        ]))
        .is_err());
    }
}
