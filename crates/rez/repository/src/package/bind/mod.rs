// SPDX-License-Identifier: Apache-2.0

//! System software binding into rez packages.
//!
//! Detects installed software (python, maya, houdini, etc.) and writes package definitions.
//! Builtins in `registry` + DCC apps in `dcc`. Custom modules from `CONFIG.bind_module_path` are
//! listed in `get_bind_modules` but execution requires Python rez (not yet implemented).
//!
//! Submodules:
//! - `system` — platform, arch, os (always succeed)
//! - `tools`  — python, pip, setuptools, cmake, gcc, rez
//! - `dcc`    — Maya, Houdini, Blender, Nuke, Mari, Katana, Substance, etc.
//! - `registry` — builtin detectors by name
//! - `util`   — find_executable, run_version_cmd, create_exe_link
//!
//! # Used by
//! - `cli/repo/bind` — `rez bind`, `rez bind --list`, `rez bind --all`

mod dcc;
mod detect;
mod registry;
mod system;
mod tools;
pub mod util;

use std::collections::{HashMap, HashSet};

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::errors::{Result, RezError};
#[cfg(test)]
use crate::serialise::dump_package_data_py;
use version::{Requirement, Version, VersionRange};

// Re-export detector functions for direct use
pub use dcc::{detect_dcc, ALL_APPS};
pub use detect::{detect_app, AppDef, BindDef};
pub use system::{detect_arch, detect_os, detect_platform, discover_sys_paths};
pub use tools::{
    detect_cmake, detect_docker, detect_gcc, detect_gh, detect_git, detect_perforce, detect_pip,
    detect_python, detect_rez, detect_setuptools,
};
pub use util::{copy_dir_recursive, create_exe_link, find_executable, run_version_cmd};

// ---------------------------------------------------------------------------
// BindFormat - output format for bound packages
// ---------------------------------------------------------------------------

/// Output format for bound packages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BindFormat {
    /// Python format (package.py) - default, matches original rez
    #[default]
    Py,
    /// YAML format (package.yaml)
    Yaml,
}

// ---------------------------------------------------------------------------
// BindInfo - metadata for a bound package
// ---------------------------------------------------------------------------

/// Metadata collected when binding system software as a rez package.
#[derive(Debug, Clone)]
pub struct BindInfo {
    pub name: String,
    pub version: String,
    pub description: String,
    pub requires: Vec<String>,
    pub tools: Vec<String>,
    /// Optional rex commands (e.g. "env.PATH.append('{root}/bin')")
    pub commands: Option<String>,
    /// Package variants (e.g. [["platform-windows", "arch-x86_64", "os-windows-10"]])
    pub variants: Vec<Vec<String>>,
    /// Optional post_commands string
    pub post_commands: Option<String>,
    /// Executables to symlink/copy into bin/ (tool_name, source_exe_path)
    pub exe_bindings: Vec<(String, PathBuf)>,
    /// Python modules to copy into python/ (dirname, source_dir_path)
    pub module_copies: Vec<(String, PathBuf)>,
    /// If set, create a Python venv from this executable path during write_bind_package
    pub venv_source: Option<PathBuf>,
    /// When creating venv: use --copies (copy binaries instead of symlinks for relocatability)
    pub venv_copies: bool,
    /// Use SHA1-hashed variant subpaths instead of nested dirs
    pub hashed_variants: bool,
}

impl BindInfo {
    /// Create minimal BindInfo with just name and version.
    pub fn new(name: &str, version: &str) -> Self {
        Self {
            name: name.to_string(),
            version: version.to_string(),
            description: String::new(),
            requires: Vec::new(),
            tools: Vec::new(),
            commands: None,
            variants: Vec::new(),
            post_commands: None,
            exe_bindings: Vec::new(),
            module_copies: Vec::new(),
            venv_source: None,
            venv_copies: false,
            hashed_variants: crate::config::CONFIG.default_hashed_variants,
        }
    }

    /// Convert to data dict for serialization.
    pub fn to_data(&self) -> HashMap<String, Value> {
        let mut data = HashMap::new();
        data.insert("name".into(), Value::String(self.name.clone()));
        data.insert("version".into(), Value::String(self.version.clone()));

        if !self.description.is_empty() {
            data.insert(
                "description".into(),
                Value::String(self.description.clone()),
            );
        }
        if !self.requires.is_empty() {
            data.insert(
                "requires".into(),
                Value::Array(
                    self.requires
                        .iter()
                        .map(|r| Value::String(r.clone()))
                        .collect(),
                ),
            );
        }
        if !self.tools.is_empty() {
            data.insert(
                "tools".into(),
                Value::Array(
                    self.tools
                        .iter()
                        .map(|t| Value::String(t.clone()))
                        .collect(),
                ),
            );
        }
        if !self.variants.is_empty() {
            data.insert("hashed_variants".into(), Value::Bool(self.hashed_variants));
            data.insert(
                "variants".into(),
                Value::Array(
                    self.variants
                        .iter()
                        .map(|var| {
                            Value::Array(var.iter().map(|v| Value::String(v.clone())).collect())
                        })
                        .collect(),
                ),
            );
        }
        if let Some(ref cmds) = self.commands {
            data.insert("commands".into(), Value::String(cmds.clone()));
        }
        if let Some(ref post) = self.post_commands {
            data.insert("post_commands".into(), Value::String(post.clone()));
        }
        data
    }

