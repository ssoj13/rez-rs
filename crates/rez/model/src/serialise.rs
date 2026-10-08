// SPDX-License-Identifier: Apache-2.0

//! Package data serialization — single source of truth for loading package definitions.
//!
//! **Canonical load path** for all package files. Supports .py (RustPython exec), .yaml/.yml, .toml.
//! `repository::load_package_data` and `DeveloperPackage::from_path` both delegate here.
//!
//! # Main entry points
//! - `load_from_file(path, format, cache)` — load by format with an explicit Python parsed-cache policy
//! - `load_package_data(dir)` — probe configured package stems and supported formats
//! - `validate_package_data(data)` — schema validation before `Package::from_data`
//! - `exec_package_py(content)` — run package.py via RustPython, collect JSON
//!
//! # File probe order
//! Configured filename stems are outermost; for each stem, Python and YAML precede additive TOML and `.yml` support.
//!
//! # Used by
//! - `repository::load_package_data` — when loading package files
//! - `package/core::DeveloperPackage::from_path` — developer package loading (validated)
//! - `package/core::Package::from_data` — calls `validate_package_data`

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::errors::{Result, RezError};
use crate::log_trace;
use version::Requirement;

// ---------------------------------------------------------------------------
// Extract @include() from package.py source
// ---------------------------------------------------------------------------

/// Prepare source using the interpreter's shared include-module cache.
/// With `call`, invoke a serialized function once and leave its return value in
/// `_result`; raw Python bodies execute directly. AST identity preserves aliases,
/// multiline signatures, annotations and comments without guessing a field name.
pub fn source_with_includes(
    source: &str,
    base: Option<&Path>,
    config: Option<&Value>,
    call: bool,
) -> Result<String> {
    let (names, function) = source_metadata(source)?;
    let mut prepared = String::new();
    if !names.is_empty() {
        prepared.push_str("def include(*names):\n    return lambda func: func\n");
        let directory = include_directory(base, config)?;
        for name in names {
            let path = include_module_path(&name, &directory, base.is_some())?;
            let content = fs::read_to_string(&path)?;
            let name_json = serde_json::to_string(&name)?;
            let path_json = serde_json::to_string(&path.to_string_lossy())?;
            let content_json = serde_json::to_string(&content)?;
            prepared.push_str(&format!(
                "globals()[{name_json}] = _rez_include_module({name_json}, {path_json}, {content_json})\n"
            ));
        }
    }
    prepared.push_str(source);
    if let Some(function) = function.filter(|_| call) {
        prepared.push_str(&format!("\n_result = {function}()\n"));
    }
    Ok(prepared)
}

/// Select the include directory once for loading, deferred execution and install.
pub fn include_directory(base: Option<&Path>, config: Option<&Value>) -> Result<PathBuf> {
    if let Some(base) = base {
        return Ok(base.join(".rez/include"));
    }
    let configured = config.and_then(|config| config.get("package_definition_python_path"));
    if let Some(configured) = configured {
        return configured
            .as_str()
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| {
                RezError::Python(
                    "Include modules require a non-empty package_definition_python_path".into(),
                )
            });
    }
    crate::config::CONFIG
        .package_definition_python_path
        .as_ref()
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            RezError::Python("Include modules require package_definition_python_path".into())
        })
}

/// Resolve a safe module file for validation, deferred execution and installation.
pub fn include_module_path(name: &str, directory: &Path, installed: bool) -> Result<PathBuf> {
    if name.is_empty() || !is_safe_rez_path_component(&format!("{name}.py"), false) {
        return Err(RezError::Python(format!(
            "Unsafe include module name: {name}"
        )));
    }
    let path = directory.join(format!("{name}.py"));
    if path.is_file() {
        return Ok(path);
    }
    if installed && directory.is_dir() {
        // Older Rez payloads embedded the identity in the filename instead of
        // writing a .sha1 sidecar. Restrict this fallback to installed packages.
        let prefix = format!("{name}-");
        let mut candidates = Vec::new();
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && entry.file_name().to_str().is_some_and(|filename| {
                    filename.starts_with(&prefix) && filename.ends_with(".py")
                })
            {
                candidates.push(entry.path());
            }
        }
        candidates.sort();
        if let Some(path) = candidates.into_iter().next() {
            return Ok(path);
        }
    }
    Err(RezError::Python(format!(
        "Include module {} does not exist",
        path.display()
    )))
}

#[cfg(test)]
use python_runtime::include_module_hash;

/// Extract serialized include decorator metadata with Python's syntax parser.
pub fn extract_includes_from_package_py(content: &str) -> Result<Vec<String>> {
    source_metadata(content).map(|(modules, _)| modules)
}

// One syntax policy for include discovery, deferred schema and hook invocation.
fn source_metadata(content: &str) -> Result<(Vec<String>, Option<String>)> {
    let data = python_runtime::exec_py_globals(
        r#"import ast
tree = ast.parse(source)
function = None
if len(tree.body) == 1:
    if isinstance(tree.body[0], ast.AsyncFunctionDef):
        raise TypeError("Serialized async functions are not supported for synchronous package hooks")
    if isinstance(tree.body[0], ast.FunctionDef):
        function = tree.body[0].name
modules = []
for node in ast.walk(tree):
    if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
        for decorator in node.decorator_list:
            if isinstance(decorator, ast.Call) and isinstance(decorator.func, ast.Name) and decorator.func.id == "include":
                for argument in decorator.args:
                    if not isinstance(argument, ast.Constant) or not isinstance(argument.value, str):
                        raise ValueError("Serialized include names must be string literals")
                    if argument.value not in modules:
                        modules.append(argument.value)
"#,
        "<include-metadata>",
        Some(&HashMap::from([(
            "source".into(),
            Value::String(content.into()),
        )])),
        &["source", "tree", "node", "decorator", "argument"],
    )?;
    let function = data
        .get("function")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let modules = data
        .get("modules")
        .and_then(Value::as_array)
        .ok_or_else(|| RezError::Python("Invalid include metadata".into()))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| RezError::Python("Non-string include module".into()))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((modules, function))
}

#[cfg(test)]
#[test]
fn test_include_module_identity_matches_python_bytes_strip() {
    assert_eq!(
        include_module_hash(b"\x0b\x0c a \t\n\r"),
        include_module_hash(b"a")
    );
    assert_eq!(
        include_module_hash(b" \t\x0b\x0c\r\n"),
        include_module_hash(b"")
    );
    assert_ne!(include_module_hash(b"\xc2\xa0a"), include_module_hash(b"a"));
}

#[cfg(test)]
#[test]
fn test_deferred_include_cache_shares_content_and_invalidates() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join(".rez/include");
    fs::create_dir_all(&directory).unwrap();
    let unique = serde_json::to_string(&root.path().to_string_lossy()).unwrap();
    let content = format!(
        "identity = {unique}\ncounter = 0\nimport sys\nassert __spec__.name == __name__\nassert __loader__ is not None\nassert __package__ == ''\nassert __name__ not in sys.modules\n"
    );
    fs::write(directory.join("first.py"), &content).unwrap();
    // Python bytes.strip includes VT. A different filename/path still shares
    // the same loaded module when source identity matches.
    fs::write(directory.join("second.py"), format!("\x0b{content}\x0b")).unwrap();
    let first = source_with_includes(
        "@include('first')\ndef pre_commands():\n    first.counter += 1\n    return first.counter",
        Some(root.path()),
        None,
        true,
    )
    .unwrap();
    let second = source_with_includes(
        "@ include(\n 'first', 'second',  # alias by content\n)\ndef commands(\n) -> int:\n    assert first is second\n    second.counter += 1\n    return first.counter",
        Some(root.path()), None, true,
    ).unwrap();
    let values = python_runtime::exec_py_globals(&first, "<include-first>", None, &[]).unwrap();
    assert_eq!(values["_result"], serde_json::json!(1));
    let values =
        python_runtime::exec_py_globals_for_rex(&second, "<include-second>", None, &[]).unwrap();
    assert_eq!(values["_result"], serde_json::json!(2));

    // A stale sidecar must not hide changed bytes.
    fs::write(
        directory.join("first.sha1"),
        include_module_hash(content.as_bytes()),
    )
    .unwrap();
    fs::write(
        directory.join("first.py"),
        format!("identity = {unique}\ncounter = 40\n"),
    )
    .unwrap();
    let changed = source_with_includes(
        "@include('first')\ndef commands():\n    first.counter += 1\n    return first.counter",
        Some(root.path()),
        None,
        true,
    )
    .unwrap();
    let values = python_runtime::exec_py_globals(&changed, "<include-changed>", None, &[]).unwrap();
    assert_eq!(values["_result"], serde_json::json!(41));
}

#[cfg(test)]
#[test]
fn test_deferred_include_failed_load_retries_identical_source() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join(".rez/include");
    fs::create_dir_all(&directory).unwrap();
    let marker = root.path().join("attempt.txt");
    fs::write(&marker, "0").unwrap();
    let marker = serde_json::to_string(&marker.to_string_lossy()).unwrap();
    let content = format!(
        "with open({marker}) as f:\n    attempts = int(f.read()) + 1\nwith open({marker}, 'w') as f:\n    f.write(str(attempts))\nif attempts == 1:\n    raise RuntimeError('first load fails')\n"
    );
    fs::write(directory.join("retry.py"), content).unwrap();
    let source = source_with_includes(
        "@include('retry')\ndef tools():\n    return retry.attempts",
        Some(root.path()),
        None,
        true,
    )
    .unwrap();
    let error = python_runtime::exec_py_globals(&source, "<include-retry>", None, &[]).unwrap_err();
    assert!(error.to_string().contains("first load fails"));
    let values = python_runtime::exec_py_globals(&source, "<include-retry>", None, &[]).unwrap();
    assert_eq!(values["_result"], serde_json::json!(2));
    let values = python_runtime::exec_py_globals(&source, "<include-retry>", None, &[]).unwrap();
    assert_eq!(values["_result"], serde_json::json!(2));
}

#[cfg(test)]
#[test]
fn test_deferred_source_calls_aliased_function_and_raw_body_once() {
    let source = "# Leading comment\ndef original(\n) -> int:\n    calls.append('function')\n    return len(calls)\n";
    let prepared = source_with_includes(source, None, None, true).unwrap();
    let code = format!("calls = []\n{prepared}");
    let values = python_runtime::exec_py_globals(&code, "<aliased-call>", None, &[]).unwrap();
    assert_eq!(values["_result"], serde_json::json!(1));
    assert_eq!(values["calls"], serde_json::json!(["function"]));

    let raw = "calls = []\ndef helper():\n    calls.append('body')\nhelper()";
    let prepared = source_with_includes(raw, None, None, true).unwrap();
    let values = python_runtime::exec_py_globals(&prepared, "<raw-body>", None, &[]).unwrap();
    assert_eq!(values["calls"], serde_json::json!(["body"]));
    assert!(!values.contains_key("_result"));

    let package = exec_package_py(
        "name = 'alias'\n@late\ndef original():\n    return ['tool']\ntools = original\n",
    )
    .unwrap();
    assert!(is_late_binding_source(&package["tools"], "tools"));
    let data: HashMap<String, Value> = package.into_iter().collect();
    assert_eq!(
        crate::python_vm::eval_late_binding(data["tools"].as_str().unwrap(), "tools", &data, false)
            .unwrap(),
        serde_json::json!(["tool"])
    );
}

#[cfg(test)]
#[test]
fn test_deferred_source_rejects_async_hooks() {
    let source = "async def commands():\n    pass";
    let error = source_with_includes(source, None, None, true).unwrap_err();
    assert!(error.to_string().contains("Serialized async functions"));
    assert!(!is_late_binding_source(
        &Value::String(source.into()),
        "commands"
    ));
}

#[cfg(test)]
#[test]
fn test_installed_include_legacy_filename_is_scoped() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join(".rez/include");
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("legacy-012345.py");
    fs::write(&path, "value = 'legacy payload'\n").unwrap();
    assert_eq!(
        include_module_path("legacy", &directory, true).unwrap(),
        path
    );
    assert!(include_module_path("legacy", &directory, false).is_err());
    let source = source_with_includes(
        "@include('legacy')\ndef tools():\n    return legacy.value",
        Some(root.path()),
        None,
        true,
    )
    .unwrap();
    let values = python_runtime::exec_py_globals(&source, "<legacy-include>", None, &[]).unwrap();
    assert_eq!(values["_result"], serde_json::json!("legacy payload"));
}

#[cfg(test)]
#[test]
fn test_extract_includes_from_package_py() {
    assert!(
        extract_includes_from_package_py("# @include('not_a_module')")
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        extract_includes_from_package_py("@include('baz', 'qux')\ndef commands():\n    pass")
            .unwrap(),
        vec!["baz", "qux"]
    );
}

// ---------------------------------------------------------------------------
// FileFormat
// ---------------------------------------------------------------------------

/// Supported package file formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileFormat {
    Py,
    Yaml,
    Toml,
    Txt,
}

/// Controls persistence of parsed Python package data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageDataCache {
    Enabled,
    Disabled,
}

impl FileFormat {
    /// File extension for this format (without dot).
    pub fn extension(&self) -> &str {
        match self {
            Self::Py => "py",
            Self::Yaml => "yaml",
            Self::Toml => "toml",
            Self::Txt => "txt",
        }
    }

    /// Resolve format from a file extension string (without dot).
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "py" => Some(Self::Py),
            "yaml" | "yml" => Some(Self::Yaml),
            "toml" => Some(Self::Toml),
            "txt" => Some(Self::Txt),
            _ => None,
        }
    }
}

impl std::fmt::Display for FileFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.extension())
    }
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default package definition filenames in upstream format priority followed by extras.
/// This constant is for static/default contexts; runtime discovery uses the configured stems.
pub const PACKAGE_FILE_NAMES: &[&str] =
    &["package.py", "package.yaml", "package.toml", "package.yml"];

/// Package-definition formats supported by this implementation, in priority order.
pub const PACKAGE_DEFINITION_EXTENSIONS: &[&str] = &["py", "yaml", "toml", "yml"];

/// Build package definition filenames from configured stems and requested extensions.
pub fn package_definition_file_names(extensions: &[&str]) -> Result<Vec<String>> {
    Ok(crate::config::CONFIG
        .package_definition_stems()?
        .iter()
        .flat_map(|stem| {
            extensions
                .iter()
                .map(move |extension| format!("{stem}.{extension}"))
        })
        .collect())
}

/// Find the first configured package definition file in a directory.
pub fn find_package_definition_file(dir: &Path, extensions: &[&str]) -> Result<Option<PathBuf>> {
    let file_names = package_definition_file_names(extensions)?;

    match fs::metadata(dir) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                format!(
                    "Package definition search path is not a directory: {}",
                    dir.display()
                ),
            )
            .into());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }

    for name in file_names {
        let path = dir.join(name);
        match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => return Ok(Some(path)),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }

    Ok(None)
}

/// Preferred key order when writing package files (mirrors Python package_key_order).
pub const PACKAGE_KEY_ORDER: &[&str] = &[
    "name",
    "version",
    "description",
    "authors",
    "tools",
    "tags",
    "has_plugins",
    "plugin_for",
    "requires",
    "build_requires",
    "private_build_requires",
    "hashed_variants",
    "relocatable",
    "cachable",
    "variants",
    "build_system",
    "build_command",
    "commands",
    "pre_build_commands",
    "pre_test_commands",
    "pre_commands",
    "post_commands",
    "help",
    "config",
    "uuid",
    "timestamp",
    "release_message",
    "changelog",
    "vcs",
    "revision",
    "previous_version",
    "previous_revision",
];

// ---------------------------------------------------------------------------
// Format detection
// ---------------------------------------------------------------------------

/// Detect file format from path extension.
pub fn detect_format(path: &Path) -> Option<FileFormat> {
    path.extension()
        .and_then(|e| e.to_str())
        .and_then(FileFormat::from_extension)
}

// ---------------------------------------------------------------------------
// Parsed package cache (Py files — RustPython is slow, ~200ms per file)
// ---------------------------------------------------------------------------

