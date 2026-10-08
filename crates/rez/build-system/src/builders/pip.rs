// SPDX-License-Identifier: Apache-2.0

//! Pip build system implementation for Python packages.
//!
//! Builds wheels in owned workspaces and prepares verified pip payloads for publication.

use crate::config::CONFIG;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use super::pip_utils;
use super::{run_cmd, BuildContext, BuildResult, BuildSystem, BuildSystemType};
use crate::errors::{Result, RezError};

/// Pip build system for PEP 517 projects and standalone requirements files.
#[derive(Debug, Clone)]
pub struct PipBuildSystem {
    pub working_dir: PathBuf,
    /// Path to pip executable (defaults to "pip")
    pub pip_path: String,
}

impl PipBuildSystem {
    pub fn new(working_dir: PathBuf) -> Self {
        Self {
            working_dir,
            pip_path: "pip".into(),
        }
    }

    pub fn is_valid_root(path: &Path) -> bool {
        Self::has_pep517_build_system(path) || Self::has_standalone_requirements(path)
    }

    fn has_standalone_requirements(path: &Path) -> bool {
        path.join("requirements.txt").is_file()
            && !path.join("setup.py").exists()
            && !path.join("pyproject.toml").exists()
    }

    fn install_args(
        ctx: &BuildContext,
        requirements_only: bool,
        destination: &Path,
    ) -> Result<Vec<std::ffi::OsString>> {
        super::native::arguments(
            &ctx.build_args,
            &[
                "-t",
                "--target",
                "--prefix",
                "--root",
                "--user",
                "-w",
                "--wheel-dir",
            ],
            Some(&["-t", "-w"]),
        )?;
        let mut args: Vec<std::ffi::OsString> = if ctx.install {
            vec![
                "install".into(),
                "--target".into(),
                destination.as_os_str().into(),
                "--ignore-installed".into(),
            ]
        } else {
            vec![
                "wheel".into(),
                "--wheel-dir".into(),
                destination.as_os_str().into(),
            ]
        };
        if requirements_only {
            args.extend(["-r".into(), "requirements.txt".into()]);
        } else if !ctx
            .build_args
            .windows(2)
            .any(|pair| ["-e", "--editable"].contains(&pair[0].as_str()) && pair[1] == ".")
            && !ctx
                .build_args
                .iter()
                .any(|arg| arg == "--editable=." || arg == "-e.")
        {
            args.push(".".into());
        }
        args.extend(ctx.build_args.iter().map(std::ffi::OsString::from));
        Ok(args)
    }

    pub(crate) fn has_pep517_build_system(path: &Path) -> bool {
        let Ok(content) = std::fs::read_to_string(path.join("pyproject.toml")) else {
            return false;
        };
        let Ok(document) = toml::from_str::<toml::Table>(&content) else {
            return false;
        };

        let Some(build_system) = document.get("build-system").and_then(toml::Value::as_table)
        else {
            return false;
        };
        let valid_requires = build_system
            .get("requires")
            .and_then(toml::Value::as_array)
            .is_some_and(|requires| requires.iter().all(toml::Value::is_str));
        let valid_backend = build_system
            .get("build-backend")
            .is_none_or(toml::Value::is_str);

        valid_requires && valid_backend
    }
}

impl BuildSystem for PipBuildSystem {
    fn name(&self) -> &str {
        "pip"
    }
    fn build_type(&self) -> BuildSystemType {
        BuildSystemType::Pip
    }

    fn is_valid(&self) -> bool {
        Self::is_valid_root(&self.working_dir)
    }

