// SPDX-License-Identifier: Apache-2.0

//! Declarative app detection engine.
//!
//! Define apps as `const AppDef` — the generic `detect_app()` handles
//! platform dispatch, version extraction, and BindInfo assembly.

use std::fs;
use std::path::{Path, PathBuf};

use super::util::{
    app_cmds, find_all_apps_min_version, find_executable, find_latest_app_min_version,
    run_version_cmd,
};
use super::BindInfo;
use crate::platform::SYSTEM;

// ---------------------------------------------------------------------------
// BindDef trait — unified detection API
// ---------------------------------------------------------------------------

/// Trait for bind-capable app definitions. Encapsulates single vs multi-version detection.
pub trait BindDef {
    /// Rez package name (e.g. "maya").
    fn name(&self) -> &str;
    /// Human-readable display name (e.g. "Autodesk Maya").
    fn display(&self) -> &str;
    /// Detect bindable versions. For multi-version apps (e.g. Maya) returns all; for others at most one.
    fn detect_versions(&self) -> Vec<BindInfo>;
}

impl BindDef for AppDef {
    fn name(&self) -> &str {
        self.name
    }
    fn display(&self) -> &str {
        self.display
    }
    fn detect_versions(&self) -> Vec<BindInfo> {
        if self.bind_all_versions {
            detect_app_all(self)
        } else if self.try_command_first {
            detect_app_or_cmd(self).into_iter().collect()
        } else {
            detect_app(self).into_iter().collect()
        }
    }
}

// ---------------------------------------------------------------------------
// Declarative types
// ---------------------------------------------------------------------------

/// Per-platform directory scan parameters: (base_dir, name_prefix).
pub type ScanPath = (&'static str, &'static str);

/// How to locate the application install directory.
#[derive(Debug, Clone)]
pub enum SearchMethod {
    /// Scan directories matching prefix. Multiple paths per platform — all are searched, results merged.
    /// `min_version`: only consider dirs with version >= this (optional filter, e.g. Maya 2022+).
    Scan {
        windows: &'static [ScanPath],
        macos: &'static [ScanPath],
        linux: &'static [ScanPath],
        min_version: Option<&'static str>,
    },
    /// Fixed path per platform (no version in dir name).
    Fixed {
        windows: Option<&'static str>,
        macos: Option<&'static str>,
        linux: Option<&'static str>,
    },
}

/// How to determine the binary subdirectory.
#[derive(Debug, Clone, Copy)]
pub enum BinSub {
    /// Same subdirectory on all platforms.
    Same(&'static str),
    /// Different on macOS vs others (default = Windows + Linux).
    PerPlatform {
        default: &'static str,
        macos: &'static str,
    },
    /// All three platforms (for Unreal etc.: Win64, Linux, Mac).
    FullPlatform {
        windows: &'static str,
        macos: &'static str,
        linux: &'static str,
    },
}

/// How to extract the application version.
#[derive(Debug, Clone, Copy)]
pub enum VersionDetect {
    /// From directory name (default — comes from `find_latest_app` result).
    DirName,
    /// Run a command, strip prefix from first line.
    Command {
        cmd: &'static str,
        args: &'static [&'static str],
        prefix: &'static str,
    },
    /// Read version from a text file relative to install dir.
    TextFile(&'static [&'static str]),
    /// Extract version from XML tag in a file relative to install dir.
    XmlTag {
        file: &'static str,
        open_tag: &'static str,
    },
}

/// Declarative application definition for bind detection.
#[derive(Debug, Clone)]
pub struct AppDef {
    /// Rez package name (e.g. "maya").
    pub name: &'static str,
    /// Human-readable display name (e.g. "Autodesk Maya").
    pub display: &'static str,
    /// Tool executables this package provides.
    pub tools: &'static [&'static str],
    /// How to find the install directory.
    pub search: SearchMethod,
    /// Subdirectory containing binaries.
    pub bin_sub: BinSub,
    /// Environment variables to set: (KEY, subdir_relative_to_install).
    pub env_vars: &'static [(&'static str, &'static str)],
    /// How to detect the version.
    pub version: VersionDetect,
    /// If true, bind all detected versions (any app: maya, blender, houdini, etc.).
    pub bind_all_versions: bool,
    /// If true, try VersionDetect::Command first (e.g. `blender --version`), fallback to dir scan.
    pub try_command_first: bool,
    /// Env vars to append (not set): (KEY, subdir). For Maya modules: ("MAYA_MODULE_PATH", "lib/maya/modules").
    pub env_append: &'static [(&'static str, &'static str)],
    /// Extra dirs to prepend to PATH (relative to install). For Houdini: ["houdini/sbin", "dsolib"].
    pub path_prepend_dirs: &'static [&'static str],
    /// Env vars to prepend: (KEY, subdir). For Houdini on Linux: ("LD_LIBRARY_PATH", "dsolib").
    pub env_prepend: &'static [(&'static str, &'static str)],
}

