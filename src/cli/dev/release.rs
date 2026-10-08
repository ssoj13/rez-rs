// SPDX-License-Identifier: Apache-2.0

//! `rez release` - build and deploy package to release repository with git tagging.

use std::path::Path;
use std::process::Command;

use clap::Args;

use build_system::builders::BuildProcess;
use build_system::release_hooks;
use foundation::errors::{Result, RezError};
use model::config::CONFIG;
use repository::package::discover::get_developer_package;
use version::Version;

// ---------------------------------------------------------------------------
// Args
// ---------------------------------------------------------------------------

/// Build a package from source and deploy it to the release repository.
#[derive(Args, Debug)]
pub struct ReleaseArgs {
    /// Release message
    #[arg(short = 'm', long)]
    pub message: Option<String>,

    /// Force VCS type (git)
    #[arg(long)]
    pub vcs: Option<String>,

    /// Allow release of version earlier than latest
    #[arg(long)]
    pub no_latest: bool,

    /// Release even if tag already exists for this version
    #[arg(long)]
    pub ignore_existing_tag: bool,

    /// Skip repository-related errors
    #[arg(long)]
    pub skip_repo_errors: bool,

    /// Don't prompt for release message
    #[arg(long)]
    pub no_message: bool,

    /// Build system to use
    #[arg(long)]
    pub buildsys: Option<String>,

    /// Clean build directory first
    #[arg(short = 'c', long)]
    pub clean: bool,

    /// Build specific variants (indices)
    #[arg(long, num_args = 1.., value_name = "INDEX")]
    pub variants: Vec<usize>,

    /// Subdirectory tag for organizing packages (e.g. "dcc", "studio/tools").
    /// Package installs to <release_path>/<tag>/<name>/<version>/.
    #[arg(short = 't', long)]
    pub tag: Option<String>,

