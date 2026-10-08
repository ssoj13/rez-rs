// SPDX-License-Identifier: Apache-2.0

//! Shared utilities for bind module detectors.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Scan `base_dir` for subdirectories starting with `prefix`, return latest by name.
pub fn find_latest_app(base: &Path, prefix: &str) -> Option<(String, PathBuf)> {
    find_latest_app_min_version(base, prefix, None)
}

/// True if s looks like a version (starts with digit). Filters out "Server", "Indie", etc.
fn looks_like_version(s: &str) -> bool {
    s.chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
}

/// Like `find_latest_app`, but only considers versions >= `min_version` (lexicographic).
/// Skips non-version dirs (e.g. "Houdini Server" -> "Server") so we pick "21.0.440" over "Server".
pub fn find_latest_app_min_version(
    base: &Path,
    prefix: &str,
    min_version: Option<&str>,
) -> Option<(String, PathBuf)> {
    let entries = fs::read_dir(base).ok()?;
    let mut best: Option<(String, PathBuf)> = None;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.len() > prefix.len() && name[..prefix.len()].eq_ignore_ascii_case(prefix) {
            let ver = name[prefix.len()..].trim().to_string();
            if ver.is_empty() || !entry.path().is_dir() || !looks_like_version(&ver) {
                continue;
            }
            if let Some(min) = min_version {
                if ver.as_str() < min {
                    continue;
                }
            }
            if best.is_none() || ver > best.as_ref().unwrap().0 {
                best = Some((ver, entry.path()));
            }
        }
    }
    if let Some((v, p)) = &best {
        crate::log_trace!(
            "bind",
            "find_latest {:?}/{} -> {} at {:?}",
            base,
            prefix,
            v,
            p
        );
    }
    best
}

/// Find all matching installs with version >= min_version, sorted by version descending (latest first).
pub fn find_all_apps_min_version(
    base: &Path,
    prefix: &str,
    min_version: Option<&str>,
) -> Vec<(String, PathBuf)> {
    crate::log_debug!("bind", "scanning {:?}/{} for {}*", base, prefix, prefix);
    let mut matches: Vec<(String, PathBuf)> = Vec::new();
    let entries = match fs::read_dir(base) {
        Ok(e) => e,
        Err(e) => {
            crate::log_trace!("bind", "scan {:?}/{}: read_dir failed: {}", base, prefix, e);
            return matches;
        }
    };
    let mut total_dirs = 0u32;
    for entry in entries.flatten() {
        total_dirs += 1;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.len() > prefix.len() && name[..prefix.len()].eq_ignore_ascii_case(prefix) {
            let ver = name[prefix.len()..].trim().to_string();
            if ver.is_empty() || !entry.path().is_dir() {
                continue;
            }
            if !looks_like_version(&ver) {
                crate::log_trace!(
                    "bind",
                    "skip {:?} (not version-like: {:?})",
                    entry.path(),
                    ver
                );
                continue;
            }
            if let Some(min) = min_version {
                if ver.as_str() < min {
                    crate::log_trace!(
                        "bind",
                        "skip {:?} (version {} < min {})",
                        entry.path(),
                        ver,
                        min
                    );
                    continue;
                }
            }
            matches.push((ver, entry.path()));
        }
    }
    matches.sort_by(|a, b| b.0.cmp(&a.0));
    if !matches.is_empty() {
        let vers: Vec<&str> = matches.iter().map(|(v, _)| v.as_str()).collect();
        crate::log_debug!(
            "bind",
            "scan {:?}/{}: {} dirs, {} match(es): {:?}",
            base,
            prefix,
            total_dirs,
            matches.len(),
            vers
        );
    } else {
        crate::log_trace!(
            "bind",
            "scan {:?}/{}: {} dirs, 0 matches",
            base,
            prefix,
            total_dirs
        );
    }
    matches
}

