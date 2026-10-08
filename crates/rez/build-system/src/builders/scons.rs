// SPDX-License-Identifier: Apache-2.0

//! SCons build system implementation.
//!
//! SCons (https://scons.org) is a Python-based build tool. Typical usage:
//!   scons -jN
//!   scons install (many projects support this with PREFIX/DESTDIR)

use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use super::{run_cmd, BuildContext, BuildResult, BuildSystem, BuildSystemType};
use crate::errors::Result;

/// SCons-based build system.
#[derive(Debug, Clone)]
pub struct SConsBuildSystem {
    pub working_dir: PathBuf,
}

impl SConsBuildSystem {
    pub fn new(working_dir: PathBuf) -> Self {
        Self { working_dir }
    }

    pub fn is_valid_root(path: &std::path::Path) -> bool {
        path.join("SConstruct").exists() || path.join("SConscript").exists()
    }
}

impl BuildSystem for SConsBuildSystem {
    fn name(&self) -> &str {
        "scons"
    }
    fn build_type(&self) -> BuildSystemType {
        BuildSystemType::SCons
    }

    fn is_valid(&self) -> bool {
        Self::is_valid_root(&self.working_dir)
    }

    fn build(&self, ctx: &BuildContext) -> Result<BuildResult> {
        let start = Instant::now();

        // scons build
        let mut cmd = Command::new("scons");
        cmd.current_dir(&ctx.source_path)
            .arg(format!("-j{}", ctx.build_threads));

        if ctx.install {
            // PREFIX = where to install; DESTDIR left empty for direct install
            cmd.env("PREFIX", ctx.install_path.display().to_string());
        }

        for arg in &ctx.build_args {
            cmd.arg(arg);
        }
        for (k, v) in &ctx.env_vars {
            cmd.env(k, v);
        }

        if let Err(e) = run_cmd(&mut cmd, "scons") {
            let elapsed = start.elapsed().as_secs_f64();
            return Ok(BuildResult::fail(
                ctx.build_path.clone(),
                elapsed,
                e.to_string(),
            ));
        }

        // Optional install step (many SCons projects support "scons install")
        if ctx.install {
            let mut install_cmd = Command::new("scons");
            install_cmd
                .current_dir(&ctx.source_path)
                .arg("install")
                .env("PREFIX", ctx.install_path.display().to_string());

            for (k, v) in &ctx.env_vars {
                install_cmd.env(k, v);
            }

            if let Err(e) = run_cmd(&mut install_cmd, "scons install") {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_scons_valid_root() {
        let dir = std::env::temp_dir().join("rez_test_scons_root");
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::create_dir_all(&dir);

        assert!(!SConsBuildSystem::is_valid_root(&dir));

        fs::write(dir.join("SConstruct"), "import glob").ok();
        assert!(SConsBuildSystem::is_valid_root(&dir));
        fs::remove_file(dir.join("SConstruct")).ok();

        fs::write(dir.join("SConscript"), "import glob").ok();
        assert!(SConsBuildSystem::is_valid_root(&dir));

        let _ = fs::remove_dir_all(&dir);
    }
}
