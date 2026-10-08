// SPDX-License-Identifier: Apache-2.0

//! Package help extraction and browser/command launching.
//!
//! Ported from Python rez package_help.py.

use crate::config::CONFIG;
use crate::errors::{Result, RezError};
use crate::package::discover::iter_packages;
use crate::repository::PackageInfo;
use model::package::Help;
use std::fmt;
use std::path::PathBuf;
use std::process::Command;
use version::VersionRange;

// ============================================================================
// HelpEntry
// ============================================================================

/// A single help entry with label and URI (URL or command).
#[derive(Clone, Debug)]
pub struct HelpEntry {
    pub label: String,
    pub uri: String,
}

impl HelpEntry {
    pub fn new(label: impl Into<String>, uri: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            uri: uri.into(),
        }
    }

    /// True if URI looks like a URL (no spaces, starts with known scheme or has dot).
    pub fn is_url(&self) -> bool {
        let u = self.uri.trim();
        !u.contains(' ')
            && (u.starts_with("http://")
                || u.starts_with("https://")
                || u.starts_with("file://")
                || u.contains('.'))
    }
}

impl fmt::Display for HelpEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.label, self.uri)
    }
}

// ============================================================================
// PackageHelp
// ============================================================================

/// Extracted help info for a package.
///
/// Given a package name and optional version range, finds the latest package
/// that has help entries and exposes them for display or opening.
#[derive(Clone, Debug)]
pub struct PackageHelp {
    /// Package that provided the help (if any).
    pub package: Option<PackageInfo>,
    entries: Vec<HelpEntry>,
}

impl PackageHelp {
    /// Find help from the latest matching package.
    ///
    /// Searches packages in descending version order and returns
    /// help from the first package that has a `help` field.
    pub fn find(
        name: &str,
        range: Option<&VersionRange>,
        paths: Option<&[PathBuf]>,
    ) -> Result<Self> {
        let mut packages = iter_packages(name, range, paths)?;
        // iter_packages returns sorted descending (latest first)
        // but let's ensure it
        packages.sort_by(|a, b| b.version.cmp(&a.version));

        for pkg in &packages {
            if let Some(entries) = parse_help_field(&pkg.data) {
                if !entries.is_empty() {
                    return Ok(Self {
                        package: Some(pkg.clone()),
                        entries,
                    });
                }
            }
        }

        Ok(Self {
            package: None,
            entries: Vec::new(),
        })
    }

    /// Create from an explicit Help value (e.g. from Package struct).
    pub fn from_help(help: &Help) -> Self {
        let entries = entries_from_help(help);
        Self {
            package: None,
            entries,
        }
    }

    /// True if any help entries were found.
    pub fn success(&self) -> bool {
        !self.entries.is_empty()
    }

    /// Get all help entries.
    pub fn entries(&self) -> &[HelpEntry] {
        &self.entries
    }

    /// Open the help entry at `index`.
    ///
    /// If the URI looks like a URL, opens it in a browser.
    /// Otherwise, runs it as a shell command.
    pub fn open(&self, index: usize) -> Result<()> {
        let entry = self.entries.get(index).ok_or_else(|| {
            RezError::PackageCommand(format!(
                "Help section index {} out of range (0..{})",
                index,
                self.entries.len()
            ))
        })?;

        if entry.is_url() {
            open_url(&entry.uri)
        } else {
            run_command(&entry.uri)
        }
    }

    /// Print all help sections to a string.
    pub fn format_sections(&self) -> String {
        let mut out = String::from("Sections:\n");
        for (i, entry) in self.entries.iter().enumerate() {
            out.push_str(&format!("  {}:\t{} ({})\n", i + 1, entry.label, entry.uri));
        }
        out
    }

    /// Open the rez manual URL from config.
    pub fn open_rez_manual() -> Result<()> {
        open_url(&CONFIG.documentation_url)
    }
}

// ============================================================================
// Parsing help field from raw package data
// ============================================================================

