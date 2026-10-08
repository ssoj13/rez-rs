// SPDX-License-Identifier: Apache-2.0

//! Package creation builder API.
//!
//! Ported from Python rez package_maker.py.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::errors::{Result, RezError};
use crate::serialise::{dump_package_data, FileFormat};
use version::{Requirement, Version, VersionRange};

/// Expand the wildcard tokens accepted by Rez's PackageMaker schema.
///
/// This intentionally runs only when PackageMaker writes package data. Normal
/// package parsing continues to validate requirements without expansion.
fn expand_requirement(request: &str, paths: Option<&[PathBuf]>) -> Result<String> {
    if !request.contains('*') {
        return Ok(request.to_string());
    }
    if !request.contains('|') {
        return expand_requirement_part(request, paths);
    }

    let mut alternatives = request.split('|');
    let first = alternatives.next().unwrap_or_default();
    let first_expanded = expand_requirement_part(first, paths)?;
    let first_requirement = Requirement::new(&first_expanded)?;
    let conflict_prefix = if first_requirement.weak() {
        "~"
    } else if first_requirement.conflict() {
        "!"
    } else {
        ""
    };
    let package_prefix = format!("{conflict_prefix}{}", first_requirement.name());
    let first_tail = first_expanded
        .strip_prefix(&package_prefix)
        .unwrap_or_default();
    let separator = first_tail
        .chars()
        .next()
        .filter(|ch| matches!(ch, '-' | '@' | '#'))
        .map(|ch| ch.to_string())
        .unwrap_or_else(|| {
            if first_tail.starts_with(['<', '>', '=']) || first_tail.is_empty() {
                String::new()
            } else {
                "-".to_string()
            }
        });

    let first_range = first_tail.strip_prefix(&separator).unwrap_or(first_tail);
    let mut expanded_ranges = vec![(first_range.to_string(), VersionRange::new(first_range)?)];
    for alternative in alternatives {
        let branch_separator = if alternative.starts_with(['<', '>', '=']) {
            ""
        } else {
            &separator
        };
        let branch = format!("{package_prefix}{branch_separator}{alternative}");
        let branch_expanded = expand_requirement_part(&branch, paths)?;
        let prefix = format!("{package_prefix}{branch_separator}");
        let branch_range = branch_expanded
            .strip_prefix(&prefix)
            .unwrap_or(&branch_expanded)
            .to_string();
        expanded_ranges.push((branch_range.clone(), VersionRange::new(&branch_range)?));
    }

    expanded_ranges.sort_by(|(_, left), (_, right)| left.cmp_by(right, |a, b| a.cmp(b)));
    let expanded_range = expanded_ranges
        .into_iter()
        .map(|(range, _)| range)
        .collect::<Vec<_>>()
        .join("|");

    Ok(format!("{package_prefix}{separator}{expanded_range}"))
}