    fn build(&self, ctx: &BuildContext) -> Result<BuildResult> {
        let start = Instant::now();

        std::fs::create_dir_all(&ctx.build_path).map_err(|e| {
            RezError::BuildSystem(format!(
                "Failed to create build dir {}: {}",
                ctx.build_path.display(),
                e
            ))
        })?;

        let work = tempfile::Builder::new()
            .prefix("rez-pip-build-")
            .tempdir()?;
        let mut sources = pip_utils::SourceMap::new(work.path(), Some(&ctx.build_path));
        let requirements_only = Self::has_standalone_requirements(&ctx.source_path);
        let target = crate::util::directory(work.path(), Path::new("target"), true)?;
        let args = Self::install_args(ctx, requirements_only, &target)?;
        let mut cmd = sources.pip(&self.pip_path, ctx, &args)?;
        cmd.envs(&ctx.env_vars);
        if let Err(error) = run_cmd(
            &mut cmd,
            if ctx.install {
                "pip install"
            } else {
                "pip wheel"
            },
        ) {
            return Ok(BuildResult::fail(
                ctx.build_path.clone(),
                start.elapsed().as_secs_f64(),
                error.to_string(),
            ));
        }
        let prepared = if ctx.install {
            let payload = Arc::new(
                tempfile::Builder::new()
                    .prefix("rez-pip-payload-")
                    .tempdir()?,
            );
            let distributions = crate::pip::metadata::distributions(&target)?;
            let mut owners = Vec::new();
            for distribution in &distributions {
                let mapping = crate::pip::metadata::mapping(
                    distribution,
                    &target,
                    &CONFIG,
                    Some(Path::new("site-packages")),
                )?;
                crate::pip::metadata::copy(&mapping, &target, payload.path())?;
                owners.push((distribution, mapping));
            }
            sources.relocate(payload.path())?;
            // Keep the selected pip interpreter encoded in its launchers. Only
            // adapter-owned staging paths may be relocated to the logical root.
            pip_utils::make_shebang_movable(
                &payload.path().join("bin"),
                Some((&target, &ctx.install_path)),
            )?;
            for (distribution, mapping) in owners {
                distribution.finalize(&mapping, &target, payload.path(), &[])?;
            }
            Some(payload)
        } else {
            let wheels = super::native::output(ctx, "wheels")?;
            foundation::filesystem::copy_dir_contents(
                &target,
                &wheels,
                false,
                true,
                Some(&ctx.build_path),
                None,
            )?;
            None
        };
        let elapsed = start.elapsed().as_secs_f64();
        let mut result = BuildResult::ok(ctx.build_path.clone(), elapsed);
        result.prepared_payload = prepared;
        result.mark_installed(ctx);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_pip_valid_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        assert!(!PipBuildSystem::is_valid_root(root));

        // A PEP 517 table with valid requirements is handled by pip.
        fs::write(
            root.join("pyproject.toml"),
            r#"[build-system]
requires = ["example-backend"]
build-backend = "example_backend"
"#,
        )
        .unwrap();
        assert!(PipBuildSystem::is_valid_root(root));
        fs::remove_file(root.join("pyproject.toml")).unwrap();

        // Tool configuration alone is not a build-system declaration.
        fs::write(
            root.join("pyproject.toml"),
            "[tool.black]\nline-length = 88",
        )
        .unwrap();
        assert!(!PipBuildSystem::is_valid_root(root));
        fs::remove_file(root.join("pyproject.toml")).unwrap();

        // Preserve detection of standalone requirements files.
        fs::write(root.join("requirements.txt"), "requests>=2.0").unwrap();
        assert!(PipBuildSystem::is_valid_root(root));
    }

    #[test]
    fn test_pip_requirements_root_must_be_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("requirements.txt")).unwrap();

        assert!(!PipBuildSystem::is_valid_root(root));
    }

    #[test]
    fn test_standalone_requirements_install_args() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = BuildContext::new(
            dir.path().to_path_buf(),
            dir.path().join("build"),
            dir.path().join("install"),
        );
        ctx.install = true;
        ctx.build_args = vec!["--no-deps".into()];
        let stage = dir.path().join("owned");
        assert_eq!(
            PipBuildSystem::install_args(&ctx, true, &stage).unwrap(),
            [
                "install",
                "--target",
                stage.to_str().unwrap(),
                "--ignore-installed",
                "-r",
                "requirements.txt",
                "--no-deps"
            ]
            .map(std::ffi::OsString::from)
        );
        ctx.install = false;
        assert_eq!(
            PipBuildSystem::install_args(&ctx, true, &stage).unwrap(),
            [
                "wheel",
                "--wheel-dir",
                stage.to_str().unwrap(),
                "-r",
                "requirements.txt",
                "--no-deps"
            ]
            .map(std::ffi::OsString::from)
        );
    }

    #[test]
    fn test_project_args_own_outputs_without_editable_toolchain_install() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = BuildContext::new(
            dir.path().to_path_buf(),
            dir.path().join("build"),
            dir.path().join("install"),
        );
        let stage = dir.path().join("owned");
        assert_eq!(
            PipBuildSystem::install_args(&ctx, false, &stage).unwrap(),
            ["wheel", "--wheel-dir", stage.to_str().unwrap(), "."].map(std::ffi::OsString::from)
        );
        ctx.install = true;
        assert_eq!(
            PipBuildSystem::install_args(&ctx, false, &stage).unwrap(),
            [
                "install",
                "--target",
                stage.to_str().unwrap(),
                "--ignore-installed",
                "."
            ]
            .map(std::ffi::OsString::from)
        );
        ctx.build_args = vec!["-e".into(), ".".into()];
        let args = PipBuildSystem::install_args(&ctx, false, &stage).unwrap();
        assert_eq!(args.iter().filter(|value| *value == ".").count(), 1);
        ctx.build_args = vec!["--prefix=foreign".into()];
        assert!(PipBuildSystem::install_args(&ctx, false, &stage).is_err());
    }

    #[test]
    fn test_pip_requirements_do_not_overlap_with_python_project() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("setup.py"), "from setuptools import setup").unwrap();
        fs::write(root.join("requirements.txt"), "requests>=2.0").unwrap();

        assert!(!PipBuildSystem::is_valid_root(root));
    }
}