    /// Serialize to package.py for focused serializer checks.
    #[cfg(test)]
    pub(crate) fn to_py(&self) -> String {
        dump_package_data_py(&self.to_data())
    }

    /// Inspect YAML serialization without maintaining a separate production writer.
    #[cfg(test)]
    pub(crate) fn to_yaml(&self) -> String {
        serde_yaml::to_string(&serde_json::to_value(self.to_data()).unwrap()).unwrap()
    }
}

// ---------------------------------------------------------------------------
// Bind modules list
// ---------------------------------------------------------------------------

/// Names from CONFIG.bind_module_path. Scans dirs for *.py (excludes _prefix).
/// Custom modules appear in get_bind_modules but bind requires Python execution (not impl).
fn custom_bind_module_names() -> Vec<String> {
    use crate::config::CONFIG;
    let mut out = Vec::new();
    for dir_str in &CONFIG.bind_module_path {
        let path = crate::config::RezConfig::expand_path(dir_str).to_os();
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_file() {
                if let (Some(stem), Some(ext)) = (p.file_stem(), p.extension()) {
                    let stem = stem.to_string_lossy();
                    if ext == "py" && !stem.starts_with('_') {
                        out.push(stem.to_string());
                    }
                }
            }
        }
    }
    out
}

/// Get list of all available bind module names (builtins + DCC + custom from bind_module_path).
pub fn get_bind_modules() -> Vec<String> {
    let mut mods: Vec<String> = registry::builtin_module_names()
        .into_iter()
        .map(String::from)
        .collect();
    for def in ALL_APPS {
        mods.push(def.name().to_string());
    }
    for name in custom_bind_module_names() {
        if !mods.contains(&name) {
            mods.push(name);
        }
    }
    mods
}

