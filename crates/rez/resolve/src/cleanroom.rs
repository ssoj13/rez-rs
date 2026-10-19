// SPDX-License-Identifier: Apache-2.0

//! Clean-Room Environment Sanitizer, Leak Inspector, and Virtual Merged Directory Materializer.
//!
//! Provides two studio-grade isolation and deployment primitives:
//! 1. **Clean-Room Environment Sanitizer & Leak Inspector**: Surgically removes
//!    hazardous host variables (`PYTHONPATH`, `LD_LIBRARY_PATH`, `QT_PLUGIN_PATH`,
//!    `PXR_PLUGINPATH_NAME`, `MAYA_PLUG_IN_PATH`, `HOUDINI_PATH`, `OCIO`, etc.)
//!    and resets `PATH` to OS system defaults while keeping desktop/display/auth
//!    session variables intact.
//! 2. **Virtual Merged Environment Directory**: Assembles a unified directory tree
//!    (`bin/`, `lib/`, `python/`, `plugins/`, etc.) across all resolved packages
//!    using cross-platform symlinks, hardlinks, or copies with full package provenance.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::errors::{Result, RezError};
use crate::platform::Platform;
use crate::resolver::ResolvedPackageInfo;

/// Category of a hazardous host environment variable that can pollute a resolved Rez context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HazardCategory {
    PythonRuntime,
    DynamicLinker,
    QtAndGui,
    UsdAndHydra,
    ColorManagement,
    DccPlugins,
    CompilerAndBuild,
}

impl HazardCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::PythonRuntime => "Python Runtime",
            Self::DynamicLinker => "Dynamic Linker",
            Self::QtAndGui => "Qt / UI Plugins",
            Self::UsdAndHydra => "USD / Hydra / MaterialX",
            Self::ColorManagement => "OpenColorIO (OCIO)",
            Self::DccPlugins => "DCC Plugins & Scripts",
            Self::CompilerAndBuild => "Compiler & Build Flags",
        }
    }
}

/// Known hazardous environment variable record detected in the host environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeakedVariable {
    pub name: String,
    pub host_value: String,
    pub category: HazardCategory,
    pub sanitized: bool,
}

/// Structured report from inspecting how a `ResolvedContext` interacts with the host environment.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnvInspectionReport {
    pub clean_room_enabled: bool,
    pub sanitized_leaks: Vec<LeakedVariable>,
    pub active_leaks: Vec<LeakedVariable>,
    pub package_defined_vars: BTreeMap<String, String>,
    pub inherited_os_vars_count: usize,
}

