// SPDX-License-Identifier: Apache-2.0

//! Conan build system implementation.
//!
//! Conan (https://conan.io) is a C/C++ package manager. conan build installs
//! dependencies and runs the build() method from conanfile.py.
//! Note: conan build only works with conanfile.py, not conanfile.txt.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use super::{run_cmd, BuildContext, BuildResult, BuildSystem, BuildSystemType};
use crate::errors::Result;

/// Conan-based build system (conanfile.py).
#[derive(Debug, Clone)]
pub struct ConanBuildSystem {
    pub working_dir: PathBuf,
}

impl ConanBuildSystem {
    pub fn new(working_dir: PathBuf) -> Self {
        Self { working_dir }
    }

    /// Valid root: conanfile.py (for conan build) or conanfile.txt (consumer, needs cmake after).
    pub fn is_valid_root(path: &Path) -> bool {
        path.join("conanfile.py").exists() || path.join("conanfile.txt").exists()
    }

    /// conan build only works with conanfile.py
    fn has_conanfile_py(path: &Path) -> bool {
        path.join("conanfile.py").exists()
    }
}

impl BuildSystem for ConanBuildSystem {
    fn name(&self) -> &str {
        "conan"
    }
    fn build_type(&self) -> BuildSystemType {
        BuildSystemType::Conan
    }

    fn is_valid(&self) -> bool {
        Self::is_valid_root(&self.working_dir)
    }

    fn build(&self, ctx: &BuildContext) -> Result<BuildResult> {
        let start = Instant::now();

        if Self::has_conanfile_py(&ctx.source_path) {
            // conan build: install deps + run build() from conanfile.py
            let mut cmd = Command::new("conan");
            cmd.current_dir(&ctx.source_path).arg("build").arg(".");

            if ctx.install {
                cmd.arg(format!("-of={}", ctx.install_path.display()));
            }

            for arg in &ctx.build_args {
                cmd.arg(arg);
            }
            for (k, v) in &ctx.env_vars {
                cmd.env(k, v);
            }

            if let Err(e) = run_cmd(&mut cmd, "conan build") {
                let elapsed = start.elapsed().as_secs_f64();
                return Ok(BuildResult::fail(
                    ctx.build_path.clone(),
                    elapsed,
                    e.to_string(),
                ));
            }

            let elapsed = start.elapsed().as_secs_f64();
            let mut result = BuildResult::ok(ctx.build_path.clone(), elapsed);
            result.mark_installed(ctx);
            Ok(result)
        } else {
            // conanfile.txt: conan install then delegate to cmake if available
            let mut cmd = Command::new("conan");
            cmd.current_dir(&ctx.source_path).arg("install").arg(".");

            for (k, v) in &ctx.env_vars {
                cmd.env(k, v);
            }

            if let Err(e) = run_cmd(&mut cmd, "conan install") {
                return Ok(BuildResult::fail(
                    ctx.build_path.clone(),
                    start.elapsed().as_secs_f64(),
                    e.to_string(),
                ));
            }

            // conanfile.txt typically uses cmake - try cmake build
            if ctx.source_path.join("CMakeLists.txt").exists() {
                let mut cmake_cmd = Command::new("cmake");
                cmake_cmd
                    .current_dir(&ctx.source_path)
                    .arg("-S")
                    .arg(&ctx.source_path)
                    .arg("-B")
                    .arg(&ctx.build_path);
                if ctx.install {
                    cmake_cmd.arg(format!(
                        "-DCMAKE_INSTALL_PREFIX={}",
                        ctx.install_path.display()
                    ));
                }
                for (k, v) in &ctx.env_vars {
                    cmake_cmd.env(k, v);
                }
                if let Err(e) = run_cmd(&mut cmake_cmd, "cmake configure") {
                    return Ok(BuildResult::fail(
                        ctx.build_path.clone(),
                        start.elapsed().as_secs_f64(),
                        e.to_string(),
                    ));
                }

                let mut build_cmd = Command::new("cmake");
                build_cmd
                    .arg("--build")
                    .arg(&ctx.build_path)
                    .arg("--config")
                    .arg("Release")
                    .arg("--parallel")
                    .arg(ctx.build_threads.to_string());
                for (k, v) in &ctx.env_vars {
                    build_cmd.env(k, v);
                }
                if let Err(e) = run_cmd(&mut build_cmd, "cmake build") {
                    return Ok(BuildResult::fail(
                        ctx.build_path.clone(),
                        start.elapsed().as_secs_f64(),
                        e.to_string(),
                    ));
                }
            }

            let elapsed = start.elapsed().as_secs_f64();
            let mut result = BuildResult::ok(ctx.build_path.clone(), elapsed);
            result.mark_installed(ctx);
            Ok(result)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_conan_valid_root() {
        let dir = std::env::temp_dir().join("rez_test_conan_root");
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::create_dir_all(&dir);

        assert!(!ConanBuildSystem::is_valid_root(&dir));

        fs::write(dir.join("conanfile.py"), "from conan import ConanFile").ok();
        assert!(ConanBuildSystem::is_valid_root(&dir));
        fs::remove_file(dir.join("conanfile.py")).ok();

        fs::write(dir.join("conanfile.txt"), "[requires]\nzlib/1.0").ok();
        assert!(ConanBuildSystem::is_valid_root(&dir));

        let _ = fs::remove_dir_all(&dir);
    }
}