fn parsed_cache_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())?;
    Some(
        PathBuf::from(home)
            .join(".rez")
            .join("parsed_package_cache"),
    )
}

fn parsed_cache_key(path: &Path, content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.update([0]);
    hasher.update(content.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect()
}

fn load_py_cached(
    path: &Path,
    cache_dir: Option<&Path>,
    build_python_paths: &[String],
    early_objects: &HashMap<String, Value>,
) -> Result<HashMap<String, Value>> {
    let content = fs::read_to_string(path)?;
    let key = parsed_cache_key(path, &content);

    if let Some(cache_dir) = cache_dir {
        let cache_path = cache_dir.join(format!("{}.json", key));
        if cache_path.exists() {
            if let Some(data) = fs::read_to_string(&cache_path)
                .ok()
                .and_then(|s| serde_json::from_str::<HashMap<String, Value>>(&s).ok())
            {
                log_trace!("serialise", "parsed cache hit: {}", path.display());
                return Ok(data);
            }
        }
    }

    let data = exec_package_py_with_context(
        &content,
        path.to_string_lossy().as_ref(),
        build_python_paths,
        early_objects,
        None,
    )?;

    if let Some(cache_dir) = cache_dir {
        let _ = fs::create_dir_all(cache_dir);
        let cache_path = cache_dir.join(format!("{}.json", key));
        let tmp_path = cache_dir.join(format!("{}.tmp", key));
        if let Ok(json) = serde_json::to_string_pretty(&data) {
            if fs::write(&tmp_path, json).is_ok() {
                let _ = fs::rename(&tmp_path, &cache_path);
            }
        }
    }
    Ok(data)
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Load package data from a file. **Canonical loader** — repository and DeveloperPackage both use this.
pub fn load_from_file(
    path: &Path,
    format: FileFormat,
    cache: PackageDataCache,
) -> Result<HashMap<String, Value>> {
    load_from_file_with_objects(path, format, cache, &HashMap::new())
}

/// Load package data with early-bound lifecycle objects.
///
/// Parsed Python data depends on these objects, so object-aware loads bypass the
/// content-only persistent cache. YAML and TOML remain data-only formats.
pub fn load_from_file_with_objects(
    path: &Path,
    format: FileFormat,
    cache: PackageDataCache,
    early_objects: &HashMap<String, Value>,
) -> Result<HashMap<String, Value>> {
    let mut data = match format {
        FileFormat::Yaml => load_yaml(path),
        FileFormat::Toml => load_toml(path),
        FileFormat::Py => {
            let cache_dir = match (cache, early_objects.is_empty()) {
                (PackageDataCache::Enabled, true) => parsed_cache_dir(),
                _ => None,
            };
            let build_python_paths = crate::config::CONFIG
                .package_definition_build_python_paths
                .clone();
            load_py_cached(
                path,
                cache_dir.as_deref(),
                &build_python_paths,
                early_objects,
            )
        }
        FileFormat::Txt => Err(RezError::ResourceContent(format!(
            "Text format is not loadable as package data: {}",
            path.display()
        ))),
    }?;
    crate::package::commands::normalize(&mut data, &crate::config::CONFIG)?;
    Ok(data)
}

/// Load YAML file into a string-keyed map.
fn load_yaml(path: &Path) -> Result<HashMap<String, Value>> {
    let content = fs::read_to_string(path)?;
    if content.trim().is_empty() {
        return Ok(HashMap::new());
    }
    // Parse YAML -> serde_json::Value, then extract as object
    let val: Value = serde_yaml::from_str(&content).map_err(|e| {
        RezError::ResourceContent(format!("YAML parse error in {}: {e}", path.display()))
    })?;
    value_to_map(val, path)
}

/// Load TOML file into a string-keyed map.
fn load_toml(path: &Path) -> Result<HashMap<String, Value>> {
    let content = fs::read_to_string(path)?;
    if content.trim().is_empty() {
        return Ok(HashMap::new());
    }
    // Parse the complete document as a table, then convert through the shared TOML value path.
    let toml_table: toml::Table = content.parse().map_err(|e| {
        RezError::ResourceContent(format!("TOML parse error in {}: {e}", path.display()))
    })?;
    let json_val = toml_to_json(&toml::Value::Table(toml_table), &path.display().to_string())?;
    value_to_map(json_val, path)
}

/// Convert serde_json::Value (expected Object) into HashMap.
fn value_to_map(val: Value, path: &Path) -> Result<HashMap<String, Value>> {
    match val {
        Value::Object(map) => Ok(map.into_iter().collect()),
        Value::Null => Ok(HashMap::new()),
        _ => Err(RezError::ResourceContent(format!(
            "Expected mapping at top level of {}, got {}",
            path.display(),
            value_type_name(&val)
        ))),
    }
}

/// Convert toml::Value to serde_json::Value, rejecting values JSON cannot represent.
fn toml_to_json(tv: &toml::Value, path: &str) -> Result<Value> {
    match tv {
        toml::Value::String(s) => Ok(Value::String(s.clone())),
        toml::Value::Integer(i) => Ok(Value::Number((*i).into())),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(Value::Number)
            .ok_or_else(|| {
                RezError::ResourceContent(format!(
                    "Non-finite TOML float at {path} cannot be represented in package metadata"
                ))
            }),
        toml::Value::Boolean(b) => Ok(Value::Bool(*b)),
        toml::Value::Datetime(dt) => Ok(Value::String(dt.to_string())),
        toml::Value::Array(arr) => arr
            .iter()
            .enumerate()
            .map(|(index, value)| toml_to_json(value, &format!("{path}[{index}]")))
            .collect::<Result<Vec<_>>>()
            .map(Value::Array),
        toml::Value::Table(tbl) => tbl
            .iter()
            .map(|(key, value)| {
                toml_to_json(value, &format!("{path}.{key}")).map(|value| (key.clone(), value))
            })
            .collect::<Result<serde_json::Map<String, Value>>>()
            .map(Value::Object),
    }
}

/// Human-readable type name for a JSON Value.
fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

// ---------------------------------------------------------------------------
// Package data discovery
// ---------------------------------------------------------------------------

/// Try to load package data from a directory using the configured filename and format order.
/// Returns the loaded data, detected format, and exact source path.
pub fn load_package_definition(
    dir: &Path,
    cache: PackageDataCache,
) -> Result<(HashMap<String, Value>, FileFormat, PathBuf)> {
    log_trace!("serialise", "Loading package data from {}", dir.display());
    let path =
        find_package_definition_file(dir, PACKAGE_DEFINITION_EXTENSIONS)?.ok_or_else(|| {
            RezError::PackageNotFound(format!(
                "No package definition file found in {}",
                dir.display()
            ))
        })?;
    let fmt = detect_format(&path).ok_or_else(|| {
        RezError::ResourceContent(format!("Cannot determine format of {}", path.display()))
    })?;
    let data = load_from_file(&path, fmt, cache)?;
    Ok((data, fmt, path))
}

/// Load package data from a directory, retaining the historical return shape.
pub fn load_package_data(dir: &Path) -> Result<(HashMap<String, Value>, FileFormat)> {
    let (data, format, _) = load_package_definition(dir, PackageDataCache::Enabled)?;
    Ok((data, format))
}

// ---------------------------------------------------------------------------
// Dumping / writing
// ---------------------------------------------------------------------------

/// Serialize a data map to a file in the specified format.
pub fn dump_to_file(data: &HashMap<String, Value>, path: &Path, format: FileFormat) -> Result<()> {
    let content = serialize_data(data, format)?;
    atomic_write(path, &content)
}

/// Dump package data with preferred key ordering and optional attribute filtering.
pub fn dump_package_data(
    data: &HashMap<String, Value>,
    path: &Path,
    format: FileFormat,
    skip_attributes: Option<&HashSet<String>>,
) -> Result<()> {
    let mut normalized = data.clone();
    crate::package::commands::normalize(&mut normalized, &crate::config::CONFIG)?;
    let data = &normalized;
    let validation_data = data
        .iter()
        .filter(|(_, value)| !value.is_null())
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    validate_package_data(&validation_data)?;

    let skip = skip_attributes.cloned().unwrap_or_default();

    // Build ordered list: known keys first, then remaining in sorted order
    let mut ordered: Vec<(String, Value)> = Vec::new();
    let mut remaining: HashMap<&String, &Value> = data.iter().collect();

    for &key in PACKAGE_KEY_ORDER {
        let k = key.to_string();
        if skip.contains(&k) {
            remaining.remove(&k);
            continue;
        }
        if let Some(val) = remaining.remove(&k) {
            if !val.is_null() {
                ordered.push((k, val.clone()));
            }
        }
    }

    // Remaining keys in sorted order
    let mut extra: Vec<_> = remaining.into_iter().collect();
    extra.sort_by_key(|(k, _)| (*k).clone());
    for (k, v) in extra {
        if !skip.contains(k) && !v.is_null() {
            ordered.push((k.clone(), v.clone()));
        }
    }

    // Pass ordered Vec directly to preserve key order
    let content = serialize_data_ordered(&ordered, format)?;
    atomic_write(path, &content)
}

/// Serialize a HashMap to a string in the given format.
fn serialize_data(data: &HashMap<String, Value>, format: FileFormat) -> Result<String> {
    // Match Rez package writing: omit top-level nulls while preserving nested values.
    let cleaned: serde_json::Map<String, Value> = data
        .iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let obj = Value::Object(cleaned);

    match format {
        FileFormat::Yaml => serde_yaml::to_string(&obj)
            .map_err(|e| RezError::ResourceContent(format!("YAML serialization error: {e}"))),
        FileFormat::Toml => {
            let tv = json_to_toml(&obj, "$")?;
            match tv {
                toml::Value::Table(tbl) => toml::to_string_pretty(&tbl).map_err(|e| {
                    RezError::ResourceContent(format!("TOML serialization error: {e}"))
                }),
                _ => Err(RezError::ResourceContent(
                    "Top-level TOML value must be a table".into(),
                )),
            }
        }
        FileFormat::Py => Ok(dump_package_data_py(data)),
        FileFormat::Txt => Err(RezError::ResourceContent(
            "Text format output not supported for package data".into(),
        )),
    }
}

/// Serialize ordered Vec of key-value pairs, preserving insertion order.
fn serialize_data_ordered(ordered: &[(String, Value)], format: FileFormat) -> Result<String> {
    // Build serde_json::Map in exact order from Vec
    let mut map = serde_json::Map::new();
    for (k, v) in ordered {
        if !v.is_null() {
            map.insert(k.clone(), v.clone());
        }
    }
    let obj = Value::Object(map);

    match format {
        FileFormat::Yaml => serde_yaml::to_string(&obj)
            .map_err(|e| RezError::ResourceContent(format!("YAML serialization error: {e}"))),
        FileFormat::Toml => {
            let tv = json_to_toml(&obj, "$")?;
            match tv {
                toml::Value::Table(tbl) => toml::to_string_pretty(&tbl).map_err(|e| {
                    RezError::ResourceContent(format!("TOML serialization error: {e}"))
                }),
                _ => Err(RezError::ResourceContent(
                    "Top-level TOML value must be a table".into(),
                )),
            }
        }
        FileFormat::Py => {
            // Convert ordered Vec to HashMap for dump_package_data_py
            let map: HashMap<String, Value> = ordered.iter().cloned().collect();
            Ok(dump_package_data_py(&map))
        }
        FileFormat::Txt => Err(RezError::ResourceContent(
            "Text format output not supported for package data".into(),
        )),
    }
}

/// Convert serde_json::Value to toml::Value, reporting nulls with their JSON path.
fn json_to_toml(jv: &Value, path: &str) -> Result<toml::Value> {
    match jv {
        Value::Null => Err(RezError::ResourceContent(format!(
            "Cannot serialize JSON null at {path} to TOML; TOML has no null value"
        ))),
        Value::Bool(b) => Ok(toml::Value::Boolean(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(toml::Value::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(toml::Value::Float(f))
            } else {
                Err(RezError::ResourceContent(format!(
                    "Cannot convert number {n} to TOML at {path}"
                )))
            }
        }
        Value::String(s) => Ok(toml::Value::String(s.clone())),
        Value::Array(arr) => {
            let items: std::result::Result<Vec<_>, _> = arr
                .iter()
                .enumerate()
                .map(|(index, value)| json_to_toml(value, &format!("{path}[{index}]")))
                .collect();
            Ok(toml::Value::Array(items?))
        }
        Value::Object(map) => {
            let mut tbl = toml::map::Map::new();
            for (key, value) in map {
                let key_path = format!("{path}[{key:?}]");
                tbl.insert(key.clone(), json_to_toml(value, &key_path)?);
            }
            Ok(toml::Value::Table(tbl))
        }
    }
}

// ---------------------------------------------------------------------------
// Atomic write
// ---------------------------------------------------------------------------

/// Write content to path atomically using temp file + rename.
pub fn atomic_write(path: &Path, content: impl AsRef<[u8]>) -> Result<()> {
    use std::io::Write;

    let dir = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    let mut temporary = tempfile::NamedTempFile::new_in(dir)?;
    temporary.write_all(content.as_ref())?;
    temporary.as_file().sync_all()?;
    // Persist replaces the target by rename. On failure the old target stays intact
    // and the temporary file is removed by RAII; never overwrite by copying.
    // Windows rename can fail transiently while another reader/replacer retains
    // a delete-pending handle. Retry the same atomic operation, never a copy.
    // Closing the writer before replacement avoids retaining a destination handle.
    let mut temporary = temporary.into_temp_path();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match temporary.persist(path) {
            Ok(_) => break,
            Err(error) => {
                let retry = cfg!(windows)
                    && matches!(error.error.raw_os_error(), Some(5 | 32 | 33))
                    && !path.is_dir()
                    && std::time::Instant::now() < deadline;
                if !retry {
                    return Err(RezError::Io(error.error));
                }
                temporary = error.path;
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Late binding support
// ---------------------------------------------------------------------------

/// Recognize serialized late functions, including functions assigned to aliases.
/// The metadata field does not have to match the original callable's name.
pub fn is_late_binding_source(value: &Value, _field_name: &str) -> bool {
    value
        .as_str()
        .is_some_and(|source| source_metadata(source).is_ok_and(|(_, function)| function.is_some()))
}

#[cfg(test)]
#[test]
fn test_decorated_deferred_schema_uses_python_syntax() {
    let source = Value::String("@include(\n    'first',  # preserve multiple modules\n    'second',\n)\ndef requires(\n) -> list[str]:\n    return []\n".into());
    assert!(is_late_binding_source(&source, "requires"));
    assert!(is_late_binding_source(&source, "aliased_requirement"));
    assert!(!is_late_binding_source(
        &Value::String("@include('x')\nname = 'foo'".into()),
        "requires"
    ));
    assert!(!is_late_binding_source(
        &Value::String("@include(\ndef requires(): pass".into()),
        "requires"
    ));
}

/// Evaluate late-binding source with package data. Uses python_vm::eval_late_binding.
pub fn eval_late_binding_for_package(
    source: &str,
    field_name: &str,
    pkg_data: &HashMap<String, Value>,
    in_context: bool,
) -> Result<Value> {
    crate::python_vm::eval_late_binding(source, field_name, pkg_data, in_context)
}

/// Evaluate late-binding with full context (request, implicits, system, etc.).
/// Use when building rex for resolved packages.
pub fn eval_late_binding_in_context(
    source: &str,
    field_name: &str,
    pkg_data: &HashMap<String, Value>,
    ctx: &python_runtime::LateBindingContext,
) -> Result<Value> {
    crate::python_vm::eval_late_binding_with_context(source, field_name, pkg_data, true, Some(ctx))
}

/// Resolve a late-bindable field for use in context. If array, return as-is.
/// If late-binding string, evaluate with in_context and context bindings.
pub fn resolve_field_for_context(
    data: &HashMap<String, Value>,
    field_name: &str,
    ctx: Option<&python_runtime::LateBindingContext>,
) -> Result<Option<Value>> {
    let Some(val) = data.get(field_name) else {
        return Ok(None);
    };
    if let Some(arr) = val.as_array() {
        return Ok(Some(Value::Array(arr.clone())));
    }
    let Some(source) = val.as_str() else {
        return Ok(None);
    };
    if !is_late_binding_source(val, field_name) {
        return Ok(None);
    }

    let value = if let Some(context) = ctx {
        eval_late_binding_in_context(source, field_name, data, context)?
    } else {
        eval_late_binding_for_package(source, field_name, data, false)?
    };
    Ok(Some(value))
}

// ---------------------------------------------------------------------------
// Package data validation
// ---------------------------------------------------------------------------

/// Validate one entry in the extensible Rez tests dictionary.
#[doc(hidden)]
pub fn validate_test_entry(name: &str, value: &Value) -> Result<()> {
    if name.is_empty() {
        return Err(test_metadata_error("tests", "test names must not be empty"));
    }
    let key = serde_json::to_string(name).unwrap_or_else(|_| format!("{name:?}"));
    let path = format!("tests[{key}]");
    match value {
        Value::String(_) => Ok(()),
        Value::Array(args) => validate_test_string_array(args, &path),
        Value::Object(fields) => {
            let command = fields.get("command").ok_or_else(|| {
                test_metadata_error(&format!("{path}.command"), "is required for a test object")
            })?;
            match command {
                Value::String(_) => {}
                Value::Array(args) => validate_test_string_array(args, &format!("{path}.command"))?,
                other => {
                    return Err(test_metadata_error(
                        &format!("{path}.command"),
                        &format!(
                            "must be a string or array of strings, got {}",
                            value_type_name(other)
                        ),
                    ));
                }
            }

            if let Some(requires) = fields.get("requires") {
                let requests = requires.as_array().ok_or_else(|| {
                    test_metadata_error(
                        &format!("{path}.requires"),
                        &format!(
                            "must be an array of package requests, got {}",
                            value_type_name(requires)
                        ),
                    )
                })?;
                validate_test_requirements(requests, &format!("{path}.requires"))?;
            }

            if let Some(run_on) = fields.get("run_on") {
                match run_on {
                    Value::String(_) => {}
                    Value::Array(tags) => {
                        validate_test_string_array(tags, &format!("{path}.run_on"))?
                    }
                    other => {
                        return Err(test_metadata_error(
                            &format!("{path}.run_on"),
                            &format!(
                                "must be a string or array of strings, got {}",
                                value_type_name(other)
                            ),
                        ));
                    }
                }
            }

            if let Some(on_variants) = fields.get("on_variants") {
                match on_variants {
                    Value::Bool(_) => {}
                    Value::Object(filter) => {
                        if filter.len() != 2
                            || filter.get("type") != Some(&Value::String("requires".into()))
                        {
                            return Err(test_metadata_error(
                                &format!("{path}.on_variants"),
                                "must be a boolean or {type: \"requires\", value: [package requests]}",
                            ));
                        }
                        let values =
                            filter
                                .get("value")
                                .and_then(Value::as_array)
                                .ok_or_else(|| {
                                    test_metadata_error(
                                        &format!("{path}.on_variants.value"),
                                        "must be an array of package requests",
                                    )
                                })?;
                        validate_test_requirements(values, &format!("{path}.on_variants.value"))?;
                    }
                    other => {
                        return Err(test_metadata_error(
                            &format!("{path}.on_variants"),
                            &format!(
                                "must be a boolean or requires filter object, got {}",
                                value_type_name(other)
                            ),
                        ));
                    }
                }
            }

            // Unknown keys are intentionally accepted and retained: Rez declares
            // the test-entry mapping extensible.
            Ok(())
        }
        other => Err(test_metadata_error(
            &path,
            &format!(
                "must be a string, array of strings, or test object, got {}",
                value_type_name(other)
            ),
        )),
    }
}

fn validate_test_string_array(values: &[Value], path: &str) -> Result<()> {
    for (index, value) in values.iter().enumerate() {
        if !value.is_string() {
            return Err(test_metadata_error(
                &format!("{path}[{index}]"),
                &format!("must be a string, got {}", value_type_name(value)),
            ));
        }
    }
    Ok(())
}

fn validate_test_requirements(values: &[Value], path: &str) -> Result<()> {
    for (index, value) in values.iter().enumerate() {
        let request = value.as_str().ok_or_else(|| {
            test_metadata_error(
                &format!("{path}[{index}]"),
                &format!(
                    "must be a package request string, got {}",
                    value_type_name(value)
                ),
            )
        })?;
        let parsed = Requirement::new(request).map_err(|error| {
            test_metadata_error(
                &format!("{path}[{index}]"),
                &format!("is not a valid package request: {error}"),
            )
        })?;
        let package_name = parsed.name().strip_prefix('.').unwrap_or(parsed.name());
        if !is_valid_rez_package_name(package_name) {
            return Err(test_metadata_error(
                &format!("{path}[{index}]"),
                &format!("has invalid package name in request {request:?}"),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
use foundation::path::is_safe_rez_path;
pub(crate) use foundation::path::{is_safe_rez_path_component, is_valid_rez_package_name};

/// Validate identifiers before using them as filesystem repository components.
/// Package names follow Rez's canonical family-name rule; names and versions
/// must also be safe path components on the current OS.
///
/// # Errors
///
/// Returns an error when the name is not valid for Rez or the current filesystem,
/// or when the version is empty, unsafe as a path component, or invalid for Rez.
pub fn validate_rez_package_path(name: &str, version: Option<&str>) -> Result<()> {
    if !is_valid_rez_package_name(name) {
        return Err(RezError::PackageRequest(format!(
            "Not a valid package name: {name:?}"
        )));
    }
    if !is_safe_rez_path_component(name, false) {
        return Err(RezError::PackageRequest(format!(
            "Not a safe package name path component on this OS: {name:?}"
        )));
    }

    if let Some(version) = version {
        if !is_safe_rez_path_component(version, false) {
            return Err(RezError::PackageRequest(format!(
                "Not a valid package version path component: {version:?}"
            )));
        }
        version::Version::new(version)?;
    }

    Ok(())
}

fn test_metadata_error(path: &str, message: &str) -> RezError {
    RezError::PackageMetadata {
        msg: format!("{path} {message}"),
        path: None,
        resource_key: None,
    }
}

fn validate_optional_field(
    data: &HashMap<String, Value>,
    field: &str,
    expected: &str,
    accepts: impl FnOnce(&Value) -> bool,
) -> Result<()> {
    if let Some(value) = data.get(field) {
        if !accepts(value) {
            return Err(RezError::PackageMetadata {
                msg: format!(
                    "field '{field}' must be {expected}, got {}",
                    value_type_name(value)
                ),
                path: None,
                resource_key: None,
            });
        }
    }
    Ok(())
}

/// Validate package data against schema before parsing. **Required** before `Package::from_data`.
/// Checks name, version, requires, variants, commands, etc. Mirrors Python package_serialise_schema.
pub fn validate_package_data(data: &HashMap<String, Value>) -> Result<()> {
    let normalized;
    let data = if ["commands", "pre_commands", "post_commands"]
        .iter()
        .any(|field| data.get(*field).is_some_and(Value::is_array))
    {
        let mut converted = data.clone();
        crate::package::commands::normalize(&mut converted, &crate::config::CONFIG)?;
        normalized = converted;
        &normalized
    } else {
        data
    };
    // name: Required, non-empty string
    let name = data.get("name").ok_or_else(|| RezError::PackageMetadata {
        msg: "missing required field 'name'".into(),
        path: None,
        resource_key: None,
    })?;

    if !name.is_string() {
        return Err(RezError::PackageMetadata {
            msg: format!(
                "field 'name' must be a string, got {}",
                value_type_name(name)
            ),
            path: None,
            resource_key: None,
        });
    }

    let name_str = name.as_str().ok_or_else(|| RezError::PackageMetadata {
        msg: "field 'name' must be a string".into(),
        path: None,
        resource_key: None,
    })?;
    if name_str.is_empty() {
        return Err(RezError::PackageMetadata {
            msg: "field 'name' cannot be empty".into(),
            path: None,
            resource_key: None,
        });
    }
    if !is_valid_rez_package_name(name_str) {
        return Err(RezError::PackageMetadata {
            msg: format!("field 'name' is not a valid Rez package name: {name_str:?}"),
            path: None,
            resource_key: None,
        });
    }

    // version: Optional string
    if let Some(v) = data.get("version") {
        if !v.is_string() {
            return Err(RezError::PackageMetadata {
                msg: format!(
                    "field 'version' must be a string, got {}",
                    value_type_name(v)
                ),
                path: None,
                resource_key: None,
            });
        }
        if let Some(version) = v.as_str().filter(|version| !version.is_empty()) {
            validate_rez_package_path(name_str, Some(version)).map_err(|error| {
                RezError::PackageMetadata {
                    msg: format!(
                        "field 'version' is not a safe Rez package path component: {error}"
                    ),
                    path: None,
                    resource_key: None,
                }
            })?;
        }
    }

    // description, uuid: Optional strings
    for field in ["description", "uuid"] {
        if let Some(v) = data.get(field) {
            if !v.is_string() {
                return Err(RezError::PackageMetadata {
                    msg: format!(
                        "field '{}' must be a string, got {}",
                        field,
                        value_type_name(v)
                    ),
                    path: None,
                    resource_key: None,
                });
            }
        }
    }

    // authors: Optional array of strings
    if let Some(v) = data.get("authors") {
        if !v.is_array() {
            return Err(RezError::PackageMetadata {
                msg: format!(
                    "field 'authors' must be an array, got {}",
                    value_type_name(v)
                ),
                path: None,
                resource_key: None,
            });
        }
        let arr = v.as_array().ok_or_else(|| RezError::PackageMetadata {
            msg: "field 'authors' must be an array".into(),
            path: None,
            resource_key: None,
        })?;
        for (i, item) in arr.iter().enumerate() {
            if !item.is_string() {
                return Err(RezError::PackageMetadata {
                    msg: format!(
                        "field 'authors[{}]' must be a string, got {}",
                        i,
                        value_type_name(item)
                    ),
                    path: None,
                    resource_key: None,
                });
            }
        }
    }

    // tools: Optional array of strings, or string (late binding)
    if let Some(v) = data.get("tools") {
        if !(v.is_array() || v.is_string() && is_late_binding_source(v, "tools")) {
            return Err(RezError::PackageMetadata {
                msg: format!(
                    "field 'tools' must be an array or late-binding string, got {}",
                    value_type_name(v)
                ),
                path: None,
                resource_key: None,
            });
        }
        if v.is_array() {
            let arr = v.as_array().ok_or_else(|| RezError::PackageMetadata {
                msg: "field 'tools' must be an array".into(),
                path: None,
                resource_key: None,
            })?;
            for (i, item) in arr.iter().enumerate() {
                if !item.is_string() {
                    return Err(RezError::PackageMetadata {
                        msg: format!(
                            "field 'tools[{}]' must be a string, got {}",
                            i,
                            value_type_name(item)
                        ),
                        path: None,
                        resource_key: None,
                    });
                }
            }
        }
    }

    // requires, build_requires, private_build_requires: Optional array of strings, or string (late binding)
    for field in ["requires", "build_requires", "private_build_requires"] {
        if let Some(v) = data.get(field) {
            if !(v.is_array() || v.is_string() && is_late_binding_source(v, field)) {
                return Err(RezError::PackageMetadata {
                    msg: format!(
                        "field '{}' must be an array or late-binding string, got {}",
                        field,
                        value_type_name(v)
                    ),
                    path: None,
                    resource_key: None,
                });
            }
            if v.is_array() {
                let arr = v.as_array().ok_or_else(|| RezError::PackageMetadata {
                    msg: format!("field '{}' must be an array", field),
                    path: None,
                    resource_key: None,
                })?;
                for (i, item) in arr.iter().enumerate() {
                    if !item.is_string() {
                        return Err(RezError::PackageMetadata {
                            msg: format!(
                                "field '{}[{}]' must be a string, got {}",
                                field,
                                i,
                                value_type_name(item)
                            ),
                            path: None,
                            resource_key: None,
                        });
                    }
                }
            }
        }
    }

    // commands, pre_commands, post_commands, pre_build_commands, pre_test_commands: Optional string
    for field in [
        "commands",
        "pre_commands",
        "post_commands",
        "pre_build_commands",
        "pre_test_commands",
    ] {
        if let Some(v) = data.get(field) {
            if !v.is_string() {
                return Err(RezError::PackageMetadata {
                    msg: format!(
                        "field '{}' must be a string, got {}",
                        field,
                        value_type_name(v)
                    ),
                    path: None,
                    resource_key: None,
                });
            }
        }
    }

    // Preserve the declared Rez package schema at the shared data boundary. These
    // checks prevent typed accessors in Package::from_data from silently dropping
    // malformed values while leaving arbitrary extension attributes untouched.
    validate_optional_field(data, "hashed_variants", "a boolean", Value::is_boolean)?;
    for field in ["relocatable", "cachable"] {
        validate_optional_field(
            data,
            field,
            "a boolean, null, or late-binding function",
            |value| value.is_boolean() || value.is_null() || is_late_binding_source(value, field),
        )?;
    }
    validate_optional_field(data, "timestamp", "an integer", |value| {
        value.is_boolean()
            || value
                .as_number()
                .is_some_and(|number| number.as_i64().is_some() || number.as_u64().is_some())
    })?;
    validate_optional_field(data, "release_message", "a string or null", |value| {
        value.is_string() || value.is_null()
    })?;
    validate_optional_field(
        data,
        "previous_version",
        "a version string",
        Value::is_string,
    )?;
    validate_optional_field(
        data,
        "help",
        "a string, nested string list, or late-binding function",
        |value| {
            value.is_string()
                || is_late_binding_source(value, "help")
                || value.as_array().is_some_and(|entries| {
                    entries.iter().all(|entry| {
                        entry
                            .as_array()
                            .is_some_and(|lines| lines.iter().all(Value::is_string))
                    })
                })
        },
    )?;

    // config: Optional object (nested key-value for scope overrides)
    if let Some(v) = data.get("config") {
        if !v.is_object() {
            return Err(RezError::PackageMetadata {
                msg: format!(
                    "field 'config' must be an object, got {}",
                    value_type_name(v)
                ),
                path: None,
                resource_key: None,
            });
        }
    }

    // changelog: Optional string
    if let Some(v) = data.get("changelog") {
        if !v.is_string() {
            return Err(RezError::PackageMetadata {
                msg: format!(
                    "field 'changelog' must be a string, got {}",
                    value_type_name(v)
                ),
                path: None,
                resource_key: None,
            });
        }
    }

    // tests: extensible object whose entries follow Rez's test schema.
    if let Some(v) = data
        .get("tests")
        .filter(|value| !is_late_binding_source(value, "tests"))
    {
        let tests = v.as_object().ok_or_else(|| RezError::PackageMetadata {
            msg: format!("tests must be an object, got {}", value_type_name(v)),
            path: None,
            resource_key: None,
        })?;
        for (name, test) in tests {
            validate_test_entry(name, test)?;
        }
    }

    // variants: Optional array of arrays of strings, or string (late binding)
    if let Some(v) = data.get("variants") {
        if !(v.is_array() || v.is_string() && is_late_binding_source(v, "variants")) {
            return Err(RezError::PackageMetadata {
                msg: format!(
                    "field 'variants' must be an array or late-binding string, got {}",
                    value_type_name(v)
                ),
                path: None,
                resource_key: None,
            });
        }
        if v.is_array() {
            let arr = v.as_array().ok_or_else(|| RezError::PackageMetadata {
                msg: "field 'variants' must be an array".into(),
                path: None,
                resource_key: None,
            })?;
            for (i, variant) in arr.iter().enumerate() {
                if !variant.is_array() {
                    return Err(RezError::PackageMetadata {
                        msg: format!(
                            "field 'variants[{}]' must be an array, got {}",
                            i,
                            value_type_name(variant)
                        ),
                        path: None,
                        resource_key: None,
                    });
                }
                let variant_arr = variant
                    .as_array()
                    .ok_or_else(|| RezError::PackageMetadata {
                        msg: format!("field 'variants[{}]' must be an array", i),
                        path: None,
                        resource_key: None,
                    })?;
                for (j, req) in variant_arr.iter().enumerate() {
                    if !req.is_string() {
                        return Err(RezError::PackageMetadata {
                            msg: format!(
                                "field 'variants[{}][{}]' must be a string, got {}",
                                i,
                                j,
                                value_type_name(req)
                            ),
                            path: None,
                            resource_key: None,
                        });
                    }
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Value normalization
// ---------------------------------------------------------------------------

/// Normalize a JSON Value for serialization:
/// - Single-element arrays become the element itself
/// - Recursively applied to nested structures
pub fn normalize_value(value: &Value) -> Value {
    match value {
        Value::Array(arr) if arr.len() == 1 => normalize_value(&arr[0]),
        Value::Array(arr) => Value::Array(arr.iter().map(normalize_value).collect()),
        Value::Object(map) => {
            let normalized: serde_json::Map<String, Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), normalize_value(v)))
                .collect();
            Value::Object(normalized)
        }
        _ => value.clone(),
    }
}

// ---------------------------------------------------------------------------
// package.py parser (via embedded RustPython VM)
// ---------------------------------------------------------------------------

/// Parse a package.py content string by executing it via embedded Python VM.
pub fn exec_package_py(content: &str) -> Result<HashMap<String, Value>> {
    let build_python_paths = crate::config::CONFIG
        .package_definition_build_python_paths
        .clone();
    exec_package_py_with_context(
        content,
        "package.py",
        &build_python_paths,
        &HashMap::new(),
        None,
    )
}

/// Execute a package definition with Rez's build-time Python imports and early objects.
fn exec_package_py_with_context(
    content: &str,
    filename: &str,
    build_python_paths: &[String],
    early_objects: &HashMap<String, Value>,
    include_paths: Option<&[String]>,
) -> Result<HashMap<String, Value>> {
    // Preamble: marker classes for @early/@late decorators + helper stubs.
    // @early → call function immediately after exec, store return value.
    // @late  → store function source as string (deferred to resolve time).
    // Include decorators retain module metadata for deferred execution.
    let preamble = r#"
import os as _os

class _EarlyMarker:
    def __init__(self, func):
        self.func = func

class _LateMarker:
    def __init__(self, func):
        self.func = func

def early(func=None):
    if func is not None:
        return _EarlyMarker(func)
    return _EarlyMarker

def late(func=None):
    if func is not None:
        return _LateMarker(func)
    return _LateMarker

class ModifyList(list): pass
from rez.exceptions import InvalidPackageError

class _EarlyThis(object):
    """The `this` object for @early bound functions."""
    def __init__(self, data):
        self._data = data
    def __getattr__(self, attr):
        try:
            value = self._data[attr]
        except KeyError:
            raise AttributeError("No such package attribute '%s'" % attr)
        if isinstance(value, (_EarlyMarker, _LateMarker)):
            raise ValueError(
                "An early binding function cannot refer to another early or "
                "late binding function: '%s'" % attr)
        return value

_REZ_DEFAULT_OBJECTS = {
    "building": False,
    "testing": False,
    "build_variant_index": 0,
    "build_variant_requires": [],
}
_REZ_EARLY_OBJECTS = _REZ_EARLY_OBJECTS or {}

def _rez_get_objects():
    result = _REZ_DEFAULT_OBJECTS.copy()
    result.update(_REZ_EARLY_OBJECTS)
    result["build_variant_requires"] = [_Requirement(value) for value in result["build_variant_requires"]]
    return result

def get_objects():
    return _rez_get_objects()

class _ScopeContext(object):
    """Stub for scope context manager used in package.py config overrides."""
    def __init__(self):
        self._stack = [{}]
        self._current_scope = None
    def __call__(self, name):
        self._current_scope = name
        return self
    def __enter__(self):
        self._stack.append({})
        return self
    def __exit__(self, *args):
        top = self._stack.pop()
        if self._stack:
            scope_name = self._current_scope if self._current_scope else 'default'
            if scope_name not in self._stack[-1]:
                self._stack[-1][scope_name] = {}
            self._stack[-1][scope_name].update(top)
    def __setattr__(self, k, v):
        if k.startswith('_'):
            object.__setattr__(self, k, v)
        else:
            self._stack[-1][k] = v
    def __getattr__(self, k):
        for d in reversed(self._stack):
            if k in d:
                return d[k]
        return self
    def to_dict(self):
        return dict(self._stack[0]) if self._stack else {}

scope = _ScopeContext()

def _rez_include_immediate(name):
    for search_path in _REZ_INCLUDE_PATHS:
        inc_file = _os.path.join(search_path, "_include", name + ".py")
        if _os.path.isfile(inc_file):
            with open(inc_file) as stream:
                exec(compile(stream.read(), inc_file, "exec"), globals())
            return None
    raise ImportError("include('%s') not found in any package path" % name)

def include(name, *names):
    """Record deferred modules without executing them while loading package data."""
    def decorated(func):
        marker = func
        while isinstance(func, (_EarlyMarker, _LateMarker)):
            func = func.func
        func._rez_includes = list(getattr(func, "_rez_includes", [])) + [name] + list(names)
        return marker
    return decorated
"#;

    // Postamble: resolve @early (call now), stringify @late and rex callables
    let execution = r#"
import sys as _rez_sys
_rez_saved_sys_path = list(_rez_sys.path)
import linecache as _rez_linecache
_rez_source_lines = _pkg_source.splitlines(True)
_rez_linecache.cache[_pkg_filename] = (
    len(_pkg_source), None, _rez_source_lines, _pkg_filename)
try:
    _rez_sys.path[:0] = _REZ_BUILD_PYTHON_PATHS
    # Preserve the direct include() extension while decorators stay deferred.
    # Source positions retain line numbers, so inspect reads the original cache.
    import ast as _rez_ast
    _rez_tree = _rez_ast.parse(_pkg_source, filename=_pkg_filename)
    _rez_decorator_calls = {
        id(decorator) for node in _rez_ast.walk(_rez_tree)
        if isinstance(node, (_rez_ast.FunctionDef, _rez_ast.AsyncFunctionDef))
        for decorator in node.decorator_list
    }
    _rez_edits = []
    for _rez_node in _rez_ast.walk(_rez_tree):
        if isinstance(_rez_node, _rez_ast.Call) and isinstance(_rez_node.func, _rez_ast.Name) and _rez_node.func.id == "include" and id(_rez_node) not in _rez_decorator_calls:
            _rez_edits.append((_rez_node.func.lineno - 1, _rez_node.func.col_offset, _rez_node.func.end_col_offset))
    _rez_lines = _pkg_source.splitlines(True)
    for _rez_row, _rez_start, _rez_end in sorted(_rez_edits, reverse=True):
        _rez_line = _rez_lines[_rez_row].encode("utf-8")
        _rez_lines[_rez_row] = (_rez_line[:_rez_start] + b"_rez_include_immediate" + _rez_line[_rez_end:]).decode("utf-8")
    exec(compile("".join(_rez_lines), _pkg_filename, "exec"), globals(), globals())
finally:
    _rez_sys.path[:] = _rez_saved_sys_path
"#;

    let postamble = r#"
def _rez_post_process():
    import inspect
    _rex = {"commands", "pre_commands", "post_commands", "pre_build_commands", "pre_test_commands"}
    def _extract(func):
        includes = getattr(func, "_rez_includes", [])
        prefix = "@include(%s)\n" % ", ".join(repr(name) for name in includes) if includes else ""
        lines, _ = inspect.getsourcelines(func)
        name = func.__name__
        offset = None
        for index, line in enumerate(lines):
            declaration = line.lstrip()
            if not declaration.startswith("def "):
                continue
            declaration = declaration[4:].lstrip()
            if not declaration.startswith(name):
                continue
            signature = declaration[len(name):].lstrip()
            if signature.startswith(("(", "[")):
                offset = index
                break
        if offset is not None:
            return prefix + ''.join(lines[offset:]).rstrip()

        # RustPython can report a code object's first line inside a multiline
        # signature, which makes inspect return only the tail of the definition.
        import ast
        module = ast.parse(_pkg_source, filename=_pkg_filename)
        definition = None
        code_line = func.__code__.co_firstlineno
        for node in module.body:
            if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            if node.name != name:
                continue
            if node.lineno <= code_line <= node.end_lineno:
                definition = node
                break
            definition = node
        if definition is None:
            raise RuntimeError("function source does not include its definition")
        source_lines = _pkg_source.splitlines(True)
        return prefix + ''.join(source_lines[definition.lineno - 1:definition.end_lineno]).rstrip()
    g = globals()
    # Keep package globals intact while evaluating each early-bound function.
    _data = {k2: v2 for k2, v2 in g.items() if not k2.startswith('_')}
    for k in list(g.keys()):
        v = g[k]
        if isinstance(v, _EarlyMarker):
            func = v.func
            args = func.__code__.co_argcount + func.__code__.co_kwonlyargcount
            if args not in (0, 1):
                raise TypeError("@early decorated function must take zero or one args only")
            saved_globals = g.copy()
            g["this"] = _EarlyThis(_data)
            g.update(_rez_get_objects())
            try:
                early_value = func(_data) if args else func()
            finally:
                g.clear()
                g.update(saved_globals)
            g[k] = early_value
        elif isinstance(v, _LateMarker):
            g[k] = _extract(v.func)
        elif k in _rex and callable(v):
            g[k] = _extract(v)
_rez_post_process()
"#;

    let full = format!(
        "{}\n{}\n{}\n{}",
        python_runtime::VERSION_BINDINGS,
        preamble,
        execution,
        postamble
    );

    // Inject source and evaluation context while keeping package-only paths scoped.
    let mut inject = HashMap::new();
    inject.insert(
        "_pkg_source".to_string(),
        Value::String(content.to_string()),
    );
    inject.insert(
        "_pkg_filename".to_string(),
        Value::String(filename.to_owned()),
    );
    inject.insert(
        "_REZ_BUILD_PYTHON_PATHS".to_string(),
        serde_json::json!(build_python_paths),
    );
    let include_paths: Vec<String> = include_paths.map_or_else(
        || {
            crate::config::CONFIG
                .expanded_packages_path()
                .iter()
                .map(|path| path.as_str().to_owned())
                .collect()
        },
        <[String]>::to_vec,
    );
    inject.insert(
        "_REZ_INCLUDE_PATHS".to_string(),
        serde_json::json!(include_paths),
    );
    inject.insert(
        "_REZ_EARLY_OBJECTS".to_string(),
        Value::Object(
            early_objects
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
    );

    python_runtime::exec_py_globals(
        &full,
        "package.py",
        Some(&inject),
        &[
            "early",
            "late",
            "ModifyList",
            "InvalidPackageError",
            "include",
            "get_objects",
            "_EarlyMarker",
            "_LateMarker",
            "_EarlyThis",
            "_ScopeContext",
            "_pkg_source",
            "_pkg_filename",
            "_REZ_BUILD_PYTHON_PATHS",
            "_REZ_INCLUDE_PATHS",
            "_REZ_EARLY_OBJECTS",
            "_rez_post_process",
            "_data",
            "_os",
            "_REZ_DEFAULT_OBJECTS",
            "_REZ_EARLY_OBJECTS",
            "_rez_get_objects",
            "_rez_include_immediate",
            "_rez_ast",
            "_rez_tree",
            "_rez_decorator_calls",
            "_rez_edits",
            "_rez_node",
            "_rez_lines",
            "_rez_row",
            "_rez_start",
            "_rez_end",
            "_rez_line",
            "_rez_sys",
            "_rez_saved_sys_path",
            "_rez_linecache",
            "_rez_source_lines",
            "scope",
            "this",
            "building",
            "testing",
            "build_variant_index",
            "build_variant_requires",
        ],
    )
}

/// Load a package.py file from disk.
pub fn load_package_py(path: &std::path::Path) -> Result<HashMap<String, Value>> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| RezError::Python(format!("read {}: {}", path.display(), e)))?;
    let build_python_paths = crate::config::CONFIG
        .package_definition_build_python_paths
        .clone();
    exec_package_py_with_context(
        &content,
        &path.to_string_lossy(),
        &build_python_paths,
        &HashMap::new(),
        None,
    )
}

// ---------------------------------------------------------------------------
// Python format generation
// ---------------------------------------------------------------------------

/// Generate Python package.py content from package data.
///
/// Mirrors Python `_dump_package_data_py()` from package_serialise.py.
pub fn dump_package_data_py(data: &HashMap<String, Value>) -> String {
    let mut lines = vec!["# -*- coding: utf-8 -*-\n".to_string()];

    // Build ordered list of fields to write
    let mut fields: Vec<(&str, &Value)> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // First pass: known keys in preferred order
    for &key in PACKAGE_KEY_ORDER {
        if let Some(val) = data.get(key) {
            if !val.is_null() {
                fields.push((key, val));
                seen.insert(key);
            }
        }
    }

    // Second pass: remaining keys in sorted order
    let mut extra: Vec<_> = data
        .iter()
        .filter(|(k, v)| !seen.contains(k.as_str()) && !v.is_null())
        .collect();
    extra.sort_by_key(|(k, _)| k.as_str());
    for (k, v) in extra {
        fields.push((k.as_str(), v));
    }

    // Command/function fields that should be rendered as Python functions
    let rex_keys = [
        "commands",
        "pre_commands",
        "post_commands",
        "pre_build_commands",
        "pre_test_commands",
    ];

    for (i, (key, value)) in fields.iter().enumerate() {
        let field_text = if *key == "config" {
            // config is rendered as a scope
            format_config_scope(value)
        } else if rex_keys.contains(key) && value.is_string() {
            // Command fields as functions
            format_commands_function(key, value.as_str().unwrap_or_default())
        } else if value.is_array() {
            let arr = value.as_array().expect("value.is_array() verified above");
            if arr.len() > 1 {
                format_list_field(key, arr)
            } else {
                format_simple_field(key, value)
            }
        } else {
            format_simple_field(key, value)
        };

        lines.push(field_text);
        if i < fields.len() - 1 {
            lines.push(String::new()); // blank line between fields
        }
    }

    lines.join("\n")
}

/// Format a simple field as `key = value` using Python repr.
fn format_simple_field(key: &str, value: &Value) -> String {
    let value_txt = python_repr(value);
    if value_txt.contains('\n') {
        format!("{} = \\\n{}", key, indent(&value_txt))
    } else {
        format!("{} = {}", key, value_txt)
    }
}

/// Format a list field with nice multi-line formatting.
fn format_list_field(key: &str, arr: &[Value]) -> String {
    let mut lines = vec![format!("{} = [", key)];
    for (i, entry) in arr.iter().enumerate() {
        let entry_txt = python_repr(entry);
        let entry_lines: Vec<&str> = entry_txt.split('\n').collect();
        for (j, line) in entry_lines.iter().enumerate() {
            let mut out_line = format!("    {}", line);
            if i < arr.len() - 1 && j == entry_lines.len() - 1 {
                out_line.push(',');
            }
            lines.push(out_line);
        }
    }
    lines.push("]".to_string());
    lines.join("\n")
}

/// Format commands as a Python function definition.
fn format_commands_function(name: &str, body: &str) -> String {
    let mut lines = vec![format!("def {}():", name)];
    for line in body.lines() {
        if line.trim().is_empty() {
            lines.push(String::new());
        } else {
            lines.push(format!("    {}", line));
        }
    }
    lines.join("\n")
}

/// Format config dict as `with scope('config') as config:` block.
fn format_config_scope(value: &Value) -> String {
    if let Some(obj) = value.as_object() {
        let attrs = dict_to_attributes_code(obj);
        format!("with scope('config') as config:\n{}", indent(&attrs))
    } else {
        format!("config = {}", python_repr(value))
    }
}

/// Convert nested dict to Python attribute assignment code.
///
/// Example: {"foo": "bar", "sub": {"a": 1}} -> "foo = 'bar'\nsub.a = 1"
fn dict_to_attributes_code(obj: &serde_json::Map<String, Value>) -> String {
    let mut lines = Vec::new();
    for (key, value) in obj {
        if let Some(nested) = value.as_object() {
            let nested_txt = dict_to_attributes_code(nested);
            for line in nested_txt.lines() {
                if !line.starts_with(' ') {
                    lines.push(format!("{}.{}", key, line));
                } else {
                    lines.push(line.to_string());
                }
            }
        } else {
            let value_txt = python_repr(value);
            if value_txt.contains('\n') {
                lines.push(format!("{} = \\", key));
                lines.extend(indent(&value_txt).lines().map(String::from));
            } else {
                lines.push(format!("{} = {}", key, value_txt));
            }
        }
    }
    lines.join("\n")
}

/// Indent text by 4 spaces.
fn indent(txt: &str) -> String {
    txt.lines()
        .map(|line| format!("    {}", line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn python_float_repr(value: f64) -> String {
    let repr = format!("{:?}", value);
    let Some((mantissa, exponent)) = repr.split_once('e') else {
        return repr;
    };
    let exponent: i32 = exponent
        .parse()
        .expect("Rust float formatting emits a valid decimal exponent");
    if (-4..16).contains(&exponent) {
        let negative = mantissa.starts_with('-');
        let mantissa = mantissa.strip_prefix('-').unwrap_or(mantissa);
        let decimal_position = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
        let digits: String = mantissa.chars().filter(|ch| *ch != '.').collect();
        let decimal_position = decimal_position + exponent;
        let mut fixed = String::new();
        if negative {
            fixed.push('-');
        }
        if decimal_position <= 0 {
            fixed.push_str("0.");
            fixed.push_str(&"0".repeat((-decimal_position) as usize));
            fixed.push_str(&digits);
        } else if decimal_position as usize >= digits.len() {
            fixed.push_str(&digits);
            fixed.push_str(&"0".repeat(decimal_position as usize - digits.len()));
            fixed.push_str(".0");
        } else {
            let decimal_position = decimal_position as usize;
            fixed.push_str(&digits[..decimal_position]);
            fixed.push('.');
            fixed.push_str(&digits[decimal_position..]);
        }
        return fixed;
    }

    let sign = if exponent < 0 { '-' } else { '+' };
    format!("{}e{}{:02}", mantissa, sign, exponent.unsigned_abs())
}

fn python_is_printable(character: char) -> bool {
    use unicode_general_category::{get_general_category, GeneralCategory};

    if character == ' ' {
        return true;
    }
    matches!(
        get_general_category(character),
        GeneralCategory::UppercaseLetter
            | GeneralCategory::LowercaseLetter
            | GeneralCategory::TitlecaseLetter
            | GeneralCategory::ModifierLetter
            | GeneralCategory::OtherLetter
            | GeneralCategory::NonspacingMark
            | GeneralCategory::SpacingMark
            | GeneralCategory::EnclosingMark
            | GeneralCategory::DecimalNumber
            | GeneralCategory::LetterNumber
            | GeneralCategory::OtherNumber
            | GeneralCategory::ConnectorPunctuation
            | GeneralCategory::DashPunctuation
            | GeneralCategory::OpenPunctuation
            | GeneralCategory::ClosePunctuation
            | GeneralCategory::InitialPunctuation
            | GeneralCategory::FinalPunctuation
            | GeneralCategory::OtherPunctuation
            | GeneralCategory::MathSymbol
            | GeneralCategory::CurrencySymbol
            | GeneralCategory::ModifierSymbol
            | GeneralCategory::OtherSymbol
    )
}

/// Convert JSON Value to Python repr string.
#[doc(hidden)]
pub fn python_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(i) = n.as_u64() {
                i.to_string()
            } else if let Some(f) = n.as_f64() {
                python_float_repr(f)
            } else {
                n.to_string()
            }
        }
        Value::String(value) => {
            let quote = if value.contains('\'') && !value.contains('"') {
                '"'
            } else {
                '\''
            };
            let mut repr = String::with_capacity(value.len() + 2);
            repr.push(quote);
            for character in value.chars() {
                match character {
                    '\\' => repr.push_str("\\\\"),
                    '\n' => repr.push_str("\\n"),
                    '\r' => repr.push_str("\\r"),
                    '\t' => repr.push_str("\\t"),
                    '\0' => repr.push_str("\\x00"),
                    character if character == quote => {
                        repr.push('\\');
                        repr.push(character);
                    }
                    character if !python_is_printable(character) => {
                        let codepoint = character as u32;
                        if codepoint <= 0xff {
                            repr.push_str(&format!("\\x{codepoint:02x}"));
                        } else if codepoint <= 0xffff {
                            repr.push_str(&format!("\\u{codepoint:04x}"));
                        } else {
                            repr.push_str(&format!("\\U{codepoint:08x}"));
                        }
                    }
                    character => repr.push(character),
                }
            }
            repr.push(quote);
            repr
        }
        Value::Array(arr) => {
            if arr.is_empty() {
                "[]".to_string()
            } else if arr.len() == 1 {
                format!("[{}]", python_repr(&arr[0]))
            } else {
                let items: Vec<String> = arr.iter().map(python_repr).collect();
                // Check if simple enough for single line
                let joined = items.join(", ");
                if joined.len() < 60 && !joined.contains('\n') {
                    format!("[{}]", joined)
                } else {
                    // Multi-line format
                    let mut lines = vec!["[".to_string()];
                    for (i, item) in items.iter().enumerate() {
                        let comma = if i < items.len() - 1 { "," } else { "" };
                        lines.push(format!("    {}{}", item, comma));
                    }
                    lines.push("]".to_string());
                    lines.join("\n")
                }
            }
        }
        Value::Object(map) => {
            if map.is_empty() {
                "{}".to_string()
            } else {
                let mut items = Vec::new();
                for (k, v) in map {
                    items.push(format!(
                        "{}: {}",
                        python_repr(&Value::String(k.clone())),
                        python_repr(v)
                    ));
                }
                let joined = items.join(", ");
                if joined.len() < 60 && !joined.contains('\n') {
                    format!("{{{}}}", joined)
                } else {
                    let mut lines = vec!["{".to_string()];
                    for (i, item) in items.iter().enumerate() {
                        let comma = if i < items.len() - 1 { "," } else { "" };
                        lines.push(format!("    {}{}", item, comma));
                    }
                    lines.push("}".to_string());
                    lines.join("\n")
                }
            }
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Create a unique temp dir for each test (no external crate needed).
    fn test_dir(prefix: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("rez_test_{prefix}_{pid}_{id}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    #[test]
    fn parsed_python_cache_key_tracks_source_content() {
        let dir = test_dir("python_cache_content");
        let cache_dir = dir.join("cache");
        let package_path = dir.join("package.py");
        let first_source = "name = 'a'";
        let second_source = "name = 'b'";
        assert_eq!(first_source.len(), second_source.len());

        fs::write(&package_path, first_source).unwrap();
        let first = load_py_cached(&package_path, Some(&cache_dir), &[], &HashMap::new()).unwrap();
        assert_eq!(first["name"], "a");

        fs::write(&package_path, second_source).unwrap();
        let second = load_py_cached(&package_path, Some(&cache_dir), &[], &HashMap::new()).unwrap();
        assert_eq!(second["name"], "b");
        assert_ne!(
            parsed_cache_key(&package_path, first_source),
            parsed_cache_key(&package_path, second_source)
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn object_aware_package_load_bypasses_content_only_cache() {
        let dir = test_dir("python_cache_early_objects");
        let package_path = dir.join("package.py");
        fs::write(
            &package_path,
            "name = 'cache_objects'\nversion = '1.0'\n@early\ndef description():\n    return 'variant-%s' % build_variant_index\n",
        )
        .unwrap();

        let first = load_from_file_with_objects(
            &package_path,
            FileFormat::Py,
            PackageDataCache::Enabled,
            &HashMap::from([("build_variant_index".to_owned(), Value::from(0))]),
        )
        .unwrap();
        let second = load_from_file_with_objects(
            &package_path,
            FileFormat::Py,
            PackageDataCache::Enabled,
            &HashMap::from([("build_variant_index".to_owned(), Value::from(1))]),
        )
        .unwrap();

        assert_eq!(first["description"], "variant-0");
        assert_eq!(second["description"], "variant-1");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn developer_package_python_cache_bypass_observes_current_import_paths() {
        let dir = test_dir("developer_python_import_paths");
        let first_path = dir.join("first");
        let second_path = dir.join("second");
        fs::create_dir_all(&first_path).unwrap();
        fs::create_dir_all(&second_path).unwrap();
        fs::write(first_path.join("shared_pkg.py"), "NAME = 'first'").unwrap();
        fs::write(second_path.join("shared_pkg.py"), "NAME = 'second'").unwrap();

        let package_path = dir.join("package.py");
        let cache_dir = dir.join("cache");
        fs::write(
            &package_path,
            "import sys\nsys.modules.pop('shared_pkg', None)\nimport shared_pkg\nname = shared_pkg.NAME",
        )
        .unwrap();

        let first = load_py_cached(
            &package_path,
            Some(&cache_dir),
            &[first_path.to_string_lossy().into_owned()],
            &HashMap::new(),
        )
        .unwrap();
        let second = load_py_cached(
            &package_path,
            None,
            &[second_path.to_string_lossy().into_owned()],
            &HashMap::new(),
        )
        .unwrap();

        assert_eq!(first["name"], "first");
        assert_eq!(second["name"], "second");
        assert!(cache_dir.exists());

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn test_find_package_definition_file_preserves_benign_absence() {
        let dir = test_dir("missing_definition");
        assert_eq!(
            find_package_definition_file(&dir, PACKAGE_DEFINITION_EXTENSIONS).unwrap(),
            None
        );
        assert_eq!(
            find_package_definition_file(&dir.join("missing"), PACKAGE_DEFINITION_EXTENSIONS)
                .unwrap(),
            None
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn test_find_package_definition_file_skips_non_file_candidates() {
        let dir = test_dir("non_file_candidate");
        fs::create_dir(dir.join("package.py")).unwrap();
        fs::write(dir.join("package.yaml"), "name: finder-test").unwrap();

        assert_eq!(
            find_package_definition_file(&dir, PACKAGE_DEFINITION_EXTENSIONS).unwrap(),
            Some(dir.join("package.yaml"))
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn test_find_package_definition_file_rejects_file_as_parent() {
        let dir = test_dir("file_parent");
        let parent_file = dir.join("not_a_directory");
        fs::write(&parent_file, "content").unwrap();

        let error =
            find_package_definition_file(&parent_file, PACKAGE_DEFINITION_EXTENSIONS).unwrap_err();
        assert!(matches!(
            error,
            RezError::Io(error) if error.kind() == std::io::ErrorKind::NotADirectory
        ));
        fs::remove_dir_all(dir).unwrap();
    }

    // -- FileFormat --

    #[test]
    fn test_format_extension() {
        assert_eq!(FileFormat::Py.extension(), "py");
        assert_eq!(FileFormat::Yaml.extension(), "yaml");
        assert_eq!(FileFormat::Toml.extension(), "toml");
        assert_eq!(FileFormat::Txt.extension(), "txt");
    }

    #[test]
    fn test_format_from_extension() {
        assert_eq!(FileFormat::from_extension("py"), Some(FileFormat::Py));
        assert_eq!(FileFormat::from_extension("yaml"), Some(FileFormat::Yaml));
        assert_eq!(FileFormat::from_extension("yml"), Some(FileFormat::Yaml));
        assert_eq!(FileFormat::from_extension("toml"), Some(FileFormat::Toml));
        assert_eq!(FileFormat::from_extension("txt"), Some(FileFormat::Txt));
        assert_eq!(FileFormat::from_extension("json"), None);
        // Case insensitive
        assert_eq!(FileFormat::from_extension("YAML"), Some(FileFormat::Yaml));
    }

    #[test]
    fn test_format_display() {
        assert_eq!(format!("{}", FileFormat::Yaml), "yaml");
        assert_eq!(format!("{}", FileFormat::Toml), "toml");
    }

    #[test]
    fn test_format_serde_roundtrip() {
        let fmt = FileFormat::Yaml;
        let json = serde_json::to_string(&fmt).unwrap();
        assert_eq!(json, "\"yaml\"");
        let back: FileFormat = serde_json::from_str(&json).unwrap();
        assert_eq!(back, FileFormat::Yaml);
    }

    // -- detect_format --

    #[test]
    fn test_detect_format() {
        assert_eq!(detect_format(Path::new("foo.yaml")), Some(FileFormat::Yaml));
        assert_eq!(detect_format(Path::new("pkg.toml")), Some(FileFormat::Toml));
        assert_eq!(detect_format(Path::new("bar.py")), Some(FileFormat::Py));
        assert_eq!(
            detect_format(Path::new("readme.txt")),
            Some(FileFormat::Txt)
        );
        assert_eq!(detect_format(Path::new("noext")), None);
    }

    // -- YAML loading --

    #[test]
    fn test_load_yaml() {
        let dir = test_dir("load_yaml");
        let path = dir.join("package.yaml");
        fs::write(
            &path,
            "name: foo\nversion: \"1.2.3\"\nrequires:\n  - bar-1+\n  - baz\n",
        )
        .unwrap();

        let data = load_from_file(&path, FileFormat::Yaml, PackageDataCache::Enabled).unwrap();
        assert_eq!(data["name"], Value::String("foo".into()));
        assert_eq!(data["version"], Value::String("1.2.3".into()));
        assert!(data["requires"].is_array());
        assert_eq!(data["requires"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_load_yaml_empty() {
        let dir = test_dir("load_yaml_empty");
        let path = dir.join("empty.yaml");
        fs::write(&path, "").unwrap();

        let data = load_from_file(&path, FileFormat::Yaml, PackageDataCache::Enabled).unwrap();
        assert!(data.is_empty());
    }

    // -- TOML loading --

    #[test]
    fn test_load_toml() {
        let dir = test_dir("load_toml");
        let path = dir.join("package.toml");
        fs::write(
            &path,
            "name = \"mylib\"\nversion = \"2.0.0\"\nauthors = [\"Alice\", \"Bob\"]\n",
        )
        .unwrap();

        let data = load_from_file(&path, FileFormat::Toml, PackageDataCache::Enabled).unwrap();
        assert_eq!(data["name"], Value::String("mylib".into()));
        assert_eq!(data["version"], Value::String("2.0.0".into()));
        let authors = data["authors"].as_array().unwrap();
        assert_eq!(authors.len(), 2);
        assert_eq!(authors[0], Value::String("Alice".into()));
    }

    #[test]
    fn test_load_toml_empty() {
        let dir = test_dir("load_toml_empty");
        let path = dir.join("empty.toml");
        fs::write(&path, "").unwrap();

        let data = load_from_file(&path, FileFormat::Toml, PackageDataCache::Enabled).unwrap();
        assert!(data.is_empty());
    }

    #[test]
    fn test_toml_non_finite_floats_return_contextual_error() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let error = toml_to_json(&toml::Value::Float(value), "package.toml.metadata.value")
                .unwrap_err();
            assert!(error.to_string().contains("package.toml.metadata.value"));
            assert!(error.to_string().contains("Non-finite TOML float"));
        }

        let parsed: toml::Table = "[metadata]\nvalues = [1.0, nan]\n".parse().unwrap();
        let error = toml_to_json(&toml::Value::Table(parsed), "package.toml").unwrap_err();
        assert!(error
            .to_string()
            .contains("package.toml.metadata.values[1]"));

        let dir = test_dir("toml_non_finite");
        let path = dir.join("package.toml");
        fs::write(&path, "metadata = [nan]").unwrap();
        let error = load_from_file(&path, FileFormat::Toml, PackageDataCache::Enabled).unwrap_err();
        assert!(error.to_string().contains(&path.display().to_string()));
        assert!(error.to_string().contains("metadata[0]"));
    }

    // -- Unsupported formats --

    #[test]
    fn test_load_py_success() {
        let dir = test_dir("load_py_ok");
        let path = dir.join("package.py");
        fs::write(&path, "name = 'test'").unwrap();

        let result = load_from_file(&path, FileFormat::Py, PackageDataCache::Enabled);
        assert!(result.is_ok());
        let data = result.unwrap();
        assert_eq!(data["name"], Value::String("test".into()));
    }

    #[test]
    fn test_load_txt_error() {
        let dir = test_dir("load_txt_err");
        let path = dir.join("data.txt");
        fs::write(&path, "hello").unwrap();

        let result = load_from_file(&path, FileFormat::Txt, PackageDataCache::Enabled);
        assert!(result.is_err());
    }

    // -- load_package_data --

    #[test]
    fn test_load_package_data_yaml() {
        let dir = test_dir("pkg_yaml");
        fs::write(dir.join("package.yaml"), "name: testpkg\n").unwrap();

        let (data, fmt) = load_package_data(&dir).unwrap();
        // package.py is tried first but not found, falls through to .yaml
        assert_eq!(fmt, FileFormat::Yaml);
        assert_eq!(data["name"], Value::String("testpkg".into()));
    }

    #[test]
    fn test_load_package_data_yml() {
        let dir = test_dir("pkg_yml");
        let path = dir.join("package.yml");
        fs::write(&path, "name: ymlpkg\n").unwrap();

        let (data, format) = load_package_data(&dir).unwrap();
        assert_eq!(format, FileFormat::Yaml);
        assert_eq!(data["name"], Value::String("ymlpkg".into()));
    }

    #[test]
    fn test_load_package_data_toml() {
        let dir = test_dir("pkg_toml");
        fs::write(dir.join("package.toml"), "name = \"tpkg\"\n").unwrap();

        let (data, fmt) = load_package_data(&dir).unwrap();
        assert_eq!(fmt, FileFormat::Toml);
        assert_eq!(data["name"], Value::String("tpkg".into()));
    }

    #[test]
    fn test_load_package_data_not_found() {
        let dir = test_dir("pkg_notfound");
        let result = load_package_data(&dir);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("No package definition file found"));
    }

    #[test]
    fn test_load_package_data_py_found_first() {
        // Configured stem order is outermost; upstream Python precedes YAML for each stem.
        let dir = test_dir("pkg_py_yaml");
        fs::write(dir.join("package.py"), "name = 'x'").unwrap();
        fs::write(dir.join("package.yaml"), "name: y\n").unwrap();

        // .py is found first and now loads successfully
        let (data, fmt) = load_package_data(&dir).unwrap();
        assert_eq!(fmt, FileFormat::Py);
        assert_eq!(data["name"], Value::String("x".into()));
    }

    // -- dump_to_file --

    #[test]
    fn test_dump_yaml_roundtrip() {
        let dir = test_dir("dump_yaml");
        let path = dir.join("out.yaml");

        let mut data = HashMap::new();
        data.insert("name".to_string(), Value::String("mypkg".into()));
        data.insert("version".to_string(), Value::String("1.0.0".into()));
        data.insert(
            "requires".to_string(),
            Value::Array(vec![Value::String("foo-1+".into())]),
        );

        dump_to_file(&data, &path, FileFormat::Yaml).unwrap();

        // Read back
        let loaded = load_from_file(&path, FileFormat::Yaml, PackageDataCache::Enabled).unwrap();
        assert_eq!(loaded["name"], Value::String("mypkg".into()));
        assert_eq!(loaded["version"], Value::String("1.0.0".into()));
    }

    #[test]
    fn test_dump_yaml_preserves_nested_nulls() {
        let dir = test_dir("dump_nested_nulls");
        let path = dir.join("nested.yaml");
        let mut data = HashMap::new();
        data.insert(
            "config".to_string(),
            serde_json::json!({"explicit": null, "values": ["kept", null]}),
        );

        dump_to_file(&data, &path, FileFormat::Yaml).unwrap();
        let loaded = load_from_file(&path, FileFormat::Yaml, PackageDataCache::Enabled).unwrap();
        assert_eq!(loaded["config"]["explicit"], Value::Null);
        assert_eq!(loaded["config"]["values"][1], Value::Null);
    }

    #[test]
    fn test_dump_toml_rejects_nested_null_with_path() {
        let mut data = HashMap::new();
        data.insert("config".to_string(), serde_json::json!({"missing": null}));

        let error = serialize_data(&data, FileFormat::Toml)
            .unwrap_err()
            .to_string();
        assert!(error.contains("$[\"config\"][\"missing\"]"), "{error}");
        assert!(error.contains("TOML has no null value"), "{error}");
    }

    #[test]
    fn test_dump_toml_roundtrip() {
        let dir = test_dir("dump_toml");
        let path = dir.join("out.toml");

        let mut data = HashMap::new();
        data.insert("name".to_string(), Value::String("mypkg".into()));
        data.insert("version".to_string(), Value::String("3.0.0".into()));
        data.insert(
            "authors".to_string(),
            Value::Array(vec![
                Value::String("Alice".into()),
                Value::String("Bob".into()),
            ]),
        );

        dump_to_file(&data, &path, FileFormat::Toml).unwrap();

        let loaded = load_from_file(&path, FileFormat::Toml, PackageDataCache::Enabled).unwrap();
        assert_eq!(loaded["name"], Value::String("mypkg".into()));
        assert_eq!(loaded["authors"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_dump_skips_nulls() {
        let dir = test_dir("dump_nulls");
        let path = dir.join("nulls.yaml");

        let mut data = HashMap::new();
        data.insert("name".to_string(), Value::String("pkg".into()));
        data.insert("description".to_string(), Value::Null);

        dump_to_file(&data, &path, FileFormat::Yaml).unwrap();
        let loaded = load_from_file(&path, FileFormat::Yaml, PackageDataCache::Enabled).unwrap();
        assert!(!loaded.contains_key("description"));
    }

    // -- dump_package_data --

    #[test]
    fn test_dump_package_data_validates_before_writing() {
        let dir = test_dir("dump_invalid_package_data");
        let path = dir.join("invalid.yaml");
        let data = HashMap::from([(
            "description".to_owned(),
            Value::String("missing required package name".into()),
        )]);

        assert!(dump_package_data(&data, &path, FileFormat::Yaml, None).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn test_dump_package_data_skip_attrs() {
        let dir = test_dir("dump_skip");
        let path = dir.join("pkg.yaml");

        let mut data = HashMap::new();
        data.insert("name".to_string(), Value::String("test".into()));
        data.insert("version".to_string(), Value::String("1.0".into()));
        data.insert("uuid".to_string(), Value::String("abc-123".into()));

        let mut skip = HashSet::new();
        skip.insert("uuid".to_string());

        dump_package_data(&data, &path, FileFormat::Yaml, Some(&skip)).unwrap();
        let loaded = load_from_file(&path, FileFormat::Yaml, PackageDataCache::Enabled).unwrap();
        assert!(loaded.contains_key("name"));
        assert!(!loaded.contains_key("uuid"));
    }

    // -- atomic_write --

    #[test]
    fn test_atomic_write_basic() {
        let dir = test_dir("atomic");
        let path = dir.join("atomic.txt");

        atomic_write(&path, "hello world").unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, "hello world");
    }

    #[test]
    fn test_atomic_write_overwrite() {
        let dir = test_dir("atomic_ow");
        let path = dir.join("overwrite.txt");

        atomic_write(&path, "first").unwrap();
        atomic_write(&path, "second").unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, "second");
    }

    #[test]
    fn test_atomic_write_creates_parent_dirs() {
        let dir = test_dir("atomic_deep");
        let path = dir.join("sub").join("dir").join("file.txt");

        atomic_write(&path, "deep").unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, "deep");
    }

    #[test]
    fn test_atomic_write_failed_replacement_preserves_destination_and_cleans_temp() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("package.yaml");
        fs::create_dir(&destination).unwrap();
        let sentinel = destination.join("sentinel");
        fs::write(&sentinel, b"existing payload").unwrap();

        assert!(atomic_write(&destination, "replacement metadata").is_err());
        assert!(destination.is_dir());
        assert_eq!(fs::read(&sentinel).unwrap(), b"existing payload");
        let entries = fs::read_dir(temp.path())
            .unwrap()
            .collect::<std::io::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(entries.len(), 1, "failed publication left a temporary file");
        assert_eq!(entries[0].path(), destination);
    }

    #[test]
    fn test_atomic_write_concurrent_readers_observe_complete_replacements() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("package.yaml");
        const SIZE: usize = 16 * 1024;
        atomic_write(&destination, "z".repeat(SIZE)).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(5));
        let mut writers = Vec::new();
        for character in ['a', 'b', 'c', 'd'] {
            let destination = destination.clone();
            let barrier = barrier.clone();
            writers.push(std::thread::spawn(move || {
                let replacement = character.to_string().repeat(SIZE);
                barrier.wait();
                for _ in 0..8 {
                    atomic_write(&destination, &replacement).unwrap();
                }
            }));
        }
        barrier.wait();
        for _ in 0..200 {
            let value = fs::read(&destination).unwrap();
            assert_eq!(value.len(), SIZE);
            assert!(matches!(value[0], b'a' | b'b' | b'c' | b'd' | b'z'));
            assert!(value.iter().all(|byte| *byte == value[0]));
            std::thread::yield_now();
        }
        for writer in writers {
            writer.join().unwrap();
        }
        let final_value = fs::read(&destination).unwrap();
        assert_eq!(final_value.len(), SIZE);
        assert!(final_value.iter().all(|byte| *byte == final_value[0]));
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    // -- normalize_value --

    #[test]
    fn test_normalize_single_element_array() {
        let v = Value::Array(vec![Value::String("only".into())]);
        let n = normalize_value(&v);
        assert_eq!(n, Value::String("only".into()));
    }

    #[test]
    fn test_normalize_multi_element_array() {
        let v = Value::Array(vec![Value::String("a".into()), Value::String("b".into())]);
        let n = normalize_value(&v);
        assert!(n.is_array());
        assert_eq!(n.as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_normalize_nested() {
        let v = Value::Object(
            vec![(
                "key".to_string(),
                Value::Array(vec![Value::Number(42.into())]),
            )]
            .into_iter()
            .collect(),
        );
        let n = normalize_value(&v);
        assert_eq!(n["key"], Value::Number(42.into()));
    }

    #[test]
    fn test_normalize_scalar_passthrough() {
        let v = Value::String("hello".into());
        assert_eq!(normalize_value(&v), v);
    }

    // -- toml <-> json conversion --

    #[test]
    fn test_toml_json_roundtrip() {
        let original = toml::Value::Table({
            let mut t = toml::map::Map::new();
            t.insert("name".into(), toml::Value::String("pkg".into()));
            t.insert("count".into(), toml::Value::Integer(42));
            t.insert("active".into(), toml::Value::Boolean(true));
            t.insert(
                "tags".into(),
                toml::Value::Array(vec![
                    toml::Value::String("a".into()),
                    toml::Value::String("b".into()),
                ]),
            );
            t
        });

        let json = toml_to_json(&original, "$").unwrap();
        assert_eq!(json["name"], Value::String("pkg".into()));
        assert_eq!(json["count"], Value::Number(42.into()));
        assert_eq!(json["active"], Value::Bool(true));

        // Back to toml
        let back = json_to_toml(&json, "$").unwrap();
        assert_eq!(original, back);
    }

    // -- edge cases --

    #[test]
    fn test_load_yaml_nested() {
        let dir = test_dir("yaml_nested");
        let path = dir.join("nested.yaml");
        fs::write(&path, "name: pkg\nconfig:\n  debug: true\n  level: 3\n").unwrap();

        let data = load_from_file(&path, FileFormat::Yaml, PackageDataCache::Enabled).unwrap();
        assert!(data["config"].is_object());
        assert_eq!(data["config"]["debug"], Value::Bool(true));
    }

    #[test]
    fn test_load_toml_nested() {
        let dir = test_dir("toml_nested");
        let path = dir.join("nested.toml");
        fs::write(
            &path,
            "name = \"pkg\"\n\n[config]\ndebug = true\nlevel = 3\n",
        )
        .unwrap();

        let data = load_from_file(&path, FileFormat::Toml, PackageDataCache::Enabled).unwrap();
        assert!(data["config"].is_object());
        assert_eq!(data["config"]["debug"], Value::Bool(true));
    }

    // -- exec_package_py --

    #[test]
    fn test_exec_package_py_basic() {
        let content = r#"
name = "foo"
version = "1.2.0"
description = "A foo package"
uuid = "abc123"
"#;
        let data = exec_package_py(content).unwrap();
        assert_eq!(data["name"], Value::String("foo".into()));
        assert_eq!(data["version"], Value::String("1.2.0".into()));
        assert_eq!(data["description"], Value::String("A foo package".into()));
        assert_eq!(data["uuid"], Value::String("abc123".into()));
    }

    #[test]
    fn test_package_invalid_error_uses_shared_exception_identity() {
        let data = exec_package_py(
            "from rez.exceptions import InvalidPackageError as ImportedInvalidPackageError\nassert InvalidPackageError is ImportedInvalidPackageError\ntry:\n    raise InvalidPackageError('invalid metadata')\nexcept ImportedInvalidPackageError:\n    name = 'shared_exception'\nversion = '1'\n",
        ).unwrap();
        assert_eq!(data["name"], Value::String("shared_exception".into()));
    }

    #[test]
    fn test_exec_package_py_single_quotes() {
        let content = "name = 'bar'\nversion = '2.0'\n";
        let data = exec_package_py(content).unwrap();
        assert_eq!(data["name"], Value::String("bar".into()));
        assert_eq!(data["version"], Value::String("2.0".into()));
    }

    #[test]
    fn test_exec_package_py_booleans() {
        let content = "relocatable = True\nhashed_variants = False\n";
        let data = exec_package_py(content).unwrap();
        assert_eq!(data["relocatable"], Value::Bool(true));
        assert_eq!(data["hashed_variants"], Value::Bool(false));
    }

    #[test]
    fn test_exec_package_py_number() {
        let content = "timestamp = 1234567890\n";
        let data = exec_package_py(content).unwrap();
        assert_eq!(data["timestamp"], Value::Number(1234567890i64.into()));
    }

    #[test]
    fn test_exec_package_py_lists() {
        let content = r#"
requires = [
    "bar-1+",
    "baz",
]
tools = ["foo_tool", "bar_tool"]
"#;
        let data = exec_package_py(content).unwrap();
        let req = data["requires"].as_array().unwrap();
        assert_eq!(req.len(), 2);
        assert_eq!(req[0], Value::String("bar-1+".into()));
        assert_eq!(req[1], Value::String("baz".into()));

        let tools = data["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0], Value::String("foo_tool".into()));
    }

    #[test]
    fn test_exec_package_py_variants() {
        let content = r#"
variants = [
    ["python-3.7"],
    ["python-3.8", "numpy-1.19"],
]
"#;
        let data = exec_package_py(content).unwrap();
        let variants = data["variants"].as_array().unwrap();
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0].as_array().unwrap().len(), 1);
        assert_eq!(variants[1].as_array().unwrap().len(), 2);
        assert_eq!(variants[0][0], Value::String("python-3.7".into()));
        assert_eq!(variants[1][1], Value::String("numpy-1.19".into()));
    }

    #[test]
    fn test_exec_package_py_multiline_string() {
        let content =
            "commands = \"\"\"\nenv.PATH.append(\"{root}/bin\")\nenv.FOO = \"bar\"\n\"\"\"\n";
        let data = exec_package_py(content).unwrap();
        let cmds = data["commands"].as_str().unwrap();
        assert!(cmds.contains("env.PATH.append"));
        assert!(cmds.contains("env.FOO"));
    }

    #[test]
    fn test_exec_package_py_comments() {
        let content = r#"
# This is a comment
name = "pkg"

# Another comment
version = "1.0"
"#;
        let data = exec_package_py(content).unwrap();
        assert_eq!(data.len(), 2);
        assert_eq!(data["name"], Value::String("pkg".into()));
        assert_eq!(data["version"], Value::String("1.0".into()));
    }

    #[test]
    fn test_exec_package_py_dict() {
        let content = r#"
tests = {
    "unit": {
        "command": "pytest",
        "requires": ["pytest"],
    },
}
"#;
        let data = exec_package_py(content).unwrap();
        let tests = data["tests"].as_object().unwrap();
        assert!(tests.contains_key("unit"));
        let unit = tests["unit"].as_object().unwrap();
        assert_eq!(unit["command"], Value::String("pytest".into()));
        let req = unit["requires"].as_array().unwrap();
        assert_eq!(req[0], Value::String("pytest".into()));
    }

    #[test]
    fn test_exec_package_py_none() {
        let content = "build_command = None\n";
        let data = exec_package_py(content).unwrap();
        assert_eq!(data["build_command"], Value::Null);
    }

    #[test]
    fn test_exec_package_py_inline_triple_quote() {
        let content = r#"help = """Some inline help"""
"#;
        let data = exec_package_py(content).unwrap();
        assert_eq!(data["help"], Value::String("Some inline help".into()));
    }

    #[test]
    fn test_exec_package_py_full() {
        // Realistic full package.py
        let content = r#"
name = "my_package"
version = "2.1.0"
description = "A realistic test package"
authors = ["Alice", "Bob"]
relocatable = True

requires = [
    "python-3.7+",
    "numpy-1.19+",
]

variants = [
    ["platform-linux", "arch-x86_64"],
    ["platform-windows"],
]

tools = ["my_tool"]

commands = """
env.PATH.append("{root}/bin")
"""

timestamp = 1700000000
"#;
        let data = exec_package_py(content).unwrap();
        assert_eq!(data["name"], Value::String("my_package".into()));
        assert_eq!(data["version"], Value::String("2.1.0".into()));
        assert_eq!(data["relocatable"], Value::Bool(true));
        assert_eq!(data["timestamp"], Value::Number(1700000000i64.into()));
        assert_eq!(data["requires"].as_array().unwrap().len(), 2);
        assert_eq!(data["variants"].as_array().unwrap().len(), 2);
        assert_eq!(data["tools"].as_array().unwrap().len(), 1);
        assert_eq!(data["authors"].as_array().unwrap().len(), 2);
        assert!(data["commands"].as_str().unwrap().contains("PATH"));
    }

    #[test]
    fn test_include_is_deferred_and_retained_on_source() {
        let content = r#"
name = "test_inc"
version = "1.0"
@include("shared_config", "other_module")
@late
def tools():
    return shared_config.tools
"#;
        let data = exec_package_py(content).unwrap();
        let source = data["tools"].as_str().unwrap();
        assert!(source.starts_with("@include('shared_config', 'other_module')"));
        let root = tempfile::tempdir().unwrap();
        let error = source_with_includes(source, Some(root.path()), None, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("shared_config.py"));
    }

    #[test]
    fn test_deferred_include_executes_installed_module_namespace() {
        let root = tempfile::tempdir().unwrap();
        let includes = root.path().join(".rez/include");
        fs::create_dir_all(&includes).unwrap();
        fs::write(includes.join("helper.py"), "value = 'installed'\n").unwrap();
        let source = "@include('helper')\ndef tools():\n    return [helper.value]\n";
        let data = HashMap::from([(
            "base".into(),
            Value::String(root.path().to_string_lossy().into_owned()),
        )]);
        assert_eq!(
            eval_late_binding_for_package(source, "tools", &data, false).unwrap(),
            serde_json::json!(["installed"])
        );
    }

    #[test]
    fn test_early_variant_requirements_use_canonical_rust_types() {
        let content = r#"
name = "typed"
@early
def description():
    req = build_variant_requires[0]
    return [req.name, req.safe_str(), str(req.range), "3.11.9" in req.range,
            "3.12" in req.range, req.conflict, req.weak]
"#;
        let objects = HashMap::from([(
            "build_variant_requires".into(),
            serde_json::json!(["python-3.11"]),
        )]);
        let data =
            exec_package_py_with_context(content, "typed/package.py", &[], &objects, None).unwrap();
        let requirement = Requirement::new("python-3.11").unwrap();
        assert_eq!(
            data["description"],
            serde_json::json!([
                "python",
                requirement.safe_str(),
                requirement.range().unwrap().to_string(),
                true,
                false,
                false,
                false
            ])
        );
    }

    // -- @early / @late decorator handling --

    #[test]
    fn test_early_decorator() {
        // @early function should be called immediately, return value stored
        let content = r#"
name = "pkg"

@early()
def description():
    return name + " is great"
"#;
        let data = exec_package_py(content).unwrap();
        assert_eq!(data["name"], Value::String("pkg".into()));
        assert_eq!(data["description"], Value::String("pkg is great".into()));
    }

    #[test]
    fn test_early_decorator_numeric() {
        // @early returning a computed number
        let content = r#"
@early()
def build_count():
    return 2 + 3
name = "test"
"#;
        let data = exec_package_py(content).unwrap();
        assert_eq!(data["build_count"], Value::Number(5.into()));
    }

    #[test]
    fn test_early_decorator_receives_rez_objects_and_package_data() {
        let content = r#"
name = "pkg"
version = "1.2"
@early()
def build_state(data):
    return [
        data["name"],
        this.version,
        build_variant_index,
        building,
        build_variant_requires,
    ]
"#;
        let objects = HashMap::from([
            ("building".to_owned(), Value::Bool(true)),
            ("build_variant_index".to_owned(), Value::Number(2.into())),
            (
                "build_variant_requires".to_owned(),
                serde_json::json!(["python-3.11"]),
            ),
        ]);
        let data =
            exec_package_py_with_context(content, "pkg/package.py", &[], &objects, None).unwrap();
        assert_eq!(
            data["build_state"],
            serde_json::json!(["pkg", "1.2", 2, true, ["python-3.11"]])
        );
    }

    #[test]
    fn test_early_decorator_can_call_get_objects() {
        let content = r#"
@early()
def build_state():
    return get_objects()
name = "pkg"
"#;
        let objects = HashMap::from([
            ("building".to_owned(), Value::Bool(true)),
            ("build_variant_index".to_owned(), Value::Number(2.into())),
            (
                "build_variant_requires".to_owned(),
                serde_json::json!(["python-3.11"]),
            ),
        ]);

        let data =
            exec_package_py_with_context(content, "pkg/package.py", &[], &objects, None).unwrap();

        assert_eq!(
            data["build_state"],
            serde_json::json!({
                "building": true,
                "testing": false,
                "build_variant_index": 2,
                "build_variant_requires": ["python-3.11"],
            })
        );
        assert!(!data.contains_key("get_objects"));
    }

    #[test]
    fn test_early_decorator_rejects_references_to_other_deferred_functions() {
        let content = r#"
@late()
def later():
    return "value"

@early()
def now():
    return this.later
name = "pkg"
"#;
        let error = exec_package_py(content).unwrap_err().to_string();
        assert!(
            error.contains("cannot refer to another early or late binding function"),
            "{error}"
        );
    }

    #[test]
    fn test_package_python_build_paths_are_scoped_to_execution() {
        let dir = test_dir("package_python_build_paths");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("build_helper.py"), "value = 'from build path'").unwrap();
        fs::write(
            dir.join("build_helper_not_cached.py"),
            "value = 'unexpected'",
        )
        .unwrap();
        let paths = vec![dir.to_string_lossy().to_string()];

        let data = exec_package_py_with_context(
            "from build_helper import value\nname = value",
            "pkg/package.py",
            &paths,
            &HashMap::new(),
            None,
        )
        .unwrap();
        assert_eq!(data["name"], Value::String("from build path".into()));

        let error = exec_package_py_with_context(
            "import build_helper_not_cached\nname = 'should not run'",
            "pkg/package.py",
            &[],
            &HashMap::new(),
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("build_helper"), "{error}");
    }

    #[test]
    fn test_late_decorator() {
        // @late function should be stored as source string, not executed
        let content = r#"
name = "pkg"

@late()
def requires():
    return ["python-3"]
"#;
        let data = exec_package_py(content).unwrap();
        assert_eq!(data["name"], Value::String("pkg".into()));
        let req_src = data["requires"].as_str().unwrap();
        assert!(req_src.contains("def requires"));
        assert!(req_src.contains("python-3"));

        // Verify eval_late_binding evaluates it correctly
        let pkg_data: HashMap<String, Value> =
            data.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let evaluated =
            eval_late_binding_for_package(req_src, "requires", &pkg_data, false).unwrap();
        let arr = evaluated.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].as_str().unwrap(), "python-3");
    }

    #[test]
    fn test_late_decorator_preserves_annotated_multiline_function() {
        let content = r#"
name = "pkg"

@late()
def requires(
) -> list:
    return ["python-3"]
"#;
        let data =
            exec_package_py_with_context(content, "pkg/package.py", &[], &HashMap::new(), None)
                .unwrap();
        let source = data["requires"].as_str().unwrap();

        assert!(is_late_binding_source(&data["requires"], "requires"));
        assert!(source.contains("def requires("), "{source}");
        assert!(source.contains(") -> list:"), "{source}");
        assert!(source.contains("return [\"python-3\"]"), "{source}");

        let package_data = data
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let evaluated =
            eval_late_binding_for_package(source, "requires", &package_data, false).unwrap();
        assert_eq!(evaluated, serde_json::json!(["python-3"]));
    }

    #[test]
    fn test_late_decorator_in_context() {
        // @late tools that checks in_context() and request
        let content = r#"
name = "maya_edit"

@late()
def tools():
    result = ["edit"]
    if in_context() and "maya" in request:
        result.append("maya-edit")
    return result
"#;
        let data = exec_package_py(content).unwrap();
        let tools_src = data["tools"].as_str().unwrap();
        assert!(tools_src.contains("def tools"));

        let pkg_data: HashMap<String, Value> =
            data.iter().map(|(k, v)| (k.clone(), v.clone())).collect();

        // in_context=false: only ["edit"]
        let evaled_false =
            eval_late_binding_for_package(tools_src, "tools", &pkg_data, false).unwrap();
        let arr = evaled_false.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].as_str().unwrap(), "edit");

        // in_context=true with request containing maya: ["edit", "maya-edit"]
        let ctx = python_runtime::LateBindingContext {
            request: [("maya".into(), "maya-2024".into())].into(),
            implicits: HashMap::new(),
            system: HashMap::new(),
            building: false,
            testing: false,
        };
        let evaled_true =
            eval_late_binding_in_context(tools_src, "tools", &pkg_data, &ctx).unwrap();
        let arr = evaled_true.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0].as_str().unwrap(), "edit");
        assert_eq!(arr[1].as_str().unwrap(), "maya-edit");
    }

    #[test]
    fn test_resolve_late_field_preserves_missing_static_and_array_semantics() {
        let data: HashMap<String, Value> = [
            ("static".into(), Value::String("plain text".into())),
            (
                "tools".into(),
                Value::Array(vec![Value::String("tool".into())]),
            ),
        ]
        .into();

        assert_eq!(
            resolve_field_for_context(&data, "missing", None).unwrap(),
            None
        );
        assert_eq!(
            resolve_field_for_context(&data, "static", None).unwrap(),
            None
        );
        assert_eq!(
            resolve_field_for_context(&data, "tools", None).unwrap(),
            Some(Value::Array(vec![Value::String("tool".into())]))
        );
    }

    #[test]
    fn test_resolve_late_field_propagates_python_errors() {
        let source = "def tools():\n    raise ValueError('late tools failed')\n";
        let data = [("tools".into(), Value::String(source.into()))].into();

        let error = resolve_field_for_context(&data, "tools", None).unwrap_err();
        assert!(error.to_string().contains("late tools failed"), "{error}");
    }

    #[test]
    fn test_commands_as_function() {
        // Rex key as a function should be stored as source string
        let content = r#"
name = "pkg"
def commands():
    env.PATH.append("{root}/bin")
"#;
        let data = exec_package_py(content).unwrap();
        assert_eq!(data["name"], Value::String("pkg".into()));
        let cmds = data["commands"].as_str().unwrap();
        assert!(cmds.contains("def commands"));
        assert!(cmds.contains("env.PATH"));
    }

    #[test]
    fn test_pre_post_commands_as_function() {
        // pre_commands and post_commands as functions
        let content = r#"
name = "pkg"
def pre_commands():
    env.FOO = "bar"
def post_commands():
    env.BAZ = "qux"
"#;
        let data = exec_package_py(content).unwrap();
        let pre = data["pre_commands"].as_str().unwrap();
        let post = data["post_commands"].as_str().unwrap();
        assert!(pre.contains("def pre_commands"));
        assert!(pre.contains("FOO"));
        assert!(post.contains("def post_commands"));
        assert!(post.contains("BAZ"));
    }

    #[test]
    fn test_commands_as_string_still_works() {
        // String commands should still work as before
        let content = "commands = \"\"\"\nenv.PATH.append(\"{root}/bin\")\n\"\"\"\n";
        let data = exec_package_py(content).unwrap();
        let cmds = data["commands"].as_str().unwrap();
        assert!(cmds.contains("env.PATH.append"));
    }

    #[test]
    fn test_include_function_with_real_file() {
        // Test real include() functionality with temp directory structure
        let dir = test_dir("include_test");
        let inc_dir = dir.join("_include");
        fs::create_dir_all(&inc_dir).unwrap();

        // Create an include file with some test data
        let utils_file = inc_dir.join("test_utils.py");
        fs::write(
            &utils_file,
            r#"
shared_version = "2.5.0"
shared_author = "Test Team"
shared_requires = ["common-1+", "base"]
"#,
        )
        .unwrap();

        // Package.py content that uses include()
        let pkg_content = r#"
name = "test_pkg"
include("test_utils")
version = shared_version
authors = [shared_author]
requires = shared_requires
"#;

        let include_paths = vec![dir.to_string_lossy().into_owned()];
        let data = exec_package_py_with_context(
            pkg_content,
            "test_package.py",
            &[],
            &HashMap::new(),
            Some(&include_paths),
        )
        .unwrap();

        // Verify that values from include file were loaded
        assert_eq!(data["name"], Value::String("test_pkg".into()));
        assert_eq!(data["version"], Value::String("2.5.0".into()));
        let authors = data["authors"].as_array().unwrap();
        assert_eq!(authors[0], Value::String("Test Team".into()));
        let requires = data["requires"].as_array().unwrap();
        assert_eq!(requires.len(), 2);
        assert_eq!(requires[0], Value::String("common-1+".into()));
        assert_eq!(requires[1], Value::String("base".into()));
    }

    #[test]
    fn test_include_not_found_error() {
        // Test that include() raises ImportError when file not found
        let content = r#"
name = "test"
include("nonexistent_module")
"#;
        let result = exec_package_py(content);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("nonexistent_module") || err_msg.contains("not found"));
    }

    // -- validate_package_data --

    #[test]
    fn test_validate_declared_optional_package_field_types() {
        let valid = serde_json::json!({
            "name": "foo",
            "hashed_variants": true,
            "relocatable": null,
            "cachable": false,
            "timestamp": -1,
            "release_message": null,
            "previous_version": "1.0",
            "help": [["usage", "details"]]
        });
        let valid = valid.as_object().unwrap().clone().into_iter().collect();
        assert!(validate_package_data(&valid).is_ok());

        let invalid = [
            ("hashed_variants", serde_json::json!(1)),
            ("relocatable", serde_json::json!(1)),
            ("cachable", serde_json::json!([])),
            ("timestamp", serde_json::json!(1.5)),
            ("release_message", serde_json::json!(1)),
            ("previous_version", serde_json::json!(null)),
            ("help", serde_json::json!([["valid"], [false]])),
        ];
        for (field, value) in invalid {
            let mut data = HashMap::from([("name".to_owned(), Value::String("foo".to_owned()))]);
            data.insert(field.to_owned(), value);
            let error = validate_package_data(&data).unwrap_err().to_string();
            assert!(error.contains(field), "{field}: {error}");
        }

        for (field, source) in [
            ("relocatable", "def relocatable(): return True"),
            ("cachable", "def cachable(): return False"),
            ("help", "def help(): return 'usage'"),
        ] {
            let mut data = HashMap::from([("name".to_owned(), Value::String("foo".to_owned()))]);
            data.insert(field.to_owned(), Value::String(source.to_owned()));
            assert!(validate_package_data(&data).is_ok(), "{field}");
        }
    }

    #[test]
    fn test_validate_rez_test_schema_reports_nested_paths() {
        let malformed = [
            (serde_json::json!(null), "tests["),
            (serde_json::json!({"requires": ["foo"]}), ".command"),
            (serde_json::json!({"command": ["python", 1]}), ".command[1]"),
            (
                serde_json::json!({"command": "echo", "requires": "foo"}),
                ".requires",
            ),
            (
                serde_json::json!({"command": "echo", "requires": ["bad/name"]}),
                ".requires[0]",
            ),
            (
                serde_json::json!({"command": "echo", "run_on": [false]}),
                ".run_on[0]",
            ),
            (
                serde_json::json!({"command": "echo", "on_variants": {"type": "other", "value": []}}),
                ".on_variants",
            ),
            (
                serde_json::json!({"command": "echo", "on_variants": {"type": "requires", "value": [false]}}),
                ".on_variants.value[0]",
            ),
        ];

        for (value, expected_path) in malformed {
            let error = validate_test_entry("suite", &value)
                .unwrap_err()
                .to_string();
            assert!(
                error.contains(expected_path),
                "{error:?} does not contain {expected_path:?}"
            );
        }
    }

    #[test]
    fn test_rez_package_name_validation_matches_reference_rules() {
        for name in ["foo", "foo.bar", "_foo2"] {
            assert!(is_valid_rez_package_name(name), "{name:?}");
        }
        for name in [
            "",
            "__pycache__",
            ".foo",
            "foo..bar",
            "foo/",
            "foo-bar",
            "foo.",
        ] {
            assert!(!is_valid_rez_package_name(name), "{name:?}");
        }
    }

    #[test]
    fn test_rez_package_path_validation_matches_reference_and_blocks_path_separators() {
        for (name, version) in [("foo", "1.0"), ("foo.bar", "1-rc1")] {
            assert!(validate_rez_package_path(name, Some(version)).is_ok());
        }
        assert!(validate_rez_package_path("foo.bar", None).is_ok());
        assert_eq!(
            validate_rez_package_path("foo", Some("1:0")).is_err(),
            cfg!(windows)
        );
        assert_eq!(
            validate_rez_package_path("CON", None).is_err(),
            cfg!(windows)
        );

        for (name, version) in [
            ("../outside", "1.0"),
            ("foo", ""),
            ("foo", "1/0"),
            ("foo", r"1\0"),
            ("foo", "C:1"),
        ] {
            assert!(
                validate_rez_package_path(name, Some(version)).is_err(),
                "accepted {name:?}-{version:?}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_path_components_reject_reserved_names_and_streams() {
        for component in [
            "1:0",
            "bad?name",
            "CON",
            "con.txt",
            "COM1",
            "LPT³",
            "trailing.",
        ] {
            assert!(
                !is_safe_rez_path_component(component, false),
                "accepted Windows-reserved component {component:?}"
            );
        }
        for component in ["1.0", "1-rc1", "conifer", "COM10"] {
            assert!(
                is_safe_rez_path_component(component, false),
                "rejected safe Windows component {component:?}"
            );
        }
    }

    #[test]
    fn relative_paths_validate_every_component_before_io() {
        for path in [
            "",
            ".",
            "..",
            "../outside",
            "studio/../outside",
            "/absolute",
            "studio//tools",
            "studio/",
        ] {
            assert!(!is_safe_rez_path(path, false), "accepted {path:?}");
        }
        assert!(is_safe_rez_path("", true));
        assert!(is_safe_rez_path("studio/tools", false));
        assert_eq!(is_safe_rez_path("studio\\tools", false), cfg!(windows));
        assert!(!is_safe_rez_path("C:\\outside", false));
    }

    #[test]
    fn test_validate_package_data_rejects_path_separated_version() {
        let data = HashMap::from([
            ("name".to_owned(), Value::String("foo".to_owned())),
            ("version".to_owned(), Value::String("1/0".to_owned())),
        ]);

        let error = validate_package_data(&data).unwrap_err().to_string();
        assert!(error.contains("safe Rez package path component"), "{error}");
    }

    #[test]
    fn test_validate_package_data_rejects_invalid_package_name() {
        let data = HashMap::from([("name".to_owned(), Value::String("../outside".to_owned()))]);

        let error = validate_package_data(&data).unwrap_err().to_string();
        assert!(error.contains("not a valid Rez package name"), "{error}");
    }

    #[test]
    fn test_validate_valid_data() {
        // Valid minimal package data
        let mut data = HashMap::new();
        data.insert("name".into(), Value::String("foo".into()));
        assert!(validate_package_data(&data).is_ok());
    }

    #[test]
    fn test_validate_valid_full_data() {
        // Valid package with all optional fields
        let data: HashMap<String, Value> = serde_json::from_str(
            r#"{
            "name": "foo",
            "version": "1.2.0",
            "description": "A test package",
            "uuid": "abc-123",
            "authors": ["Alice", "Bob"],
            "tools": ["foo_tool", "bar_tool"],
            "requires": ["bar-1+", "baz"],
            "build_requires": ["cmake"],
            "private_build_requires": ["pytest"],
            "variants": [["python-3.9"], ["python-3.10", "numpy"]]
        }"#,
        )
        .unwrap();
        assert!(validate_package_data(&data).is_ok());
    }

    #[test]
    fn test_validate_missing_name() {
        let data = HashMap::new();
        let result = validate_package_data(&data);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("missing") && err.contains("name"));
    }

    #[test]
    fn test_validate_name_not_string() {
        let mut data = HashMap::new();
        data.insert("name".into(), Value::Number(42.into()));
        let result = validate_package_data(&data);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("name") && err.contains("string"));
    }

    #[test]
    fn test_validate_name_empty() {
        let mut data = HashMap::new();
        data.insert("name".into(), Value::String("".into()));
        let result = validate_package_data(&data);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("name") && err.contains("empty"));
    }

    #[test]
    fn test_validate_version_not_string() {
        let mut data = HashMap::new();
        data.insert("name".into(), Value::String("foo".into()));
        data.insert("version".into(), Value::Number(123.into()));
        let result = validate_package_data(&data);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("version") && err.contains("string"));
    }

    #[test]
    fn test_validate_description_not_string() {
        let mut data = HashMap::new();
        data.insert("name".into(), Value::String("foo".into()));
        data.insert("description".into(), Value::Bool(true));
        let result = validate_package_data(&data);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("description") && err.contains("string"));
    }

    #[test]
    fn test_validate_uuid_not_string() {
        let mut data = HashMap::new();
        data.insert("name".into(), Value::String("foo".into()));
        data.insert("uuid".into(), Value::Array(vec![]));
        let result = validate_package_data(&data);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("uuid") && err.contains("string"));
    }

    #[test]
    fn test_validate_authors_not_array() {
        let mut data = HashMap::new();
        data.insert("name".into(), Value::String("foo".into()));
        data.insert("authors".into(), Value::String("Alice".into()));
        let result = validate_package_data(&data);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("authors") && err.contains("array"));
    }

    #[test]
    fn test_validate_authors_item_not_string() {
        let mut data = HashMap::new();
        data.insert("name".into(), Value::String("foo".into()));
        data.insert(
            "authors".into(),
            Value::Array(vec![
                Value::String("Alice".into()),
                Value::Number(42.into()),
            ]),
        );
        let result = validate_package_data(&data);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("authors") && err.contains("string"));
    }

    #[test]
    fn test_validate_tools_not_array() {
        let mut data = HashMap::new();
        data.insert("name".into(), Value::String("foo".into()));
        data.insert("tools".into(), Value::String("tool1".into()));
        let result = validate_package_data(&data);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("tools") && err.contains("array"));
    }

    #[test]
    fn test_validate_requires_not_array() {
        let mut data = HashMap::new();
        data.insert("name".into(), Value::String("foo".into()));
        data.insert("requires".into(), Value::String("bar-1+".into()));
        let result = validate_package_data(&data);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("requires") && err.contains("array"));
    }

    #[test]
    fn test_validate_requires_item_not_string() {
        let mut data = HashMap::new();
        data.insert("name".into(), Value::String("foo".into()));
        data.insert(
            "requires".into(),
            Value::Array(vec![Value::String("bar-1+".into()), Value::Bool(false)]),
        );
        let result = validate_package_data(&data);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("requires") && err.contains("string"));
    }

    #[test]
    fn test_validate_build_requires_item_not_string() {
        let mut data = HashMap::new();
        data.insert("name".into(), Value::String("foo".into()));
        data.insert(
            "build_requires".into(),
            Value::Array(vec![Value::Number(123.into())]),
        );
        let result = validate_package_data(&data);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("build_requires") && err.contains("string"));
    }

    #[test]
    fn test_validate_variants_not_array() {
        let mut data = HashMap::new();
        data.insert("name".into(), Value::String("foo".into()));
        data.insert("variants".into(), Value::String("python-3.9".into()));
        let result = validate_package_data(&data);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("variants") && err.contains("array"));
    }

    #[test]
    fn test_validate_variants_item_not_array() {
        let mut data = HashMap::new();
        data.insert("name".into(), Value::String("foo".into()));
        data.insert(
            "variants".into(),
            Value::Array(vec![
                Value::Array(vec![Value::String("python-3.9".into())]),
                Value::String("python-3.10".into()),
            ]),
        );
        let result = validate_package_data(&data);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("variants") && err.contains("array"));
    }

    // -- dump_package_data_py --

    #[test]
    fn test_python_repr_matches_python_strings_and_numbers() {
        for (value, expected) in [
            (Value::String("plain".into()), "'plain'"),
            (Value::String("can't".into()), "\"can't\""),
            (
                Value::String("it's \"quoted\"".into()),
                "'it\\'s \"quoted\"'",
            ),
            (Value::String("line\n\tend".into()), "'line\\n\\tend'"),
            (
                Value::String(
                    "\u{007f}\u{0085}\u{00a0}\u{00ad}\u{200b}\u{2028}\u{202e}\u{e000}\u{fdd0}"
                        .into(),
                ),
                "'\\x7f\\x85\\xa0\\xad\\u200b\\u2028\\u202e\\ue000\\ufdd0'",
            ),
            (Value::String("😀".into()), "'😀'"),
            (Value::Number(u64::MAX.into()), "18446744073709551615"),
            (
                Value::Number(serde_json::Number::from_f64(1.0).unwrap()),
                "1.0",
            ),
            (
                Value::Number(serde_json::Number::from_f64(-0.0).unwrap()),
                "-0.0",
            ),
            (
                Value::Number(serde_json::Number::from_f64(1e-7).unwrap()),
                "1e-07",
            ),
            (
                Value::Number(serde_json::Number::from_f64(1e-5).unwrap()),
                "1e-05",
            ),
            (
                Value::Number(serde_json::Number::from_f64(1e20).unwrap()),
                "1e+20",
            ),
        ] {
            assert_eq!(python_repr(&value), expected);
        }
    }

    #[test]
    fn test_dump_py_simple() {
        let mut data = HashMap::new();
        data.insert("name".into(), Value::String("test_pkg".into()));
        data.insert("version".into(), Value::String("1.0.0".into()));

        let output = dump_package_data_py(&data);
        assert!(output.contains("# -*- coding: utf-8 -*-"));
        assert!(output.contains("name = 'test_pkg'"));
        assert!(output.contains("version = '1.0.0'"));
    }

    #[test]
    fn test_dump_py_requires() {
        let mut data = HashMap::new();
        data.insert("name".into(), Value::String("pkg".into()));
        data.insert(
            "requires".into(),
            Value::Array(vec![
                Value::String("python-3.10+".into()),
                Value::String("numpy".into()),
            ]),
        );

        let output = dump_package_data_py(&data);
        assert!(output.contains("requires = ["));
        assert!(output.contains("'python-3.10+',"));
        assert!(output.contains("'numpy'"));
        assert!(output.contains("]"));
    }

    #[test]
    fn test_dump_py_variants() {
        let mut data = HashMap::new();
        data.insert("name".into(), Value::String("pkg".into()));
        data.insert(
            "variants".into(),
            Value::Array(vec![
                Value::Array(vec![
                    Value::String("platform-linux".into()),
                    Value::String("arch-x86_64".into()),
                ]),
                Value::Array(vec![Value::String("platform-windows".into())]),
            ]),
        );

        let output = dump_package_data_py(&data);
        assert!(output.contains("variants = ["));
        assert!(output.contains("['platform-linux', 'arch-x86_64'],"));
        assert!(output.contains("['platform-windows']"));
    }

    #[test]
    fn test_dump_py_commands_function() {
        let mut data = HashMap::new();
        data.insert("name".into(), Value::String("pkg".into()));
        data.insert(
            "commands".into(),
            Value::String(
                "env.PATH.append(\"{root}/bin\")\nenv.PYTHONPATH.append(\"{root}/python\")".into(),
            ),
        );

        let output = dump_package_data_py(&data);
        assert!(output.contains("def commands():"));
        assert!(output.contains("    env.PATH.append(\"{root}/bin\")"));
        assert!(output.contains("    env.PYTHONPATH.append(\"{root}/python\")"));
    }

    #[test]
    fn test_dump_py_booleans() {
        let mut data = HashMap::new();
        data.insert("name".into(), Value::String("pkg".into()));
        data.insert("relocatable".into(), Value::Bool(true));
        data.insert("hashed_variants".into(), Value::Bool(false));

        let output = dump_package_data_py(&data);
        assert!(output.contains("relocatable = True"));
        assert!(output.contains("hashed_variants = False"));
    }

    #[test]
    fn test_dump_py_config() {
        let mut data = HashMap::new();
        data.insert("name".into(), Value::String("pkg".into()));
        let mut config = serde_json::Map::new();
        config.insert("debug".into(), Value::Bool(true));
        config.insert("level".into(), Value::Number(3.into()));
        data.insert("config".into(), Value::Object(config));

        let output = dump_package_data_py(&data);
        assert!(output.contains("with scope('config') as config:"));
        assert!(output.contains("debug = True"));
        assert!(output.contains("level = 3"));
    }

    #[test]
    fn test_dump_py_roundtrip() {
        // Test that we can dump and re-load package.py
        let mut data = HashMap::new();
        data.insert("name".into(), Value::String("roundtrip".into()));
        data.insert("version".into(), Value::String("2.0.0".into()));
        let description =
            "A test package\nwith 'single' and \"double\" quotes,\ttabs and \rcontrol";
        data.insert("description".into(), Value::String(description.into()));
        data.insert(
            "authors".into(),
            Value::Array(vec![
                Value::String("Alice".into()),
                Value::String("Bob".into()),
            ]),
        );

        let output = dump_package_data_py(&data);

        // Parse it back
        let loaded = exec_package_py(&output).unwrap();
        assert_eq!(loaded["name"], Value::String("roundtrip".into()));
        assert_eq!(loaded["version"], Value::String("2.0.0".into()));
        assert_eq!(loaded["description"], Value::String(description.into()));
        let authors = loaded["authors"].as_array().unwrap();
        assert_eq!(authors.len(), 2);
        assert_eq!(authors[0], Value::String("Alice".into()));
    }

    #[test]
    fn test_dump_py_field_order() {
        // Verify fields are output in preferred order
        let mut data = HashMap::new();
        data.insert("uuid".into(), Value::String("abc-123".into()));
        data.insert("name".into(), Value::String("pkg".into()));
        data.insert("version".into(), Value::String("1.0".into()));
        data.insert("description".into(), Value::String("test".into()));

        let output = dump_package_data_py(&data);
        let name_pos = output.find("name = ").unwrap();
        let version_pos = output.find("version = ").unwrap();
        let desc_pos = output.find("description = ").unwrap();
        let uuid_pos = output.find("uuid = ").unwrap();

        // name should come first, then version, then description, then uuid
        assert!(name_pos < version_pos);
        assert!(version_pos < desc_pos);
        assert!(desc_pos < uuid_pos);
    }

    #[test]
    fn test_validate_variants_nested_item_not_string() {
        let mut data = HashMap::new();
        data.insert("name".into(), Value::String("foo".into()));
        data.insert(
            "variants".into(),
            Value::Array(vec![Value::Array(vec![
                Value::String("python-3.9".into()),
                Value::Number(42.into()),
            ])]),
        );
        let result = validate_package_data(&data);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("variants") && err.contains("string"));
    }
}
