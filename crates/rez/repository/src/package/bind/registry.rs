// SPDX-License-Identifier: Apache-2.0

//! Registry of bind detectors — system, tools, DCC. Replaces match-by-name with a unified lookup.

use super::dcc::{detect_dcc, ALL_APPS};
use super::detect::{AppDef, BindDef};
use super::BindInfo;
use super::BindOptions;
use crate::errors::{Result, RezError};

/// Registry entry for system packages and tools. DCC apps use ALL_APPS.
struct RegistryEntry {
    name: &'static str,
    detect: fn(&BindOptions) -> Result<Vec<BindInfo>>,
}

fn detect_platform(_: &BindOptions) -> Result<Vec<BindInfo>> {
    Ok(vec![super::detect_platform()])
}
fn detect_arch(_: &BindOptions) -> Result<Vec<BindInfo>> {
    Ok(vec![super::detect_arch()])
}
fn detect_os(_: &BindOptions) -> Result<Vec<BindInfo>> {
    Ok(vec![super::detect_os()])
}
fn detect_rez(_: &BindOptions) -> Result<Vec<BindInfo>> {
    Ok(vec![super::detect_rez()])
}
fn detect_python_reg(opts: &BindOptions) -> Result<Vec<BindInfo>> {
    super::tools::detect_python_with_options(opts.python_copy)
        .map(|i| vec![i])
        .ok_or_else(|| RezError::Bind("Python not found".into()))
}
fn detect_pip_reg(opts: &BindOptions) -> Result<Vec<BindInfo>> {
    super::tools::detect_pip_with_options(opts)
        .map(|i| vec![i])
        .ok_or_else(|| RezError::Bind("pip not found".into()))
}
fn detect_setuptools_reg(_: &BindOptions) -> Result<Vec<BindInfo>> {
    super::detect_setuptools()
        .map(|i| vec![i])
        .ok_or_else(|| RezError::Bind("setuptools not found".into()))
}
fn detect_cmake_reg(_: &BindOptions) -> Result<Vec<BindInfo>> {
    super::detect_cmake()
        .map(|i| vec![i])
        .ok_or_else(|| RezError::Bind("CMake not found".into()))
}
fn detect_gcc_reg(_: &BindOptions) -> Result<Vec<BindInfo>> {
    super::detect_gcc()
        .map(|i| vec![i])
        .ok_or_else(|| RezError::Bind("GCC not found".into()))
}
fn detect_docker_reg(_: &BindOptions) -> Result<Vec<BindInfo>> {
    super::detect_docker()
        .map(|i| vec![i])
        .ok_or_else(|| RezError::Bind("Docker not found".into()))
}
fn detect_perforce_reg(_: &BindOptions) -> Result<Vec<BindInfo>> {
    super::detect_perforce()
        .map(|i| vec![i])
        .ok_or_else(|| RezError::Bind("Perforce (p4) not found".into()))
}
fn detect_gh_reg(_: &BindOptions) -> Result<Vec<BindInfo>> {
    super::detect_gh()
        .map(|i| vec![i])
        .ok_or_else(|| RezError::Bind("GitHub CLI (gh) not found".into()))
}
fn detect_git_reg(_: &BindOptions) -> Result<Vec<BindInfo>> {
    super::detect_git()
        .map(|i| vec![i])
        .ok_or_else(|| RezError::Bind("Git not found".into()))
}

const REGISTRY: &[RegistryEntry] = &[
    RegistryEntry {
        name: "platform",
        detect: detect_platform,
    },
    RegistryEntry {
        name: "arch",
        detect: detect_arch,
    },
    RegistryEntry {
        name: "os",
        detect: detect_os,
    },
    RegistryEntry {
        name: "rez",
        detect: detect_rez,
    },
    RegistryEntry {
        name: "python",
        detect: detect_python_reg,
    },
    RegistryEntry {
        name: "pip",
        detect: detect_pip_reg,
    },
    RegistryEntry {
        name: "setuptools",
        detect: detect_setuptools_reg,
    },
    RegistryEntry {
        name: "cmake",
        detect: detect_cmake_reg,
    },
    RegistryEntry {
        name: "gcc",
        detect: detect_gcc_reg,
    },
    RegistryEntry {
        name: "docker",
        detect: detect_docker_reg,
    },
    RegistryEntry {
        name: "perforce",
        detect: detect_perforce_reg,
    },
    RegistryEntry {
        name: "gh",
        detect: detect_gh_reg,
    },
    RegistryEntry {
        name: "git",
        detect: detect_git_reg,
    },
];

/// Built-in module names from registry (system + tools).
pub fn builtin_module_names() -> Vec<&'static str> {
    REGISTRY.iter().map(|e| e.name).collect()
}

/// Detect by name using registry + DCC.
pub fn detect_by_name_registry(name: &str, options: &BindOptions) -> Result<Vec<BindInfo>> {
    crate::log_info!("bind", "detecting module: {}", name);
    crate::log_debug!("bind", "checking registry + DCC for {}", name);
    for entry in REGISTRY {
        if entry.name == name {
            crate::log_trace!("bind", "  -> registry (system/tool)");
            return (entry.detect)(options)
                .inspect(|v| {
                    crate::log_info!("bind", "detected {}: {} version(s)", name, v.len());
                })
                .map_err(|e| {
                    crate::log_debug!("bind", "{} not found: {}", name, e);
                    e
                });
        }
    }
    for def in ALL_APPS {
        if def.name() == name {
            crate::log_trace!("bind", "  -> DCC app");
            return detect_dcc::<AppDef>(def)
                .inspect(|v| {
                    crate::log_info!(
                        "bind",
                        "detected {}: {} version(s): {:?}",
                        name,
                        v.len(),
                        v.iter().map(|i| i.version.as_str()).collect::<Vec<_>>()
                    );
                })
                .ok_or_else(|| {
                    let e = RezError::Bind(format!("{} not found", def.display()));
                    crate::log_debug!("bind", "{} not found", name);
                    e
                });
        }
    }
    crate::log_debug!("bind", "unknown module: {}", name);
    Err(RezError::Bind(format!("Unknown bind module: '{}'", name)))
}
