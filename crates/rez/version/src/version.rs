//! Version parsing, comparison, and manipulation.
//!
//! Ported from Python rez _version.py.

use super::token::AlphanumericToken;
use foundation::errors::RezError;
use regex::Regex;
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;
use std::sync::LazyLock;

static TOKEN_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[a-zA-Z0-9_]+").expect("Invalid token regex"));

/// Represents a version with support for infinity and empty versions.
///
/// Version ordering:
/// - Infinity > everything else
/// - Empty version < everything except empty
/// - Regular versions compared lexicographically by tokens
///
/// Examples:
/// - `Version::empty()` - smallest version, evaluates to false
/// - `Version::inf()` - largest version, infinity
/// - `Version::new("1.2.3")` - regular version
#[derive(Clone, Debug)]
pub struct Version {
    /// None = infinity, Some(vec) = normal or empty version
    tokens: Option<Vec<AlphanumericToken>>,
    seps: Vec<char>,
}

impl Version {
    /// Create a new version from a string.
    ///
    /// # Arguments
    /// * `ver_str` - Version string like "1.2.3" or "1-alpha-2"
    ///
    /// # Returns
    /// * `Ok(Version)` on success
    /// * `Err(RezError)` if parsing fails
    pub fn new(ver_str: &str) -> Result<Self, RezError> {
        if ver_str.is_empty() {
            return Ok(Self::empty());
        }

        let mut tokens = Vec::new();
        let mut seps = Vec::new();
        let mut last_end = 0;

        for mat in TOKEN_REGEX.find_iter(ver_str) {
            let start = mat.start();
            let end = mat.end();

            // Check for leading separators (invalid)
            if last_end == 0 && start > 0 {
                return Err(RezError::Version(format!(
                    "Version string cannot start with separator: '{}'",
                    ver_str
                )));
            }

            // Extract separator between tokens
            if start > last_end {
                let sep_str = &ver_str[last_end..start];
                if sep_str.len() != 1 {
                    return Err(RezError::Version(format!(
                        "Version separators must be single character, got '{}' in '{}'",
                        sep_str, ver_str
                    )));
                }
                seps.push(sep_str.chars().next().unwrap());
            }

            // Parse token
            let token_str = mat.as_str();
            let token = AlphanumericToken::new(token_str)?;
            tokens.push(token);

            last_end = end;
        }

        // Check for trailing separators (invalid)
        if last_end < ver_str.len() {
            return Err(RezError::Version(format!(
                "Version string cannot end with separator: '{}'",
                ver_str
            )));
        }

        Ok(Version {
            tokens: Some(tokens),
            seps,
        })
    }

    /// Create an empty version (smallest possible version).
    pub fn empty() -> Self {
        Version {
            tokens: Some(Vec::new()),
            seps: Vec::new(),
        }
    }

    /// Create infinity version (largest possible version).
    pub fn inf() -> Self {
        Version {
            tokens: None,
            seps: Vec::new(),
        }
    }

    /// Check if this is the infinity version.
    pub fn is_inf(&self) -> bool {
        self.tokens.is_none()
    }

    /// Check if this is an empty version (no tokens, but not infinity).
    pub fn is_empty(&self) -> bool {
        matches!(self.tokens, Some(ref t) if t.is_empty())
    }

    /// Get the next version using Rez's token semantics.
    ///
    /// - Empty version -> infinity
    /// - Regular version -> next value of the last token
    /// - Infinity -> infinity (unchanged)
    pub fn next(&self) -> Self {
        match &self.tokens {
            None => Self::inf(),                              // inf.next() = inf
            Some(tokens) if tokens.is_empty() => Self::inf(), // empty.next() = inf
            Some(tokens) => {
                let mut new_tokens = tokens.clone();
                if let Some(last_token) = new_tokens.last_mut() {
                    *last_token = last_token.next();
                }
                Version {
                    tokens: Some(new_tokens),
                    seps: self.seps.clone(),
                }
            }
        }
    }

    /// Create a deep copy of this version.
    pub fn copy(&self) -> Self {
        self.clone()
    }

    /// Create a new version with only the first n tokens.
    ///
    /// # Arguments
    /// * `n` - Number of tokens to keep
    ///
    /// # Returns
    /// New version trimmed to n tokens
    pub fn trim(&self, n: usize) -> Self {
        match &self.tokens {
            None => Self::inf(),
            Some(tokens) => {
                let new_tokens = tokens.iter().take(n).cloned().collect();
                let new_seps = self
                    .seps
                    .iter()
                    .take(n.saturating_sub(1))
                    .copied()
                    .collect();
                Version {
                    tokens: Some(new_tokens),
                    seps: new_seps,
                }
            }
        }
    }

    /// Get the major version component (first token).
    pub fn major(&self) -> Option<&AlphanumericToken> {
        self.get(0)
    }

    /// Get the minor version component (second token).
    pub fn minor(&self) -> Option<&AlphanumericToken> {
        self.get(1)
    }

    /// Get the patch version component (third token).
    pub fn patch(&self) -> Option<&AlphanumericToken> {
        self.get(2)
    }