fn expand_requirement_part(request: &str, paths: Option<&[PathBuf]>) -> Result<String> {
    if !request.contains('*') {
        return Ok(request.to_string());
    }

    let mut protected = request.to_string();
    let mut wildcards = Vec::<(String, bool)>::new();
    let mut index = 0usize;

    for (pattern, full_version) in [("**", true), ("*", false)] {
        while let Some(position) = protected.find(pattern) {
            let marker = loop {
                let marker = format!("rezrswildcardmarker{}", index * 1_000_003);
                index += 1;
                if !protected.contains(&marker) && !request.contains(&marker) {
                    break marker;
                }
            };
            protected.replace_range(position..position + pattern.len(), &marker);
            wildcards.push((marker, full_version));
        }
    }

    let requirement = Requirement::new(&protected)?;
    let Some(range) = requirement.range() else {
        return Ok(request.to_string());
    };
    let expanded_versions = RefCell::new(HashMap::<Version, Version>::new());
    let range = range.visit_versions(|version| {
        // Rez ranges represent a closed version as an interval ending at its
        // successor. Reuse the expansion for that synthetic upper endpoint.
        if let Some((_, expanded)) = expanded_versions
            .borrow()
            .iter()
            .find(|(original, _)| original.next() == *version)
        {
            return Ok(expanded.next());
        }

        let mut prefix = version.clone();
        let mut rank = prefix.len();
        let mut found = false;
        while let Some(token) = prefix.get(prefix.len().saturating_sub(1)) {
            let token_text = token.to_string();
            let Some((_, full_version)) =
                wildcards.iter().find(|(marker, _)| *marker == token_text)
            else {
                break;
            };
            if *full_version {
                // ** means all remaining version tokens; it cannot be mixed
                // with * in the same version endpoint.
                if found {
                    return Ok(version.clone());
                }
                found = true;
                prefix = prefix.trim(prefix.len() - 1);
                rank = 0;
                break;
            }
            prefix = prefix.trim(prefix.len() - 1);
            found = true;
        }
        if !found {
            return Ok(version.clone());
        }

        let packages = crate::package::discover::iter_packages(requirement.name(), None, paths)?;
        let package = packages
            .into_iter()
            .find(|package| prefix.is_empty() || package.version.trim(prefix.len()) == prefix);
        let expanded = package.map_or(prefix, |package| {
            if rank == 0 {
                package.version
            } else {
                package.version.trim(rank)
            }
        });
        expanded_versions
            .borrow_mut()
            .insert(version.clone(), expanded.clone());
        Ok(expanded)
    })?;

    let range_to_write = if requirement.weak() {
        range.inverse().unwrap_or_else(VersionRange::any)
    } else {
        range
    };
    let request_body = protected
        .strip_prefix('!')
        .or_else(|| protected.strip_prefix('~'))
        .unwrap_or(&protected);
    let separator = request_body
        .get(requirement.name().len()..)
        .and_then(|suffix| suffix.chars().next())
        .filter(|ch| matches!(ch, '-' | '@' | '#'))
        .unwrap_or('-');
    let range_text = range_to_write.to_string();
    let separator = if range_text.is_empty() || range_text.starts_with(['=', '<', '>']) {
        ""
    } else {
        match separator {
            '-' => "-",
            '@' => "@",
            '#' => "#",
            _ => unreachable!(),
        }
    };
    let has_explicit_version_operator = protected.contains('+')
        || protected.contains('<')
        || protected.contains('>')
        || protected.contains('=');
    let mut expanded = Requirement::new(&format!(
        "{}{}{}{}",
        if requirement.conflict() {
            if requirement.weak() {
                "~"
            } else {
                "!"
            }
        } else {
            ""
        },
        requirement.name(),
        separator,
        range_text
    ))?
    .to_string();
    for (marker, _) in wildcards {
        expanded = expanded.replace(&marker, "*");
    }
    // A bare Rez version request is serialized without the parser's implicit
    // lower-bound '+' marker (for example, foo-3 rather than foo-3+).
    if !has_explicit_version_operator {
        expanded = expanded.replace('+', "");
    }

    // Parse once more after cleanup: this rejects partial, misplaced, or mixed
    // wildcard syntax and canonicalizes OR alternatives like upstream Rez.
    let canonical = Requirement::new(&expanded)?.to_string();
    if has_explicit_version_operator {
        Ok(canonical)
    } else {
        Ok(canonical.replace('+', ""))
    }
}

fn expand_request_list(value: &mut Value, paths: Option<&[PathBuf]>) -> Result<()> {
    if let Some(requests) = value.as_array_mut() {
        for request in requests {
            if let Value::String(request) = request {
                *request = expand_requirement(request, paths)?;
            }
        }
    }
    Ok(())
}