/// Curated table of environment variables that frequently cause subtle studio/DCC crashes
/// when leaked from the parent shell into a Rez environment.
const HAZARD_VARIABLES: &[(&str, HazardCategory)] = &[
    // Python runtime hazards
    ("PYTHONPATH", HazardCategory::PythonRuntime),
    ("PYTHONHOME", HazardCategory::PythonRuntime),
    ("PYTHONSTARTUP", HazardCategory::PythonRuntime),
    ("PYTHONUSERBASE", HazardCategory::PythonRuntime),
    ("VIRTUAL_ENV", HazardCategory::PythonRuntime),
    ("CONDA_PREFIX", HazardCategory::PythonRuntime),
    ("CONDA_DEFAULT_ENV", HazardCategory::PythonRuntime),
    ("PIP_TARGET", HazardCategory::PythonRuntime),
    // Dynamic linker hazards
    ("LD_LIBRARY_PATH", HazardCategory::DynamicLinker),
    ("LD_PRELOAD", HazardCategory::DynamicLinker),
    ("LD_AUDIT", HazardCategory::DynamicLinker),
    ("LD_RUN_PATH", HazardCategory::DynamicLinker),
    ("DYLD_LIBRARY_PATH", HazardCategory::DynamicLinker),
    ("DYLD_FALLBACK_LIBRARY_PATH", HazardCategory::DynamicLinker),
    ("DYLD_INSERT_LIBRARIES", HazardCategory::DynamicLinker),
    ("DYLD_FRAMEWORK_PATH", HazardCategory::DynamicLinker),
    // Qt / UI hazards
    ("QT_PLUGIN_PATH", HazardCategory::QtAndGui),
    ("QT_QPA_PLATFORM_PLUGIN_PATH", HazardCategory::QtAndGui),
    ("QML_IMPORT_PATH", HazardCategory::QtAndGui),
    ("QML2_IMPORT_PATH", HazardCategory::QtAndGui),
    ("PYSIDE_DESIGNER_PLUGINS", HazardCategory::QtAndGui),
    // USD / Hydra / MaterialX hazards
    ("PXR_PLUGINPATH_NAME", HazardCategory::UsdAndHydra),
    ("FNPXR_PLUGINPATH", HazardCategory::UsdAndHydra),
    ("PXR_MTLX_STDLIB_SEARCH_PATHS", HazardCategory::UsdAndHydra),
    ("PXR_USD_WINDOWS_DLL_PATH", HazardCategory::UsdAndHydra),
    // Color Management (OCIO)
    ("OCIO", HazardCategory::ColorManagement),
    ("OCIO_ACTIVE_DISPLAYS", HazardCategory::ColorManagement),
    ("OCIO_ACTIVE_VIEWS", HazardCategory::ColorManagement),
    // DCC plugin and script search paths
    ("MAYA_PLUG_IN_PATH", HazardCategory::DccPlugins),
    ("MAYA_MODULE_PATH", HazardCategory::DccPlugins),
    ("MAYA_SCRIPT_PATH", HazardCategory::DccPlugins),
    ("XBMLANGPATH", HazardCategory::DccPlugins),
    ("HOUDINI_PATH", HazardCategory::DccPlugins),
    ("HOUDINI_DSO_PATH", HazardCategory::DccPlugins),
    ("HOUDINI_OTLSCAN_PATH", HazardCategory::DccPlugins),
    ("NUKE_PATH", HazardCategory::DccPlugins),
    ("OFX_PLUGIN_PATH", HazardCategory::DccPlugins),
    ("KATANA_RESOURCES", HazardCategory::DccPlugins),
    ("ARNOLD_PLUGIN_PATH", HazardCategory::DccPlugins),
    ("RV_SUPPORT_PATH", HazardCategory::DccPlugins),
    ("BLENDER_USER_SCRIPTS", HazardCategory::DccPlugins),
    ("BLENDER_SYSTEM_SCRIPTS", HazardCategory::DccPlugins),
    // Compiler / build flags
    ("CMAKE_PREFIX_PATH", HazardCategory::CompilerAndBuild),
    ("PKG_CONFIG_PATH", HazardCategory::CompilerAndBuild),
    ("CPATH", HazardCategory::CompilerAndBuild),
    ("C_INCLUDE_PATH", HazardCategory::CompilerAndBuild),
    ("CPLUS_INCLUDE_PATH", HazardCategory::CompilerAndBuild),
    ("LIBRARY_PATH", HazardCategory::CompilerAndBuild),
];

/// Classify whether `var_name` is a known studio/runtime hazard variable.
pub fn classify_hazard_variable(var_name: &str) -> Option<HazardCategory> {
    HAZARD_VARIABLES
        .iter()
        .find(|(name, _)| var_name.eq_ignore_ascii_case(name))
        .map(|(_, category)| *category)
}

