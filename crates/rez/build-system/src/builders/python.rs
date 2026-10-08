// SPDX-License-Identifier: Apache-2.0

//! Legacy Python setup.py build system implementation.

use super::pip_utils;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use super::{run_cmd, BuildContext, BuildResult, BuildSystem, BuildSystemType};
use crate::errors::{Result, RezError};

/// Legacy Python build system using setup.py.
#[derive(Debug, Clone)]
pub struct PythonBuildSystem {
    pub working_dir: PathBuf,
    /// Path to python executable (defaults to "python")
    pub python_path: String,
}

impl PythonBuildSystem {
    pub fn new(working_dir: PathBuf) -> Self {
        Self {
            working_dir,
            python_path: "python".into(),
        }
    }

    pub fn is_valid_root(path: &Path) -> bool {
        path.join("setup.py").exists() && !super::pip::PipBuildSystem::has_pep517_build_system(path)
    }
}

impl BuildSystem for PythonBuildSystem {
    fn name(&self) -> &str {
        "python"
    }
    fn build_type(&self) -> BuildSystemType {
        BuildSystemType::Python
    }

    fn is_valid(&self) -> bool {
        Self::is_valid_root(&self.working_dir)
    }

    fn build(&self, ctx: &BuildContext) -> Result<BuildResult> {
        let start = Instant::now();
        super::native::arguments(
            &ctx.build_args,
            &[
                "-b",
                "--build-base",
                "--build-lib",
                "--build-temp",
                "--build-scripts",
            ],
            Some(&["-b"]),
        )?;

        std::fs::create_dir_all(&ctx.build_path).map_err(|e| {
            RezError::BuildSystem(format!(
                "Failed to create build dir {}: {}",
                ctx.build_path.display(),
                e
            ))
        })?;

        let work = tempfile::Builder::new()
            .prefix("rez-python-build-")
            .tempdir()?;
        let mut sources = pip_utils::SourceMap::new(work.path(), Some(&ctx.build_path));
        let source = sources.stage(&ctx.source_path)?;
        sources.inputs(ctx, "python")?;
        let build = std::path::absolute(&ctx.build_path)?;
        let mut cmd = pip_utils::command(&self.python_path, ctx)?;
        cmd.current_dir(&source)
            .arg("setup.py")
            .arg("build")
            .arg(format!("--build-base={}", build.display()));
        for arg in &ctx.build_args {
            cmd.arg(arg);
        }
        for (k, v) in &ctx.env_vars {
            cmd.env(k, v);
        }

        if let Err(e) = run_cmd(&mut cmd, "python setup.py build") {
            let elapsed = start.elapsed().as_secs_f64();
            return Ok(BuildResult::fail(
                ctx.build_path.clone(),
                elapsed,
                e.to_string(),
            ));
        }

        let prepared = if ctx.install {
            let logical = std::path::absolute(&ctx.install_path)?;
            let install_root = crate::util::directory(work.path(), Path::new("install"), true)?;
            let mut icmd = pip_utils::command(&self.python_path, ctx)?;
            // Distutils --root stages the installation while --prefix remains
            // the final prefix visible to configuration and generated metadata.
            icmd.current_dir(&source)
                .args(["setup.py", "build"])
                .arg(format!("--build-base={}", build.display()))
                .args(&ctx.build_args)
                .args(["install", "--skip-build"])
                .arg(format!("--prefix={}", logical.display()))
                .arg(format!("--root={}", install_root.display()));
            if let Err(error) = run_cmd(&mut icmd, "python setup.py install") {
                return Ok(BuildResult::fail(
                    ctx.build_path.clone(),
                    start.elapsed().as_secs_f64(),
                    error.to_string(),
                ));
            }
            let relative = logical
                .components()
                .filter_map(|component| {
                    if let std::path::Component::Normal(value) = component {
                        Some(value)
                    } else {
                        None
                    }
                })
                .collect::<PathBuf>();
            let installed = crate::util::directory(&install_root, &relative, false)?;
            let payload = Arc::new(
                tempfile::Builder::new()
                    .prefix("rez-python-payload-")
                    .tempdir()?,
            );
            foundation::filesystem::copy_dir_contents(
                &installed,
                payload.path(),
                false,
                true,
                Some(payload.path()),
                None,
            )?;
            Some(payload)
        } else {
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
    fn test_python_valid_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Empty dir -> invalid
        assert!(!PythonBuildSystem::is_valid_root(root));

        // Legacy setup.py remains supported with unrelated tool settings.
        fs::write(root.join("setup.py"), "from setuptools import setup").unwrap();
        fs::write(
            root.join("pyproject.toml"),
            "[tool.black]\nline-length = 88",
        )
        .unwrap();
        assert!(PythonBuildSystem::is_valid_root(root));

        // An explicit PEP 517 backend selects the pip builder, even when
        // setup.py is also present.
        fs::write(
            root.join("pyproject.toml"),
            r#"[build-system]
requires = ["setuptools"]
build-backend = "setuptools.build_meta"
"#,
        )
        .unwrap();
        assert!(!PythonBuildSystem::is_valid_root(root));
    }
}
