// SPDX-License-Identifier: Apache-2.0

//! Detectors for development tools: python, pip, setuptools, cmake, gcc, rez.

use std::process::Command;

use super::util::{
    find_executable, find_python_module_dir, find_venv_pip_in_python_packages, path_append_cmd,
    path_for_commands, run_version_cmd, run_version_cmd_path,
};
use super::BindInfo;
use super::BindOptions;
use crate::platform::SYSTEM;

/// Detect Python and create a venv-based package.
pub fn detect_python() -> Option<BindInfo> {
    detect_python_with_options(false)
}

/// Detect Python with optional --copies (venv --copies for relocatable binaries).
pub fn detect_python_with_options(copy: bool) -> Option<BindInfo> {
    let version = run_version_cmd("python3", &["--version"])
        .or_else(|| run_version_cmd("python", &["--version"]))?;

    let ver = version.strip_prefix("Python ")?.trim().to_string();

    let mut info = BindInfo::new("python", &ver);
    info.description = format!("Python {}", ver);
    info.tools = vec!["python".to_string(), "pip".to_string()];
    info.variants = vec![SYSTEM.variant()];

    let exe_path = if cfg!(windows) {
        find_executable("python3.exe")
            .or_else(|| find_executable("python.exe"))
            .or_else(|| find_executable("python3"))
            .or_else(|| find_executable("python"))
    } else {
        find_executable("python3").or_else(|| find_executable("python"))
    };

    info.venv_source = exe_path;
    info.venv_copies = copy;

    let venv_bin = if cfg!(windows) {
        "venv/Scripts"
    } else {
        "venv/bin"
    };
    info.commands = Some(format!("env.PATH.append('{{this.root}}/{venv_bin}')"));

    Some(info)
}

/// Detect CMake installation via `cmake --version`. Path-only: appends system bin dir.
pub fn detect_cmake() -> Option<BindInfo> {
    let version = run_version_cmd("cmake", &["--version"])?;
    let first_line = version.lines().next()?;
    let ver = first_line
        .strip_prefix("cmake version ")?
        .trim()
        .to_string();

    let mut info = BindInfo::new("cmake", &ver);
    info.description = format!("CMake {}", ver);
    info.tools = vec!["cmake".to_string()];
    info.variants = vec![SYSTEM.variant()];

    if let Some(exe_path) = find_executable("cmake") {
        if let Some(bin_dir) = exe_path.parent() {
            info.commands = Some(path_append_cmd(bin_dir));
        }
    }

    Some(info)
}

/// Detect pip installation via `pip --version` or `pip3 --version`. Path-only: no copies.
pub fn detect_pip() -> Option<BindInfo> {
    detect_pip_with_options(&BindOptions::default())
}

/// Detect pip, preferring venv pip from python package when install_path is set in options.
pub fn detect_pip_with_options(opts: &BindOptions) -> Option<BindInfo> {
    if let Some(ref install_path) = opts.install_path {
        if let Some((venv_pip_path, python_ver)) = find_venv_pip_in_python_packages(install_path) {
            if let Some(version) = run_version_cmd_path(&venv_pip_path, &["--version"]) {
                if let Some(ver) = version.split_whitespace().nth(1) {
                    let py_req = format!("python-{}", python_ver);
                    let mut info = BindInfo::new("pip", ver);
                    info.description = format!("pip {}", ver);
                    info.variants = vec![{
                        let mut variant = super::system::system_variant(true);
                        variant.push(py_req);
                        variant
                    }];
                    info.tools = vec!["pip".to_string(), "pip3".to_string()];
                    info.commands = Some("env.PATH.append('{root}/bin')".to_string());
                    info.exe_bindings
                        .push(("pip".to_string(), venv_pip_path.clone()));
                    info.exe_bindings.push(("pip3".to_string(), venv_pip_path));
                    crate::log_info!(
                        "bind",
                        "pip from venv at {:?}",
                        info.exe_bindings.first().map(|(_, p)| p)
                    );
                    return Some(info);
                }
            }
        }
    }
    detect_pip_from_path()
}

/// Detect pip from system PATH (used when no venv pip is available).
fn detect_pip_from_path() -> Option<BindInfo> {
    let version = run_version_cmd("pip3", &["--version"])
        .or_else(|| run_version_cmd("pip", &["--version"]))?;

    let ver = version.split_whitespace().nth(1)?.to_string();
    let py_ver = version
        .rsplit("python ")
        .next()
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or("3")
        .to_string();
    let py_req = format!("python-{}", py_ver);

    let mut info = BindInfo::new("pip", &ver);
    info.description = format!("pip {}", ver);
    info.variants = vec![{
        let mut variant = super::system::system_variant(true);
        variant.push(py_req);
        variant
    }];

    let mut cmds = Vec::new();
    if let Some(mod_dir) = find_python_module_dir("pip") {
        cmds.push(format!(
            "env.PYTHONPATH.append('{}')",
            path_for_commands(&mod_dir)
        ));
    }
    if let Some(pip_exe) = find_executable("pip3").or_else(|| find_executable("pip")) {
        if let Some(scripts_dir) = pip_exe.parent() {
            cmds.push(path_append_cmd(scripts_dir));
        }
    }
    for tool in &["pip", "pip3"] {
        if find_executable(tool).is_some() {
            info.tools.push(tool.to_string());
        }
    }
    if info.tools.is_empty() && !cmds.is_empty() {
        info.tools.push("pip".to_string());
    }
    // Don't return a bind package without commands: path lookup would fail.
    if cmds.is_empty() {
        return None;
    }
    info.commands = Some(cmds.join("\n"));
    Some(info)
}