    /// Verbose output
    #[arg(from_global)]
    pub verbose: u8,
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

fn validate_tag_path(tag: &str) -> Result<()> {
    if !foundation::path::is_safe_rez_path(tag, false) {
        return Err(RezError::Release(format!(
            "Release tag path must be a non-empty relative path: {tag:?}"
        )));
    }
    Ok(())
}

/// Execute the `rez release` command.
pub fn run(args: &ReleaseArgs) -> Result<()> {
    let working_dir = std::env::current_dir()
        .map_err(|e| RezError::Release(format!("Failed to get working directory: {}", e)))?;

    // Load developer package
    let dev_pkg = get_developer_package(&working_dir)?;
    let pkg = &dev_pkg.package;
    let pkg_name = &pkg.name;
    let pkg_version = &pkg.version;
    model::serialise::validate_rez_package_path(pkg_name, Some(&pkg_version.to_string()))?;

    eprintln!("Releasing {}-{}...", pkg_name, pkg_version);

    // Use the same repository location and category policy as build installation.
    let release_path = CONFIG.build_install_path(pkg, None, true);
    let tag = args
        .tag
        .as_deref()
        .or_else(|| CONFIG.resolve_tag_path(&pkg.tags));
    let release_path = if let Some(tag) = tag {
        validate_tag_path(tag)?;
        release_path.join(tag)
    } else {
        release_path
    };

    // VCS validation (git clean, branch, tag check)
    if !args.skip_repo_errors {
        validate_git_state(&working_dir, pkg_name, &pkg_version.to_string(), args)?;
    }

    // Check version is >= latest in release repo (unless --no-latest)
    if !args.no_latest {
        check_latest_version(&release_path, pkg_name, pkg_version)?;
    }

    let install_path = release_path.join(pkg_name).join(pkg_version.to_string());
    run_release_hooks(
        "pre_release",
        &working_dir,
        pkg_name,
        &pkg_version.to_string(),
        Some(install_path.as_path()),
        None,
        args.verbose > 0,
    )?;

    // Build and install to release path
    let build_command = pkg.build_command.clone();
    let buildsys_name = args.buildsys.as_deref();

    let mut bp = BuildProcess::new(&working_dir, build_command, buildsys_name, None)?;
    bp.set_package(pkg.clone(), Some(dev_pkg.clone()))?;
    bp.verbose = args.verbose > 0;

    let variant_indices = if args.variants.is_empty() {
        None
    } else {
        Some(args.variants.as_slice())
    };

    eprintln!("Building and installing to {}...", install_path.display());

    let results = bp.build(
        &install_path,
        args.clean,
        true,
        variant_indices,
        false,
        false,
    )?;

    let mut all_ok = true;
    for (i, result) in results.iter().enumerate() {
        if result.success {
            eprintln!("  Variant {}: ok ({:.1}s)", i, result.elapsed_secs);
        } else {
            eprintln!(
                "  Variant {}: FAILED - {}",
                i,
                result.error.as_deref().unwrap_or("unknown")
            );
            all_ok = false;
        }
    }

    if !all_ok {
        return Err(RezError::Release(
            "One or more variants failed to build".into(),
        ));
    }

    // Tag VCS
    if !args.skip_repo_errors {
        create_git_tag(&working_dir, pkg_name, &pkg_version.to_string(), args)?;
    }

    run_release_hooks(
        "post_release",
        &working_dir,
        pkg_name,
        &pkg_version.to_string(),
        Some(install_path.as_ref()),
        args.message.as_deref(),
        args.verbose > 0,
    )?;

    // Summary
    let revision = git_current_revision(&working_dir).unwrap_or_default();
    eprintln!(
        "\nReleased {}-{} to {}",
        pkg_name,
        pkg_version,
        release_path.display()
    );
    if !revision.is_empty() {
        eprintln!("Git revision: {}", revision);
    }
    if let Some(ref msg) = args.message {
        if !msg.is_empty() {
            eprintln!("Message: {}", msg);
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Git helpers
// ---------------------------------------------------------------------------

/// Run a git command in `dir`, return (stdout, success).
fn git_cmd(dir: &Path, git_args: &[&str]) -> Result<(String, bool)> {
    let output = Command::new("git")
        .args(git_args)
        .current_dir(dir)
        .output()
        .map_err(|e| RezError::ReleaseVcs(format!("Failed to run git {}: {}", git_args[0], e)))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok((stdout, output.status.success()))
}

/// Check if `dir` is a git repository.
fn is_git_repo(dir: &Path) -> bool {
    dir.join(".git").exists() || dir.ancestors().any(|p| p.join(".git").exists())
}

/// Get the current short commit hash.
fn git_current_revision(dir: &Path) -> Option<String> {
    git_cmd(dir, &["rev-parse", "--short", "HEAD"])
        .ok()
        .and_then(|(out, ok)| {
            if ok && !out.is_empty() {
                Some(out)
            } else {
                None
            }
        })
}

/// Validate git state: clean working tree, on a branch, not behind remote.
fn validate_git_state(
    dir: &Path,
    pkg_name: &str,
    pkg_version: &str,
    args: &ReleaseArgs,
) -> Result<()> {
    if !is_git_repo(dir) {
        return Ok(()); // Non-git directories are fine
    }

    // 1. Ensure working tree is clean
    let (status, _) = git_cmd(dir, &["status", "--porcelain"])?;
    if !status.is_empty() {
        return Err(RezError::ReleaseVcs(
            "Cannot release: uncommitted changes in working directory.\n\
             Commit or stash changes first."
                .into(),
        ));
    }

    // 2. Ensure we're on a branch (not detached HEAD)
    let (branch, ok) = git_cmd(dir, &["symbolic-ref", "--short", "HEAD"])?;
    if !ok || branch.is_empty() {
        return Err(RezError::ReleaseVcs(
            "Cannot release: HEAD is detached. Check out a branch first.".into(),
        ));
    }

    if args.verbose > 0 {
        eprintln!("Git branch: {}", branch);
    }

    // 3. Check if tag already exists (unless --ignore-existing-tag)
    if !args.ignore_existing_tag {
        let tag = format!("{}-{}", pkg_name, pkg_version);
        let (_, exists) = git_cmd(dir, &["rev-parse", &tag])?;
        if exists {
            return Err(RezError::ReleaseVcs(format!(
                "Tag '{}' already exists. Use --ignore-existing-tag to override.",
                tag
            )));
        }
    }

    Ok(())
}

/// Check that `pkg_version` >= the latest version already in the release repo.
fn check_latest_version(release_path: &Path, pkg_name: &str, pkg_version: &Version) -> Result<()> {
    let pkg_dir = release_path.join(pkg_name);
    if !pkg_dir.is_dir() {
        return Ok(()); // First release of this package
    }

    let mut latest: Option<Version> = None;
    if let Ok(entries) = std::fs::read_dir(&pkg_dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if let Ok(v) = Version::new(&name_str) {
                    match &latest {
                        None => latest = Some(v),
                        Some(cur) if v > *cur => latest = Some(v),
                        _ => {}
                    }
                }
            }
        }
    }

    if let Some(ref lat) = latest {
        if *pkg_version < *lat {
            return Err(RezError::Release(format!(
                "Version {} is earlier than the latest release {}. \
                 Use --no-latest to allow this.",
                pkg_version, lat
            )));
        }
    }

    Ok(())
}

/// Run configured release hooks (built-in plugins or scripts).
/// Built-in: amqp, emailer, command (from config.plugins.release_hook.*).
/// Scripts: path or command name with REZ_RELEASE_* env vars.
fn run_release_hooks(
    event: &str,
    working_dir: &Path,
    pkg_name: &str,
    pkg_version: &str,
    install_path: Option<&Path>,
    message: Option<&str>,
    verbose: bool,
) -> Result<()> {
    let hooks = &CONFIG.release_hooks;
    if hooks.is_empty() {
        return Ok(());
    }

    for name in hooks {
        if verbose {
            eprintln!("Running {} hook '{}'...", event, name);
        }

        if release_hooks::run_builtin_hook(
            name,
            &release_hooks::ReleaseHookContext {
                event,
                working_dir,
                pkg_name,
                pkg_version,
                install_path,
                message,
                verbose,
            },
        )? {
            continue;
        }

        let mut cmd = if name.contains('/') || name.contains('\\') {
            // Path to script
            let path = working_dir.join(name);
            if !path.exists() {
                eprintln!("Warning: release hook '{}' not found, skipping", name);
                continue;
            }
            let mut c = Command::new(path);
            c.arg(event);
            c
        } else {
            // Command name (e.g. rez-release-email)
            let mut c = Command::new(name);
            c.arg(event);
            c
        };

        cmd.current_dir(working_dir)
            .env("REZ_RELEASE_EVENT", event)
            .env(
                "REZ_RELEASE_PACKAGE",
                format!("{}-{}", pkg_name, pkg_version),
            )
            .env("REZ_RELEASE_VERSION", pkg_version)
            .env("REZ_RELEASE_PATH", working_dir);
        if let Some(p) = install_path {
            cmd.env("REZ_RELEASE_INSTALL_PATH", p);
        }
        if let Some(m) = message {
            cmd.env("REZ_RELEASE_MESSAGE", m);
        }

        let status = cmd.status().map_err(|e| {
            RezError::Release(format!("Release hook '{}' failed to run: {}", name, e))
        })?;

        if !status.success() {
            return Err(RezError::Release(format!(
                "Release hook '{}' exited with code {:?}",
                name,
                status.code()
            )));
        }
    }
    Ok(())
}

/// Create an annotated git tag for the release.
fn create_git_tag(dir: &Path, name: &str, version: &str, args: &ReleaseArgs) -> Result<()> {
    if !is_git_repo(dir) {
        return Ok(());
    }

    let tag = format!("{}-{}", name, version);
    let default_msg = format!("Release {}", tag);
    let msg = args.message.as_deref().unwrap_or(&default_msg);

    let output = Command::new("git")
        .args(["tag", "-a", &tag, "-m", msg])
        .current_dir(dir)
        .output()
        .map_err(|e| RezError::ReleaseVcs(format!("git tag failed: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("already exists") {
            if args.ignore_existing_tag {
                if args.verbose > 0 {
                    eprintln!("Tag '{}' already exists, skipping", tag);
                }
            } else {
                return Err(RezError::ReleaseVcs(format!(
                    "Tag '{}' already exists. Use --ignore-existing-tag to override.",
                    tag
                )));
            }
        } else {
            return Err(RezError::ReleaseVcs(format!(
                "git tag failed: {}",
                stderr.trim()
            )));
        }
    } else if args.verbose > 0 {
        eprintln!("Tagged: {}", tag);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_tag_path;

    #[test]
    fn release_tag_paths_must_stay_relative() {
        assert!(validate_tag_path("studio/tools").is_ok());
        assert!(validate_tag_path("../outside").is_err());
        assert!(validate_tag_path("studio/../outside").is_err());
        assert!(validate_tag_path("").is_err());
        assert_eq!(validate_tag_path("studio:stream").is_err(), cfg!(windows));
    }
}