    /// Convert version to a tuple of strings.
    pub fn as_tuple(&self) -> Vec<String> {
        match &self.tokens {
            None => vec!["INF".to_string()],
            Some(tokens) => tokens.iter().map(|t| t.to_string()).collect(),
        }
    }

    /// Get number of tokens in this version.
    pub fn len(&self) -> usize {
        match &self.tokens {
            None => 0,
            Some(tokens) => tokens.len(),
        }
    }

    /// Get a token by index.
    pub fn get(&self, index: usize) -> Option<&AlphanumericToken> {
        match &self.tokens {
            None => None,
            Some(tokens) => tokens.get(index),
        }
    }

    /// Check if version is truthy (has tokens and is not empty).
    pub fn is_truthy(&self) -> bool {
        match &self.tokens {
            None => true, // inf is truthy
            Some(tokens) => !tokens.is_empty(),
        }
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.tokens == other.tokens
    }
}

impl Eq for Version {}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        match (&self.tokens, &other.tokens) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater, // inf > everything
            (Some(_), None) => Ordering::Less,    // everything < inf
            (Some(self_tokens), Some(other_tokens)) => {
                // Lexicographic comparison of token vectors
                self_tokens.cmp(other_tokens)
            }
        }
    }
}

impl Hash for Version {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match &self.tokens {
            None => None::<()>.hash(state),
            Some(tokens) => {
                // Hash as tuple of token strings
                for token in tokens {
                    token.to_string().hash(state);
                }
            }
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.tokens {
            None => write!(f, "[INF]"),
            Some(tokens) => {
                if tokens.is_empty() {
                    write!(f, "")
                } else {
                    let parts: Vec<String> = tokens.iter().map(|t| t.to_string()).collect();
                    let mut result = parts[0].clone();
                    for (i, part) in parts.iter().enumerate().skip(1) {
                        let sep = self.seps.get(i - 1).unwrap_or(&'.');
                        result.push(*sep);
                        result.push_str(part);
                    }
                    write!(f, "{}", result)
                }
            }
        }
    }
}

impl FromStr for Version {
    type Err = RezError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Version::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_version() {
        let v = Version::empty();
        assert!(v.is_empty());
        assert!(!v.is_inf());
        assert_eq!(v.len(), 0);
        assert!(!v.is_truthy());
    }

    #[test]
    fn test_inf_version() {
        let v = Version::inf();
        assert!(v.is_inf());
        assert!(!v.is_empty());
        assert!(v.is_truthy());
    }

    #[test]
    fn test_parse_simple() {
        let v = Version::new("1.2.3").unwrap();
        assert_eq!(v.len(), 3);
        assert_eq!(v.to_string(), "1.2.3");
    }

    #[test]
    fn test_parse_with_dashes() {
        let v = Version::new("1-alpha-2").unwrap();
        assert_eq!(v.len(), 3);
        assert_eq!(v.to_string(), "1-alpha-2");
    }

    #[test]
    fn test_ordering() {
        let v1 = Version::new("1.0.0").unwrap();
        let v2 = Version::new("2.0.0").unwrap();
        let empty = Version::empty();
        let inf = Version::inf();

        assert!(empty < v1);
        assert!(v1 < v2);
        assert!(v2 < inf);
        assert!(empty < inf);
    }

    #[test]
    fn test_next() {
        let v = Version::new("1.2.3").unwrap();
        let next = v.next();
        assert_eq!(next.to_string(), "1.2.3_");

        let empty = Version::empty();
        let next_empty = empty.next();
        assert!(next_empty.is_inf());
    }

    #[test]
    fn test_trim() {
        let v = Version::new("1.2.3.4").unwrap();
        let trimmed = v.trim(2);
        assert_eq!(trimmed.to_string(), "1.2");
        assert_eq!(trimmed.len(), 2);
    }

    #[test]
    fn test_major_minor_patch() {
        let v = Version::new("1.2.3").unwrap();
        assert_eq!(v.major().map(|t| t.to_string()), Some("1".to_string()));
        assert_eq!(v.minor().map(|t| t.to_string()), Some("2".to_string()));
        assert_eq!(v.patch().map(|t| t.to_string()), Some("3".to_string()));
    }

    #[test]
    fn test_invalid_leading_separator() {
        let result = Version::new(".1.2.3");
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_trailing_separator() {
        let result = Version::new("1.2.3.");
        assert!(result.is_err());
    }

    #[test]
    fn test_as_tuple() {
        let v = Version::new("1.2.3").unwrap();
        assert_eq!(v.as_tuple(), vec!["1", "2", "3"]);

        let inf = Version::inf();
        assert_eq!(inf.as_tuple(), vec!["INF"]);
    }

    #[test]
    fn test_equality() {
        let v1 = Version::new("1.2.3").unwrap();
        let v2 = Version::new("1.2.3").unwrap();
        assert_eq!(v1, v2);
    }

    #[test]
    fn test_hash() {
        use std::collections::HashMap;
        let mut map = HashMap::new();
        let v = Version::new("1.2.3").unwrap();
        map.insert(v.clone(), "test");
        assert_eq!(map.get(&v), Some(&"test"));
    }
}