/// Build commands string that sets PATH, env vars (set), append vars, path_prepend_dirs, env_prepend.
pub fn app_cmds(
    install: &Path,
    bin_sub: &str,
    env_vars: &[(&str, &str)],
    env_append: &[(&str, &str)],
    path_prepend_dirs: &[&str],
    env_prepend: &[(&str, &str)],
) -> String {
    let norm = |p: std::path::Display| p.to_string().replace('\\', "/");
    // Prepend path_prepend_dirs first (so they end up after bin), then bin - yields PATH=[bin, dir1, dir2, ...]
    let mut cmds = String::new();
    for sub in path_prepend_dirs {
        let val = norm(install.join(sub).display());
        cmds.push_str(&format!("env.PATH.prepend('{}')\n", val));
    }
    cmds.push_str(&format!(
        "env.PATH.prepend('{}')",
        norm(install.join(bin_sub).display())
    ));
    for (key, sub) in env_vars {
        let val = if sub.is_empty() {
            norm(install.display())
        } else {
            norm(install.join(sub).display())
        };
        cmds.push_str(&format!("\nenv.{}.set('{}')", key, val));
    }
    for (key, sub) in env_append {
        let val = if sub.is_empty() {
            norm(install.display())
        } else {
            norm(install.join(sub).display())
        };
        cmds.push_str(&format!("\nenv.{}.append('{}')", key, val));
    }
    for (key, sub) in env_prepend {
        let val = if sub.is_empty() {
            norm(install.display())
        } else {
            norm(install.join(sub).display())
        };
        cmds.push_str(&format!("\nenv.{}.prepend('{}')", key, val));
    }
    cmds
}

/// Path string for use in rex commands (escape backslashes for Python).
pub fn path_for_commands(p: &Path) -> String {
    p.display().to_string().replace('\\', "\\\\")
}

/// Generate path-only command: env.PATH.append('bin_dir').
pub fn path_append_cmd(bin_dir: &Path) -> String {
    format!("env.PATH.append('{}')", path_for_commands(bin_dir))
}

/// Find full path to an executable via `where` (Windows) or `which` (Unix).
///
/// On Windows, prefers .exe files over .cmd/.bat wrappers when multiple matches exist.
pub fn find_executable(name: &str) -> Option<PathBuf> {
    let cmd = if cfg!(windows) { "where" } else { "which" };
    let output = Command::new(cmd).arg(name).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let path_str = String::from_utf8_lossy(&output.stdout);

    #[cfg(windows)]
    {
        let lines: Vec<&str> = path_str
            .lines()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        if lines.is_empty() {
            return None;
        }
        // Prefer .exe files over .cmd/.bat wrappers
        for line in &lines {
            if line.ends_with(".exe") {
                return Some(PathBuf::from(line));
            }
        }
        Some(PathBuf::from(lines[0]))
    }

    #[cfg(not(windows))]
    {
        let first_line = path_str.lines().next()?.trim();
        if first_line.is_empty() {
            return None;
        }
        Some(PathBuf::from(first_line))
    }
}

/// Run a command by path and capture its stdout (first meaningful output).
pub fn run_version_cmd_path(program: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    if text.is_empty() {
        let text = String::from_utf8_lossy(&output.stderr);
        let text = text.trim().to_string();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    } else {
        Some(text.to_string())
    }
}

/// Run a command and capture its stdout (first meaningful output).
pub fn run_version_cmd(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    if text.is_empty() {
        let text = String::from_utf8_lossy(&output.stderr);
        let text = text.trim().to_string();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    } else {
        Some(text.to_string())
    }
}

/// Find directory containing a Python module via `python -c "import X; print(X.__file__)"`.
pub fn find_python_module_dir(module_name: &str) -> Option<PathBuf> {
    let code = format!("import {m}; print({m}.__file__)", m = module_name);
    let output = Command::new("python3")
        .args(["-c", &code])
        .output()
        .ok()
        .or_else(|| Command::new("python").args(["-c", &code]).output().ok())?;

    if !output.status.success() {
        return None;
    }

    let file_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if file_path.is_empty() {
        return None;
    }

    let p = PathBuf::from(&file_path);
    let mut dir = p.parent()?.to_path_buf();
    if dir.file_name().and_then(|n| n.to_str()) == Some(module_name) {
        dir = dir.parent()?.to_path_buf();
    }
    Some(dir)
}

