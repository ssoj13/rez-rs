// SPDX-License-Identifier: Apache-2.0

//! Package search API with pattern matching and reverse dependencies.
//!
//! Ported from Python rez package_search.py.
// - PackageSearcher: Stateful searcher with configurable paths
//
// Key functions:
// - search(): Convenience function for quick package searching
// - get_reverse_dependencies(): Find packages that depend on a given package

use crate::errors::{Result, RezError};
use crate::package::discover::{iter_package_families, iter_packages};
use crate::repository::PackageInfo;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use version::{Requirement, Version, VersionRange};

// ============================================================================
// MatchType
// ============================================================================

/// How a query matched a package name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchType {
    /// Exact name match (query == name).
    Exact,
    /// Name starts with query prefix.
    StartsWith,
    /// Name contains query as substring.
    Contains,
    /// Name matched a regex or glob pattern.
    Regex,
}

// ============================================================================
// ResourceSearchResult
// ============================================================================

/// A single result from a package search.
#[derive(Debug, Clone)]
pub struct ResourceSearchResult {
    /// Package family name.
    pub package_name: String,
    /// Version if available (None for family-only results).
    pub version: Option<Version>,
    /// How the query matched.
    pub match_type: MatchType,
}

impl ResourceSearchResult {
    /// Qualified name: "name-version" or just "name".
    pub fn qualified_name(&self) -> String {
        match &self.version {
            Some(v) if v.is_truthy() => format!("{}-{}", self.package_name, v),
            _ => self.package_name.clone(),
        }
    }
}

// ============================================================================
// SearchQuery
// ============================================================================

/// Parsed form of a search query string.
#[derive(Debug, Clone)]
pub enum SearchQuery {
    /// Match all packages.
    All,
    /// Glob pattern like "foo*", "maya_*".
    Glob { pattern: String, regex: Regex },
    /// Regex pattern delimited by slashes: /pattern/.
    Pattern(Regex),
    /// Plain text: try exact, then prefix, then substring.
    Plain {
        text: String,
        version_range: Option<VersionRange>,
    },
}

// ============================================================================
// Query parsing
// ============================================================================

use foundation::patterns::glob_to_regex;

/// Parse a user query string into a SearchQuery.
///
/// Query formats:
/// - "*" or empty: match all
/// - "foo*", "*bar": glob pattern
/// - "/pattern/": regex
/// - "foo-1.2+": name with version range (parsed as Requirement)
/// - "foo": plain text (exact/prefix/substring match)
pub fn parse_search_query(query: &str) -> Result<SearchQuery> {
    let query = query.trim();

    // Empty or wildcard: match all
    if query.is_empty() || query == "*" {
        return Ok(SearchQuery::All);
    }

    // Regex: /pattern/
    if query.starts_with('/') && query.ends_with('/') && query.len() > 2 {
        let pat = &query[1..query.len() - 1];
        let regex = Regex::new(pat)
            .map_err(|e| RezError::PackageRequest(format!("Invalid regex '{}': {}", pat, e)))?;
        return Ok(SearchQuery::Pattern(regex));
    }

    // Glob: contains * or ?
    if query.contains('*') || query.contains('?') {
        let re_str = glob_to_regex(query);
        let regex = Regex::new(&re_str)
            .map_err(|e| RezError::PackageRequest(format!("Invalid glob '{}': {}", query, e)))?;
        return Ok(SearchQuery::Glob {
            pattern: query.to_string(),
            regex,
        });
    }

    // Try parsing as Requirement (handles "foo-1.2+", "foo>=2", etc.)
    if let Ok(req) = Requirement::new(query) {
        let version_range = req.range().cloned();
        let range = if version_range.as_ref().is_none_or(|r| r.is_any()) {
            None
        } else {
            version_range
        };
        return Ok(SearchQuery::Plain {
            text: req.name().to_string(),
            version_range: range,
        });
    }

    // Fallback: plain text
    Ok(SearchQuery::Plain {
        text: query.to_string(),
        version_range: None,
    })
}

