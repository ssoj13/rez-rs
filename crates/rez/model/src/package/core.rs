// SPDX-License-Identifier: Apache-2.0

//! Core package types — Package, Variant, PackageFamily, DeveloperPackage.
//!
//! `Package` = full metadata from package.py/yaml/toml; built via `Package::from_data` which calls
//! `serialise::validate_package_data`. `DeveloperPackage` = Package + filepath; loads via
//! `DeveloperPackage::from_path` (probes PACKAGE_FILE_NAMES, validates, parses).
//!
//! # Used by
//! - `package/discover::get_developer_package` — delegates to `DeveloperPackage::from_path`
//! - `builders` — BuildProcess uses Package for env vars
//! - `resolve` — ResolvedPackageInfo carries Package-like data

use crate::errors::RezError;
use crate::serialise::{eval_late_binding_for_package, is_late_binding_source, FileFormat};
use version::{Requirement, Version};
// use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

// ---------------------------------------------------------------------------
// PackageRequest - alias for Requirement (used in resolver context)
// ---------------------------------------------------------------------------

/// Package request string like "foo-1.2+", "!bar", "~baz<2.0".
///
/// This is a type alias for Requirement, used in resolver/request contexts.
/// Supports normal requirements, conflict requirements (!), and weak requirements (~).
///
/// # Examples
/// ```no_run
/// use model::package::PackageRequest;
///
/// let req = PackageRequest::new("foo-1.2+<2.0").unwrap();
/// assert_eq!(req.name(), "foo");
/// ```
pub type PackageRequest = Requirement;

// ---------------------------------------------------------------------------
// Parsing helpers for from_data()
// ---------------------------------------------------------------------------

use serde_json::Value;