/// Detect setuptools via Python import.
pub fn detect_setuptools() -> Option<BindInfo> {
    let output = Command::new("python3")
        .args(["-c", "import setuptools; print(setuptools.__version__)"])
        .output()
        .ok()
        .or_else(|| {
            Command::new("python")
                .args(["-c", "import setuptools; print(setuptools.__version__)"])
                .output()
                .ok()
        })?;

    if !output.status.success() {
        return None;
    }

    let ver = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if ver.is_empty() {
        return None;
    }

    let mut info = BindInfo::new("setuptools", &ver);
    info.description = format!("setuptools {}", ver);
    Some(info)
}

/// Detect rez-rs itself using cargo version.
/// Copies the current rez binary into the bound package's bin/ (via exe_bindings).
pub fn detect_rez() -> BindInfo {
    let mut info = BindInfo::new("rez", env!("CARGO_PKG_VERSION"));
    info.description = format!("rez-rs {}", env!("CARGO_PKG_VERSION"));
    info.requires = vec!["python-3.8+<3.14".to_string()];
    info.tools = vec!["rez".to_string()];
    info.commands = Some("env.PATH.append('{root}/bin')".to_string());
    info.variants = vec![SYSTEM.variant()];

    if let Ok(exe_path) = std::env::current_exe() {
        if exe_path.exists() {
            info.exe_bindings.push(("rez".to_string(), exe_path));
        }
    }

    info
}

/// Detect Docker via `docker --version`. Path-only: appends system bin dir.
pub fn detect_docker() -> Option<BindInfo> {
    let version = run_version_cmd("docker", &["--version"])?;
    let ver = version
        .strip_prefix("Docker version ")?
        .trim()
        .split(',')
        .next()?
        .trim()
        .to_string();

    let mut info = BindInfo::new("docker", &ver);
    info.description = format!("Docker {}", ver);
    info.tools = vec!["docker".to_string(), "docker-compose".to_string()];
    info.variants = vec![SYSTEM.variant()];

    if let Some(exe) = find_executable("docker") {
        if let Some(bin_dir) = exe.parent() {
            info.commands = Some(path_append_cmd(bin_dir));
        }
    }

    Some(info)
}

/// Detect Perforce Helix (p4) via `p4 -V`. Path-only: appends system bin dir.
pub fn detect_perforce() -> Option<BindInfo> {
    let version = run_version_cmd("p4", &["-V"])?;
    let first_line = version.lines().next()?;
    let ver = first_line
        .strip_prefix("Perforce - ")?
        .split('/')
        .next()?
        .trim()
        .to_string();

    let mut info = BindInfo::new("perforce", &ver);
    info.description = format!("Perforce Helix {}", ver);
    info.tools = vec!["p4".to_string(), "p4v".to_string()];
    info.variants = vec![SYSTEM.variant()];

    let p4_name = if cfg!(windows) { "p4.exe" } else { "p4" };
    if let Some(exe) = find_executable(p4_name) {
        if let Some(bin_dir) = exe.parent() {
            info.commands = Some(path_append_cmd(bin_dir));
        }
    }

    Some(info)
}

/// Detect GitHub CLI via `gh --version`. Path-only: appends system bin dir.
pub fn detect_gh() -> Option<BindInfo> {
    let gh_name = if cfg!(windows) { "gh.exe" } else { "gh" };
    let version = run_version_cmd(gh_name, &["--version"])?;
    let ver = version
        .lines()
        .next()?
        .strip_prefix("gh version ")?
        .split_whitespace()
        .next()?
        .to_string();

    let mut info = BindInfo::new("gh", &ver);
    info.description = format!("GitHub CLI {}", ver);
    info.tools = vec!["gh".to_string()];
    info.variants = vec![SYSTEM.variant()];

    if let Some(exe) = find_executable("gh") {
        if let Some(bin_dir) = exe.parent() {
            info.commands = Some(path_append_cmd(bin_dir));
        }
    }

    Some(info)
}

/// Detect Git via `git --version`. Path-only: appends system bin dir.
pub fn detect_git() -> Option<BindInfo> {
    let git_name = if cfg!(windows) { "git.exe" } else { "git" };
    let version = run_version_cmd(git_name, &["--version"])?;
    let ver = version
        .trim()
        .strip_prefix("git version ")?
        .split_whitespace()
        .next()?
        .trim_end_matches('.')
        .to_string();

    let mut info = BindInfo::new("git", &ver);
    info.description = format!("Git {}", ver);
    info.tools = vec!["git".to_string()];
    info.variants = vec![SYSTEM.variant()];

    if let Some(exe) = find_executable("git") {
        if let Some(bin_dir) = exe.parent() {
            info.commands = Some(path_append_cmd(bin_dir));
        }
    }

    Some(info)
}

/// Detect GCC via `-dumpfullversion -dumpversion`. Path-only: appends system bin dir.
pub fn detect_gcc() -> Option<BindInfo> {
    let gcc_ver = run_version_cmd("gcc", &["-dumpfullversion", "-dumpversion"])?;
    let ver = gcc_ver.lines().next()?.trim().to_string();
    if ver.is_empty() {
        return None;
    }

    if let Some(gpp_ver) = run_version_cmd("g++", &["-dumpfullversion", "-dumpversion"]) {
        let gpp = gpp_ver.lines().next().unwrap_or("").trim();
        if gpp != ver {
            return None;
        }
    }

    let mut info = BindInfo::new("gcc", &ver);
    info.description = format!("GCC {}", ver);
    info.tools = vec!["gcc".to_string(), "g++".to_string()];
    info.variants = vec![SYSTEM.variant()];

    if let Some(gcc_path) = find_executable("gcc") {
        if let Some(bin_dir) = gcc_path.parent() {
            info.commands = Some(path_append_cmd(bin_dir));
        }
    }

    Some(info)
}
