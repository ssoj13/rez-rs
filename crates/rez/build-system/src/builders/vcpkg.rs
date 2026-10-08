// SPDX-License-Identifier: Apache-2.0

//! vcpkg build system implementation.
//!
//! vcpkg (https://vcpkg.io) is a C/C++ package manager by Microsoft.
//! In manifest mode, vcpkg.json defines dependencies and the project.
//! Installs manifest dependencies/ports; application compilation belongs to its own build adapter.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use super::native::{self, Artifact, ArtifactRoot};
use super::{run_cmd, BuildContext, BuildResult, BuildSystem, BuildSystemType};
use crate::errors::{Result, RezError};

/// vcpkg-based build system (manifest mode).
#[derive(Debug, Clone)]
pub struct VcpkgBuildSystem {
    pub working_dir: PathBuf,
}

impl VcpkgBuildSystem {
    pub fn new(working_dir: PathBuf) -> Self {
        Self { working_dir }
    }

    pub fn is_valid_root(path: &Path) -> bool {
        path.join("vcpkg.json").is_file()
    }

    fn command(&self, ctx: &BuildContext, work: &Path) -> Result<(Command, bool, bool)> {
        // Upstream response files contain whitespace-trimmed, newline-separated
        // arguments, not shell words. Expand once so managed output guards apply.
        let mut args = Vec::new();
        for argument in &ctx.build_args {
            if let Some(file) = argument.strip_prefix('@') {
                let text = std::fs::read_to_string(ctx.source_path.join(file))?;
                for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
                    if line.starts_with('@') {
                        return Err(RezError::BuildSystem(
                            "Nested vcpkg response files are not supported".into(),
                        ));
                    }
                    args.push(line.to_owned());
                }
            } else {
                args.push(argument.clone());
            }
        }
        // vcpkg option names are case-insensitive; preserve original argument values.
        let normalized = args
            .iter()
            .map(|arg| arg.to_ascii_lowercase())
            .collect::<Vec<_>>();
        // Unlike shell-style argument parsers, vcpkg scans options after "--".
        let guarded = normalized
            .iter()
            .filter(|arg| arg.as_str() != "--")
            .cloned()
            .collect::<Vec<_>>();
        native::arguments(
            &guarded,
            &[
                "--x-install-root",
                "--x-buildtrees-root",
                "--x-packages-root",
            ],
            None,
        )?;
        let no_install = normalized
            .iter()
            .any(|arg| matches!(arg.as_str(), "--dry-run" | "--only-downloads"));
        let skip_cached = normalized
            .iter()
            .any(|arg| arg == "--skip-install-if-cached");
        let root = ctx
            .env_vars
            .iter()
            .find(|(key, _)| {
                if cfg!(windows) {
                    key.eq_ignore_ascii_case("VCPKG_ROOT")
                } else {
                    key.as_str() == "VCPKG_ROOT"
                }
            })
            .map(|(_, value)| value.clone())
            .or_else(|| std::env::var("VCPKG_ROOT").ok());
        let executable = if let Some(root) = root {
            let candidate =
                PathBuf::from(root).join(if cfg!(windows) { "vcpkg.exe" } else { "vcpkg" });
            let candidate = candidate.to_str().ok_or_else(|| {
                RezError::BuildSystem("VCPKG_ROOT executable must be UTF-8".into())
            })?;
            crate::shell::types::find_executable(candidate, Some((&ctx.env_vars, &ctx.source_path)))
        } else {
            None
        };
        let mut command = super::pip_utils::command(executable.as_deref().unwrap_or("vcpkg"), ctx)?;
        command.current_dir(&ctx.source_path).arg("install");
        for (flag, relative) in [
            ("--x-install-root", "installed"),
            ("--x-buildtrees-root", "buildtrees"),
            ("--x-packages-root", "packages"),
        ] {
            let path = crate::util::directory(work, Path::new(relative), true)?;
            command.arg(format!("{flag}={}", path.display()));
        }
        command.args(args);
        Ok((command, no_install, skip_cached))
    }
}