/// Parse "help" field from raw JSON data map.
///
/// Returns None if no help field, Some(entries) otherwise.
/// Accepts: string, list of [label,url] pairs, or null.
fn parse_help_field(
    data: &std::collections::HashMap<String, serde_json::Value>,
) -> Option<Vec<HelpEntry>> {
    let val = data.get("help")?;

    match val {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => {
            if s.is_empty() {
                None
            } else {
                Some(vec![HelpEntry::new("Help", s.as_str())])
            }
        }
        serde_json::Value::Array(arr) => {
            let mut entries = Vec::new();
            for item in arr {
                if let serde_json::Value::Array(pair) = item {
                    if pair.len() >= 2 {
                        let label = pair[0].as_str().unwrap_or("Help");
                        let uri = pair[1].as_str().unwrap_or("");
                        if !uri.is_empty() {
                            entries.push(HelpEntry::new(label, uri));
                        }
                    } else if pair.len() == 1 {
                        let uri = pair[0].as_str().unwrap_or("");
                        if !uri.is_empty() {
                            entries.push(HelpEntry::new("Help", uri));
                        }
                    }
                } else if let serde_json::Value::String(s) = item {
                    // Flat list of strings
                    if !s.is_empty() {
                        entries.push(HelpEntry::new("Help", s.as_str()));
                    }
                }
            }
            if entries.is_empty() {
                None
            } else {
                Some(entries)
            }
        }
        _ => None,
    }
}

/// Convert Help enum to HelpEntry vec.
fn entries_from_help(help: &Help) -> Vec<HelpEntry> {
    match help {
        Help::Single(s) => {
            if s.is_empty() {
                Vec::new()
            } else {
                vec![HelpEntry::new("Help", s.as_str())]
            }
        }
        Help::Multiple(pairs) => pairs
            .iter()
            .filter_map(|pair| {
                if pair.len() >= 2 && !pair[1].is_empty() {
                    Some(HelpEntry::new(&pair[0], &pair[1]))
                } else if pair.len() == 1 && !pair[0].is_empty() {
                    Some(HelpEntry::new("Help", &pair[0]))
                } else {
                    None
                }
            })
            .collect(),
    }
}

// ============================================================================
// URL / command helpers
// ============================================================================

/// Open a URL in the system browser.
///
/// Uses config.browser if set, otherwise platform default (start/xdg-open/open).
fn open_url(url: &str) -> Result<()> {
    if let Some(ref browser) = CONFIG.browser {
        if !CONFIG.quiet {
            eprintln!("running command: {} {}", browser, url);
        }
        Command::new(browser)
            .arg(url)
            .spawn()
            .map_err(|e| {
                RezError::PackageCommand(format!("Failed to launch browser '{}': {}", browser, e))
            })?
            .wait()
            .map_err(|e| RezError::PackageCommand(format!("Browser process error: {}", e)))?;
    } else {
        if !CONFIG.quiet {
            eprintln!("opening URL in browser: {}", url);
        }
        open_url_platform(url)?;
    }
    Ok(())
}