fn expand_package_requests(
    data: &mut HashMap<String, Value>,
    paths: Option<&[PathBuf]>,
) -> Result<()> {
    for field in ["requires", "build_requires", "private_build_requires"] {
        if let Some(value) = data.get_mut(field) {
            expand_request_list(value, paths)?;
        }
    }
    if let Some(variants) = data.get_mut("variants").and_then(Value::as_array_mut) {
        for variant in variants {
            expand_request_list(variant, paths)?;
        }
    }
    if let Some(tests) = data.get_mut("tests").and_then(Value::as_object_mut) {
        for test in tests.values_mut().filter_map(Value::as_object_mut) {
            if let Some(requires) = test.get_mut("requires") {
                expand_request_list(requires, paths)?;
            }
            if let Some(on_variants) = test.get_mut("on_variants").and_then(Value::as_object_mut) {
                if on_variants.get("type").and_then(Value::as_str) == Some("requires") {
                    if let Some(value) = on_variants.get_mut("value") {
                        expand_request_list(value, paths)?;
                    }
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// PackageMaker - builder for package definitions
// ---------------------------------------------------------------------------

/// Builder for constructing rez package definitions.
///
/// Uses a consuming builder pattern: each setter takes `self` and returns `Self`.
/// Call `to_package_data()` to get serializable data, or `write_to()` to write
/// a package definition file directly.
///
/// # Examples
/// ```no_run
/// use repository::package::maker::PackageMaker;
/// use version::Version;
///
/// let maker = PackageMaker::new("foo")
///     .version(Version::new("1.2.3").unwrap())
///     .description("A foo package")
///     .tools(vec!["foo_tool".into()]);
///
/// let data = maker.to_package_data();
/// assert_eq!(data["name"], "foo");
/// ```
#[derive(Clone, Debug)]
pub struct PackageMaker {
    // Core (name is required)
    pub name: String,
    pub version: Option<Version>,
    pub description: Option<String>,
    pub authors: Option<Vec<String>>,

    // Dependencies
    pub requires: Option<Vec<Requirement>>,
    pub build_requires: Option<Vec<Requirement>>,
    pub private_build_requires: Option<Vec<Requirement>>,

    // Variants
    pub variants: Option<Vec<Vec<Requirement>>>,

    // Commands
    pub commands: Option<String>,
    pub pre_commands: Option<String>,
    pub post_commands: Option<String>,
    pub pre_build_commands: Option<String>,
    pub pre_test_commands: Option<String>,

    // Metadata
    pub tools: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
    pub uuid: Option<String>,
    pub timestamp: Option<u64>,
    pub help: Option<String>,
    pub relocatable: Option<bool>,
    pub cachable: Option<bool>,
    pub hashed_variants: Option<bool>,

    // Build
    pub build_system: Option<String>,
    pub build_command: Option<String>,

    // Arbitrary extra fields
    pub data: HashMap<String, Value>,
}

impl PackageMaker {
    /// Create a new package maker with the given name.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            version: None,
            description: None,
            authors: None,
            requires: None,
            build_requires: None,
            private_build_requires: None,
            variants: None,
            commands: None,
            pre_commands: None,
            post_commands: None,
            pre_build_commands: None,
            pre_test_commands: None,
            tools: None,
            tags: None,
            uuid: None,
            timestamp: None,
            help: None,
            relocatable: None,
            cachable: None,
            hashed_variants: None,
            build_system: None,
            build_command: None,
            data: HashMap::new(),
        }
    }

    // -- Builder setters (consuming self) --

    pub fn version(mut self, v: Version) -> Self {
        self.version = Some(v);
        self
    }

    pub fn description(mut self, desc: &str) -> Self {
        self.description = Some(desc.to_string());
        self
    }

    pub fn authors(mut self, a: Vec<String>) -> Self {
        self.authors = Some(a);
        self
    }

    pub fn requires(mut self, r: Vec<Requirement>) -> Self {
        self.requires = Some(r);
        self
    }

    pub fn build_requires(mut self, r: Vec<Requirement>) -> Self {
        self.build_requires = Some(r);
        self
    }

    pub fn private_build_requires(mut self, r: Vec<Requirement>) -> Self {
        self.private_build_requires = Some(r);
        self
    }

    pub fn variants(mut self, v: Vec<Vec<Requirement>>) -> Self {
        self.variants = Some(v);
        self
    }

    pub fn commands(mut self, c: &str) -> Self {
        self.commands = Some(c.to_string());
        self
    }

    pub fn pre_commands(mut self, c: &str) -> Self {
        self.pre_commands = Some(c.to_string());
        self
    }

    pub fn post_commands(mut self, c: &str) -> Self {
        self.post_commands = Some(c.to_string());
        self
    }

    pub fn tools(mut self, t: Vec<String>) -> Self {
        self.tools = Some(t);
        self
    }

    pub fn tags(mut self, t: Vec<String>) -> Self {
        self.tags = Some(t);
        self
    }

    pub fn uuid(mut self, u: &str) -> Self {
        self.uuid = Some(u.to_string());
        self
    }

    pub fn timestamp(mut self, ts: u64) -> Self {
        self.timestamp = Some(ts);
        self
    }

    pub fn help(mut self, h: &str) -> Self {
        self.help = Some(h.to_string());
        self
    }

    pub fn relocatable(mut self, r: bool) -> Self {
        self.relocatable = Some(r);
        self
    }

    pub fn cachable(mut self, c: bool) -> Self {
        self.cachable = Some(c);
        self
    }

    pub fn hashed_variants(mut self, h: bool) -> Self {
        self.hashed_variants = Some(h);
        self
    }

    pub fn build_system(mut self, bs: &str) -> Self {
        self.build_system = Some(bs.to_string());
        self
    }

    pub fn build_command(mut self, bc: &str) -> Self {
        self.build_command = Some(bc.to_string());
        self
    }

    /// Set an arbitrary extra field (stored in data map).
    pub fn set(mut self, key: &str, value: Value) -> Self {
        self.data.insert(key.to_string(), value);
        self
    }

    // -- Data conversion --

    /// Convert requirement list to JSON array of strings.
    fn reqs_to_value(reqs: &[Requirement]) -> Value {
        Value::Array(reqs.iter().map(|r| Value::String(r.to_string())).collect())
    }

    /// Convert variant list (Vec<Vec<Requirement>>) to JSON nested array.
    fn variants_to_value(variants: &[Vec<Requirement>]) -> Value {
        Value::Array(
            variants
                .iter()
                .map(|v| Value::Array(v.iter().map(|r| Value::String(r.to_string())).collect()))
                .collect(),
        )
    }

    /// Build a serializable HashMap from the current state.
    ///
    /// Only includes fields that are set (non-None). The "name" field
    /// is always present.
    pub fn to_package_data(&self) -> HashMap<String, Value> {
        let mut data = HashMap::new();

        // Always include name
        data.insert("name".into(), Value::String(self.name.clone()));

        // Version: skip if None (unversioned package)
        if let Some(ref v) = self.version {
            if !v.is_empty() {
                data.insert("version".into(), Value::String(v.to_string()));
            }
        }

        if let Some(ref d) = self.description {
            data.insert("description".into(), Value::String(d.clone()));
        }

        if let Some(ref a) = self.authors {
            data.insert(
                "authors".into(),
                Value::Array(a.iter().map(|s| Value::String(s.clone())).collect()),
            );
        }

        // Dependencies
        if let Some(ref r) = self.requires {
            data.insert("requires".into(), Self::reqs_to_value(r));
        }
        if let Some(ref r) = self.build_requires {
            data.insert("build_requires".into(), Self::reqs_to_value(r));
        }
        if let Some(ref r) = self.private_build_requires {
            data.insert("private_build_requires".into(), Self::reqs_to_value(r));
        }

        // Variants
        if let Some(ref v) = self.variants {
            data.insert("variants".into(), Self::variants_to_value(v));
        }

        // Commands
        if let Some(ref c) = self.commands {
            data.insert("commands".into(), Value::String(c.clone()));
        }
        if let Some(ref c) = self.pre_commands {
            data.insert("pre_commands".into(), Value::String(c.clone()));
        }
        if let Some(ref c) = self.post_commands {
            data.insert("post_commands".into(), Value::String(c.clone()));
        }
        if let Some(ref c) = self.pre_build_commands {
            data.insert("pre_build_commands".into(), Value::String(c.clone()));
        }
        if let Some(ref c) = self.pre_test_commands {
            data.insert("pre_test_commands".into(), Value::String(c.clone()));
        }

        // Metadata
        if let Some(ref t) = self.tools {
            data.insert(
                "tools".into(),
                Value::Array(t.iter().map(|s| Value::String(s.clone())).collect()),
            );
        }
        if let Some(ref t) = self.tags {
            data.insert(
                "tags".into(),
                Value::Array(t.iter().map(|s| Value::String(s.clone())).collect()),
            );
        }
        if let Some(ref u) = self.uuid {
            data.insert("uuid".into(), Value::String(u.clone()));
        }
        if let Some(ts) = self.timestamp {
            data.insert("timestamp".into(), Value::Number(ts.into()));
        }
        if let Some(ref h) = self.help {
            data.insert("help".into(), Value::String(h.clone()));
        }
        if let Some(r) = self.relocatable {
            data.insert("relocatable".into(), Value::Bool(r));
        }
        if let Some(c) = self.cachable {
            data.insert("cachable".into(), Value::Bool(c));
        }
        if let Some(h) = self.hashed_variants {
            data.insert("hashed_variants".into(), Value::Bool(h));
        }

        // Build
        if let Some(ref bs) = self.build_system {
            data.insert("build_system".into(), Value::String(bs.clone()));
        }
        if let Some(ref bc) = self.build_command {
            data.insert("build_command".into(), Value::String(bc.clone()));
        }

        // Merge arbitrary extra fields (overrides typed fields if key collides)
        for (k, v) in &self.data {
            data.insert(k.clone(), v.clone());
        }

        data
    }

    /// Write the package definition to a directory.
    ///
    /// Creates `{dir}/package.{ext}` using `dump_package_data` with
    /// preferred key ordering. Creates `dir` if it doesn't exist.
    ///
    /// Returns the path to the written file.
    pub fn write_to(&self, dir: &Path, format: FileFormat) -> Result<PathBuf> {
        // Don't write .py format - not supported for generation
        if format == FileFormat::Py {
            return Err(RezError::InvalidPackage(
                "Cannot write package.py format (use yaml or toml)".into(),
            ));
        }

        let mut data = self.to_package_data();
        expand_package_requests(&mut data, None)?;

        fs::create_dir_all(dir)?;

        let filename = format!("package.{}", format.extension());
        let filepath = dir.join(filename);

        dump_package_data(&data, &filepath, format, None)?;

        Ok(filepath)
    }
}

// ---------------------------------------------------------------------------
// make_package - convenience function
// ---------------------------------------------------------------------------

/// Create and write a package definition in one call.
///
/// Constructs a `PackageMaker`, passes it to `configure` for setup,
/// then writes `package.{ext}` to `{path}/{name}/{version}/`.
/// If version is unset, writes to `{path}/{name}/`.
///
/// # Arguments
/// * `name` - Package name
/// * `path` - Root repository path
/// * `format` - File format (Yaml or Toml)
/// * `configure` - Closure that configures the `PackageMaker`
///
/// # Returns
/// Path to the written package definition file.
///
/// # Examples
/// ```no_run
/// use repository::package::maker::make_package;
/// use model::serialise::FileFormat;
/// use version::Version;
///
/// let path = make_package("foo", std::path::Path::new("/packages"), FileFormat::Yaml, |pkg| {
///     pkg.version = Some(Version::new("1.0.0").unwrap());
///     pkg.description = Some("A foo package".into());
/// }).unwrap();
/// ```
pub fn make_package<F>(name: &str, path: &Path, format: FileFormat, configure: F) -> Result<PathBuf>
where
    F: FnOnce(&mut PackageMaker),
{
    let mut maker = PackageMaker::new(name);
    configure(&mut maker);

    // Build target directory: {path}/{name}/{version}/
    let mut dir = path.join(&maker.name);
    if let Some(ref v) = maker.version {
        if !v.is_empty() {
            dir = dir.join(v.to_string());
        }
    }

    maker.write_to(&dir, format)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- PackageMaker builder tests --

    #[test]
    fn test_new_minimal() {
        let m = PackageMaker::new("foo");
        assert_eq!(m.name, "foo");
        assert!(m.version.is_none());
        assert!(m.description.is_none());
        assert!(m.data.is_empty());
    }

    #[test]
    fn test_builder_chain() {
        let m = PackageMaker::new("bar")
            .version(Version::new("2.0.1").unwrap())
            .description("bar package")
            .authors(vec!["Alice".into(), "Bob".into()])
            .tools(vec!["bar_tool".into()])
            .uuid("test-uuid-1234")
            .timestamp(1700000000)
            .relocatable(true)
            .cachable(false)
            .hashed_variants(true)
            .build_system("cmake")
            .build_command("cmake --build .");

        assert_eq!(m.version.as_ref().unwrap().to_string(), "2.0.1");
        assert_eq!(m.description.as_deref(), Some("bar package"));
        assert_eq!(m.authors.as_ref().unwrap().len(), 2);
        assert_eq!(m.tools.as_ref().unwrap(), &["bar_tool"]);
        assert_eq!(m.uuid.as_deref(), Some("test-uuid-1234"));
        assert_eq!(m.timestamp, Some(1700000000));
        assert_eq!(m.relocatable, Some(true));
        assert_eq!(m.cachable, Some(false));
        assert_eq!(m.hashed_variants, Some(true));
        assert_eq!(m.build_system.as_deref(), Some("cmake"));
        assert_eq!(m.build_command.as_deref(), Some("cmake --build ."));
    }

    #[test]
    fn test_builder_requires() {
        let m = PackageMaker::new("baz")
            .requires(vec![
                Requirement::new("python-2.7+").unwrap(),
                Requirement::new("numpy-1.0+").unwrap(),
            ])
            .build_requires(vec![Requirement::new("cmake-3+").unwrap()])
            .private_build_requires(vec![Requirement::new("gcc-9+").unwrap()]);

        assert_eq!(m.requires.as_ref().unwrap().len(), 2);
        assert_eq!(m.build_requires.as_ref().unwrap().len(), 1);
        assert_eq!(m.private_build_requires.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_builder_commands() {
        let m = PackageMaker::new("cmd_pkg")
            .commands("env.PATH.append('{root}/bin')")
            .pre_commands("echo pre")
            .post_commands("echo post");

        assert_eq!(m.commands.as_deref(), Some("env.PATH.append('{root}/bin')"));
        assert_eq!(m.pre_commands.as_deref(), Some("echo pre"));
        assert_eq!(m.post_commands.as_deref(), Some("echo post"));
    }

    #[test]
    fn test_builder_variants() {
        let m = PackageMaker::new("varpkg").variants(vec![
            vec![Requirement::new("python-2.7").unwrap()],
            vec![Requirement::new("python-3.7").unwrap()],
        ]);

        let v = m.variants.as_ref().unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].len(), 1);
    }

    #[test]
    fn test_set_arbitrary() {
        let m = PackageMaker::new("extra")
            .set("custom_flag", Value::Bool(true))
            .set("custom_data", Value::String("hello".into()));

        assert_eq!(m.data.get("custom_flag"), Some(&Value::Bool(true)));
        assert_eq!(
            m.data.get("custom_data"),
            Some(&Value::String("hello".into()))
        );
    }

    // -- to_package_data tests --

    #[test]
    fn test_to_data_minimal() {
        let data = PackageMaker::new("foo").to_package_data();
        assert_eq!(data.get("name"), Some(&Value::String("foo".into())));
        assert!(!data.contains_key("version"));
        assert!(!data.contains_key("description"));
    }

    #[test]
    fn test_to_data_with_version() {
        let data = PackageMaker::new("foo")
            .version(Version::new("1.2.3").unwrap())
            .to_package_data();

        assert_eq!(data.get("version"), Some(&Value::String("1.2.3".into())));
    }

    #[test]
    fn test_to_data_empty_version_skipped() {
        // Empty version should not be included
        let data = PackageMaker::new("foo")
            .version(Version::empty())
            .to_package_data();

        assert!(!data.contains_key("version"));
    }

    #[test]
    fn test_to_data_requires() {
        let data = PackageMaker::new("foo")
            .requires(vec![
                Requirement::new("bar-1.0+").unwrap(),
                Requirement::new("baz-2.0").unwrap(),
            ])
            .to_package_data();

        let reqs = data.get("requires").unwrap().as_array().unwrap();
        assert_eq!(reqs.len(), 2);
        // Verify they're string representations
        assert!(reqs[0].is_string());
        assert!(reqs[1].is_string());
    }

    #[test]
    fn test_to_data_variants() {
        let data = PackageMaker::new("foo")
            .variants(vec![
                vec![Requirement::new("python-2.7").unwrap()],
                vec![Requirement::new("python-3.7").unwrap()],
            ])
            .to_package_data();

        let variants = data.get("variants").unwrap().as_array().unwrap();
        assert_eq!(variants.len(), 2);
        assert!(variants[0].is_array());
    }

    #[test]
    fn test_to_data_all_fields() {
        let data = PackageMaker::new("full")
            .version(Version::new("1.0").unwrap())
            .description("full package")
            .authors(vec!["Author".into()])
            .requires(vec![Requirement::new("dep-1+").unwrap()])
            .build_requires(vec![Requirement::new("cmake-3+").unwrap()])
            .commands("env.PATH.append('{root}/bin')")
            .tools(vec!["mytool".into()])
            .uuid("uuid-123")
            .timestamp(1700000000)
            .help("https://example.com")
            .relocatable(true)
            .cachable(false)
            .hashed_variants(true)
            .build_system("cmake")
            .set("custom", Value::Number(42.into()))
            .to_package_data();

        assert_eq!(data.get("name"), Some(&Value::String("full".into())));
        assert_eq!(data.get("version"), Some(&Value::String("1.0".into())));
        assert_eq!(
            data.get("description"),
            Some(&Value::String("full package".into()))
        );
        assert_eq!(data.get("uuid"), Some(&Value::String("uuid-123".into())));
        assert_eq!(
            data.get("timestamp"),
            Some(&Value::Number(1700000000.into()))
        );
        assert_eq!(data.get("relocatable"), Some(&Value::Bool(true)));
        assert_eq!(data.get("cachable"), Some(&Value::Bool(false)));
        assert_eq!(data.get("hashed_variants"), Some(&Value::Bool(true)));
        assert_eq!(
            data.get("build_system"),
            Some(&Value::String("cmake".into()))
        );
        assert_eq!(data.get("custom"), Some(&Value::Number(42.into())));
    }

    #[test]
    fn test_extra_data_overrides() {
        // Extra data should override typed fields if same key
        let data = PackageMaker::new("foo")
            .description("original")
            .set("description", Value::String("overridden".into()))
            .to_package_data();

        assert_eq!(
            data.get("description"),
            Some(&Value::String("overridden".into()))
        );
    }

    // -- write_to tests --

    #[test]
    fn test_write_to_yaml() {
        let dir = std::env::temp_dir().join("rez_test_maker_yaml");
        let _ = fs::remove_dir_all(&dir);

        let result = PackageMaker::new("testpkg")
            .version(Version::new("1.0.0").unwrap())
            .description("test package")
            .write_to(&dir, FileFormat::Yaml);

        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(path.exists());
        assert_eq!(path.file_name().unwrap().to_str().unwrap(), "package.yaml");

        // Read back and verify it contains expected content
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("testpkg"));
        assert!(content.contains("1.0.0"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_write_to_toml() {
        let dir = std::env::temp_dir().join("rez_test_maker_toml");
        let _ = fs::remove_dir_all(&dir);

        let result = PackageMaker::new("tomlpkg")
            .version(Version::new("2.1").unwrap())
            .write_to(&dir, FileFormat::Toml);

        assert!(result.is_ok());
        let path = result.unwrap();
        assert_eq!(path.file_name().unwrap().to_str().unwrap(), "package.toml");
        assert!(path.exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_write_to_py_rejected() {
        let dir = std::env::temp_dir().join("rez_test_maker_py");
        let result = PackageMaker::new("nopy").write_to(&dir, FileFormat::Py);
        assert!(result.is_err());
    }

    #[test]
    fn test_write_to_creates_dir() {
        let dir = std::env::temp_dir()
            .join("rez_test_maker_newdir")
            .join("sub");
        let _ = fs::remove_dir_all(dir.parent().unwrap());

        let result = PackageMaker::new("dirtest").write_to(&dir, FileFormat::Yaml);
        assert!(result.is_ok());
        assert!(dir.exists());

        let _ = fs::remove_dir_all(dir.parent().unwrap());
    }

    // -- make_package tests --

    #[test]
    fn test_make_package_basic() {
        let root = std::env::temp_dir().join("rez_test_make_pkg");
        let _ = fs::remove_dir_all(&root);

        let result = make_package("mypkg", &root, FileFormat::Yaml, |pkg| {
            pkg.version = Some(Version::new("3.0.0").unwrap());
            pkg.description = Some("made with make_package".into());
        });

        assert!(result.is_ok());
        let path = result.unwrap();

        // Should be at {root}/mypkg/3.0.0/package.yaml
        assert!(path.exists());
        assert_eq!(path.file_name().unwrap().to_str().unwrap(), "package.yaml");

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("mypkg"));
        assert!(content.contains("3.0.0"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_make_package_no_version() {
        let root = std::env::temp_dir().join("rez_test_make_pkg_nover");
        let _ = fs::remove_dir_all(&root);

        let result = make_package("simplpkg", &root, FileFormat::Toml, |pkg| {
            pkg.description = Some("unversioned".into());
        });

        assert!(result.is_ok());
        let path = result.unwrap();

        // Should be at {root}/simplpkg/package.toml (no version subdir)
        assert!(path.exists());
        assert!(path.parent().unwrap().ends_with("simplpkg"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_make_package_with_requires() {
        let root = std::env::temp_dir().join("rez_test_make_pkg_reqs");
        let _ = fs::remove_dir_all(&root);

        let result = make_package("depspkg", &root, FileFormat::Yaml, |pkg| {
            pkg.version = Some(Version::new("1.0").unwrap());
            pkg.requires = Some(vec![
                Requirement::new("python-3.7+").unwrap(),
                Requirement::new("numpy-1.20+").unwrap(),
            ]);
            pkg.tools = Some(vec!["depstool".into()]);
        });

        assert!(result.is_ok());
        let content = fs::read_to_string(result.unwrap()).unwrap();
        assert!(content.contains("depspkg"));
        assert!(content.contains("python"));
        assert!(content.contains("numpy"));

        let _ = fs::remove_dir_all(&root);
    }

    fn wildcard_test_repo() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for version in ["1.5.0", "2.7.12", "2.7.13", "3.1.4"] {
            let package_dir = root.path().join("foo").join(version);
            fs::create_dir_all(&package_dir).unwrap();
            fs::write(
                package_dir.join("package.yaml"),
                format!("name: foo\nversion: \"{version}\"\n"),
            )
            .unwrap();
        }
        root
    }

    fn expand_request_in_data(request: &str, paths: &[PathBuf]) -> Result<String> {
        let mut data = HashMap::from([(
            "requires".to_string(),
            Value::Array(vec![Value::String(request.to_string())]),
        )]);
        expand_package_requests(&mut data, Some(paths))?;
        Ok(data["requires"][0].as_str().unwrap().to_string())
    }

    #[test]
    fn maker_wildcard_expansion_matches_rez() {
        let repo = wildcard_test_repo();
        let paths = [repo.path().to_path_buf()];

        assert_eq!(expand_request_in_data("foo-*", &paths).unwrap(), "foo-3");
        assert_eq!(
            expand_request_in_data("foo-2.*", &paths).unwrap(),
            "foo-2.7"
        );
        assert_eq!(
            expand_request_in_data("foo-**", &paths).unwrap(),
            "foo-3.1.4"
        );
        assert_eq!(
            expand_request_in_data("foo<**", &paths).unwrap(),
            "foo<3.1.4"
        );
        assert_eq!(
            expand_request_in_data("foo-1.*|2.**", &paths).unwrap(),
            "foo-1.5|2.7.13"
        );
        assert_eq!(
            expand_request_in_data("foo-**|1.5", &paths).unwrap(),
            "foo-1.5|3.1.4"
        );
    }

    #[test]
    fn maker_wildcards_cover_each_request_field() {
        let repo = wildcard_test_repo();
        let paths = [repo.path().to_path_buf()];
        let mut data = HashMap::from([
            ("requires".to_string(), serde_json::json!(["foo-*"])),
            ("build_requires".to_string(), serde_json::json!(["foo-**"])),
            (
                "private_build_requires".to_string(),
                serde_json::json!(["foo-2.*"]),
            ),
            ("variants".to_string(), serde_json::json!([["foo-*"]])),
            (
                "tests".to_string(),
                serde_json::json!({
                    "smoke": {
                        "command": "true",
                        "requires": ["foo-**"],
                        "on_variants": {"type": "requires", "value": ["foo-2.*"]}
                    }
                }),
            ),
        ]);

        expand_package_requests(&mut data, Some(&paths)).unwrap();

        assert_eq!(data["requires"][0], "foo-3");
        assert_eq!(data["build_requires"][0], "foo-3.1.4");
        assert_eq!(data["private_build_requires"][0], "foo-2.7");
        assert_eq!(data["variants"][0][0], "foo-3");
        assert_eq!(data["tests"]["smoke"]["requires"][0], "foo-3.1.4");
        assert_eq!(data["tests"]["smoke"]["on_variants"]["value"][0], "foo-2.7");
    }

    #[test]
    fn maker_wildcard_missing_family_preserves_prefix() {
        let empty = tempfile::tempdir().unwrap();
        let paths = [empty.path().to_path_buf()];
        assert_eq!(
            expand_request_in_data("missing-1.*", &paths).unwrap(),
            "missing-1"
        );
    }

    #[test]
    fn maker_wildcard_invalid_placement_and_mixing_are_rejected() {
        let repo = wildcard_test_repo();
        let paths = [repo.path().to_path_buf()];
        for request in ["foo-1.v*", "foo-1.*.0", "foo-*.**", "foo-1.**.*"] {
            assert!(
                expand_request_in_data(request, &paths).is_err(),
                "expected {request} to be rejected"
            );
        }
    }

    #[test]
    fn ordinary_package_loading_does_not_expand_wildcards() {
        let package_data = HashMap::from([
            ("name".to_string(), Value::String("consumer".into())),
            ("requires".to_string(), serde_json::json!(["foo-*"])),
        ]);
        assert!(model::package::Package::from_data(package_data).is_err());
    }
}