impl BuildSystem for VcpkgBuildSystem {
    fn name(&self) -> &str {
        "vcpkg"
    }
    fn build_type(&self) -> BuildSystemType {
        BuildSystemType::Vcpkg
    }

    fn is_valid(&self) -> bool {
        Self::is_valid_root(&self.working_dir)
    }

    fn build(&self, ctx: &BuildContext) -> Result<BuildResult> {
        let start = Instant::now();

        native::manifest(ctx, Path::new("vcpkg.json"))?;
        let work = native::output(ctx, "vcpkg")?;
        let (mut command, no_install, skip_cached) = self.command(ctx, &work)?;
        if let Err(error) = run_cmd(&mut command, "vcpkg install") {
            return Ok(BuildResult::fail(
                ctx.build_path.clone(),
                start.elapsed().as_secs_f64(),
                error.to_string(),
            ));
        }
        let installed = work.join("installed");
        // A successful planning/download/cache-only operation is not an installation.
        // In manifest mode --skip-install-if-cached may return without deploying
        // any ports. Its fresh install database is authoritative for that case.
        let cached_without_install = skip_cached && {
            let status = installed.join("vcpkg/status");
            let text = match std::fs::read_to_string(&status) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(error) => return Err(error.into()),
            };
            !text
                .lines()
                .any(|line| line.trim() == "Status: install ok installed")
        };
        if ctx.install && (no_install || cached_without_install) {
            return Ok(BuildResult::fail(ctx.build_path.clone(), start.elapsed().as_secs_f64(),
                "vcpkg completed without an installed payload; this mode cannot publish a Rez package".into()));
        }
        let prepared_payload = if !no_install && !cached_without_install {
            let mut entries =
                std::fs::read_dir(&installed)?.collect::<std::io::Result<Vec<_>>>()?;
            entries.sort_by_key(|entry| entry.file_name());
            let artifacts = entries
                .into_iter()
                .map(|entry| Artifact {
                    source: Path::new("vcpkg/installed").join(entry.file_name()),
                    destination: entry.file_name().into(),
                    from: ArtifactRoot::Build,
                })
                .collect::<Vec<_>>();
            native::install(ctx, &artifacts)?
        } else {
            None
        };
        let mut result = BuildResult::ok(ctx.build_path.clone(), start.elapsed().as_secs_f64());
        result.prepared_payload = prepared_payload;
        result.mark_installed(ctx);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_vcpkg_valid_root() {
        let owned = tempfile::tempdir().unwrap();
        let dir = owned.path();

        assert!(!VcpkgBuildSystem::is_valid_root(dir));

        fs::write(
            dir.join("vcpkg.json"),
            r#"{"name":"test","version":"1.0","dependencies":[]}"#,
        )
        .unwrap();
        assert!(VcpkgBuildSystem::is_valid_root(dir));
    }
    fn fixture() -> (tempfile::TempDir, BuildContext) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let build = root.path().join("build");
        let install = root.path().join("live");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&build).unwrap();
        fs::write(
            source.join("vcpkg.json"),
            r#"{"name":"fixture","version":"1","dependencies":[]}"#,
        )
        .unwrap();
        let tools = root.path().join("tools");
        fs::create_dir(&tools).unwrap();
        let executable = tools.join(if cfg!(windows) { "vcpkg.exe" } else { "vcpkg" });
        fs::write(&executable, b"fixture-not-executed").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut ctx = BuildContext::new(source, build, install);
        ctx.env_vars
            .insert("VCPKG_ROOT".into(), tools.to_str().unwrap().into());
        (root, ctx)
    }

    #[test]
    fn effective_tool_and_owned_roots_preserve_triplets_features_and_response_arguments() {
        let (_root, mut ctx) = fixture();
        fs::write(ctx.source_path.join("args.rsp"), " --triplet=x64-windows \r\n\r\n--x-feature=renderer\n--overlay-ports=ports with spaces\n--binarysource=clear\n").unwrap();
        ctx.build_args = vec!["@args.rsp".into(), "--no-downloads".into()];
        let builder = VcpkgBuildSystem::new(ctx.source_path.clone());
        let work = native::output(&ctx, "vcpkg").unwrap();
        let (command, no_install, skip_cached) = builder.command(&ctx, &work).unwrap();
        assert!(!no_install && !skip_cached);
        assert_eq!(
            Path::new(command.get_program()).file_name().unwrap(),
            if cfg!(windows) { "vcpkg.exe" } else { "vcpkg" }
        );
        assert!(Path::new(command.get_program())
            .canonicalize()
            .unwrap()
            .starts_with(
                Path::new(&ctx.env_vars["VCPKG_ROOT"])
                    .canonicalize()
                    .unwrap()
            ));
        let args = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(args[0], "install");
        assert!(args.contains(&"--triplet=x64-windows"));
        assert!(args.contains(&"--x-feature=renderer"));
        assert!(args.contains(&"--overlay-ports=ports with spaces"));
        assert!(args.contains(&"--binarysource=clear"));
        assert!(args.contains(&"--no-downloads"));
        for (flag, directory) in [
            ("--x-install-root", "installed"),
            ("--x-buildtrees-root", "buildtrees"),
            ("--x-packages-root", "packages"),
        ] {
            assert!(args.contains(&format!("{flag}={}", work.join(directory).display()).as_str()));
        }
        assert!(!ctx.source_path.join("vcpkg_installed").exists());
        assert!(!ctx.install_path.exists());
    }

    #[test]
    fn response_files_cannot_bypass_managed_roots_and_modes_do_not_imply_installation() {
        let (_root, mut ctx) = fixture();
        let builder = VcpkgBuildSystem::new(ctx.source_path.clone());
        let work = native::output(&ctx, "vcpkg").unwrap();
        for flag in [
            "--X-INSTALL-ROOT=foreign",
            "--x-buildtrees-root",
            "--x-packages-root=foreign",
        ] {
            fs::write(ctx.source_path.join("args.rsp"), flag).unwrap();
            ctx.build_args = vec!["@args.rsp".into()];
            assert!(builder.command(&ctx, &work).is_err(), "{flag}");
        }
        ctx.build_args = vec!["--".into(), "--x-install-root=foreign".into()];
        assert!(builder.command(&ctx, &work).is_err());
        ctx.build_args = vec!["@args.rsp".into()];
        fs::write(ctx.source_path.join("args.rsp"), "@nested.rsp").unwrap();
        assert!(builder.command(&ctx, &work).is_err());
        for mode in [
            "--dry-run",
            "--ONLY-DOWNLOADS",
            "--skip-install-if-cached",
            "--only-binarycaching",
        ] {
            ctx.build_args = vec![mode.into()];
            let (_, no_install, skip_cached) = builder.command(&ctx, &work).unwrap();
            assert_eq!(
                no_install,
                mode == "--dry-run" || mode == "--ONLY-DOWNLOADS"
            );
            assert_eq!(skip_cached, mode == "--skip-install-if-cached");
        }
    }

    #[test]
    fn installed_triplet_tree_is_prepared_without_touching_live_or_source_payloads() {
        let (_root, mut ctx) = fixture();
        let work = native::output(&ctx, "vcpkg").unwrap();
        fs::create_dir_all(work.join("installed/x64-windows/include")).unwrap();
        fs::write(
            work.join("installed/x64-windows/include/fixture.h"),
            "header",
        )
        .unwrap();
        fs::create_dir_all(&ctx.install_path).unwrap();
        fs::write(ctx.install_path.join("valuable"), "preserve").unwrap();
        let artifacts = [Artifact {
            source: "vcpkg/installed/x64-windows".into(),
            destination: "x64-windows".into(),
            from: ArtifactRoot::Build,
        }];
        assert!(native::install(&ctx, &artifacts).unwrap().is_none());
        ctx.install = true;
        let prepared = native::install(&ctx, &artifacts).unwrap().unwrap();
        assert_eq!(
            fs::read_to_string(prepared.path().join("x64-windows/include/fixture.h")).unwrap(),
            "header"
        );
        assert_eq!(
            fs::read_to_string(ctx.install_path.join("valuable")).unwrap(),
            "preserve"
        );
        assert!(!ctx.install_path.join("x64-windows").exists());
        assert!(!ctx.source_path.join("vcpkg_installed").exists());
    }
}