/// Extract optional string from data map.
fn get_str(data: &HashMap<String, Value>, key: &str) -> Option<String> {
    data.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Extract optional bool from data map.
fn get_bool(data: &HashMap<String, Value>, key: &str) -> Option<bool> {
    data.get(key).and_then(|v| v.as_bool())
}

/// Extract an optional integer from package data without losing negative timestamps.
fn get_integer(data: &HashMap<String, Value>, key: &str) -> Option<i128> {
    data.get(key).and_then(|value| match value {
        Value::Number(number) => number
            .as_i64()
            .map(i128::from)
            .or_else(|| number.as_u64().map(i128::from)),
        Value::Bool(value) => Some(if *value { 1 } else { 0 }),
        _ => None,
    })
}

/// Parse JSON array of strings into Vec<String>.
fn parse_str_list(val: Option<&Value>) -> Vec<String> {
    val.and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Resolve value for a field that may be array or late-binding string. Returns the value to parse.
fn resolve_late_or_direct(
    data: &HashMap<String, Value>,
    key: &str,
) -> std::result::Result<Option<Value>, RezError> {
    let Some(val) = data.get(key) else {
        return Ok(None);
    };
    if let Some(arr) = val.as_array() {
        return Ok(Some(Value::Array(arr.clone())));
    }
    if let Some(source) = val.as_str().filter(|_| is_late_binding_source(val, key)) {
        return eval_late_binding_for_package(source, key, data, false).map(Some);
    }
    Ok(None)
}

/// Parse JSON array of requirement strings into Vec<Requirement>.
fn parse_req_list(val: Option<&Value>) -> Result<Vec<Requirement>, RezError> {
    let Some(arr) = val.and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    arr.iter()
        .map(|v| {
            let s = v.as_str().ok_or_else(|| RezError::PackageMetadata {
                msg: format!("requirement must be a string, got: {v}"),
                path: None,
                resource_key: None,
            })?;
            Requirement::new(s)
        })
        .collect()
}

/// Parse JSON 2D array of variants: [["python-3.9"], ["python-3.10"]].
fn parse_variants(val: Option<&Value>) -> Result<Vec<Vec<Requirement>>, RezError> {
    let Some(arr) = val.and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    arr.iter().map(|row| parse_req_list(Some(row))).collect()
}

/// Parse help field: string -> Help::Single, array of arrays -> Help::Multiple.
fn parse_help(val: Option<&Value>) -> Option<Help> {
    let v = val?;
    if let Some(s) = v.as_str() {
        return Some(Help::Single(s.to_string()));
    }
    if let Some(arr) = v.as_array() {
        let entries: Vec<Vec<String>> = arr
            .iter()
            .filter_map(|item| {
                item.as_array().map(|inner| {
                    inner
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
            })
            .collect();
        if !entries.is_empty() {
            return Some(Help::Multiple(entries));
        }
    }
    None
}

/// Convert serde_json::Value (must be Object) to HashMap for from_data().
fn value_to_map(val: Value) -> Result<HashMap<String, Value>, RezError> {
    match val {
        Value::Object(m) => Ok(m.into_iter().collect()),
        _ => Err(RezError::PackageMetadata {
            msg: "package data must be a mapping/object".into(),
            path: None,
            resource_key: None,
        }),
    }
}

// ---------------------------------------------------------------------------
// Package - main package structure
// ---------------------------------------------------------------------------

/// A rez package with all metadata, dependencies, and commands.
///
/// This represents a package definition loaded from package.py/toml/yaml.
/// Contains all package attributes from the schema.
///
/// # Examples
/// ```no_run
/// use model::package::Package;
/// use version::Version;
///
/// let pkg = Package {
///     name: "foo".into(),
///     version: Version::new("1.2.3").unwrap(),
///     ..Default::default()
/// };
/// assert_eq!(pkg.name, "foo");
/// ```
#[derive(Clone, Debug)]
pub struct Package {
    // Core
    pub name: String,
    pub version: Version,
    pub description: Option<String>,
    pub authors: Vec<String>,

    // Dependencies
    pub requires: Vec<Requirement>,
    pub build_requires: Vec<Requirement>,
    pub private_build_requires: Vec<Requirement>,

    // Variants: each inner Vec is a variant combination
    pub variants: Vec<Vec<Requirement>>,

    // Plugin
    pub has_plugins: bool,
    pub plugin_for: Vec<String>,

    // Metadata
    pub uuid: Option<String>,
    pub tools: Vec<String>,
    pub tags: Vec<String>,
    pub help: Option<Help>,
    pub hashed_variants: bool,
    pub relocatable: Option<bool>,
    pub cachable: Option<bool>,

    // Build
    pub build_system: Option<String>,
    pub build_command: Option<String>,
    pub requires_rez_version: Option<String>,

    // Commands (as strings for now, will be SourceCode later)
    pub commands: Option<String>,
    pub pre_commands: Option<String>,
    pub post_commands: Option<String>,
    pub pre_build_commands: Option<String>,
    pub pre_test_commands: Option<String>,

    // Tests
    pub tests: Option<HashMap<String, serde_json::Value>>,

    // Release info
    pub timestamp: Option<i128>,
    pub revision: Option<serde_json::Value>,
    pub changelog: Option<String>,
    pub release_message: Option<String>,
    pub previous_version: Option<Version>,
    pub previous_revision: Option<serde_json::Value>,
    pub vcs: Option<String>,

    // Repository attributes
    /// Complete parsed package attribute mapping, including unknown Rez extensions.
    /// Typed fields above provide normalized access to common values; this mapping
    /// preserves the original data for consumers that use custom package attributes.
    pub attributes: HashMap<String, Value>,

    // Internal
    pub base: Option<PathBuf>,
    pub config: Option<serde_json::Value>,
}

impl Default for Package {
    fn default() -> Self {
        Self {
            name: String::new(),
            version: Version::empty(),
            description: None,
            authors: Vec::new(),
            requires: Vec::new(),
            build_requires: Vec::new(),
            private_build_requires: Vec::new(),
            variants: Vec::new(),
            has_plugins: false,
            plugin_for: Vec::new(),
            uuid: None,
            tools: Vec::new(),
            tags: Vec::new(),
            help: None,
            hashed_variants: crate::constants::DEFAULT_HASHED_VARIANTS,
            relocatable: None,
            cachable: None,
            build_system: None,
            build_command: None,
            requires_rez_version: None,
            commands: None,
            pre_commands: None,
            post_commands: None,
            pre_build_commands: None,
            pre_test_commands: None,
            tests: None,
            timestamp: None,
            revision: None,
            changelog: None,
            release_message: None,
            previous_version: None,
            previous_revision: None,
            vcs: None,
            attributes: HashMap::new(),
            base: None,
            config: None,
        }
    }
}

impl Package {
    /// Create a simple package with name and version.
    pub fn new(name: impl Into<String>, version: Version) -> Self {
        Self {
            name: name.into(),
            version,
            ..Default::default()
        }
    }

    /// Build a Package from a key-value data map (the core parsing method).
    ///
    /// All format-specific loaders (YAML, TOML, JSON) convert to this
    /// intermediate representation before constructing a Package.
    pub fn from_data(mut data: HashMap<String, Value>) -> Result<Self, RezError> {
        crate::package::commands::normalize(&mut data, &crate::config::CONFIG)?;
        // Validate schema first
        crate::serialise::validate_package_data(&data)?;

        let name = get_str(&data, "name").ok_or_else(|| RezError::PackageMetadata {
            msg: "missing 'name' field".into(),
            path: None,
            resource_key: None,
        })?;

        let version = match get_str(&data, "version") {
            Some(s) => Version::new(&s)?,
            None => Version::empty(),
        };

        let previous_version = get_str(&data, "previous_version")
            .map(|s| Version::new(&s))
            .transpose()?;

        // Parse tests: keep as raw JSON map for flexibility
        let tests = data.get("tests").and_then(|v| {
            v.as_object()
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        });

        Ok(Package {
            name,
            version,
            description: get_str(&data, "description"),
            authors: parse_str_list(data.get("authors")),
            requires: parse_req_list(resolve_late_or_direct(&data, "requires")?.as_ref())?,
            build_requires: parse_req_list(
                resolve_late_or_direct(&data, "build_requires")?.as_ref(),
            )?,
            private_build_requires: parse_req_list(
                resolve_late_or_direct(&data, "private_build_requires")?.as_ref(),
            )?,
            variants: parse_variants(resolve_late_or_direct(&data, "variants")?.as_ref())?,
            // Hardcoded false: plugins are rare enough that no config default is warranted.
            has_plugins: get_bool(&data, "has_plugins").unwrap_or(false),
            plugin_for: parse_str_list(data.get("plugin_for")),
            uuid: get_str(&data, "uuid"),
            tools: parse_str_list(resolve_late_or_direct(&data, "tools")?.as_ref()),
            tags: parse_str_list(data.get("tags")),
            help: parse_help(data.get("help")),
            hashed_variants: get_bool(&data, "hashed_variants")
                .unwrap_or_else(|| crate::config::CONFIG.default_hashed_variants),
            relocatable: get_bool(&data, "relocatable"),
            cachable: get_bool(&data, "cachable"),
            build_system: get_str(&data, "build_system"),
            build_command: get_str(&data, "build_command"),
            requires_rez_version: get_str(&data, "requires_rez_version"),
            commands: get_str(&data, "commands"),
            pre_commands: get_str(&data, "pre_commands"),
            post_commands: get_str(&data, "post_commands"),
            pre_build_commands: get_str(&data, "pre_build_commands"),
            pre_test_commands: get_str(&data, "pre_test_commands"),
            tests,
            timestamp: get_integer(&data, "timestamp"),
            revision: data.get("revision").cloned(),
            changelog: get_str(&data, "changelog"),
            release_message: get_str(&data, "release_message"),
            previous_version,
            previous_revision: data.get("previous_revision").cloned(),
            vcs: get_str(&data, "vcs"),
            base: get_str(&data, "base").map(PathBuf::from),
            config: data.get("config").cloned(),
            attributes: data,
        })
    }

    /// Export normalized metadata while retaining custom and deferred attributes.
    pub fn to_data(&self) -> Result<HashMap<String, Value>, RezError> {
        let mut data = self.attributes.clone();
        data.insert("name".into(), Value::String(self.name.clone()));
        if self.version.is_empty() {
            data.remove("version");
        } else {
            data.insert("version".into(), Value::String(self.version.to_string()));
        }
        for (key, requirements) in [
            ("requires", &self.requires),
            ("build_requires", &self.build_requires),
            ("private_build_requires", &self.private_build_requires),
        ] {
            if !data.contains_key(&format!("late_{key}")) {
                data.insert(
                    key.into(),
                    serde_json::json!(requirements
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()),
                );
            }
        }
        data.insert(
            "variants".into(),
            serde_json::json!(self
                .variants
                .iter()
                .map(|variant| variant.iter().map(ToString::to_string).collect::<Vec<_>>())
                .collect::<Vec<_>>()),
        );
        for (key, values) in [
            ("authors", &self.authors),
            ("plugin_for", &self.plugin_for),
            ("tools", &self.tools),
            ("tags", &self.tags),
        ] {
            if !data.contains_key(&format!("late_{key}")) {
                data.insert(key.into(), serde_json::json!(values));
            }
        }
        for (key, value) in [
            ("description", self.description.as_ref()),
            ("uuid", self.uuid.as_ref()),
            ("build_system", self.build_system.as_ref()),
            ("build_command", self.build_command.as_ref()),
            ("requires_rez_version", self.requires_rez_version.as_ref()),
            ("commands", self.commands.as_ref()),
            ("pre_commands", self.pre_commands.as_ref()),
            ("post_commands", self.post_commands.as_ref()),
            ("pre_build_commands", self.pre_build_commands.as_ref()),
            ("pre_test_commands", self.pre_test_commands.as_ref()),
            ("changelog", self.changelog.as_ref()),
            ("release_message", self.release_message.as_ref()),
            ("vcs", self.vcs.as_ref()),
        ] {
            match value {
                Some(value) => {
                    data.insert(key.into(), Value::String(value.clone()));
                }
                None => {
                    data.remove(key);
                }
            }
        }
        data.insert("hashed_variants".into(), Value::Bool(self.hashed_variants));
        data.insert("has_plugins".into(), Value::Bool(self.has_plugins));
        for (key, value) in [
            ("relocatable", self.relocatable),
            ("cachable", self.cachable),
        ] {
            match value {
                Some(value) => {
                    data.insert(key.into(), Value::Bool(value));
                }
                None => {
                    data.remove(key);
                }
            }
        }
        if let Some(help) = &self.help {
            data.insert(
                "help".into(),
                match help {
                    Help::Single(value) => serde_json::json!(value),
                    Help::Multiple(values) => serde_json::json!(values),
                },
            );
        }
        if let Some(tests) = &self.tests {
            data.insert("tests".into(), serde_json::json!(tests));
        }
        if let Some(timestamp) = self.timestamp {
            data.insert("timestamp".into(), serde_json::to_value(timestamp)?);
        }
        for (key, value) in [
            ("revision", self.revision.as_ref()),
            ("previous_revision", self.previous_revision.as_ref()),
            ("config", self.config.as_ref()),
        ] {
            match value {
                Some(value) => {
                    data.insert(key.into(), value.clone());
                }
                None => {
                    data.remove(key);
                }
            }
        }
        if let Some(version) = &self.previous_version {
            data.insert(
                "previous_version".into(),
                serde_json::json!(version.to_string()),
            );
        }
        // Base is repository provenance, not installable package metadata.
        data.remove("base");
        crate::serialise::validate_package_data(&data)?;
        Ok(data)
    }

    /// Load package from YAML string. Parses YAML then delegates to from_data().
    pub fn from_yaml(yaml_str: &str) -> Result<Self, RezError> {
        let val: Value = serde_yaml::from_str(yaml_str).map_err(|e| RezError::PackageMetadata {
            msg: format!("YAML parse error: {e}"),
            path: None,
            resource_key: None,
        })?;
        Self::from_data(value_to_map(val)?)
    }

    /// Load package from TOML string. Parses TOML then delegates to from_data().
    pub fn from_toml(toml_str: &str) -> Result<Self, RezError> {
        let toml_val: toml::Value =
            toml::from_str(toml_str).map_err(|e| RezError::PackageMetadata {
                msg: format!("TOML parse error: {e}"),
                path: None,
                resource_key: None,
            })?;
        let json_val = serde_json::to_value(toml_val).map_err(|e| RezError::PackageMetadata {
            msg: format!("TOML->JSON conversion error: {e}"),
            path: None,
            resource_key: None,
        })?;
        Self::from_data(value_to_map(json_val)?)
    }

    /// Load package from JSON string. Parses JSON then delegates to from_data().
    pub fn from_json(json_str: &str) -> Result<Self, RezError> {
        let val: Value = serde_json::from_str(json_str)?;
        Self::from_data(value_to_map(val)?)
    }

    /// Get qualified package name: "name-version".
    pub fn qualified_name(&self) -> String {
        if self.version.is_truthy() {
            format!("{}-{}", self.name, self.version)
        } else {
            self.name.clone()
        }
    }

    /// Check if this package has variants.
    pub fn has_variants(&self) -> bool {
        !self.variants.is_empty()
    }

    /// Get number of variants.
    pub fn num_variants(&self) -> usize {
        self.variants.len()
    }

    /// Iterate over all variants.
    ///
    /// Returns an iterator that yields Variant instances for each variant.
    pub fn iter_variants(&self) -> impl Iterator<Item = Variant> + '_ {
        VariantIter {
            parent: Rc::new(self.clone()),
            index: 0,
        }
    }

    /// Resolve package configuration through the canonical typed config loader.
    ///
    /// An invalid package override fails without changing the caller's configuration.
    pub fn config(
        &self,
        base: Option<&crate::config::RezConfig>,
    ) -> crate::errors::Result<crate::config::RezConfig> {
        let mut config = base.unwrap_or(&crate::config::CONFIG).clone();
        if let Some(overrides) = &self.config {
            if !overrides.is_null() {
                let values = overrides.as_object().ok_or_else(|| {
                    RezError::Config(format!(
                        "{}: config must be a mapping",
                        self.qualified_name()
                    ))
                })?;
                config.merge_config_values(
                    &format!("package {}", self.qualified_name()),
                    values
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect(),
                )?;
            }
        }
        Ok(config)
    }

    /// Check if package is relocatable (safe to copy).
    ///
    /// Cascade: self.relocatable -> per_repo -> per_package -> default
    pub fn is_relocatable(
        &self,
        repo_path: Option<&str>,
        config: Option<&crate::config::RezConfig>,
    ) -> bool {
        let config = config.unwrap_or(&crate::config::CONFIG);
        self.relocatable
            .or_else(|| {
                repo_path.and_then(|repo| {
                    config
                        .default_relocatable_per_repository
                        .as_ref()?
                        .get(repo)
                        .copied()
                        .flatten()
                })
            })
            .or_else(|| {
                config
                    .default_relocatable_per_package
                    .as_ref()?
                    .get(&self.name)
                    .copied()
                    .flatten()
            })
            .unwrap_or(config.default_relocatable)
    }

    /// Check if package is cachable (safe to cache locally).
    ///
    /// Cascade: self.cachable -> per_repo -> per_package -> default_cachable -> is_relocatable
    pub fn is_cachable(
        &self,
        repo_path: Option<&str>,
        config: Option<&crate::config::RezConfig>,
    ) -> bool {
        let config = config.unwrap_or(&crate::config::CONFIG);
        self.cachable
            .or_else(|| {
                repo_path.and_then(|repo| {
                    config
                        .default_cachable_per_repository
                        .as_ref()?
                        .get(repo)
                        .copied()
                        .flatten()
                })
            })
            .or_else(|| {
                config
                    .default_cachable_per_package
                    .as_ref()?
                    .get(&self.name)
                    .copied()
                    .flatten()
            })
            .or(config.default_cachable)
            .unwrap_or_else(|| self.is_relocatable(repo_path, Some(config)))
    }
}

impl std::fmt::Display for Package {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.qualified_name())
    }
}

// ---------------------------------------------------------------------------
// Help - package help/documentation
// ---------------------------------------------------------------------------

/// Package help information.
///
/// Can be a single string or list of [title, url] pairs.
#[derive(Clone, Debug)]
pub enum Help {
    /// Single help string
    Single(String),
    /// Multiple help entries: [[title, url], ...]
    Multiple(Vec<Vec<String>>),
}

impl std::fmt::Display for Help {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Help::Single(s) => write!(f, "{}", s),
            Help::Multiple(entries) => {
                for entry in entries {
                    if entry.len() >= 2 {
                        writeln!(f, "{}: {}", entry[0], entry[1])?;
                    } else if entry.len() == 1 {
                        writeln!(f, "{}", entry[0])?;
                    }
                }
                Ok(())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Variant - a specific variant of a package
// ---------------------------------------------------------------------------

/// A specific variant of a package.
///
/// Variants represent different builds of the same package version,
/// typically for different platforms, architectures, or dependency sets.
///
/// # Examples
/// ```no_run
/// use model::package::{Package, Variant};
/// use version::Version;
/// use std::rc::Rc;
///
/// let pkg = Package::new("foo", Version::new("1.2.3").unwrap());
/// let variant = Variant {
///     parent: Rc::new(pkg),
///     index: Some(0),
///     variant_requires: vec![],
///     root: None,
///     subpath: None,
/// };
/// ```
#[derive(Clone, Debug)]
pub struct Variant {
    /// Reference to parent package
    pub parent: Rc<Package>,
    /// Variant index (None = non-variant package)
    pub index: Option<usize>,
    /// Variant-specific requirements
    pub variant_requires: Vec<Requirement>,
    /// Root install path for this variant
    pub root: Option<PathBuf>,
    /// Subpath within package (for variants)
    pub subpath: Option<String>,
}

impl Variant {
    /// Compute variant subpath from variant_requires.
    /// If hashed=true, returns SHA1 hex digest matching Python rez format.
    /// Otherwise returns nested dirs like "platform-windows/arch-x86_64".
    /// An empty indexed variant hashes `[]`; a non-variant package uses its root
    /// without calling this function. Empty non-hashed requirements return None.
    pub fn compute_subpath(requires: &[Requirement], hashed: bool) -> Option<String> {
        if requires.is_empty() && !hashed {
            return None;
        }
        if hashed {
            // Match Python: str(list(map(str, variant_requires)))
            // e.g. "['platform-windows', 'arch-x86_64']"
            // Rez hashes the Python list representation of canonical requirements.
            let items: Vec<String> = requires.iter().map(|r| format!("'{r}'")).collect();
            let vars_str = format!("[{}]", items.join(", "));
            use sha1::{Digest, Sha1};
            let hash = Sha1::digest(vars_str.as_bytes());
            return Some(crate::util::hex_encode(hash));
        }
        // Use "/" for subpath (Rez/Unix convention); PathBuf::join handles OS conversion
        let parts: Vec<String> = requires.iter().map(Requirement::safe_str).collect();
        Some(parts.join("/"))
    }

    /// Create a non-variant package variant (index=None).
    pub fn from_package(package: Package) -> Self {
        Self {
            parent: Rc::new(package),
            index: None,
            variant_requires: Vec::new(),
            root: None,
            subpath: None,
        }
    }

    /// Create a variant from package and index.
    pub fn new(package: Package, index: usize) -> Result<Self, RezError> {
        if index >= package.variants.len() {
            return Err(RezError::InvalidPackage(format!(
                "Variant index {} out of range (max {})",
                index,
                package.variants.len()
            )));
        }

        let variant_requires = package.variants[index].clone();
        let subpath = Self::compute_subpath(&variant_requires, package.hashed_variants);
        Ok(Self {
            parent: Rc::new(package),
            index: Some(index),
            variant_requires,
            root: None,
            subpath,
        })
    }

    /// Resolve the payload root, preserving explicitly supplied roots.
    pub fn root(&self) -> Option<PathBuf> {
        self.root.clone().or_else(|| {
            self.parent.base.as_ref().map(|base| {
                self.subpath
                    .as_ref()
                    .map_or_else(|| base.clone(), |subpath| base.join(subpath))
            })
        })
    }

    /// Get package name.
    pub fn name(&self) -> &str {
        &self.parent.name
    }

    /// Get package version.
    pub fn version(&self) -> &Version {
        &self.parent.version
    }

    /// Get qualified package name: "name-version".
    pub fn qualified_package_name(&self) -> String {
        self.parent.qualified_name()
    }

    /// Get qualified name with variant index: "name-version[index]".
    pub fn qualified_name(&self) -> String {
        if let Some(idx) = self.index {
            format!("{}[{}]", self.parent.qualified_name(), idx)
        } else {
            self.parent.qualified_name()
        }
    }

    /// Check if this is a variant (has index).
    pub fn is_variant(&self) -> bool {
        self.index.is_some()
    }

    /// Get variant requirements (parent requires + variant-specific requires).
    pub fn requires(&self) -> Vec<Requirement> {
        let mut reqs = self.parent.requires.clone();
        reqs.extend(self.variant_requires.clone());
        reqs
    }

    /// Get full build request: requires + build_requires + private_build_requires.
    /// Used by create_build_context when resolving the build environment.
    pub fn build_request(&self) -> Vec<Requirement> {
        let mut reqs = self.requires();
        reqs.extend(self.parent.build_requires.clone());
        reqs.extend(self.parent.private_build_requires.clone());
        reqs
    }
}

impl std::fmt::Display for Variant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.qualified_name())
    }
}