// ---------------------------------------------------------------------------
// Generic detection engine
// ---------------------------------------------------------------------------

/// Resolve binary subdirectory for current platform.
fn resolve_bin_sub(def: &AppDef) -> &'static str {
    match def.bin_sub {
        BinSub::Same(s) => s,
        BinSub::PerPlatform { default, macos } => {
            if cfg!(target_os = "macos") {
                macos
            } else {
                default
            }
        }
        BinSub::FullPlatform {
            windows,
            macos,
            linux,
        } => {
            if cfg!(windows) {
                windows
            } else if cfg!(target_os = "macos") {
                macos
            } else {
                linux
            }
        }
    }
}

/// Detect all versions of an app when bind_all_versions is true.
pub fn detect_app_all(def: &AppDef) -> Vec<BindInfo> {
    if !def.bind_all_versions {
        return Vec::new();
    }
    let installs = find_install_all(def);
    let bin_sub = resolve_bin_sub(def);
    installs
        .into_iter()
        .map(|(ver, install)| {
            let mut info = BindInfo::new(def.name, &ver);
            info.description = format!("{} {}", def.display, ver);
            info.tools = def.tools.iter().map(|s| s.to_string()).collect();
            info.variants = vec![SYSTEM.variant()];
            info.commands = Some(app_cmds(
                &install,
                bin_sub,
                def.env_vars,
                def.env_append,
                def.path_prepend_dirs,
                def.env_prepend,
            ));
            info
        })
        .collect()
}

/// Detect an application from its declarative definition.
pub fn detect_app(def: &AppDef) -> Option<BindInfo> {
    let (ver, install) = find_install(def)?;
    let bin_sub = resolve_bin_sub(def);

    let mut info = BindInfo::new(def.name, &ver);
    info.description = format!("{} {}", def.display, ver);
    info.tools = def.tools.iter().map(|s| s.to_string()).collect();
    info.variants = vec![SYSTEM.variant()];
    info.commands = Some(app_cmds(
        &install,
        bin_sub,
        def.env_vars,
        def.env_append,
        def.path_prepend_dirs,
        def.env_prepend,
    ));
    Some(info)
}

/// Locate all install dirs (Scan: all paths per platform, merge, dedupe by version).
fn find_install_all(def: &AppDef) -> Vec<(String, PathBuf)> {
    match &def.search {
        SearchMethod::Scan {
            windows,
            macos,
            linux,
            min_version,
        } => {
            let paths = if cfg!(windows) {
                windows
            } else if cfg!(target_os = "macos") {
                macos
            } else {
                linux
            };
            crate::log_info!(
                "bind",
                "detecting {}: {} search path(s)",
                def.name,
                paths.len()
            );
            crate::log_debug!(
                "bind",
                "paths: {:?}",
                paths
                    .iter()
                    .map(|(b, p)| format!("{}/{}", b, p))
                    .collect::<Vec<_>>()
            );
            let mut seen = std::collections::HashSet::new();
            let mut results = Vec::new();
            for (base, prefix) in *paths {
                crate::log_trace!("bind", "  trying base={} prefix={}", base, prefix);
                for (ver_from_dir, install) in
                    find_all_apps_min_version(Path::new(base), prefix, *min_version)
                {
                    // Command uses system exe (no install) — keep dir name. TextFile/XmlTag use install.
                    let ver = match def.version {
                        VersionDetect::DirName | VersionDetect::Command { .. } => ver_from_dir,
                        _ => extract_version(def, &install).unwrap_or(ver_from_dir),
                    };
                    if seen.insert(ver.clone()) {
                        results.push((ver, install));
                    }
                }
            }
            results.sort_by(|a, b| b.0.cmp(&a.0));
            crate::log_info!(
                "bind",
                "found {} version(s) for {}: {:?}",
                results.len(),
                def.name,
                results.iter().map(|(v, _)| v.as_str()).collect::<Vec<_>>()
            );
            results
        }
        _ => Vec::new(),
    }
}