/// Surgically sanitize a host environment map for Clean-Room execution:
/// - Strips all known `HAZARD_VARIABLES` unless explicitly present in `allowlist`.
/// - Resets `PATH` to the clean OS system `PATH` (`crate::environment::system_paths`)
///   unless `"PATH"` is explicitly in `allowlist`.
/// - Preserves desktop/display/session/identity variables (`DISPLAY`, `WAYLAND_DISPLAY`,
///   `SSH_AUTH_SOCK`, `USERPROFILE`, `APPDATA`, `SYSTEMROOT`, etc.).
pub fn sanitize_host_environ(
    platform: Platform,
    host_env: &HashMap<String, String>,
    allowlist: &[String],
) -> (HashMap<String, String>, Vec<LeakedVariable>) {
    let mut sanitized = HashMap::new();
    let mut leaks = Vec::new();
    let is_windows = platform == Platform::Windows;

    let is_allowed = |key: &str| -> bool {
        allowlist.iter().any(|a| {
            if is_windows {
                a.eq_ignore_ascii_case(key)
            } else {
                a == key
            }
        })
    };

    for (key, val) in host_env {
        if key.eq_ignore_ascii_case("PATH") && !is_allowed(key) {
            continue;
        }
        if let Some(category) = classify_hazard_variable(key) {
            if !val.is_empty() && !is_allowed(key) {
                leaks.push(LeakedVariable {
                    name: key.clone(),
                    host_value: val.clone(),
                    category,
                    sanitized: true,
                });
                continue;
            }
        }
        let norm_key = if is_windows {
            key.to_ascii_uppercase()
        } else {
            key.clone()
        };
        sanitized.insert(norm_key, val.clone());
    }

    if !is_allowed("PATH") {
        let sep = if is_windows { ";" } else { ":" };
        let sys_path = crate::environment::system_paths(platform, host_env).join(sep);
        sanitized.insert("PATH".to_string(), sys_path);
    }

    leaks.sort_by(|a, b| a.name.cmp(&b.name));
    (sanitized, leaks)
}

/// Detect any known hazard variables in `host_env` that are NOT sanitized.
pub fn detect_unsanitized_leaks(
    host_env: &HashMap<String, String>,
    effective_parent_env: &HashMap<String, String>,
) -> Vec<LeakedVariable> {
    let mut leaks = Vec::new();
    for (key, val) in host_env {
        if val.is_empty() {
            continue;
        }
        if let Some(category) = classify_hazard_variable(key) {
            let survived = effective_parent_env
                .iter()
                .any(|(ek, ev)| ek.eq_ignore_ascii_case(key) && !ev.is_empty());
            if survived {
                leaks.push(LeakedVariable {
                    name: key.clone(),
                    host_value: val.clone(),
                    category,
                    sanitized: false,
                });
            }
        }
    }
    leaks.sort_by(|a, b| a.name.cmp(&b.name));
    leaks
}

impl EnvInspectionReport {
    /// Format a studio-friendly report of sanitized leaks, active warnings, and package variables.
    pub fn format_human(&self) -> String {
        let mut lines = Vec::new();
        lines.push(
            "================================================================================"
                .to_string(),
        );
        lines.push(format!(
            "Rez Environment Isolation & Leak Inspection (clean_room: {})",
            self.clean_room_enabled
        ));
        lines.push(
            "================================================================================"
                .to_string(),
        );

        if !self.sanitized_leaks.is_empty() {
            lines.push(format!(
                "\n[Sanitized Host Leaks ({}) — stripped before context execution]:",
                self.sanitized_leaks.len()
            ));
            for leak in &self.sanitized_leaks {
                lines.push(format!(
                    "  [STRIPPED] {:<28} ({}) = {}",
                    leak.name,
                    leak.category.label(),
                    leak.host_value
                ));
            }
        } else {
            lines.push("\n[Sanitized Host Leaks]: none".to_string());
        }

        if !self.active_leaks.is_empty() {
            lines.push(format!(
                "\n[WARNING — Active Host Leaks ({}) inherited into context]:",
                self.active_leaks.len()
            ));
            for leak in &self.active_leaks {
                lines.push(format!(
                    "  [LEAKED]   {:<28} ({}) = {}",
                    leak.name,
                    leak.category.label(),
                    leak.host_value
                ));
            }
            lines.push(
                "  Hint: Pass `--clean-room` to `rez env` to automatically strip hazardous host variables."
                    .to_string(),
            );
        } else {
            lines.push(
                "\n[Active Host Leaks]: 0 (environment is free of known host runtime leaks)"
                    .to_string(),
            );
        }

        lines.push(format!(
            "\n[Package-Defined / Modified Variables ({})]:",
            self.package_defined_vars.len()
        ));
        for (k, v) in &self.package_defined_vars {
            lines.push(format!("  {k} = {v}"));
        }
        lines.push(
            "================================================================================"
                .to_string(),
        );
        lines.join("\n")
    }
}