// ---------------------------------------------------------------------------
// VariantIter - iterator over package variants
// ---------------------------------------------------------------------------

/// Iterator over package variants.
struct VariantIter {
    parent: Rc<Package>,
    index: usize,
}

impl Iterator for VariantIter {
    type Item = Variant;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index < self.parent.variants.len() {
            let variant_requires = self.parent.variants[self.index].clone();
            let subpath = Variant::compute_subpath(&variant_requires, self.parent.hashed_variants);
            let variant = Variant {
                parent: Rc::clone(&self.parent),
                index: Some(self.index),
                variant_requires,
                root: None,
                subpath,
            };
            self.index += 1;
            Some(variant)
        } else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.parent.variants.len().saturating_sub(self.index);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for VariantIter {}

// ---------------------------------------------------------------------------
// PackageFamily - collection of packages with same name
// ---------------------------------------------------------------------------

/// A package family - collection of all versions of a package.
///
/// # Examples
/// ```no_run
/// use model::package::PackageFamily;
///
/// let family = PackageFamily {
///     name: "foo".into(),
///     packages: vec![],
/// };
/// ```
#[derive(Clone, Debug)]
pub struct PackageFamily {
    /// Package name
    pub name: String,
    /// All package versions in this family
    pub packages: Vec<Package>,
}

impl PackageFamily {
    /// Create a new package family.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            packages: Vec::new(),
        }
    }

    /// Add a package to this family.
    pub fn add_package(&mut self, package: Package) {
        self.packages.push(package);
    }

    /// Get number of packages in this family.
    pub fn num_packages(&self) -> usize {
        self.packages.len()
    }

    /// Iterate over all packages.
    pub fn iter_packages(&self) -> impl Iterator<Item = &Package> {
        self.packages.iter()
    }
}