/// Platform-specific URL open (no custom browser).
#[cfg(target_os = "windows")]
fn open_url_platform(url: &str) -> Result<()> {
    Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn()
        .map_err(|e| RezError::PackageCommand(format!("Failed to open URL: {}", e)))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn open_url_platform(url: &str) -> Result<()> {
    Command::new("open")
        .arg(url)
        .spawn()
        .map_err(|e| RezError::PackageCommand(format!("Failed to open URL: {}", e)))?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn open_url_platform(url: &str) -> Result<()> {
    Command::new("xdg-open")
        .arg(url)
        .spawn()
        .map_err(|e| RezError::PackageCommand(format!("Failed to open URL: {}", e)))?;
    Ok(())
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn open_url_platform(_url: &str) -> Result<()> {
    Err(RezError::PackageCommand(
        "No browser support on this platform".into(),
    ))
}

/// Run a shell command string.
fn run_command(cmd: &str) -> Result<()> {
    if !CONFIG.quiet {
        eprintln!("running command: {}", cmd);
    }

    #[cfg(target_os = "windows")]
    let status = Command::new("cmd")
        .args(["/C", cmd])
        .status()
        .map_err(|e| RezError::PackageCommand(format!("Failed to run '{}': {}", cmd, e)))?;

    #[cfg(not(target_os = "windows"))]
    let status = Command::new("sh")
        .args(["-c", cmd])
        .status()
        .map_err(|e| RezError::PackageCommand(format!("Failed to run '{}': {}", cmd, e)))?;

    if !status.success() {
        return Err(RezError::PackageCommand(format!(
            "Command '{}' exited with status: {}",
            cmd,
            status.code().unwrap_or(-1)
        )));
    }
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::MemoryPackageRepository;
    use crate::repository::PackageRepositoryManager;
    use serde_json::json;
    use std::collections::HashMap;

    // -- HelpEntry tests --

    #[test]
    fn test_entry_is_url() {
        assert!(HelpEntry::new("docs", "https://example.com").is_url());
        assert!(HelpEntry::new("docs", "http://example.com/help").is_url());
        assert!(HelpEntry::new("docs", "file:///tmp/help.html").is_url());
        assert!(HelpEntry::new("docs", "example.com/help").is_url());
        assert!(!HelpEntry::new("run", "man maya").is_url());
        assert!(!HelpEntry::new("run", "echo hello world").is_url());
    }

    #[test]
    fn test_entry_display() {
        let e = HelpEntry::new("API Docs", "https://docs.rs");
        assert_eq!(format!("{}", e), "API Docs: https://docs.rs");
    }

    // -- parse_help_field tests --

    #[test]
    fn test_parse_null() {
        let mut data = HashMap::new();
        data.insert("help".into(), json!(null));
        assert!(parse_help_field(&data).is_none());
    }

    #[test]
    fn test_parse_no_field() {
        let data = HashMap::new();
        assert!(parse_help_field(&data).is_none());
    }

    #[test]
    fn test_parse_string() {
        let mut data = HashMap::new();
        data.insert("help".into(), json!("https://example.com"));
        let entries = parse_help_field(&data).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label, "Help");
        assert_eq!(entries[0].uri, "https://example.com");
    }

    #[test]
    fn test_parse_empty_string() {
        let mut data = HashMap::new();
        data.insert("help".into(), json!(""));
        assert!(parse_help_field(&data).is_none());
    }

    #[test]
    fn test_parse_pairs() {
        let mut data = HashMap::new();
        data.insert(
            "help".into(),
            json!([
                ["User Guide", "https://guide.com"],
                ["API", "https://api.com"]
            ]),
        );
        let entries = parse_help_field(&data).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].label, "User Guide");
        assert_eq!(entries[0].uri, "https://guide.com");
        assert_eq!(entries[1].label, "API");
        assert_eq!(entries[1].uri, "https://api.com");
    }

    #[test]
    fn test_parse_flat_string_list() {
        let mut data = HashMap::new();
        data.insert("help".into(), json!(["https://one.com", "https://two.com"]));
        let entries = parse_help_field(&data).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].uri, "https://one.com");
        assert_eq!(entries[1].uri, "https://two.com");
    }

    #[test]
    fn test_parse_mixed_empty() {
        let mut data = HashMap::new();
        data.insert("help".into(), json!([["Label", ""]]));
        assert!(parse_help_field(&data).is_none());
    }

    // -- entries_from_help tests --

    #[test]
    fn test_from_help_single() {
        let help = Help::Single("https://example.com".into());
        let entries = entries_from_help(&help);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].uri, "https://example.com");
    }

    #[test]
    fn test_from_help_single_empty() {
        let help = Help::Single(String::new());
        let entries = entries_from_help(&help);
        assert!(entries.is_empty());
    }

    #[test]
    fn test_from_help_multiple() {
        let help = Help::Multiple(vec![
            vec!["Guide".into(), "https://guide.com".into()],
            vec!["API".into(), "https://api.com".into()],
        ]);
        let entries = entries_from_help(&help);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].label, "Guide");
        assert_eq!(entries[1].label, "API");
    }

    #[test]
    fn test_from_help_multiple_with_empty() {
        let help = Help::Multiple(vec![
            vec!["Good".into(), "https://ok.com".into()],
            vec!["Bad".into(), "".into()], // empty URI filtered
            vec![],                        // empty pair filtered
        ]);
        let entries = entries_from_help(&help);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label, "Good");
    }

    // -- PackageHelp tests --

    #[test]
    fn test_package_help_empty() {
        let help = PackageHelp {
            package: None,
            entries: Vec::new(),
        };
        assert!(!help.success());
        assert!(help.entries().is_empty());
    }

    #[test]
    fn test_package_help_with_entries() {
        let help = PackageHelp {
            package: None,
            entries: vec![
                HelpEntry::new("Guide", "https://guide.com"),
                HelpEntry::new("API", "https://api.com"),
            ],
        };
        assert!(help.success());
        assert_eq!(help.entries().len(), 2);
    }

    #[test]
    fn test_format_sections() {
        let help = PackageHelp {
            package: None,
            entries: vec![
                HelpEntry::new("Guide", "https://guide.com"),
                HelpEntry::new("API", "https://api.com"),
            ],
        };
        let out = help.format_sections();
        assert!(out.contains("1:\tGuide (https://guide.com)"));
        assert!(out.contains("2:\tAPI (https://api.com)"));
    }

    #[test]
    fn test_open_out_of_range() {
        let help = PackageHelp {
            package: None,
            entries: vec![HelpEntry::new("X", "https://x.com")],
        };
        assert!(help.open(5).is_err());
    }

    // -- PackageHelp::from_help tests --

    #[test]
    fn test_from_help_enum() {
        let h = Help::Multiple(vec![vec!["Docs".into(), "https://docs.com".into()]]);
        let ph = PackageHelp::from_help(&h);
        assert!(ph.success());
        assert_eq!(ph.entries()[0].label, "Docs");
    }

    // -- PackageHelp::find with memory repo --

    #[test]
    fn test_find_with_help() {
        let mut repo = MemoryPackageRepository::new();
        let mut data1 = HashMap::new();
        data1.insert("name".into(), json!("mypkg"));
        data1.insert("version".into(), json!("1.0"));
        // no help field
        let info1 = PackageInfo::from_data(data1).unwrap();
        repo.add_package(info1).unwrap();

        let mut data2 = HashMap::new();
        data2.insert("name".into(), json!("mypkg"));
        data2.insert("version".into(), json!("2.0"));
        data2.insert("help".into(), json!("https://mypkg.io"));
        let info2 = PackageInfo::from_data(data2).unwrap();
        repo.add_package(info2).unwrap();

        let mut manager = PackageRepositoryManager::new();
        manager.add_repo(Box::new(repo));

        // Use manager directly to get packages, then test parse
        let pkgs = manager.iter_packages("mypkg").unwrap();
        assert_eq!(pkgs.len(), 2);

        // Find first with help (should be 2.0 - latest)
        let with_help = pkgs.iter().find(|p| parse_help_field(&p.data).is_some());
        assert!(with_help.is_some());
        assert_eq!(with_help.unwrap().version.to_string(), "2.0");

        let entries = parse_help_field(&with_help.unwrap().data).unwrap();
        assert_eq!(entries[0].uri, "https://mypkg.io");
    }

    #[test]
    fn test_find_help_pairs() {
        let mut data = HashMap::new();
        data.insert("name".into(), json!("pkg"));
        data.insert("version".into(), json!("1.0"));
        data.insert(
            "help".into(),
            json!([
                ["User Guide", "https://guide.io"],
                ["API Reference", "https://api.io"]
            ]),
        );

        let entries = parse_help_field(&data).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].label, "User Guide");
        assert_eq!(entries[1].label, "API Reference");
    }

    #[test]
    fn test_find_no_help() {
        let mut data = HashMap::new();
        data.insert("name".into(), json!("bare"));
        data.insert("version".into(), json!("1.0"));

        assert!(parse_help_field(&data).is_none());
    }
}