// =============================================================================
// Virtual Merged Environment Directory Materializer
// =============================================================================

/// Strategy used to materialize files in a virtual merged environment directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkStrategy {
    /// Try native symlink -> hardlink -> file copy fallback automatically.
    Auto,
    /// Use symbolic links only.
    Symlink,
    /// Use hard links only (with copy fallback if cross-device).
    Hardlink,
    /// Copy all files.
    Copy,
}

impl LinkStrategy {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "symlink" | "sym" => Some(Self::Symlink),
            "hardlink" | "hard" => Some(Self::Hardlink),
            "copy" => Some(Self::Copy),
            _ => None,
        }
    }
}

/// Default package subdirectories merged when no explicit list is provided.
pub const DEFAULT_MERGED_SUBDIRS: &[&str] = &[
    "bin",
    "lib",
    "lib64",
    "python",
    "plugins",
    "include",
    "cmake",
    "share",
    "resources",
];

/// Provenance record for a single file in a virtual merged directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergedFileEntry {
    pub relative_path: String,
    pub package_name: String,
    pub package_version: String,
    pub source_path: String,
    pub link_method: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overridden_packages: Vec<String>,
}

/// Manifest written to `rez-merged-manifest.json` in a virtual merged directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergedDirManifest {
    pub schema_version: u32,
    pub target_directory: String,
    pub merged_subdirs: Vec<String>,
    pub packages: Vec<String>,
    pub total_files: usize,
    pub conflicts_overridden: usize,
    pub files: BTreeMap<String, MergedFileEntry>,
}