impl std::fmt::Display for PackageFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({} versions)", self.name, self.packages.len())
    }
}

// ---------------------------------------------------------------------------
// DeveloperPackage - package with filepath tracking
// ---------------------------------------------------------------------------

/// A developer package loaded from filesystem.
///
/// Extends Package with filepath tracking for source packages
/// being developed locally.
///
/// # Examples
/// ```no_run
/// use model::package::{DeveloperPackage, Package};
/// use version::Version;
/// use std::path::PathBuf;
///
/// let pkg = Package::new("foo", Version::new("1.2.3").unwrap());
/// let dev_pkg = DeveloperPackage::new(pkg, PathBuf::from("/path/to/package.toml"));
/// assert_eq!(dev_pkg.name(), "foo");
/// ```
#[derive(Clone, Debug)]
pub struct DeveloperPackage {
    /// The underlying package
    pub package: Package,
    /// Path to package definition file (package.py, package.toml, etc.)
    pub filepath: PathBuf,
    /// Include modules found in the package definition source.
    pub includes: Vec<String>,
    /// Source format used to reload this developer package for build/test objects.
    pub source_format: Option<FileFormat>,
}

impl DeveloperPackage {
    /// Create a developer package.
    pub fn new(package: Package, filepath: PathBuf) -> Self {
        let source_format = crate::serialise::detect_format(&filepath);
        Self {
            package,
            filepath,
            includes: Vec::new(),
            source_format,
        }
    }

    /// Root directory of the developer package (parent of filepath).
    pub fn root(&self) -> Option<&std::path::Path> {
        self.filepath.parent()
    }

    /// Load a developer package from a directory or file path.
    ///
    /// Directory loading uses the configured filename/format order and retains the exact source path.
    /// Full validation via `validate_package_data` + `Package::from_data`.
    /// This is the canonical path for developer packages; `get_developer_package` delegates here.
    pub fn from_path(path: &std::path::Path) -> Result<Self, RezError> {
        use crate::serialise::{
            detect_format, load_from_file, load_package_definition, PackageDataCache,
        };

        let (data, filepath, source_format) = if path.is_dir() {
            let (data, format, source_path) =
                load_package_definition(path, PackageDataCache::Disabled)?;
            (data, source_path, format)
        } else {
            if !path.is_file() {
                return Err(RezError::PackageNotFound(format!(
                    "Package file not found: {}",
                    path.display()
                )));
            }
            let format = detect_format(path).ok_or_else(|| {
                RezError::PackageNotFound(format!("Cannot determine format of {}", path.display()))
            })?;
            let data = load_from_file(path, format, PackageDataCache::Disabled)?;
            (data, path.to_path_buf(), format)
        };

        Self::from_loaded(data, filepath, source_format)
    }

    /// Re-evaluate this package definition with Rez lifecycle objects.
    ///
    /// The source is read again through the canonical loader with parsed-package
    /// caching disabled, because @early values can depend on the supplied objects.
    pub fn reevaluate(
        &self,
        early_objects: &HashMap<String, Value>,
    ) -> std::result::Result<Self, RezError> {
        let source_format = self
            .source_format
            .ok_or_else(|| RezError::PackageMetadata {
                msg: "Cannot reevaluate a developer package without a recognized source format"
                    .into(),
                path: Some(self.filepath.clone()),
                resource_key: None,
            })?;
        let data = crate::serialise::load_from_file_with_objects(
            &self.filepath,
            source_format,
            crate::serialise::PackageDataCache::Disabled,
            early_objects,
        )?;
        Self::from_loaded(data, self.filepath.clone(), source_format)
    }

    /// Reevaluate only the build target with the shared per-variant lifecycle objects.
    /// Dependency candidates retain their ordinary installed-package metadata.
    pub fn build_variant(&self, index: usize) -> std::result::Result<Self, RezError> {
        let variant = if self.package.variants.is_empty() {
            if index != 0 {
                return Err(RezError::InvalidPackage(format!(
                    "Variant index {index} out of range"
                )));
            }
            Vec::new()
        } else {
            self.package
                .variants
                .get(index)
                .ok_or_else(|| {
                    RezError::InvalidPackage(format!("Variant index {index} out of range"))
                })?
                .iter()
                .map(|requirement| Value::String(requirement.to_string()))
                .collect()
        };
        self.reevaluate(&HashMap::from([
            ("building".to_owned(), Value::Bool(true)),
            ("build_variant_index".to_owned(), Value::from(index)),
            ("build_variant_requires".to_owned(), Value::Array(variant)),
        ]))
    }