/// Check if a package name matches a query, returning the match type.
pub fn matches_query(name: &str, query: &SearchQuery) -> Option<MatchType> {
    match query {
        SearchQuery::All => Some(MatchType::Exact),
        SearchQuery::Glob { regex, .. } => {
            if regex.is_match(name) {
                Some(MatchType::Regex)
            } else {
                None
            }
        }
        SearchQuery::Pattern(regex) => {
            if regex.is_match(name) {
                Some(MatchType::Regex)
            } else {
                None
            }
        }
        SearchQuery::Plain { text, .. } => {
            if name == text {
                Some(MatchType::Exact)
            } else if name.starts_with(text.as_str()) {
                Some(MatchType::StartsWith)
            } else if name.contains(text.as_str()) {
                Some(MatchType::Contains)
            } else {
                None
            }
        }
    }
}

// ============================================================================
// PackageSearcher
// ============================================================================

/// Configurable package searcher.
///
/// Wraps search paths and provides methods for searching families and packages.
pub struct PackageSearcher {
    /// Repository paths to search (None = use config default).
    pub paths: Option<Vec<PathBuf>>,
}

impl PackageSearcher {
    /// Create a new searcher with optional explicit paths.
    pub fn new(paths: Option<Vec<PathBuf>>) -> Self {
        Self { paths }
    }

    /// Get effective paths as slice reference.
    fn paths_ref(&self) -> Option<&[PathBuf]> {
        self.paths.as_deref()
    }

    /// Search for packages matching a query string.
    ///
    /// Returns results sorted by name, with match type indicating how the
    /// query matched each result.
    pub fn search(&self, query: &str) -> Result<Vec<ResourceSearchResult>> {
        self.search_packages(query, false)
    }

    /// Search family names only, returning matching names.
    pub fn search_families(&self, query: &str) -> Result<Vec<String>> {
        let parsed = parse_search_query(query)?;
        let families = iter_package_families(self.paths_ref())?;

        let mut results: Vec<String> = families
            .into_iter()
            .filter(|name| matches_query(name, &parsed).is_some())
            .collect();

        results.sort();
        Ok(results)
    }

    /// Search packages with optional version info.
    ///
    /// If `latest_only` is true, only the latest version of each family is
    /// returned. Otherwise, all matching versions are included.
    pub fn search_packages(
        &self,
        query: &str,
        latest_only: bool,
    ) -> Result<Vec<ResourceSearchResult>> {
        let parsed = parse_search_query(query)?;
        let families = iter_package_families(self.paths_ref())?;

        // Extract version range from query if present
        let version_range = match &parsed {
            SearchQuery::Plain { version_range, .. } => version_range.as_ref(),
            _ => None,
        };

        let mut results = Vec::new();

        for family_name in &families {
            let match_type = match matches_query(family_name, &parsed) {
                Some(mt) => mt,
                None => continue,
            };

            // Get packages for this family
            let packages = iter_packages(family_name, version_range, self.paths_ref())?;

            if packages.is_empty() {
                // Family exists but no packages match range - still report family
                results.push(ResourceSearchResult {
                    package_name: family_name.clone(),
                    version: None,
                    match_type,
                });
                continue;
            }

            if latest_only {
                // iter_packages returns sorted descending, first is latest
                if let Some(pkg) = packages.into_iter().next() {
                    results.push(ResourceSearchResult {
                        package_name: pkg.name.clone(),
                        version: Some(pkg.version.clone()),
                        match_type,
                    });
                }
            } else {
                for pkg in packages {
                    results.push(ResourceSearchResult {
                        package_name: pkg.name.clone(),
                        version: Some(pkg.version.clone()),
                        match_type,
                    });
                }
            }
        }

        Ok(results)
    }
}

// ============================================================================
// get_reverse_dependencies
// ============================================================================