/// Materialize a unified virtual merged directory from `packages` into `dest_dir`.
///
/// Packages are applied in resolution order; when multiple packages provide the
/// same relative file path under a merged subdirectory, later packages override
/// earlier ones and the overridden package names are recorded in `overridden_packages`.
pub fn materialize_merged_directory(
    packages: &[ResolvedPackageInfo],
    dest_dir: &Path,
    subdirs: &[String],
    strategy: LinkStrategy,
) -> Result<MergedDirManifest> {
    std::fs::create_dir_all(dest_dir)?;

    let active_subdirs: Vec<String> = if subdirs.is_empty() {
        DEFAULT_MERGED_SUBDIRS
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    } else {
        subdirs
            .iter()
            .map(|s| s.trim_matches(['/', '\\']).to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };

    let mut files_map: BTreeMap<String, MergedFileEntry> = BTreeMap::new();
    let mut conflicts_overridden = 0usize;
    let mut package_names = Vec::new();

    for pkg in packages {
        package_names.push(pkg.qualified_name());
        let Some(pkg_root) = pkg.root.as_deref().or(pkg.repo_path.as_deref()) else {
            continue;
        };
        if !pkg_root.is_dir() {
            continue;
        }

        let merge_entire_root = active_subdirs.iter().any(|s| s == "." || s == "*");
        if merge_entire_root {
            merge_tree_recursive(
                pkg_root,
                pkg_root,
                dest_dir,
                pkg,
                strategy,
                &mut files_map,
                &mut conflicts_overridden,
            )?;
        } else {
            for sub in &active_subdirs {
                let src_sub = pkg_root.join(sub);
                if src_sub.is_dir() {
                    merge_tree_recursive(
                        pkg_root,
                        &src_sub,
                        dest_dir,
                        pkg,
                        strategy,
                        &mut files_map,
                        &mut conflicts_overridden,
                    )?;
                }
            }
        }
    }

    let manifest = MergedDirManifest {
        schema_version: 1,
        target_directory: dest_dir.display().to_string(),
        merged_subdirs: active_subdirs,
        packages: package_names,
        total_files: files_map.len(),
        conflicts_overridden,
        files: files_map,
    };

    let manifest_path = dest_dir.join("rez-merged-manifest.json");
    let json = serde_json::to_string_pretty(&manifest)?;
    crate::serialise::atomic_write(&manifest_path, json)?;

    Ok(manifest)
}

fn merge_tree_recursive(
    pkg_root: &Path,
    current_src: &Path,
    dest_root: &Path,
    pkg: &ResolvedPackageInfo,
    strategy: LinkStrategy,
    files_map: &mut BTreeMap<String, MergedFileEntry>,
    conflicts_overridden: &mut usize,
) -> Result<()> {
    let mut entries: Vec<_> =
        std::fs::read_dir(current_src)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let src_path = entry.path();
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();

        // Skip package definition metadata and internal .rez directories when merging root
        if current_src == pkg_root
            && matches!(
                name_str.as_ref(),
                "package.py" | "package.yaml" | "package.toml" | "package.yml" | ".rez"
            )
        {
            continue;
        }

        let meta = std::fs::symlink_metadata(&src_path)?;
        if meta.is_dir() {
            merge_tree_recursive(
                pkg_root,
                &src_path,
                dest_root,
                pkg,
                strategy,
                files_map,
                conflicts_overridden,
            )?;
        } else {
            let rel = src_path.strip_prefix(pkg_root).map_err(|e| {
                RezError::ResolvedContext(format!("failed to strip package root prefix: {e}"))
            })?;
            let rel_slash = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            let dst_path = dest_root.join(rel);
            if let Some(parent) = dst_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            // Remove existing file/symlink at dst_path if overriding an earlier package
            if dst_path.exists() || std::fs::symlink_metadata(&dst_path).is_ok() {
                let _ = std::fs::remove_file(&dst_path);
            }

            let method = link_or_copy_file(&src_path, &dst_path, strategy)?;

            let mut overridden = Vec::new();
            if let Some(prev) = files_map.remove(&rel_slash) {
                *conflicts_overridden += 1;
                overridden = prev.overridden_packages;
                overridden.push(format!("{}-{}", prev.package_name, prev.package_version));
            }

            files_map.insert(
                rel_slash.clone(),
                MergedFileEntry {
                    relative_path: rel_slash,
                    package_name: pkg.name.clone(),
                    package_version: pkg.version.to_string(),
                    source_path: src_path.display().to_string(),
                    link_method: method.to_string(),
                    overridden_packages: overridden,
                },
            );
        }
    }

    Ok(())
}

fn link_or_copy_file(src: &Path, dst: &Path, strategy: LinkStrategy) -> Result<&'static str> {
    match strategy {
        LinkStrategy::Symlink => {
            try_symlink_file(src, dst)?;
            Ok("symlink")
        }
        LinkStrategy::Hardlink => {
            if std::fs::hard_link(src, dst).is_ok() {
                Ok("hardlink")
            } else {
                std::fs::copy(src, dst)?;
                Ok("copy")
            }
        }
        LinkStrategy::Copy => {
            std::fs::copy(src, dst)?;
            Ok("copy")
        }
        LinkStrategy::Auto => {
            if try_symlink_file(src, dst).is_ok() {
                Ok("symlink")
            } else if std::fs::hard_link(src, dst).is_ok() {
                Ok("hardlink")
            } else {
                std::fs::copy(src, dst)?;
                Ok("copy")
            }
        }
    }
}