    fn from_loaded(
        data: HashMap<String, Value>,
        filepath: PathBuf,
        source_format: FileFormat,
    ) -> std::result::Result<Self, RezError> {
        let name =
            data.get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| RezError::PackageMetadata {
                    msg: "Missing or non-string 'name' field".into(),
                    path: Some(filepath.clone()),
                    resource_key: None,
                })?;

        if name.is_empty() {
            return Err(RezError::PackageMetadata {
                msg: "Package 'name' field is empty".into(),
                path: Some(filepath.clone()),
                resource_key: None,
            });
        }

        let mut includes = Vec::new();
        for source in data
            .values()
            .filter_map(Value::as_str)
            .filter(|source| source.starts_with("@include("))
        {
            for name in crate::serialise::extract_includes_from_package_py(source)? {
                if !includes.contains(&name) {
                    includes.push(name);
                }
            }
        }
        if !includes.is_empty() {
            let directory = crate::serialise::include_directory(None, data.get("config"))?;
            for name in &includes {
                crate::serialise::include_module_path(name, &directory, false)?;
            }
        }
        let mut package = Package::from_data(data)?;
        package.base = filepath.parent().map(std::path::Path::to_path_buf);

        Ok(Self {
            package,
            filepath,
            includes,
            source_format: Some(source_format),
        })
    }

    /// Get package name.
    pub fn name(&self) -> &str {
        &self.package.name
    }

    /// Get package version.
    pub fn version(&self) -> &Version {
        &self.package.version
    }

    /// Get qualified name.
    pub fn qualified_name(&self) -> String {
        self.package.qualified_name()
    }
}