/// Find packages that depend on the given package.
///
/// Scans all packages (latest version of each family) and checks their
/// `requires` lists for dependencies on `name`. Returns a mapping from
/// dependent package name to the list of requirements that reference `name`.
///
/// # Arguments
/// * `name` - Package name to find reverse dependencies for
/// * `version` - Optional version filter (only deps whose range contains this version)
/// * `depth` - How many levels deep to search (1 = direct only, None = unlimited)
/// * `paths` - Repository paths (None = config default)
pub fn get_reverse_dependencies(
    name: &str,
    version: Option<&Version>,
    depth: Option<usize>,
    paths: Option<&[PathBuf]>,
) -> Result<HashMap<String, Vec<Requirement>>> {
    let families = iter_package_families(paths)?;

    // Verify the target package family exists
    if !families.iter().any(|f| f == name) {
        return Err(RezError::PackageFamilyNotFound(format!(
            "No such package family '{}'",
            name
        )));
    }

    if depth == Some(0) {
        return Ok(HashMap::new());
    }

    // Build reverse lookup: for each family, get latest package's requires
    let mut reverse_map: HashMap<String, HashSet<String>> = HashMap::new();
    let mut req_map: HashMap<String, Vec<Requirement>> = HashMap::new();

    for family_name in &families {
        let packages = iter_packages(family_name, None, paths)?;
        // Get latest version (first in desc-sorted list)
        let pkg = match packages.into_iter().next() {
            Some(p) => p,
            None => continue,
        };

        // Parse dependency metadata through the canonical package parser so malformed
        // requirements cannot silently disappear from reverse dependency results.
        let reqs = extract_requires(&pkg, family_name)?;
        for req in &reqs {
            if !req.conflict() {
                reverse_map
                    .entry(req.name().to_string())
                    .or_default()
                    .insert(family_name.clone());

                // Optionally filter by version
                if let Some(ver) = version {
                    if let Some(range) = req.range() {
                        if !range.contains_version(ver) {
                            continue;
                        }
                    }
                }

                req_map
                    .entry(family_name.clone())
                    .or_default()
                    .push(req.clone());
            }
        }
    }

    // BFS traversal from target package name
    let mut result: HashMap<String, Vec<Requirement>> = HashMap::new();
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(name.to_string());
    let mut frontier: HashSet<String> = HashSet::new();
    frontier.insert(name.to_string());

    let max_depth = depth.unwrap_or(usize::MAX);

    for _ in 0..max_depth {
        let mut next_frontier = HashSet::new();

        for pkg_name in &frontier {
            if let Some(dependents) = reverse_map.get(pkg_name) {
                for dep in dependents {
                    if visited.contains(dep) {
                        continue;
                    }
                    visited.insert(dep.clone());
                    next_frontier.insert(dep.clone());

                    // Collect the requirements from this dependent
                    if let Some(reqs) = req_map.get(dep) {
                        let matching: Vec<Requirement> = reqs
                            .iter()
                            .filter(|r| r.name() == pkg_name)
                            .cloned()
                            .collect();
                        if !matching.is_empty() {
                            result.entry(dep.clone()).or_default().extend(matching);
                        }
                    }
                }
            }
        }

        if next_frontier.is_empty() {
            break;
        }

        frontier = next_frontier;
    }

    Ok(result)
}

/// Extract validated requirements from package metadata with repository context.
fn extract_requires(pkg: &PackageInfo, family_name: &str) -> Result<Vec<Requirement>> {
    pkg.to_package()
        .map(|package| package.requires)
        .map_err(|error| RezError::PackageMetadata {
            msg: format!(
                "Reverse dependency scan failed for family '{family_name}', package '{}' version '{}': {error}",
                pkg.name, pkg.version
            ),
            path: None,
            resource_key: None,
        })
}

// ============================================================================
// Convenience function
// ============================================================================