fn try_symlink_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(src, dst)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(src, dst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use version::Version;

    #[test]
    fn test_sanitize_host_environ_strips_hazards_and_preserves_session() {
        let mut host = HashMap::new();
        host.insert(
            "PYTHONPATH".to_string(),
            "/polluted/site-packages".to_string(),
        );
        host.insert("LD_LIBRARY_PATH".to_string(), "/polluted/lib".to_string());
        host.insert(
            "MAYA_PLUG_IN_PATH".to_string(),
            "/polluted/maya".to_string(),
        );
        host.insert("OCIO".to_string(), "/polluted/config.ocio".to_string());
        host.insert("DISPLAY".to_string(), ":0".to_string());
        host.insert("USER".to_string(), "artist".to_string());
        host.insert("PATH".to_string(), "/polluted/bin:/usr/bin".to_string());

        let (clean, leaks) = sanitize_host_environ(
            Platform::Linux,
            &host,
            &["OCIO".to_string()], // explicitly allow OCIO
        );

        assert!(!clean.contains_key("PYTHONPATH"));
        assert!(!clean.contains_key("LD_LIBRARY_PATH"));
        assert!(!clean.contains_key("MAYA_PLUG_IN_PATH"));
        // OCIO was allowlisted
        assert_eq!(
            clean.get("OCIO").map(String::as_str),
            Some("/polluted/config.ocio")
        );
        // Session variable preserved
        assert_eq!(clean.get("DISPLAY").map(String::as_str), Some(":0"));
        // PATH reset to system default
        assert!(!clean["PATH"].contains("/polluted/bin"));
        assert_eq!(leaks.len(), 3);
    }

    #[test]
    fn test_virtual_merged_directory_materialization_and_provenance() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg_a_root = tmp.path().join("pkg_a");
        let pkg_b_root = tmp.path().join("pkg_b");
        let merged_root = tmp.path().join("merged");

        std::fs::create_dir_all(pkg_a_root.join("plugins")).unwrap();
        std::fs::create_dir_all(pkg_a_root.join("python")).unwrap();
        std::fs::write(pkg_a_root.join("plugins/a_plugin.so"), b"a_plugin_v1").unwrap();
        std::fs::write(pkg_a_root.join("plugins/shared.so"), b"from_a").unwrap();
        std::fs::write(pkg_a_root.join("python/mod_a.py"), b"x = 1").unwrap();

        std::fs::create_dir_all(pkg_b_root.join("plugins")).unwrap();
        std::fs::write(pkg_b_root.join("plugins/b_plugin.so"), b"b_plugin_v2").unwrap();
        std::fs::write(pkg_b_root.join("plugins/shared.so"), b"from_b").unwrap();

        let packages = vec![
            ResolvedPackageInfo {
                name: "pkg_a".to_string(),
                version: Version::new("1.0.0").unwrap(),
                variant_index: None,
                resource_handle: None,
                requires: Vec::new(),
                repo_path: Some(pkg_a_root.clone()),
                root: Some(pkg_a_root),
                commands: None,
                pre_commands: None,
                post_commands: None,
            },
            ResolvedPackageInfo {
                name: "pkg_b".to_string(),
                version: Version::new("2.0.0").unwrap(),
                variant_index: None,
                resource_handle: None,
                requires: Vec::new(),
                repo_path: Some(pkg_b_root.clone()),
                root: Some(pkg_b_root),
                commands: None,
                pre_commands: None,
                post_commands: None,
            },
        ];

        let manifest =
            materialize_merged_directory(&packages, &merged_root, &[], LinkStrategy::Auto).unwrap();

        assert_eq!(manifest.total_files, 4);
        assert_eq!(manifest.conflicts_overridden, 1);
        assert_eq!(
            std::fs::read_to_string(merged_root.join("plugins/shared.so")).unwrap(),
            "from_b"
        );
        let shared_entry = &manifest.files["plugins/shared.so"];
        assert_eq!(shared_entry.package_name, "pkg_b");
        assert_eq!(shared_entry.overridden_packages, vec!["pkg_a-1.0.0"]);
        assert!(merged_root.join("rez-merged-manifest.json").is_file());
    }
}