/// Create symlink for executables (Unix: symlink, Windows: symlink_file with copy fallback).
/// On Windows, when copying a running exe (e.g. rez bind -a of itself), copy via temp file
/// to avoid "file in use" when the destination already exists or source is locked.
pub fn create_exe_link(src: &Path, dest: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(src, dest)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(src, dest).or_else(|_| {
            let tmp = dest.with_extension("exe.tmp");
            std::fs::copy(src, &tmp)?;
            std::fs::rename(&tmp, dest).inspect_err(|_| {
                let _ = std::fs::remove_file(&tmp);
            })
        })
    }
}

/// Find pip executable in python package venvs under install_path.
/// Scans install_path/python/{version}/.../venv/ for pip (Scripts/pip.exe on Windows, bin/pip on Unix).
/// Returns (pip_path, python_version) e.g. ("3.12.0") for use in pip requires.
pub fn find_venv_pip_in_python_packages(install_path: &Path) -> Option<(PathBuf, String)> {
    let python_base = install_path.join("python");
    if !python_base.is_dir() {
        return None;
    }
    let version_dirs = fs::read_dir(&python_base).ok()?;
    let pip_name = if cfg!(windows) { "pip.exe" } else { "pip" };
    let venv_pip_sub = if cfg!(windows) {
        "venv/Scripts"
    } else {
        "venv/bin"
    };

    for entry in version_dirs.flatten() {
        let version_dir = entry.path();
        if !version_dir.is_dir() {
            continue;
        }
        let version = version_dir.file_name()?.to_string_lossy().to_string();
        // Try venv directly under version (no variant subdir)
        let direct_venv = version_dir.join(venv_pip_sub).join(pip_name);
        if direct_venv.exists() {
            return Some((direct_venv, version));
        }
        // Scan variant subdirs: {hash}/venv/ or platform-X/arch-Y/os-Z/venv/
        let subdirs = fs::read_dir(&version_dir).ok()?;
        for sub in subdirs.flatten() {
            let variant_root = sub.path();
            if !variant_root.is_dir() {
                continue;
            }
            let venv_pip = variant_root.join(venv_pip_sub).join(pip_name);
            if venv_pip.exists() {
                return Some((venv_pip, version));
            }
        }
    }
    None
}

/// Validate followed module trees before creating any destination directories.
pub(crate) fn validate_module_tree(src: &Path) -> std::io::Result<()> {
    fn visit(
        src: &Path,
        ancestors: &mut std::collections::HashSet<PathBuf>,
    ) -> std::io::Result<()> {
        let actual = fs::canonicalize(src)?;
        if !ancestors.insert(actual.clone()) {
            return Err(std::io::Error::other(format!(
                "Module directory link cycle at {}",
                src.display()
            )));
        }
        if !fs::metadata(src)?.is_dir() {
            return Err(std::io::Error::other("Module source is not a directory"));
        }
        for entry in fs::read_dir(src)? {
            let source = entry?.path();
            let metadata = fs::metadata(&source)?;
            if metadata.is_dir() {
                visit(&source, ancestors)?;
            } else if !metadata.is_file() {
                return Err(std::io::Error::other(format!(
                    "Unsupported module object {}",
                    source.display()
                )));
            }
        }
        ancestors.remove(&actual);
        Ok(())
    }
    visit(src, &mut std::collections::HashSet::new())
}

/// Recursively copy a directory tree, following file and directory links.
pub fn copy_dir_recursive(src: &Path, dest: &Path) -> std::io::Result<()> {
    validate_module_tree(src)?;
    crate::package::ops::copy_dir_contents(src, dest, true, false, None, None)
        .map(|_| ())
        .map_err(|error| match error {
            crate::errors::RezError::Io(error) => error,
            error => std::io::Error::other(error),
        })
}
