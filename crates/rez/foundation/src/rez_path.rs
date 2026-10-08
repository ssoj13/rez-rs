// SPDX-License-Identifier: Apache-2.0

//! Centralized path handling for rez.
//!
//! All paths inside rez are stored in Unix format (`/`).
//! Convert to OS-native only at the boundary: `to_os()` before fs::, Command::env, etc.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use path_slash::{PathBufExt, PathExt};
use serde::{Deserialize, Serialize};

/// Path in Unix format. Primary type for storing paths inside rez.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RezPath(String);

impl RezPath {
    /// From string (config, .rxt, CLI). Normalizes `\` -> `/`.
    /// Convenience wrapper around the `FromStr` impl (which is infallible).
    pub fn new(s: &str) -> Self {
        Self(normalize_to_unix(s))
    }

    /// From OS path (read_dir, env). Converts to Unix.
    pub fn from_os(p: &Path) -> Self {
        Self(p.to_slash_lossy().into_owned())
    }

    /// Only before fs::, Command::env, writing to scripts.
    pub fn to_os(&self) -> PathBuf {
        PathBuf::from_slash(&self.0)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Join a segment. Keeps Unix format.
    pub fn join(&self, segment: &str) -> Self {
        let seg = segment.trim_start_matches('/');
        if self.0.is_empty() || self.0.ends_with('/') {
            Self(format!("{}{}", self.0, seg))
        } else {
            Self(format!("{}/{}", self.0, seg))
        }
    }

    /// Parent path. Returns None if at root.
    pub fn parent(&self) -> Option<Self> {
        let p = self.0.trim_end_matches('/');
        p.rfind('/').map(|i| Self(p[..i].to_string()))
    }

    /// Last component (file/dir name).
    pub fn file_name(&self) -> Option<&str> {
        let p = self.0.trim_end_matches('/');
        p.rfind('/')
            .map(|i| &p[i + 1..])
            .or_else(|| Some(p).filter(|s| !s.is_empty()))
    }
}

impl FromStr for RezPath {
    type Err = std::convert::Infallible;

    /// Parse from string. Normalizes `\` -> `/`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(normalize_to_unix(s)))
    }
}

impl fmt::Display for RezPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for RezPath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RezPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(Self::new(&s))
    }
}