/// Modules in dependency order: platform, arch, os first; then python stack; then tools; then DCC.
/// Use for bind_all so requires (e.g. os needs platform+arch, pip/rez need python) are satisfied.
fn get_bind_modules_ordered() -> Vec<String> {
    const ORDER: &[&str] = &[
        "platform",
        "arch",
        "os", // system (os requires platform, arch)
        "python",
        "pip",
        "setuptools", // python stack
        "rez",        // rez needs python
        "cmake",
        "gcc",
        "docker",
        "perforce",
        "gh", // tools
    ];
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for &n in ORDER {
        if seen.insert(n.to_string()) {
            out.push(n.to_string());
        }
    }
    for def in ALL_APPS {
        let n = def.name().to_string();
        if seen.insert(n.clone()) {
            out.push(n);
        }
    }
    for n in registry::builtin_module_names() {
        let s = n.to_string();
        if seen.insert(s.clone()) {
            out.push(s);
        }
    }
    for n in custom_bind_module_names() {
        if seen.insert(n.clone()) {
            out.push(n);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Bind package - detect and write to disk
// ---------------------------------------------------------------------------

/// Bind a system package and, unless disabled, its non-conflict requirements.
/// Returns every package directory created by the primary bind and its dependencies.
pub fn bind_package(
    name: &str,
    path: &Path,
    version_override: Option<&str>,
    format: BindFormat,
    options: &BindOptions,
) -> Result<Vec<PathBuf>> {
    crate::log_info!("bind", "binding {} -> {:?}", name, path);
    let mut opts = options.clone();
    opts.install_path = Some(path.to_path_buf());

    let custom_modules = custom_bind_module_names();
    let mut pending = vec![(name.to_owned(), true, opts.version_range.clone())];
    let mut visited = HashSet::new();
    let mut written = HashSet::new();
    let mut results = Vec::new();

    while let Some((module, primary, version_range)) = pending.pop() {
        let request_key = (
            module.clone(),
            version_range
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
        );
        if !visited.insert(request_key) {
            continue;
        }
        let mut infos = match detect_by_name(&module, &opts, &custom_modules) {
            Ok(infos) => infos,
            Err(error) if primary => return Err(error),
            Err(error) => {
                eprintln!("Could not bind dependency '{}': {}", module, error);
                continue;
            }
        };
        if let Some(range) = version_range.as_ref() {
            let mut matching = Vec::with_capacity(infos.len());
            for info in infos {
                let version = Version::new(&info.version).map_err(|error| {
                    RezError::Bind(format!(
                        "Bind module '{}' detected invalid version '{}': {}",
                        module, info.version, error
                    ))
                })?;
                if range.contains_version(&version) {
                    matching.push(info);
                }
            }
            infos = matching;
        }
        if primary {
            if let Some(version) = version_override {
                crate::log_debug!("bind", "version override: {}", version);
                for info in &mut infos {
                    info.version = version.to_owned();
                }
            }
        }
        for (index, info) in infos.iter().enumerate() {
            let dependencies = if opts.no_deps {
                Vec::new()
            } else {
                info.requires
                    .iter()
                    .chain(info.variants.iter().flatten())
                    .map(|requirement| Requirement::new(requirement))
                    .collect::<Result<Vec<_>>>()?
                    .into_iter()
                    .filter(|requirement| !requirement.conflict())
                    .map(|requirement| {
                        (
                            requirement.name().to_owned(),
                            requirement.range().filter(|range| !range.is_any()).cloned(),
                        )
                    })
                    .collect()
            };
            crate::log_debug!(
                "bind",
                "writing {}/{} ({}/{})",
                info.name,
                info.version,
                index + 1,
                infos.len()
            );
            let package_key = (info.name.clone(), info.version.clone());
            if written.insert(package_key) {
                results.push(write_bind_package(info, path, format, &opts)?);
            }
            pending.extend(
                dependencies
                    .into_iter()
                    .map(|(dependency, range)| (dependency, false, range)),
            );
        }
    }
    crate::log_info!(
        "bind",
        "bound {}: {} package(s) written",
        name,
        results.len()
    );
    Ok(results)
}

/// Bind all available packages, optionally skipping some.
/// Uses dependency order (platform, arch, os → python stack → tools → DCC) so requires are satisfied.
pub fn bind_all(
    path: &Path,
    skip: &[&str],
    format: BindFormat,
    options: &BindOptions,
) -> Result<Vec<PathBuf>> {
    let modules = get_bind_modules_ordered();
    let to_bind: Vec<_> = modules
        .iter()
        .filter(|n| !skip.contains(&n.as_str()))
        .cloned()
        .collect();
    crate::log_info!(
        "bind",
        "bind_all: {} module(s), skip {} -> {} to bind",
        modules.len(),
        skip.len(),
        to_bind.len()
    );
    crate::log_debug!("bind", "modules: {:?}", to_bind);

    let mut opts = options.clone();
    opts.install_path = Some(path.to_path_buf());
    opts.version_range = None;
    opts.no_deps = true;

    let mut results = Vec::new();
    let mut skipped = 0;
    for (i, name) in to_bind.iter().enumerate() {
        crate::log_info!("bind", "  [{}/{}] {}", i + 1, to_bind.len(), name);
        match bind_package(name, path, None, format, &opts) {
            Ok(paths) => results.extend(paths),
            Err(error) => {
                crate::log_debug!("bind", "    skipped: {}", error);
                skipped += 1;
            }
        }
    }
    crate::log_info!(
        "bind",
        "bind_all done: {} package(s) written, {} module(s) skipped",
        results.len(),
        skipped
    );
    Ok(results)
}

/// Detect package info by bind module name. Returns one or more (e.g. maya 2022, 2024).
/// Custom modules from bind_module_path take precedence but need Python execution.
fn detect_by_name(
    name: &str,
    options: &BindOptions,
    custom_modules: &[String],
) -> Result<Vec<BindInfo>> {
    if custom_modules.iter().any(|module| module == name) {
        return Err(RezError::Bind(format!(
            "Custom bind module '{}' from bind_module_path is not yet supported (would require Python execution). Use Python rez for custom bind modules.",
            name
        )));
    }

    registry::detect_by_name_registry(name, options)
}

/// Options for bind operations (package-specific flags from CLI).
#[derive(Debug, Clone, Default)]
pub struct BindOptions {
    /// For python: use venv --copies when rez bind python --copies
    pub python_copy: bool,
    /// Install path; when set, pip/setuptools prefer venv from python package over system PATH
    pub install_path: Option<PathBuf>,
    /// Filter primary detected versions to this Rez version range.
    pub version_range: Option<VersionRange>,
    /// Bind only the requested module without traversing its requirements.
    pub no_deps: bool,
}

/// Prepare every bound variant under the shared repository publication lock.
fn write_bind_package(
    info: &BindInfo,
    base_path: &Path,
    format: BindFormat,
    options: &BindOptions,
) -> Result<PathBuf> {
    // Parse once before output I/O; the publisher and payload use the same typed variants.
    let package = model::package::Package::from_data(info.to_data())?;
    let version = package.version.to_string();
    crate::serialise::validate_rez_package_path(
        &package.name,
        (!version.is_empty()).then_some(version.as_str()),
    )?;
    for (name, source) in &info.exe_bindings {
        if !foundation::path::is_safe_rez_path_component(name, false) || !source.is_file() {
            return Err(RezError::Bind(format!(
                "Invalid executable binding {name:?} from {}",
                source.display()
            )));
        }
    }
    for (name, source) in &info.module_copies {
        if !foundation::path::is_safe_rez_path_component(name, false) || !source.is_dir() {
            return Err(RezError::Bind(format!(
                "Invalid module binding {name:?} from {}",
                source.display()
            )));
        }
    }
    for (_, source) in &info.module_copies {
        util::validate_module_tree(source).map_err(|error| {
            RezError::Bind(format!("Invalid module tree {}: {error}", source.display()))
        })?;
    }
    if let Some(source) = &info.venv_source {
        if !source.is_file() {
            return Err(RezError::Bind(format!(
                "Python venv source is not a file: {}",
                source.display()
            )));
        }
    }
    let pkg_dir = base_path.join(&package.name).join(&version);
    let indices: Vec<_> = (0..package.variants.len().max(1)).collect();
    let mut roots = Vec::new();
    for index in &indices {
        let subpath = package.variants.get(*index).and_then(|requires| {
            model::package::Variant::compute_subpath(requires, package.hashed_variants)
        });
        if subpath
            .as_deref()
            .is_some_and(|path| !foundation::path::is_safe_rez_path(path, false))
        {
            return Err(RezError::Bind("Unsafe bound variant subpath".into()));
        }
        let root = subpath.map_or_else(|| pkg_dir.clone(), |path| pkg_dir.join(path));
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    let prepare_payload = |roots: &[PathBuf], authority: &Path| -> Result<()> {
        for root in roots {
            let relative = root
                .strip_prefix(authority)
                .map_err(|error| RezError::Bind(error.to_string()))?;
            crate::util::directory(authority, relative, true)?;
            if !info.exe_bindings.is_empty() {
                let bin_dir = root.join("bin");
                crate::util::directory(
                    authority,
                    bin_dir
                        .strip_prefix(authority)
                        .map_err(|error| RezError::Bind(error.to_string()))?,
                    true,
                )?;
                for (tool_name, source) in &info.exe_bindings {
                    let destination = bin_dir.join(if cfg!(windows) {
                        format!("{tool_name}.exe")
                    } else {
                        tool_name.to_owned()
                    });
                    create_exe_link(source, &destination).map_err(|error| {
                        let hint = if error.raw_os_error() == Some(32) {
                            " Close other rez processes or run bind from a different rez."
                        } else {
                            ""
                        };
                        RezError::Bind(format!(
                            "Failed to link/copy {} -> {}: {error}{hint}",
                            source.display(),
                            destination.display()
                        ))
                    })?;
                }
            }
            if !info.module_copies.is_empty() {
                let python_dir = root.join("python");
                crate::util::directory(
                    authority,
                    python_dir
                        .strip_prefix(authority)
                        .map_err(|error| RezError::Bind(error.to_string()))?,
                    true,
                )?;
                for (name, source) in &info.module_copies {
                    let destination = python_dir.join(name);
                    crate::util::directory(
                        authority,
                        destination
                            .strip_prefix(authority)
                            .map_err(|error| RezError::Bind(error.to_string()))?,
                        true,
                    )?;
                    copy_dir_recursive(source, &destination).map_err(|error| {
                        RezError::Bind(format!(
                            "Failed to copy {} -> {}: {error}",
                            source.display(),
                            destination.display()
                        ))
                    })?;
                }
            }
            if let Some(source) = &info.venv_source {
                // Venv launchers embed their destination; create it at the final path.
                let venv_dir = root.join("venv");
                crate::util::directory(
                    authority,
                    venv_dir
                        .strip_prefix(authority)
                        .map_err(|error| RezError::Bind(error.to_string()))?,
                    true,
                )?;
                let mut command = Command::new(source);
                command.args(["-m", "venv"]);
                if info.venv_copies || options.python_copy {
                    command.arg("--copies");
                }
                let status = command.arg(&venv_dir).status().map_err(|error| {
                    RezError::Bind(format!(
                        "Failed to create venv with {} at {}: {error}",
                        source.display(),
                        venv_dir.display()
                    ))
                })?;
                if !status.success() {
                    return Err(RezError::Bind(format!(
                        "python -m venv at {} failed (exit code {:?})",
                        venv_dir.display(),
                        status.code()
                    )));
                }
            }
        }
        Ok(())
    };
    let format = match format {
        BindFormat::Py => crate::serialise::FileFormat::Py,
        BindFormat::Yaml => crate::serialise::FileFormat::Yaml,
    };
    if info.venv_source.is_some() {
        // Venv launchers embed the final prefix. This callback retains its
        // legacy partial-payload effects if venv creation or metadata fails.
        let payload = || prepare_payload(&roots, base_path);
        crate::repository::publish_package(
            &package,
            &pkg_dir,
            &indices,
            Some(crate::repository::PublicationPayload::Existing(&payload)),
            Some(format),
        )?;
    } else {
        let stage = tempfile::tempdir()?;
        let staged_roots = roots
            .iter()
            .map(|root| {
                root.strip_prefix(&pkg_dir)
                    .map(|relative| stage.path().join(relative))
                    .map_err(|error| RezError::Bind(error.to_string()))
            })
            .collect::<Result<Vec<_>>>()?;
        prepare_payload(&staged_roots, stage.path())?;
        crate::repository::publish_package(
            &package,
            &pkg_dir,
            &indices,
            Some(crate::repository::PublicationPayload::Merge {
                root: stage,
                paths: None,
                metadata: crate::repository::PublicationMetadata::Installed,
            }),
            Some(format),
        )?;
    }
    Ok(pkg_dir)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn bind_publishes_all_variants_and_roundtrips_quoted_metadata() {
        for format in [BindFormat::Py, BindFormat::Yaml] {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("source");
            fs::create_dir(&source).unwrap();
            fs::write(source.join("payload.py"), "value = 42").unwrap();
            let mut info = BindInfo::new("quoted_bind", "1.0");
            info.description = "Quoted \"value\", backslash \\ and newline\nnext".into();
            info.commands = Some("env.PROBE.set('quoted value')\nenv.OTHER.set('next')".into());
            info.variants = vec![vec!["python-3".into()], vec!["python-4".into()], vec![]];
            info.hashed_variants = true;
            info.module_copies.push(("probe".into(), source));
            let root = write_bind_package(
                &info,
                &temp.path().join("repo"),
                format,
                &BindOptions::default(),
            )
            .unwrap();
            let (data, _) = crate::serialise::load_package_data(&root).unwrap();
            let package = model::package::Package::from_data(data).unwrap();
            assert_eq!(
                package.description.as_deref(),
                Some(info.description.as_str())
            );
            assert_eq!(package.variants.len(), 3);
            for requirements in &package.variants {
                let subpath = model::package::Variant::compute_subpath(requirements, true).unwrap();
                assert_eq!(
                    fs::read_to_string(root.join(subpath).join("python/probe/payload.py")).unwrap(),
                    "value = 42"
                );
            }
            let definition = if format == BindFormat::Py {
                "package.py"
            } else {
                "package.yaml"
            };
            assert!(root.join(definition).is_file());
            let other = if format == BindFormat::Py {
                "package.yaml"
            } else {
                "package.py"
            };
            assert!(!root.join(other).exists());
        }
    }

    #[test]
    fn bind_rejects_invalid_identity_and_explicit_sources_before_output_io() {
        let temp = tempfile::tempdir().unwrap();
        for info in [
            BindInfo::new("../unsafe", "1.0"),
            BindInfo::new("safe_bind", "../unsafe"),
            {
                let mut info = BindInfo::new("safe_bind", "1.0");
                info.exe_bindings
                    .push(("tool".into(), temp.path().join("absent")));
                info
            },
            {
                let mut info = BindInfo::new("safe_bind", "1.0");
                info.module_copies
                    .push(("module".into(), temp.path().join("absent")));
                info
            },
            {
                let mut info = BindInfo::new("safe_bind", "1.0");
                info.module_copies
                    .push(("../unsafe".into(), temp.path().to_path_buf()));
                info
            },
        ] {
            let repo = temp.path().join("repo");
            assert!(
                write_bind_package(&info, &repo, BindFormat::Py, &BindOptions::default()).is_err()
            );
            assert!(!repo.exists());
        }
    }

    #[test]
    fn bind_venv_failure_keeps_old_metadata_and_never_advertises_new_package() {
        let temp = tempfile::tempdir().unwrap();
        let invalid_python = temp.path().join("invalid-python.exe");
        fs::write(&invalid_python, "not an executable").unwrap();
        for existing in [false, true] {
            let repo = temp.path().join(if existing { "existing" } else { "new" });
            let mut info = BindInfo::new("venv_failure", "1.0");
            let previous = if existing {
                let root =
                    write_bind_package(&info, &repo, BindFormat::Py, &BindOptions::default())
                        .unwrap();
                Some(fs::read(root.join("package.py")).unwrap())
            } else {
                None
            };
            info.description = "must not be published".into();
            info.venv_source = Some(invalid_python.clone());
            let error = write_bind_package(&info, &repo, BindFormat::Py, &BindOptions::default())
                .unwrap_err();
            assert!(
                error.to_string().contains("Failed to create venv"),
                "{error}"
            );
            let definition = repo.join("venv_failure/1.0/package.py");
            match previous {
                Some(previous) => assert_eq!(fs::read(definition).unwrap(), previous),
                None => assert!(!definition.exists()),
            }
        }
    }

    #[test]
    fn bind_stages_modules_and_retains_nested_existing_files() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(source.join("nested/change"), "incoming").unwrap();
        let mut info = BindInfo::new("module_merge", "1");
        info.module_copies.push(("module".into(), source));
        let repo = temp.path().join("repo");
        let root =
            write_bind_package(&info, &repo, BindFormat::Yaml, &BindOptions::default()).unwrap();
        fs::write(root.join("python/module/nested/keep"), "retained").unwrap();
        fs::write(root.join("python/module/nested/change"), "old").unwrap();
        write_bind_package(&info, &repo, BindFormat::Yaml, &BindOptions::default()).unwrap();
        assert_eq!(
            fs::read(root.join("python/module/nested/keep")).unwrap(),
            b"retained"
        );
        assert_eq!(
            fs::read(root.join("python/module/nested/change")).unwrap(),
            b"incoming"
        );
    }

    #[test]
    fn bind_later_module_copy_failure_leaves_live_payload_and_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(second.join("collision")).unwrap();
        fs::write(first.join("collision"), "file").unwrap();
        fs::write(second.join("collision/child"), "directory child").unwrap();
        let repo = temp.path().join("repo");
        let mut info = BindInfo::new("staged_failure", "1");
        let root =
            write_bind_package(&info, &repo, BindFormat::Yaml, &BindOptions::default()).unwrap();
        fs::create_dir_all(root.join("python/module")).unwrap();
        fs::write(root.join("python/module/live"), "original").unwrap();
        let definition = fs::read(root.join("package.yaml")).unwrap();
        info.description = "must not publish".into();
        info.module_copies = vec![("module".into(), first), ("module".into(), second)];
        assert!(
            write_bind_package(&info, &repo, BindFormat::Yaml, &BindOptions::default()).is_err()
        );
        assert_eq!(fs::read(root.join("package.yaml")).unwrap(), definition);
        assert_eq!(
            fs::read(root.join("python/module/live")).unwrap(),
            b"original"
        );
        assert!(!root.join("python/module/collision").exists());
    }

    #[cfg(unix)]
    #[test]
    fn bind_module_copy_follows_links_and_rejects_cycles_before_output() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let external = temp.path().join("external");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&external).unwrap();
        fs::write(external.join("value"), "foreign").unwrap();
        std::os::unix::fs::symlink("../external", source.join("directory_link")).unwrap();
        std::os::unix::fs::symlink("../external/value", source.join("file_link")).unwrap();
        let destination = temp.path().join("copied");
        copy_dir_recursive(&source, &destination).unwrap();
        assert_eq!(
            fs::read(destination.join("directory_link/value")).unwrap(),
            b"foreign"
        );
        assert_eq!(fs::read(destination.join("file_link")).unwrap(), b"foreign");
        assert!(!fs::symlink_metadata(destination.join("directory_link"))
            .unwrap()
            .file_type()
            .is_symlink());
        std::os::unix::fs::symlink(".", external.join("cycle")).unwrap();
        let rejected = temp.path().join("rejected");
        assert!(copy_dir_recursive(&source, &rejected).is_err());
        assert!(!rejected.exists());
    }

    #[test]
    fn test_get_bind_modules() {
        let mods = get_bind_modules();
        // Builtins
        for name in &[
            "platform",
            "arch",
            "os",
            "python",
            "pip",
            "setuptools",
            "rez",
            "cmake",
            "gcc",
        ] {
            assert!(
                mods.iter().any(|m| m.as_str() == *name),
                "missing: {}",
                name
            );
        }
        // DCC apps
        for name in &[
            "maya",
            "houdini",
            "blender",
            "nuke",
            "mari",
            "katana",
            "substance_designer",
            "substance_painter",
            "embergen",
            "liquigen",
            "geogen",
            "illugen",
            "davinci_resolve",
            "maya_usd",
            "maya_lookdevx",
            "maya_bifrost",
            "maya_arnold",
            "deadline",
            "unreal",
            "meshroom",
            "cinema4d",
            "cursor",
        ] {
            assert!(
                mods.iter().any(|m| m.as_str() == *name),
                "missing: {}",
                name
            );
        }
        for name in &["docker", "perforce", "gh"] {
            assert!(
                mods.iter().any(|m| m.as_str() == *name),
                "missing tool: {}",
                name
            );
        }
    }

    #[test]
    fn test_detect_platform() {
        let info = detect_platform();
        assert_eq!(info.name, "platform");
        assert!(!info.version.is_empty());
        let commands = info.commands.as_deref().unwrap();
        assert!(commands.contains("system.paths"));
        assert!(commands.contains("system.environ"));
    }

    #[test]
    fn test_detect_arch() {
        let info = detect_arch();
        assert_eq!(info.name, "arch");
        assert!(!info.version.is_empty());
    }

    #[test]
    fn test_detect_os() {
        let info = detect_os();
        assert_eq!(info.name, "os");
        assert!(!info.version.is_empty());
        assert_eq!(info.requires.len(), 2);
        assert!(info.requires[0].starts_with("platform-"));
        assert!(info.requires[1].starts_with("arch-"));
        assert!(
            info.commands.is_none(),
            "platform owns the shared OS environment"
        );
    }

    #[test]
    fn test_bind_info_to_yaml() {
        let mut info = BindInfo::new("test_pkg", "1.2.3");
        info.description = "A test package".to_string();
        info.requires = vec!["foo-1+".to_string(), "bar-2".to_string()];
        info.tools = vec!["mytool".to_string()];
        info.commands = Some("env.PATH.append('{this.root}/bin')".to_string());
        info.variants = vec![vec![
            "platform-linux".to_string(),
            "arch-x86_64".to_string(),
        ]];

        let yaml = info.to_yaml();
        let data: serde_json::Value = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(data["name"], "test_pkg");
        assert_eq!(data["version"], "1.2.3");
        assert_eq!(data["requires"][0], "foo-1+");
        assert_eq!(data["commands"], info.commands.as_deref().unwrap());
    }

    #[test]
    fn test_bind_info_to_py_with_variants() {
        let mut info = BindInfo::new("cmake", "3.28.1");
        info.tools = vec!["cmake".to_string()];
        info.commands = Some("env.PATH.append('{root}/bin')".to_string());
        info.variants = vec![vec![
            "platform-windows".to_string(),
            "arch-x86_64".to_string(),
            "os-windows-10".to_string(),
        ]];

        let py = info.to_py();
        assert!(py.contains("name = 'cmake'"));
        assert!(py.contains("variants = ["));
        assert!(py.contains("def commands():"));
        assert!(py.contains("{root}/bin"));
    }

    #[test]
    fn test_bind_info_commands_py() {
        let mut info = BindInfo::new("python", "3.12.1");
        info.commands = Some("env.PATH.append('{this.root}/venv/bin')".to_string());
        let py = info.to_py();
        assert!(py.contains("def commands():"));
        assert!(py.contains("venv/bin"));
        let yaml = info.to_yaml();
        let data: serde_json::Value = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(data["commands"], info.commands.as_deref().unwrap());
    }

    #[test]
    fn test_bind_info_minimal_yaml() {
        let info = BindInfo::new("simple", "1.0");
        let yaml = info.to_yaml();
        let data: serde_json::Value = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(data["name"], "simple");
        assert!(!yaml.contains("requires"));
        assert!(!yaml.contains("commands"));
    }

    #[test]
    fn test_write_bind_py() {
        let tmp = std::env::temp_dir().join("rez_test_bind_write");
        let _ = fs::remove_dir_all(&tmp);
        let info = BindInfo::new("test_pkg", "1.0.0");
        let opts = BindOptions::default();
        let result = write_bind_package(&info, &tmp, BindFormat::Py, &opts);
        assert!(result.is_ok());
        let pkg_dir = result.unwrap();
        assert!(pkg_dir.join("package.py").exists());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_bind_payload_uses_canonical_variant_paths_and_hashes() {
        // Golden identities from the reference Python Rez Requirement/variant SHA1.
        for (requirement, canonical, hash) in [
            (
                "python-3.11",
                "python-3.11",
                "b99a49e4ec48ad4d9833734782ee775813473768",
            ),
            (
                "python-3.11+",
                "python-3.11+",
                "ce0c5839d284f3a2e720b51980ad1b0c2d7ccb1f",
            ),
            (
                "python>=3.11",
                "python-3.11+",
                "ce0c5839d284f3a2e720b51980ad1b0c2d7ccb1f",
            ),
        ] {
            for hashed in [false, true] {
                let temp = tempfile::tempdir().unwrap();
                let source = temp.path().join("module_source");
                fs::create_dir(&source).unwrap();
                fs::write(source.join("payload.py"), "value = 42").unwrap();
                let repo = temp.path().join("repo");
                let mut info = BindInfo::new("test_bind", "1.0");
                info.variants = vec![vec![requirement.into()]];
                info.hashed_variants = hashed;
                info.module_copies = vec![("probe".into(), source)];

                let package_dir =
                    write_bind_package(&info, &repo, BindFormat::Yaml, &BindOptions::default())
                        .unwrap();
                let expected = if hashed { hash } else { canonical };
                assert!(package_dir
                    .join(expected)
                    .join("python/probe/payload.py")
                    .is_file());
                assert!(package_dir.join("package.yaml").is_file());
                if requirement == "python-3.11+" && hashed {
                    assert!(!package_dir
                        .join("b99a49e4ec48ad4d9833734782ee775813473768")
                        .exists());
                }
            }
        }
    }

    #[test]
    fn test_bind_rejects_invalid_variant_before_writing_package() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let mut info = BindInfo::new("test_bind", "1.0");
        info.variants = vec![vec!["python-@".into()]];
        assert!(
            write_bind_package(&info, &repo, BindFormat::Yaml, &BindOptions::default(),).is_err()
        );
        assert!(!repo.exists());
    }

    #[test]
    fn test_bind_package_platform() {
        let tmp = std::env::temp_dir().join("rez_test_bind_platform");
        let _ = fs::remove_dir_all(&tmp);
        let opts = BindOptions::default();
        let result = bind_package("platform", &tmp, None, BindFormat::default(), &opts);
        assert!(result.is_ok());
        let paths = result.unwrap();
        assert!(!paths.is_empty());
        assert!(paths[0].join("package.py").exists());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn bind_package_traverses_non_conflict_dependencies_by_default() {
        let tmp = std::env::temp_dir().join("rez_test_bind_dependencies");
        let _ = fs::remove_dir_all(&tmp);
        let paths = bind_package(
            "os",
            &tmp,
            None,
            BindFormat::default(),
            &BindOptions::default(),
        )
        .unwrap();

        assert!(paths
            .iter()
            .any(|path| path.starts_with(tmp.join("platform"))));
        assert!(paths.iter().any(|path| path.starts_with(tmp.join("arch"))));
        assert!(paths.iter().any(|path| path.starts_with(tmp.join("os"))));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn bind_package_no_deps_limits_output_to_requested_module() {
        let tmp = std::env::temp_dir().join("rez_test_bind_no_deps");
        let _ = fs::remove_dir_all(&tmp);
        let opts = BindOptions {
            no_deps: true,
            ..Default::default()
        };
        let paths = bind_package("os", &tmp, None, BindFormat::default(), &opts).unwrap();

        assert_eq!(paths.len(), 1);
        assert!(paths[0].starts_with(tmp.join("os")));
        assert!(!tmp.join("platform").exists());
        assert!(!tmp.join("arch").exists());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn bind_package_filters_detected_versions_by_range() {
        let tmp = std::env::temp_dir().join("rez_test_bind_version_range");
        let _ = fs::remove_dir_all(&tmp);
        let request = Requirement::new("arch-==0").unwrap();
        let opts = BindOptions {
            version_range: request.range().cloned(),
            no_deps: true,
            ..Default::default()
        };

        let paths = bind_package("arch", &tmp, None, BindFormat::default(), &opts).unwrap();

        assert!(paths.is_empty());
        assert!(!tmp.join("arch").exists());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_bind_package_version_override() {
        let tmp = std::env::temp_dir().join("rez_test_bind_override");
        let _ = fs::remove_dir_all(&tmp);
        let opts = BindOptions::default();
        let result = bind_package(
            "arch",
            &tmp,
            Some("custom_arch"),
            BindFormat::default(),
            &opts,
        );
        assert!(result.is_ok());
        let paths = result.unwrap();
        assert!(paths[0].ends_with("custom_arch"));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_bind_package_unknown() {
        let tmp = std::env::temp_dir().join("rez_test_bind_unknown");
        let opts = BindOptions::default();
        let result = bind_package("nonexistent", &tmp, None, BindFormat::default(), &opts);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Unknown bind module"));
    }

    #[test]
    fn test_bind_all() {
        let tmp = std::env::temp_dir().join("rez_test_bind_all");
        let _ = fs::remove_dir_all(&tmp);
        let opts = BindOptions::default();
        let result = bind_all(&tmp, &[], BindFormat::default(), &opts);
        assert!(result.is_ok());
        assert!(result.unwrap().len() >= 3);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_bind_all_with_skip() {
        let tmp = std::env::temp_dir().join("rez_test_bind_skip");
        let _ = fs::remove_dir_all(&tmp);
        let opts = BindOptions::default();
        let result = bind_all(
            &tmp,
            &["python", "cmake", "os"],
            BindFormat::default(),
            &opts,
        );
        assert!(result.is_ok());
        assert!(result.unwrap().len() >= 3);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_detect_by_name_all_builtins() {
        let opts = BindOptions::default();
        assert!(detect_by_name("platform", &opts, &[]).is_ok());
        assert!(detect_by_name("arch", &opts, &[]).is_ok());
        assert!(detect_by_name("os", &opts, &[]).is_ok());
        assert!(detect_by_name("rez", &opts, &[]).is_ok());
    }

    #[test]
    fn test_custom_bind_module_shadows_builtin() {
        let opts = BindOptions::default();
        let custom_modules = vec!["platform".to_owned()];

        let error = detect_by_name("platform", &opts, &custom_modules).unwrap_err();
        assert!(error.to_string().contains("Custom bind module 'platform'"));
    }

    #[test]
    fn test_detect_rez() {
        let info = detect_rez();
        assert_eq!(info.name, "rez");
        assert!(!info.version.is_empty());
        assert!(info.description.contains("rez-rs"));
    }

    #[test]
    fn test_detect_by_name_unknown() {
        let opts = BindOptions::default();
        assert!(detect_by_name("foobar", &opts, &[]).is_err());
    }

    #[test]
    fn test_find_venv_pip_in_python_packages() {
        let tmp = std::env::temp_dir().join("rez_test_venv_pip_scan");
        let _ = fs::remove_dir_all(&tmp);
        // Create layout: install_path/python/3.12.0/venv/Scripts/pip.exe (win) or venv/bin/pip (unix)
        let (venv_sub, pip_name) = if cfg!(windows) {
            ("venv/Scripts", "pip.exe")
        } else {
            ("venv/bin", "pip")
        };
        let pip_path = tmp
            .join("python")
            .join("3.12.0")
            .join(venv_sub)
            .join(pip_name);
        fs::create_dir_all(pip_path.parent().unwrap()).unwrap();
        fs::write(&pip_path, "").unwrap();
        let found = super::util::find_venv_pip_in_python_packages(&tmp);
        assert!(found.is_some(), "should find venv pip");
        let (path, ver) = found.unwrap();
        assert_eq!(ver, "3.12.0");
        assert!(path.ends_with(pip_name));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_find_venv_pip_in_python_packages_variant_subdir() {
        let tmp = std::env::temp_dir().join("rez_test_venv_pip_variant");
        let _ = fs::remove_dir_all(&tmp);
        let (venv_sub, pip_name) = if cfg!(windows) {
            ("venv/Scripts", "pip.exe")
        } else {
            ("venv/bin", "pip")
        };
        // python/3.11.0/<hash>/venv/... layout
        let pip_path = tmp
            .join("python")
            .join("3.11.0")
            .join("abc123")
            .join(venv_sub)
            .join(pip_name);
        fs::create_dir_all(pip_path.parent().unwrap()).unwrap();
        fs::write(&pip_path, "").unwrap();
        let found = super::util::find_venv_pip_in_python_packages(&tmp);
        assert!(found.is_some());
        let (path, ver) = found.unwrap();
        assert_eq!(ver, "3.11.0");
        assert!(path.ends_with(pip_name));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    #[cfg(unix)]
    fn test_detect_pip_prefers_venv() {
        // Create venv layout with executable pip script that prints version
        let tmp = std::env::temp_dir().join("rez_test_detect_pip_venv");
        let _ = fs::remove_dir_all(&tmp);
        let pip_path = tmp
            .join("python")
            .join("3.12.0")
            .join("venv")
            .join("bin")
            .join("pip");
        fs::create_dir_all(pip_path.parent().unwrap()).unwrap();
        let script = r#"#!/bin/sh
echo "pip 24.0 from /fake/site-packages (python 3.12)"
"#;
        fs::write(&pip_path, script).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&pip_path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&pip_path, perms).unwrap();
        }
        let opts = BindOptions {
            install_path: Some(tmp.clone()),
            ..Default::default()
        };
        let info = super::tools::detect_pip_with_options(&opts);
        let _ = fs::remove_dir_all(&tmp);
        assert!(info.is_some(), "should detect pip from venv");
        let info = info.unwrap();
        assert_eq!(info.name, "pip");
        assert!(info
            .variants
            .iter()
            .flatten()
            .any(|requirement| { requirement.starts_with("platform-") }));
        assert!(info
            .variants
            .iter()
            .flatten()
            .any(|requirement| { requirement.starts_with("arch-") }));
        assert!(info
            .variants
            .iter()
            .flatten()
            .any(|requirement| { requirement.starts_with("os-") }));
        assert!(info
            .variants
            .iter()
            .flatten()
            .any(|requirement| { requirement.starts_with("python-") }));
        assert!(
            !info.exe_bindings.is_empty(),
            "should use exe_bindings from venv"
        );
        assert!(
            info.exe_bindings.iter().any(|(_, p)| p.starts_with(&tmp)),
            "exe_bindings should point to venv pip"
        );
    }
}