impl std::fmt::Display for DeveloperPackage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({})",
            self.package.qualified_name(),
            self.filepath.display()
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_late_bound_metadata_errors_propagate_from_package_construction() {
        let data = HashMap::from([
            ("name".to_owned(), Value::String("foo".to_owned())),
            ("version".to_owned(), Value::String("1.0".to_owned())),
            (
                "requires".to_owned(),
                Value::String("def requires():\n    return 1 / 0".to_owned()),
            ),
        ]);

        let error = Package::from_data(data).unwrap_err().to_string();
        assert!(error.contains("exec late requires"), "{error}");
        assert!(error.contains("ZeroDivisionError"), "{error}");
    }

    #[test]
    fn test_from_data_preserves_unknown_and_known_attributes() {
        let mut data = HashMap::from([
            ("name".to_owned(), Value::String("foo".to_owned())),
            (
                "custom_scalar".to_owned(),
                Value::String("extension".to_owned()),
            ),
            (
                "custom_object".to_owned(),
                serde_json::json!({"enabled": true, "labels": ["a", "b"]}),
            ),
        ]);

        let package = Package::from_data(data.clone()).expect("valid package");

        assert_eq!(package.name, "foo");
        assert_eq!(package.attributes, data);
        data.insert("custom_scalar".to_owned(), Value::Null);
        assert_ne!(
            package.attributes.get("custom_scalar"),
            data.get("custom_scalar")
        );
    }

    #[test]
    fn test_package_new() {
        let pkg = Package::new("foo", Version::new("1.2.3").unwrap());
        assert_eq!(pkg.name, "foo");
        assert_eq!(pkg.version.to_string(), "1.2.3");
    }

    #[test]
    fn test_package_default() {
        let pkg = Package::default();
        assert_eq!(pkg.name, "");
        assert!(pkg.version.is_empty());
        assert_eq!(pkg.authors.len(), 0);
        assert_eq!(pkg.requires.len(), 0);
    }

    #[test]
    fn test_package_qualified_name() {
        let pkg = Package::new("foo", Version::new("1.2.3").unwrap());
        assert_eq!(pkg.qualified_name(), "foo-1.2.3");

        let pkg_no_ver = Package {
            name: "bar".into(),
            ..Default::default()
        };
        assert_eq!(pkg_no_ver.qualified_name(), "bar");
    }

    #[test]
    fn test_package_display() {
        let pkg = Package::new("foo", Version::new("1.2.3").unwrap());
        assert_eq!(pkg.to_string(), "foo-1.2.3");
    }

    #[test]
    fn test_package_has_variants() {
        let mut pkg = Package::new("foo", Version::new("1.2.3").unwrap());
        assert!(!pkg.has_variants());

        pkg.variants.push(vec![]);
        assert!(pkg.has_variants());
        assert_eq!(pkg.num_variants(), 1);
    }

    #[test]
    fn test_timestamp_preserves_signed_and_boolean_integer_values() {
        for (value, expected) in [(serde_json::json!(-1), -1), (serde_json::json!(true), 1)] {
            let data = serde_json::json!({"name": "foo", "timestamp": value});
            let data = data.as_object().unwrap().clone().into_iter().collect();
            let package = Package::from_data(data).unwrap();
            assert_eq!(package.timestamp, Some(expected));
        }
    }

    #[test]
    fn test_from_data_minimal() {
        let mut data = HashMap::new();
        data.insert("name".into(), serde_json::json!("foo"));
        let pkg = Package::from_data(data.clone()).unwrap();
        assert_eq!(pkg.name, "foo");
        assert!(pkg.version.is_empty());
        assert!(!pkg.hashed_variants);
        data.insert("variants".into(), serde_json::json!([["platform-windows"]]));
        let package = Package::from_data(data).unwrap();
        let variant = Variant::new(package, 0).unwrap();
        assert_eq!(variant.subpath.as_deref(), Some("platform-windows"));
    }

    #[test]
    fn test_from_data_missing_name() {
        let data = HashMap::new();
        assert!(Package::from_data(data).is_err());
    }

    #[test]
    fn test_from_data_full() {
        let data: HashMap<String, serde_json::Value> = serde_json::from_str(
            r#"{
            "name": "foo",
            "version": "1.2.0",
            "description": "A foo package",
            "authors": ["John", "Jane"],
            "requires": ["bar-1+", "baz-2.0"],
            "build_requires": ["cmake"],
            "variants": [["python-3.9"], ["python-3.10"]],
            "tools": ["foo_tool", "bar_tool"],
            "uuid": "abc-123",
            "has_plugins": true,
            "plugin_for": ["core"],
            "hashed_variants": true,
            "relocatable": true,
            "cachable": false,
            "build_system": "cmake",
            "commands": "env.PATH.append('{root}/bin')",
            "timestamp": 1700000000,
            "vcs": "git",
            "help": "See docs"
        }"#,
        )
        .unwrap();

        let pkg = Package::from_data(data).unwrap();
        assert_eq!(pkg.name, "foo");
        assert_eq!(pkg.version.to_string(), "1.2.0");
        assert_eq!(pkg.description.as_deref(), Some("A foo package"));
        assert_eq!(pkg.authors, vec!["John", "Jane"]);
        assert_eq!(pkg.requires.len(), 2);
        assert_eq!(pkg.requires[0].name(), "bar");
        assert_eq!(pkg.build_requires.len(), 1);
        assert_eq!(pkg.variants.len(), 2);
        assert_eq!(pkg.tools, vec!["foo_tool", "bar_tool"]);
        assert_eq!(pkg.uuid.as_deref(), Some("abc-123"));
        assert!(pkg.has_plugins);
        assert_eq!(pkg.plugin_for, vec!["core"]);
        assert!(pkg.hashed_variants);
        assert_eq!(pkg.relocatable, Some(true));
        assert_eq!(pkg.cachable, Some(false));
        assert_eq!(pkg.build_system.as_deref(), Some("cmake"));
        assert!(pkg.commands.is_some());
        assert_eq!(pkg.timestamp, Some(1700000000));
        assert_eq!(pkg.vcs.as_deref(), Some("git"));
        assert!(matches!(pkg.help, Some(Help::Single(_))));
    }

    #[test]
    fn test_from_data_help_multiple() {
        let data: HashMap<String, serde_json::Value> = serde_json::from_str(
            r#"{
            "name": "foo",
            "help": [["Docs", "https://example.com"], ["Wiki", "https://wiki.com"]]
        }"#,
        )
        .unwrap();
        let pkg = Package::from_data(data).unwrap();
        assert!(matches!(pkg.help, Some(Help::Multiple(ref v)) if v.len() == 2));
    }

    #[test]
    fn test_from_yaml_basic() {
        let yaml = r#"
name: foo
version: "2.0.1"
description: "Test package"
requires:
  - "bar-1+"
  - "baz-2.0"
variants:
  - ["python-3.9"]
  - ["python-3.10"]
tools:
  - foo_tool
commands: |
  env.PATH.append('{root}/bin')
"#;
        let pkg = Package::from_yaml(yaml).unwrap();
        assert_eq!(pkg.name, "foo");
        assert_eq!(pkg.version.to_string(), "2.0.1");
        assert_eq!(pkg.requires.len(), 2);
        assert_eq!(pkg.variants.len(), 2);
        assert_eq!(pkg.tools, vec!["foo_tool"]);
        assert!(pkg.commands.is_some());
    }

    #[test]
    fn test_from_yaml_invalid() {
        assert!(Package::from_yaml(": : :").is_err());
        assert!(Package::from_yaml("just a string").is_err());
    }

    #[test]
    fn test_from_toml_basic() {
        let toml_str = r#"
name = "foo"
version = "1.0.0"
description = "A test"
requires = ["bar-1+"]
tools = ["my_tool"]
"#;
        let pkg = Package::from_toml(toml_str).unwrap();
        assert_eq!(pkg.name, "foo");
        assert_eq!(pkg.version.to_string(), "1.0.0");
        assert_eq!(pkg.requires.len(), 1);
        assert_eq!(pkg.tools, vec!["my_tool"]);
    }

    #[test]
    fn test_from_toml_invalid() {
        assert!(Package::from_toml("{{{").is_err());
        assert!(Package::from_toml("").is_err()); // empty = no name
    }

    #[test]
    fn test_from_json_basic() {
        let json = r#"{
            "name": "baz",
            "version": "3.1.4",
            "authors": ["Alice"],
            "requires": ["foo-1.0"]
        }"#;
        let pkg = Package::from_json(json).unwrap();
        assert_eq!(pkg.name, "baz");
        assert_eq!(pkg.version.to_string(), "3.1.4");
        assert_eq!(pkg.authors, vec!["Alice"]);
        assert_eq!(pkg.requires.len(), 1);
    }

    #[test]
    fn test_from_json_invalid() {
        assert!(Package::from_json("not json").is_err());
        assert!(Package::from_json("42").is_err()); // not an object
    }

    #[test]
    fn test_variant_from_package() {
        let pkg = Package::new("foo", Version::new("1.2.3").unwrap());
        let variant = Variant::from_package(pkg);

        assert_eq!(variant.name(), "foo");
        assert_eq!(variant.version().to_string(), "1.2.3");
        assert!(!variant.is_variant());
        assert_eq!(variant.qualified_name(), "foo-1.2.3");
    }

    #[test]
    fn test_variant_subpath_preserves_prefix_and_open_range_identity() {
        // Golden hashes from Python Rez: sha1(str(list(map(str, requires)))).
        for (requirement, hash) in [
            ("python-3.11", "b99a49e4ec48ad4d9833734782ee775813473768"),
            ("python-3.11+", "ce0c5839d284f3a2e720b51980ad1b0c2d7ccb1f"),
        ] {
            let requires = vec![Requirement::new(requirement).unwrap()];
            assert_eq!(
                Variant::compute_subpath(&requires, false).as_deref(),
                Some(requirement)
            );
            assert_eq!(
                Variant::compute_subpath(&requires, true).as_deref(),
                Some(hash)
            );
        }
        assert_eq!(
            Variant::compute_subpath(&[], true).as_deref(),
            Some("97d170e1550eee4afc0af065b78cda302a97674c")
        );
        let requires = vec![
            Requirement::new("platform-windows").unwrap(),
            Requirement::new("arch-x86_64").unwrap(),
        ];
        assert_eq!(
            Variant::compute_subpath(&requires, false).as_deref(),
            Some("platform-windows/arch-x86_64")
        );
    }

    #[test]
    fn test_package_to_data_preserves_extensions_and_normalized_mutations() {
        let mut package = Package::from_yaml(
            "name: export_probe\nversion: '1.0'\npackage_type: dcc\ncustom: {nested: [1, 2]}\ncommands: \"env.FOO = 'bar'\"\n",
        ).unwrap();
        package
            .requires
            .push(Requirement::new("python-3.11+").unwrap());
        package
            .variants
            .push(vec![Requirement::new("platform-windows").unwrap()]);
        let data = package.to_data().unwrap();
        assert_eq!(data["package_type"], serde_json::json!("dcc"));
        assert_eq!(data["custom"], serde_json::json!({"nested": [1, 2]}));
        assert_eq!(data["requires"], serde_json::json!(["python-3.11+"]));
        let loaded = Package::from_data(data).unwrap();
        assert_eq!(loaded.requires, package.requires);
        assert_eq!(loaded.variants, package.variants);
        assert_eq!(loaded.commands, package.commands);
    }

    #[test]
    fn test_variant_new() {
        let mut pkg = Package::new("foo", Version::new("1.2.3").unwrap());
        pkg.variants
            .push(vec![Requirement::new("python-3.9").unwrap()]);

        let variant = Variant::new(pkg.clone(), 0).unwrap();
        assert_eq!(variant.name(), "foo");
        assert!(variant.is_variant());
        assert_eq!(variant.index, Some(0));
        assert_eq!(variant.qualified_name(), "foo-1.2.3[0]");
    }

    #[test]
    fn test_variant_new_out_of_range() {
        let pkg = Package::new("foo", Version::new("1.2.3").unwrap());
        let result = Variant::new(pkg, 0);
        assert!(result.is_err());
    }

    #[test]
    fn test_variant_qualified_names() {
        let mut pkg = Package::new("foo", Version::new("1.2.3").unwrap());
        pkg.variants.push(vec![]);

        let variant = Variant::new(pkg, 0).unwrap();
        assert_eq!(variant.qualified_package_name(), "foo-1.2.3");
        assert_eq!(variant.qualified_name(), "foo-1.2.3[0]");
    }

    #[test]
    fn test_variant_display() {
        let mut pkg = Package::new("foo", Version::new("1.2.3").unwrap());
        pkg.variants.push(vec![]);

        let variant = Variant::new(pkg, 0).unwrap();
        assert_eq!(variant.to_string(), "foo-1.2.3[0]");
    }

    #[test]
    fn test_package_iter_variants() {
        let mut pkg = Package::new("foo", Version::new("1.2.3").unwrap());
        pkg.variants
            .push(vec![Requirement::new("python-3.9").unwrap()]);
        pkg.variants
            .push(vec![Requirement::new("python-3.10").unwrap()]);

        let variants: Vec<_> = pkg.iter_variants().collect();
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0].index, Some(0));
        assert_eq!(variants[1].index, Some(1));
    }

    #[test]
    fn test_package_iter_variants_empty() {
        let pkg = Package::new("foo", Version::new("1.2.3").unwrap());
        let variants: Vec<_> = pkg.iter_variants().collect();
        assert_eq!(variants.len(), 0);
    }

    #[test]
    fn test_package_family_new() {
        let family = PackageFamily::new("foo");
        assert_eq!(family.name, "foo");
        assert_eq!(family.num_packages(), 0);
    }

    #[test]
    fn test_package_family_add_package() {
        let mut family = PackageFamily::new("foo");
        family.add_package(Package::new("foo", Version::new("1.0.0").unwrap()));
        family.add_package(Package::new("foo", Version::new("2.0.0").unwrap()));

        assert_eq!(family.num_packages(), 2);
        assert_eq!(family.packages[0].version.to_string(), "1.0.0");
        assert_eq!(family.packages[1].version.to_string(), "2.0.0");
    }

    #[test]
    fn test_package_family_iter_packages() {
        let mut family = PackageFamily::new("foo");
        family.add_package(Package::new("foo", Version::new("1.0.0").unwrap()));
        family.add_package(Package::new("foo", Version::new("2.0.0").unwrap()));

        let count = family.iter_packages().count();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_package_family_display() {
        let mut family = PackageFamily::new("foo");
        family.add_package(Package::new("foo", Version::new("1.0.0").unwrap()));
        family.add_package(Package::new("foo", Version::new("2.0.0").unwrap()));

        assert_eq!(family.to_string(), "foo (2 versions)");
    }

    #[test]
    fn test_developer_package_new() {
        let pkg = Package::new("foo", Version::new("1.2.3").unwrap());
        let filepath = PathBuf::from("/path/to/package.toml");
        let dev_pkg = DeveloperPackage::new(pkg, filepath.clone());

        assert_eq!(dev_pkg.name(), "foo");
        assert_eq!(dev_pkg.version().to_string(), "1.2.3");
        assert_eq!(dev_pkg.filepath, filepath);
    }

    #[test]
    fn test_developer_package_qualified_name() {
        let pkg = Package::new("foo", Version::new("1.2.3").unwrap());
        let dev_pkg = DeveloperPackage::new(pkg, PathBuf::from("/path/to/package.toml"));
        assert_eq!(dev_pkg.qualified_name(), "foo-1.2.3");
    }

    #[test]
    fn test_developer_package_root() {
        let pkg = Package::new("foo", Version::new("1.2.3").unwrap());
        let dev_pkg = DeveloperPackage::new(pkg, PathBuf::from("/path/to/package.toml"));
        assert_eq!(dev_pkg.root(), Some(std::path::Path::new("/path/to")));
    }

    #[test]
    fn test_developer_package_from_path_dir_yaml() {
        use std::fs;
        use std::sync::atomic::{AtomicUsize, Ordering};

        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("rez_test_dev_pkg_{pid}_{id}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let pkg_file = dir.join("package.yaml");
        fs::write(
            &pkg_file,
            "name: test_pkg\nversion: \"2.0.0\"\ndescription: Test package\nrequires:\n  - foo-1+\n",
        )
        .unwrap();

        let dev_pkg = DeveloperPackage::from_path(&dir).unwrap();
        assert_eq!(dev_pkg.name(), "test_pkg");
        assert_eq!(dev_pkg.version().to_string(), "2.0.0");
        assert_eq!(dev_pkg.package.description.as_deref(), Some("Test package"));
        assert_eq!(dev_pkg.package.requires.len(), 1);
        assert_eq!(dev_pkg.filepath, dir.join("package.yaml"));
        assert_eq!(dev_pkg.root(), Some(dir.as_path()));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_developer_package_from_path_dir_yml_retains_source_path() {
        use std::fs;
        use std::sync::atomic::{AtomicUsize, Ordering};

        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "rez_test_dev_pkg_yml_{}_{}",
            std::process::id(),
            id
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let pkg_file = dir.join("package.yml");
        fs::write(&pkg_file, "name: yml_pkg\nversion: \"1.0.0\"\n").unwrap();

        let package = DeveloperPackage::from_path(&dir).unwrap();
        assert_eq!(package.name(), "yml_pkg");
        assert_eq!(package.filepath, pkg_file);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_developer_package_from_path_file() {
        use std::fs;
        use std::sync::atomic::{AtomicUsize, Ordering};

        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("rez_test_dev_pkg_file_{pid}_{id}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let pkg_file = dir.join("package.toml");
        fs::write(&pkg_file, "name = \"my_pkg\"\nversion = \"1.5.0\"\n").unwrap();

        let dev_pkg = DeveloperPackage::from_path(&pkg_file).unwrap();
        assert_eq!(dev_pkg.name(), "my_pkg");
        assert_eq!(dev_pkg.version().to_string(), "1.5.0");
        assert_eq!(dev_pkg.filepath, pkg_file);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_developer_package_reevaluate_uses_lifecycle_objects() {
        use std::fs;
        use std::sync::atomic::{AtomicUsize, Ordering};

        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "rez_test_dev_pkg_reevaluate_{}_{}",
            std::process::id(),
            id
        ));
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("package.py");
        fs::write(
            &source,
            "name = 'reevaluated'\nversion = '1.0'\n@early\ndef description():\n    return 'build' if building else 'source'\n",
        )
        .unwrap();

        let package = DeveloperPackage::from_path(&source).unwrap();
        assert_eq!(package.package.description.as_deref(), Some("source"));

        let evaluated = package
            .reevaluate(&HashMap::from([
                ("building".to_owned(), Value::Bool(true)),
                ("build_variant_index".to_owned(), Value::from(2)),
            ]))
            .unwrap();

        assert_eq!(evaluated.package.description.as_deref(), Some("build"));
        assert_eq!(package.package.description.as_deref(), Some("source"));
        assert_eq!(evaluated.filepath, source);
        assert_eq!(evaluated.source_format, Some(FileFormat::Py));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_developer_package_from_path_missing_name() {
        use std::fs;
        use std::sync::atomic::{AtomicUsize, Ordering};

        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("rez_test_dev_pkg_noname_{pid}_{id}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let pkg_file = dir.join("package.yaml");
        fs::write(&pkg_file, "version: \"1.0.0\"\n").unwrap();

        let result = DeveloperPackage::from_path(&dir);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("name"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_developer_package_from_path_not_found() {
        let result = DeveloperPackage::from_path(std::path::Path::new("/nonexistent/path"));
        assert!(result.is_err());
    }

    #[test]
    fn test_help_single() {
        let help = Help::Single("Help text".into());
        assert_eq!(help.to_string(), "Help text");
    }

    #[test]
    fn test_help_multiple() {
        let help = Help::Multiple(vec![
            vec!["Docs".into(), "https://example.com".into()],
            vec!["Wiki".into(), "https://wiki.example.com".into()],
        ]);
        let display = help.to_string();
        assert!(display.contains("Docs: https://example.com"));
        assert!(display.contains("Wiki: https://wiki.example.com"));
    }

    #[test]
    fn test_variant_requires_concat() {
        // Test that variant.requires() returns parent.requires + variant_requires
        let mut pkg = Package::new("foo", Version::new("1.0.0").unwrap());
        pkg.requires = vec![
            Requirement::new("python-2.7").unwrap(),
            Requirement::new("numpy-1.0+").unwrap(),
        ];
        pkg.variants = vec![
            vec![Requirement::new("platform-linux").unwrap()],
            vec![Requirement::new("platform-windows").unwrap()],
        ];

        // Variant 0: linux
        let variant0 = Variant::new(pkg.clone(), 0).unwrap();
        let reqs0 = variant0.requires();
        assert_eq!(reqs0.len(), 3); // 2 parent + 1 variant
        assert_eq!(reqs0[0].name(), "python");
        assert_eq!(reqs0[1].name(), "numpy");
        assert_eq!(reqs0[2].name(), "platform");

        // Variant 1: windows
        let variant1 = Variant::new(pkg.clone(), 1).unwrap();
        let reqs1 = variant1.requires();
        assert_eq!(reqs1.len(), 3);
        assert_eq!(reqs1[2].name(), "platform");

        // Non-variant package
        let non_variant = Variant::from_package(pkg.clone());
        let reqs = non_variant.requires();
        assert_eq!(reqs.len(), 2); // only parent requires
    }

    #[test]
    fn test_is_relocatable_cascade() {
        use crate::config::RezConfig;

        let states = [None, Some(false), Some(true)];
        for default in [false, true] {
            for package in states {
                for repository in states {
                    for family in states {
                        let mut pkg = Package::new("foo", Version::new("1.0.0").unwrap());
                        pkg.relocatable = package;
                        let config = RezConfig {
                            default_relocatable: default,
                            default_relocatable_per_repository: Some(
                                [("repo".into(), repository)].into_iter().collect(),
                            ),
                            default_relocatable_per_package: Some(
                                [("foo".into(), family)].into_iter().collect(),
                            ),
                            ..RezConfig::default()
                        };
                        assert_eq!(
                            pkg.is_relocatable(Some("repo"), Some(&config)),
                            package.or(repository).or(family).unwrap_or(default),
                            "package={package:?}, repository={repository:?}, family={family:?}, default={default}",
                        );
                        assert_eq!(
                            pkg.is_relocatable(Some("other"), Some(&config)),
                            package.or(family).unwrap_or(default),
                        );
                        assert_eq!(
                            pkg.is_relocatable(None, Some(&config)),
                            package.or(family).unwrap_or(default),
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn test_variant_root_has_one_canonical_fallback() {
        let mut package = Package::new("rooted", Version::new("1").unwrap());
        package.base = Some(PathBuf::from("repo/rooted/1"));
        package.variants = vec![vec![Requirement::new("platform-windows").unwrap()]];
        package.hashed_variants = false;
        let plain = Variant::new(package.clone(), 0).unwrap();
        assert_eq!(
            plain.root(),
            Some(package.base.as_ref().unwrap().join("platform-windows"))
        );
        package.hashed_variants = true;
        let mut hashed = Variant::new(package.clone(), 0).unwrap();
        assert_eq!(
            hashed.root(),
            Some(
                package
                    .base
                    .as_ref()
                    .unwrap()
                    .join(hashed.subpath.as_ref().unwrap())
            )
        );
        hashed.root = Some(PathBuf::from("cached"));
        assert_eq!(hashed.root(), Some(PathBuf::from("cached")));
        assert_eq!(Variant::from_package(package.clone()).root(), package.base);
        package.base = None;
        assert_eq!(Variant::new(package, 0).unwrap().root(), None);
    }

    #[test]
    fn test_package_config_reuses_typed_merge_without_mutating_base() {
        let base = crate::config::RezConfig::default();
        let mut package = Package::new("configured", Version::new("1").unwrap());
        package.config = Some(serde_json::json!({
            "default_cachable": true,
            "package_cache_same_device": true,
            "plugins": {"custom": {"value": 3}}
        }));
        let effective = package.config(Some(&base)).unwrap();
        assert_eq!(effective.default_cachable, Some(true));
        assert!(effective.package_cache_same_device);
        assert_eq!(effective.plugins["custom"]["value"], 3);
        assert_eq!(base.default_cachable, Some(false));
        assert!(!base.package_cache_same_device);
        package.config = Some(serde_json::json!({"package_cache_same_device": "invalid"}));
        assert!(package.config(Some(&base)).is_err());
        assert!(!base.package_cache_same_device);
        package.config = Some(serde_json::json!([]));
        assert!(package.config(Some(&base)).is_err());
        package.config = Some(serde_json::Value::Null);
        assert_eq!(
            package.config(Some(&base)).unwrap().default_cachable,
            base.default_cachable
        );
    }

    #[test]
    fn test_is_cachable_cascade() {
        use crate::config::RezConfig;

        let states = [None, Some(false), Some(true)];
        for default in states {
            for relocatable in [false, true] {
                for package in states {
                    for repository in states {
                        for family in states {
                            let mut pkg = Package::new("foo", Version::new("1.0.0").unwrap());
                            pkg.cachable = package;
                            let config = RezConfig {
                                default_relocatable: relocatable,
                                default_cachable: default,
                                default_cachable_per_repository: Some(
                                    [("repo".into(), repository)].into_iter().collect(),
                                ),
                                default_cachable_per_package: Some(
                                    [("foo".into(), family)].into_iter().collect(),
                                ),
                                ..RezConfig::default()
                            };
                            assert_eq!(
                                pkg.is_cachable(Some("repo"), Some(&config)),
                                package
                                    .or(repository)
                                    .or(family)
                                    .or(default)
                                    .unwrap_or(relocatable),
                                "package={package:?}, repository={repository:?}, family={family:?}, default={default:?}, relocatable={relocatable}",
                            );
                            assert_eq!(
                                pkg.is_cachable(Some("other"), Some(&config)),
                                package.or(family).or(default).unwrap_or(relocatable),
                            );
                            assert_eq!(
                                pkg.is_cachable(None, Some(&config)),
                                package.or(family).or(default).unwrap_or(relocatable),
                            );
                        }
                    }
                }
            }
        }

        // An absent cache default delegates to the complete relocation policy,
        // including repository/family overrides and the package's own setting.
        let mut pkg = Package::new("foo", Version::new("1.0.0").unwrap());
        let mut config = RezConfig {
            default_cachable: None,
            default_relocatable: false,
            default_relocatable_per_package: Some(
                [("foo".into(), Some(true))].into_iter().collect(),
            ),
            default_relocatable_per_repository: Some(
                [("repo".into(), Some(false))].into_iter().collect(),
            ),
            ..RezConfig::default()
        };
        assert!(!pkg.is_cachable(Some("repo"), Some(&config)));
        assert!(pkg.is_cachable(None, Some(&config)));
        pkg.relocatable = Some(true);
        assert!(pkg.is_cachable(Some("repo"), Some(&config)));
        config.default_cachable = Some(false);
        assert!(!pkg.is_cachable(Some("repo"), Some(&config)));
        config.default_cachable = Some(true);
        pkg.relocatable = Some(false);
        assert!(pkg.is_cachable(Some("repo"), Some(&config)));
    }

    #[test]
    fn test_hashed_variant_subpath() {
        // Verify SHA1 matches Python rez: sha1(str(['platform-windows', 'arch-x86_64']))
        let reqs = vec![
            Requirement::new("platform-windows").unwrap(),
            Requirement::new("arch-x86_64").unwrap(),
        ];
        let hashed = Variant::compute_subpath(&reqs, true).unwrap();
        assert_eq!(hashed, "c05c599993c2b1ed88babc35858f1d00995605f0");

        // Non-hashed: nested dirs
        let nested = Variant::compute_subpath(&reqs, false).unwrap();
        assert!(nested.contains("platform-windows"));
        assert!(nested.contains("arch-x86_64"));

        // An empty indexed variant still hashes the Python list representation.
        assert_eq!(
            Variant::compute_subpath(&[], true).as_deref(),
            Some("97d170e1550eee4afc0af065b78cda302a97674c")
        );
        assert!(Variant::compute_subpath(&[], false).is_none());
    }

    #[test]
    fn test_variant_new_hashed() {
        let mut pkg = Package::new("foo", Version::new("1.0.0").unwrap());
        pkg.variants = vec![vec![
            Requirement::new("platform-windows").unwrap(),
            Requirement::new("arch-x86_64").unwrap(),
        ]];

        // Rez defaults to requirement-based paths; hashing is explicitly enabled.
        let v = Variant::new(pkg.clone(), 0).unwrap();
        assert_eq!(v.subpath.as_deref(), Some("platform-windows/arch-x86_64"));
        pkg.hashed_variants = true;
        let v = Variant::new(pkg.clone(), 0).unwrap();
        assert_eq!(
            v.subpath.as_deref(),
            Some("c05c599993c2b1ed88babc35858f1d00995605f0")
        );

        // Explicit hashed_variants=false -> nested dirs
        pkg.hashed_variants = false;
        let v = Variant::new(pkg, 0).unwrap();
        assert!(v.subpath.as_ref().unwrap().contains("platform-windows"));
    }
}
