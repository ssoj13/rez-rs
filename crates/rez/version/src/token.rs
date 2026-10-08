// SPDX-License-Identifier: Apache-2.0

//! Version token parsing and comparison.
//!
//! Ported from Python rez _version.py.

use foundation::errors::RezError;
use regex::Regex;
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;
use std::sync::LazyLock;

// Regex for splitting alphanumeric tokens into numeric/alpha subtokens
static NUMERIC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[0-9]+").expect("valid NUMERIC_RE pattern"));

/// Internal subtoken for AlphanumericToken parsing.
/// Contains its original representation and whether it is a numeric digit run.
/// Ordering: alphas < numbers; numbers compare by arbitrary-size decimal value,
/// then by their original spelling to preserve padding-sensitive order.
#[derive(Clone, Debug)]
pub struct SubToken {
    s: String,
    numeric: bool,
}

impl SubToken {
    /// Create a new SubToken from a string.
    /// Numeric classification is independent of machine integer limits.
    fn new(s: String) -> Self {
        let numeric = !s.is_empty() && s.bytes().all(|byte| byte.is_ascii_digit());
        Self { s, numeric }
    }
}

impl PartialEq for SubToken {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for SubToken {}

impl PartialOrd for SubToken {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SubToken {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.numeric, other.numeric) {
            (true, true) => {
                let lhs = self.s.trim_start_matches('0');
                let rhs = other.s.trim_start_matches('0');
                lhs.len()
                    .cmp(&rhs.len())
                    .then_with(|| lhs.cmp(rhs))
                    .then_with(|| self.s.cmp(&other.s))
            }
            // Both alpha: compare by string
            (false, false) => self.s.cmp(&other.s),
            // Alpha < Numeric
            (false, true) => Ordering::Less,
            (true, false) => Ordering::Greater,
        }
    }
}

impl Hash for SubToken {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.s.hash(state);
    }
}

impl fmt::Display for SubToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.s)
    }
}

/// Main version token type - alphanumeric token with subtoken parsing.
/// Represents a version component like "1alpha2" as subtokens ["1", "alpha", "2"].
/// Ordering is lexicographic comparison of subtoken lists.
#[derive(Clone, Debug)]
pub struct AlphanumericToken {
    subtokens: Vec<SubToken>,
}

impl AlphanumericToken {
    /// Create a new AlphanumericToken from a string.
    /// Convenience wrapper for parse().
    pub fn new(s: &str) -> Result<Self, RezError> {
        Self::parse(s)
    }

    /// Parse a string into an AlphanumericToken.
    /// Splits by numeric regex [0-9]+ into alternating alpha/numeric groups.
    fn parse(s: &str) -> Result<Self, RezError> {
        // Validate: must match [a-zA-Z0-9_]+
        if s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(RezError::Version(format!(
                "Invalid token '{}': must match [a-zA-Z0-9_]+",
                s
            )));
        }

        let mut subtokens = Vec::new();
        let mut last_end = 0;

        // Find all numeric matches
        for mat in NUMERIC_RE.find_iter(s) {
            let start = mat.start();
            let end = mat.end();

            // Add alpha part before this numeric match (if any)
            if start > last_end {
                let alpha_part = &s[last_end..start];
                subtokens.push(SubToken::new(alpha_part.to_string()));
            }

            // Add numeric part
            subtokens.push(SubToken::new(mat.as_str().to_string()));
            last_end = end;
        }

        // Add remaining alpha part (if any)
        if last_end < s.len() {
            let alpha_part = &s[last_end..];
            subtokens.push(SubToken::new(alpha_part.to_string()));
        }

        // If no subtokens were created, the whole string is alpha
        if subtokens.is_empty() {
            subtokens.push(SubToken::new(s.to_string()));
        }

        Ok(Self { subtokens })
    }

    /// Get the next token in sequence, matching Rez's AlphanumericVersionToken.
    /// If the last subtoken is numeric, append an underscore subtoken; otherwise
    /// append '_' to the last alpha subtoken.
    pub fn next(&self) -> Self {
        let mut new_subtokens = self.subtokens.clone();

        if new_subtokens.last().is_some_and(|last| last.numeric) {
            new_subtokens.push(SubToken::new("_".to_string()));
        } else if let Some(last) = new_subtokens.last_mut() {
            last.s.push('_');
        } else {
            // Empty subtokens (shouldn't happen with valid token): create "_"
            new_subtokens.push(SubToken::new("_".to_string()));
        }

        Self {
            subtokens: new_subtokens,
        }
    }
}