/// Locate install dir and extract version (single, latest).
fn find_install(def: &AppDef) -> Option<(String, PathBuf)> {
    match &def.search {
        SearchMethod::Scan {
            windows,
            macos,
            linux,
            min_version,
        } => {
            let paths = if cfg!(windows) {
                windows
            } else if cfg!(target_os = "macos") {
                macos
            } else {
                linux
            };
            crate::log_trace!(
                "bind",
                "find_install {}: trying {} path(s)",
                def.name,
                paths.len()
            );
            let mut best: Option<(String, PathBuf)> = None;
            for (base, prefix) in *paths {
                crate::log_trace!("bind", "  find_install {}: {}/{}", def.name, base, prefix);
                if let Some((ver, install)) =
                    find_latest_app_min_version(Path::new(base), prefix, *min_version)
                {
                    if best.as_ref().map(|b| &b.0).is_none_or(|v| ver > *v) {
                        best = Some((ver, install));
                    }
                }
            }
            let (ver, install) = best?;
            let ver = match def.version {
                VersionDetect::DirName => ver,
                _ => extract_version(def, &install).unwrap_or(ver),
            };
            Some((ver, install))
        }
        SearchMethod::Fixed {
            windows,
            macos,
            linux,
        } => {
            let install = fixed_platform(*windows, *macos, *linux)?;
            let ver = extract_version(def, &install).unwrap_or_else(|| "latest".into());
            Some((ver, install))
        }
    }
}

/// Infer install root from exe dir: exe is at install/bin_sub/exe_name, so go up N levels.
fn infer_install_from_exe(exe_dir: &Path, bin_sub: &str) -> PathBuf {
    let depth = if bin_sub.is_empty() {
        0
    } else {
        bin_sub.matches(['/', '\\']).count() + 1
    };
    (0..depth).fold(exe_dir.to_path_buf(), |p, _| {
        p.parent().unwrap_or(&p).to_path_buf()
    })
}

/// Try platform-specific fixed paths.
fn fixed_platform(
    windows: Option<&str>,
    macos: Option<&str>,
    linux: Option<&str>,
) -> Option<PathBuf> {
    let path_str = if cfg!(windows) {
        windows?
    } else if cfg!(target_os = "macos") {
        macos?
    } else {
        linux?
    };
    let p = PathBuf::from(path_str);
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

/// Extract version using the specified method.
fn extract_version(def: &AppDef, install: &Path) -> Option<String> {
    match def.version {
        VersionDetect::DirName => None, // handled by caller
        VersionDetect::Command { cmd, args, prefix } => {
            let output = run_version_cmd(cmd, args)?;
            let line = output.lines().next()?;
            Some(line.strip_prefix(prefix).unwrap_or(line).trim().to_string())
        }
        VersionDetect::TextFile(paths) => {
            for name in paths {
                let p = install.join(name);
                if let Ok(content) = fs::read_to_string(&p) {
                    let v = content.trim().to_string();
                    if !v.is_empty() {
                        return Some(v);
                    }
                }
            }
            None
        }
        VersionDetect::XmlTag { file, open_tag } => {
            let p = install.join(file);
            let content = fs::read_to_string(&p).ok()?;
            let start = content.find(open_tag)? + open_tag.len();
            let end = content[start..].find('<')? + start;
            Some(content[start..end].trim().to_string())
        }
    }
}

/// Detect app by command execution first, fallback to dir scan.
/// Used for apps like Blender that may be on PATH.
pub fn detect_app_or_cmd(def: &AppDef) -> Option<BindInfo> {
    // Try command-based detection first
    if let VersionDetect::Command { cmd, args, prefix } = def.version {
        if let Some(output) = run_version_cmd(cmd, args) {
            if let Some(line) = output.lines().next() {
                let ver = line.strip_prefix(prefix).unwrap_or(line).trim().to_string();
                if !ver.is_empty() {
                    let mut info = BindInfo::new(def.name, &ver);
                    info.description = format!("{} {}", def.display, ver);
                    info.tools = def.tools.iter().map(|s| s.to_string()).collect();
                    info.variants = vec![SYSTEM.variant()];

                    // Find install dir from executable location (exe is at install/bin_sub/cmd)
                    if let Some(exe) = find_executable(cmd) {
                        if let Some(exe_dir) = exe.parent() {
                            let bin_sub = resolve_bin_sub(def);
                            let install = infer_install_from_exe(exe_dir, bin_sub);
                            info.commands = Some(app_cmds(
                                install.as_path(),
                                bin_sub,
                                def.env_vars,
                                def.env_append,
                                def.path_prepend_dirs,
                                def.env_prepend,
                            ));
                        }
                    }
                    return Some(info);
                }
            }
        }
    }
    // Fallback to directory scan
    detect_app(def)
}