fn normalize_to_unix(s: &str) -> String {
    s.replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // --- construction / from_str ---

    #[test]
    fn from_str_unix_unchanged() {
        let p = RezPath::new("/some/unix/path");
        assert_eq!(p.as_str(), "/some/unix/path");
    }

    #[test]
    fn from_str_normalizes_backslashes() {
        let p = RezPath::new("C:\\some\\windows\\path");
        assert_eq!(p.as_str(), "C:/some/windows/path");
    }

    #[test]
    fn from_str_mixed_slashes() {
        let p = RezPath::new("foo\\bar/baz");
        assert_eq!(p.as_str(), "foo/bar/baz");
    }

    #[test]
    fn from_str_empty() {
        let p = RezPath::new("");
        assert_eq!(p.as_str(), "");
    }

    // --- as_str ---

    #[test]
    fn as_str_returns_inner() {
        let p = RezPath::new("/a/b/c");
        assert_eq!(p.as_str(), "/a/b/c");
    }

    // --- to_os / from_os round-trip ---

    #[test]
    fn from_os_unix_path() {
        let p = RezPath::from_os(Path::new("/usr/local/bin"));
        // on any platform path_slash produces forward slashes
        assert_eq!(p.as_str(), "/usr/local/bin");
    }

    #[cfg(windows)]
    #[test]
    fn to_os_produces_backslashes_on_windows() {
        let p = RezPath::new("C:/some/path");
        let os = p.to_os();
        assert_eq!(os, std::path::PathBuf::from("C:\\some\\path"));
    }

    #[cfg(unix)]
    #[test]
    fn to_os_produces_forward_slashes_on_unix() {
        let p = RezPath::new("/some/path");
        let os = p.to_os();
        assert_eq!(os, std::path::PathBuf::from("/some/path"));
    }

    #[test]
    fn from_os_to_os_round_trip() {
        let original = Path::new("/packages/foo/1.0");
        let rez = RezPath::from_os(original);
        // to_os on the same platform should give back an equivalent path
        assert_eq!(rez.to_os(), PathBuf::from("/packages/foo/1.0"));
    }

    // --- join ---

    #[test]
    fn join_simple_segment() {
        let p = RezPath::new("/a/b");
        assert_eq!(p.join("c").as_str(), "/a/b/c");
    }

    #[test]
    fn join_segment_with_leading_slash_is_stripped() {
        let p = RezPath::new("/a/b");
        assert_eq!(p.join("/c").as_str(), "/a/b/c");
    }

    #[test]
    fn join_onto_trailing_slash_no_double_slash() {
        let p = RezPath::new("/a/b/");
        assert_eq!(p.join("c").as_str(), "/a/b/c");
    }

    #[test]
    fn join_onto_empty_base() {
        let p = RezPath::new("");
        assert_eq!(p.join("foo").as_str(), "foo");
    }

    #[test]
    fn join_multi_segment() {
        let p = RezPath::new("/root");
        let result = p.join("a").join("b").join("c");
        assert_eq!(result.as_str(), "/root/a/b/c");
    }

    // --- parent ---

    #[test]
    fn parent_deep_path() {
        let p = RezPath::new("/a/b/c");
        assert_eq!(p.parent().unwrap().as_str(), "/a/b");
    }

    #[test]
    fn parent_one_level_below_root() {
        let p = RezPath::new("/a");
        assert_eq!(p.parent().unwrap().as_str(), "");
    }

    #[test]
    fn parent_root_returns_none() {
        // "/" has nothing after trimming trailing slash
        let p = RezPath::new("/");
        assert!(p.parent().is_none());
    }

    #[test]
    fn parent_no_slash_returns_none() {
        let p = RezPath::new("just_a_name");
        assert!(p.parent().is_none());
    }

    #[test]
    fn parent_trailing_slash_ignored() {
        let p = RezPath::new("/a/b/");
        assert_eq!(p.parent().unwrap().as_str(), "/a");
    }

    // --- file_name ---

    #[test]
    fn file_name_simple() {
        let p = RezPath::new("/a/b/c");
        assert_eq!(p.file_name(), Some("c"));
    }

    #[test]
    fn file_name_trailing_slash_ignored() {
        let p = RezPath::new("/a/b/c/");
        assert_eq!(p.file_name(), Some("c"));
    }

    #[test]
    fn file_name_no_slash_returns_whole() {
        let p = RezPath::new("just_name");
        assert_eq!(p.file_name(), Some("just_name"));
    }

    #[test]
    fn file_name_empty_returns_none() {
        let p = RezPath::new("");
        assert_eq!(p.file_name(), None);
    }

    #[test]
    fn file_name_root_slash_returns_none() {
        // "/" trims to "", rfind fails, filter("") -> None
        let p = RezPath::new("/");
        assert_eq!(p.file_name(), None);
    }

    // --- Display ---

    #[test]
    fn display_outputs_unix_string() {
        let p = RezPath::new("/foo/bar");
        assert_eq!(format!("{p}"), "/foo/bar");
    }

    #[test]
    fn display_empty() {
        let p = RezPath::new("");
        assert_eq!(format!("{p}"), "");
    }

    // --- serde round-trip ---

    #[test]
    fn serde_round_trip() {
        let p = RezPath::new("/packages/maya/2025");
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, "\"/packages/maya/2025\"");
        let back: RezPath = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn serde_deserialize_normalizes_backslashes() {
        // JSON coming from Windows config files
        let json = "\"C:\\\\packages\\\\maya\"";
        let p: RezPath = serde_json::from_str(json).unwrap();
        assert_eq!(p.as_str(), "C:/packages/maya");
    }

    // --- equality / clone / hash ---

    #[test]
    fn eq_same_paths() {
        let a = RezPath::new("/a/b");
        let b = RezPath::new("/a/b");
        assert_eq!(a, b);
    }

    #[test]
    fn clone_is_independent() {
        let a = RezPath::new("/a/b");
        let b = a.clone();
        assert_eq!(a, b);
    }
}