impl FromStr for AlphanumericToken {
    type Err = RezError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl PartialEq for AlphanumericToken {
    fn eq(&self, other: &Self) -> bool {
        self.subtokens == other.subtokens
    }
}

impl Eq for AlphanumericToken {}

impl PartialOrd for AlphanumericToken {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for AlphanumericToken {
    fn cmp(&self, other: &Self) -> Ordering {
        // Lexicographic comparison of subtoken lists
        self.subtokens.cmp(&other.subtokens)
    }
}

impl Hash for AlphanumericToken {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for subtoken in &self.subtokens {
            subtoken.hash(state);
        }
    }
}

impl fmt::Display for AlphanumericToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for subtoken in &self.subtokens {
            write!(f, "{}", subtoken)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subtoken_ordering() {
        // Alpha before numeric
        let alpha = SubToken::new("alpha".to_string());
        let num = SubToken::new("1".to_string());
        assert!(alpha < num);

        // Alpha comparison
        let a1 = SubToken::new("a".to_string());
        let a2 = SubToken::new("b".to_string());
        assert!(a1 < a2);

        // Numeric comparison: "01" < "1" because Rez uses padding-sensitive ties.
        let n01 = SubToken::new("01".to_string());
        let n1 = SubToken::new("1".to_string());
        assert!(n01 < n1);

        // Numeric classification and comparison are not limited by u64.
        let max = SubToken::new("18446744073709551615".to_string());
        let above_max = SubToken::new("18446744073709551616".to_string());
        assert!(max.numeric);
        assert!(above_max.numeric);
        assert!(max < above_max);
        assert!(SubToken::new("99999999999999999999".to_string()) > SubToken::new("z".to_string()));

        // Numeric comparison: 1 < 2
        let n1 = SubToken::new("1".to_string());
        let n2 = SubToken::new("2".to_string());
        assert!(n1 < n2);
    }

    #[test]
    fn test_alphanumeric_token_parsing() {
        let t1 = AlphanumericToken::parse("1alpha2").unwrap();
        assert_eq!(t1.subtokens.len(), 3);
        assert_eq!(t1.to_string(), "1alpha2");

        let t2 = AlphanumericToken::parse("alpha").unwrap();
        assert_eq!(t2.subtokens.len(), 1);
        assert_eq!(t2.to_string(), "alpha");

        let t3 = AlphanumericToken::parse("123").unwrap();
        assert_eq!(t3.subtokens.len(), 1);
        assert_eq!(t3.to_string(), "123");

        let t4 = AlphanumericToken::parse("1_2_3").unwrap();
        assert_eq!(t4.to_string(), "1_2_3");
    }

    #[test]
    fn test_alphanumeric_token_validation() {
        assert!(AlphanumericToken::parse("").is_err());
        assert!(AlphanumericToken::parse("alpha-beta").is_err());
        assert!(AlphanumericToken::parse("alpha.beta").is_err());
        assert!(AlphanumericToken::parse("alpha beta").is_err());
    }

    #[test]
    fn test_alphanumeric_token_ordering() {
        let t1 = AlphanumericToken::parse("1alpha").unwrap();
        let t2 = AlphanumericToken::parse("1beta").unwrap();
        assert!(t1 < t2);

        let t3 = AlphanumericToken::parse("2alpha").unwrap();
        assert!(t1 < t3);

        let t4 = AlphanumericToken::parse("1alpha2").unwrap();
        assert!(t1 < t4);
    }

    #[test]
    fn test_alphanumeric_token_next() {
        // Alpha ending: append '_'
        let t1 = AlphanumericToken::parse("alpha").unwrap();
        let next1 = t1.next();
        assert_eq!(next1.to_string(), "alpha_");

        // Numeric ending: Rez appends an underscore subtoken.
        let t2 = AlphanumericToken::parse("1").unwrap();
        let next2 = t2.next();
        assert_eq!(next2.to_string(), "1_");

        // Mixed ending in alpha
        let t3 = AlphanumericToken::parse("1alpha").unwrap();
        let next3 = t3.next();
        assert_eq!(next3.to_string(), "1alpha_");

        // Mixed ending in numeric
        let t4 = AlphanumericToken::parse("alpha1").unwrap();
        let next4 = t4.next();
        assert_eq!(next4.to_string(), "alpha1_");

        // Multi-digit numeric ending
        let t5 = AlphanumericToken::parse("99").unwrap();
        let next5 = t5.next();
        assert_eq!(next5.to_string(), "99_");

        // u64::MAX follows Rez semantics without integer overflow.
        let max = AlphanumericToken::parse("18446744073709551615").unwrap();
        assert_eq!(max.next().to_string(), "18446744073709551615_");
    }

    #[test]
    fn test_from_str() {
        let t: AlphanumericToken = "1alpha2".parse().unwrap();
        assert_eq!(t.to_string(), "1alpha2");

        let err: Result<AlphanumericToken, _> = "invalid-token".parse();
        assert!(err.is_err());
    }
}