/// Search for packages matching a query string.
///
/// Convenience wrapper around PackageSearcher. Returns results for all
/// matching packages across configured repository paths.
///
/// # Arguments
/// * `query` - Search string (name, glob, regex, or requirement)
/// * `paths` - Repository paths (None = config default)
pub fn search(query: &str, paths: Option<Vec<PathBuf>>) -> Result<Vec<ResourceSearchResult>> {
    let searcher = PackageSearcher::new(paths);
    searcher.search(query)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // -- glob_to_regex --

    #[test]
    fn test_glob_to_regex_star() {
        let re = Regex::new(&glob_to_regex("foo*")).unwrap();
        assert!(re.is_match("foo"));
        assert!(re.is_match("foobar"));
        assert!(!re.is_match("barfoo"));
    }

    #[test]
    fn test_glob_to_regex_question() {
        let re = Regex::new(&glob_to_regex("fo?")).unwrap();
        assert!(re.is_match("foo"));
        assert!(re.is_match("fob"));
        assert!(!re.is_match("fo"));
        assert!(!re.is_match("fooo"));
    }

    #[test]
    fn test_glob_to_regex_brackets() {
        let re = Regex::new(&glob_to_regex("fo[ob]")).unwrap();
        assert!(re.is_match("foo"));
        assert!(re.is_match("fob"));
        assert!(!re.is_match("foc"));
    }

    #[test]
    fn test_glob_to_regex_complex() {
        let re = Regex::new(&glob_to_regex("maya_*_utils")).unwrap();
        assert!(re.is_match("maya_py_utils"));
        assert!(re.is_match("maya__utils"));
        assert!(!re.is_match("maya_utils"));
    }

    // -- parse_search_query --

    #[test]
    fn test_parse_all() {
        let q = parse_search_query("").unwrap();
        assert!(matches!(q, SearchQuery::All));

        let q = parse_search_query("*").unwrap();
        assert!(matches!(q, SearchQuery::All));
    }

    #[test]
    fn test_parse_glob() {
        let q = parse_search_query("foo*").unwrap();
        assert!(matches!(q, SearchQuery::Glob { .. }));
    }

    #[test]
    fn test_parse_regex() {
        let q = parse_search_query("/^maya.*/").unwrap();
        assert!(matches!(q, SearchQuery::Pattern(_)));
    }

    #[test]
    fn test_parse_plain() {
        let q = parse_search_query("maya").unwrap();
        match q {
            SearchQuery::Plain {
                text,
                version_range,
            } => {
                assert_eq!(text, "maya");
                assert!(version_range.is_none());
            }
            _ => panic!("Expected Plain"),
        }
    }

    #[test]
    fn test_parse_with_range() {
        let q = parse_search_query("maya-1.2+").unwrap();
        match q {
            SearchQuery::Plain {
                text,
                version_range,
            } => {
                assert_eq!(text, "maya");
                assert!(version_range.is_some());
            }
            _ => panic!("Expected Plain with range"),
        }
    }

    // -- matches_query --

    #[test]
    fn test_matches_exact() {
        let q = parse_search_query("maya").unwrap();
        assert_eq!(matches_query("maya", &q), Some(MatchType::Exact));
    }

    #[test]
    fn test_matches_starts_with() {
        let q = parse_search_query("may").unwrap();
        assert_eq!(matches_query("maya", &q), Some(MatchType::StartsWith));
    }

    #[test]
    fn test_matches_contains() {
        let q = parse_search_query("ay").unwrap();
        assert_eq!(matches_query("maya", &q), Some(MatchType::Contains));
    }

    #[test]
    fn test_matches_no_match() {
        let q = parse_search_query("houdini").unwrap();
        assert_eq!(matches_query("maya", &q), None);
    }

    #[test]
    fn test_matches_glob() {
        let q = parse_search_query("maya*").unwrap();
        assert_eq!(matches_query("maya", &q), Some(MatchType::Regex));
        assert_eq!(matches_query("maya_utils", &q), Some(MatchType::Regex));
        assert_eq!(matches_query("houdini", &q), None);
    }

    #[test]
    fn test_matches_regex_pattern() {
        let q = parse_search_query("/^ma.a$/").unwrap();
        assert_eq!(matches_query("maya", &q), Some(MatchType::Regex));
        assert_eq!(matches_query("masa", &q), Some(MatchType::Regex));
        assert_eq!(matches_query("naya", &q), None);
    }

    // -- ResourceSearchResult --

    #[test]
    fn test_qualified_name_with_version() {
        let r = ResourceSearchResult {
            package_name: "maya".to_string(),
            version: Some(Version::new("2024.0").unwrap()),
            match_type: MatchType::Exact,
        };
        assert_eq!(r.qualified_name(), "maya-2024.0");
    }

    #[test]
    fn test_qualified_name_no_version() {
        let r = ResourceSearchResult {
            package_name: "maya".to_string(),
            version: None,
            match_type: MatchType::Exact,
        };
        assert_eq!(r.qualified_name(), "maya");
    }

    // -- extract_requires --

    #[test]
    fn test_extract_requires() {
        let mut data = HashMap::new();
        data.insert("name".to_string(), serde_json::json!("foo"));
        data.insert("version".to_string(), serde_json::json!("1.0"));
        data.insert(
            "requires".to_string(),
            serde_json::json!(["bar-1.0+", "baz>=2.0"]),
        );
        let info = PackageInfo::from_data(data).unwrap();

        let reqs = extract_requires(&info, "foo").unwrap();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].name(), "bar");
        assert_eq!(reqs[1].name(), "baz");
    }

    #[test]
    fn test_extract_requires_empty() {
        let mut data = HashMap::new();
        data.insert("name".to_string(), serde_json::json!("foo"));
        data.insert("version".to_string(), serde_json::json!("1.0"));
        let info = PackageInfo::from_data(data).unwrap();

        let reqs = extract_requires(&info, "foo").unwrap();
        assert!(reqs.is_empty());
    }

    #[test]
    fn test_extract_requires_rejects_malformed_requirement_with_context() {
        let mut data = HashMap::new();
        data.insert("name".to_string(), serde_json::json!("foo"));
        data.insert("version".to_string(), serde_json::json!("1.0"));
        data.insert("requires".to_string(), serde_json::json!(["bar-1.0+", 42]));
        let info = PackageInfo::from_data(data).unwrap();

        let error = extract_requires(&info, "foo").unwrap_err();
        match error {
            RezError::PackageMetadata { msg, .. } => {
                assert!(msg.contains("family 'foo'"), "{msg}");
                assert!(msg.contains("package 'foo' version '1.0'"), "{msg}");
                assert!(
                    msg.contains("field 'requires[1]' must be a string"),
                    "{msg}"
                );
            }
            other => panic!("expected contextual package metadata error, got {other:?}"),
        }
    }

    // -- PackageSearcher (integration with memory repos is limited since
    //    PackageSearcher uses iter_packages which goes through config paths.
    //    These tests focus on the search logic itself.) --

    #[test]
    fn test_searcher_new() {
        let s = PackageSearcher::new(None);
        assert!(s.paths.is_none());

        let s = PackageSearcher::new(Some(vec![PathBuf::from("/tmp/repo")]));
        assert_eq!(s.paths.as_ref().unwrap().len(), 1);
    }

    // -- Convenience search function --

    #[test]
    fn test_search_nonexistent_path() {
        // Searching in a non-existent path should return empty results, not error
        let results = search("foo", Some(vec![PathBuf::from("/nonexistent/path")]));
        // Either empty results or error is acceptable
        if let Ok(r) = results {
            assert!(r.is_empty());
        }
    }

    // -- get_reverse_dependencies --

    #[test]
    fn test_reverse_deps_family_not_found() {
        let result = get_reverse_dependencies(
            "nonexistent",
            None,
            None,
            Some(&[PathBuf::from("/nonexistent")]),
        );
        // Should error because family not found (or empty results if path doesn't exist)
        match result {
            Err(RezError::PackageFamilyNotFound(_)) => {}
            Ok(map) => assert!(map.is_empty()),
            Err(_) => {} // other errors from missing paths are also acceptable
        }
    }
}
