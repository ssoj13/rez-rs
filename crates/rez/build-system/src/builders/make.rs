// SPDX-License-Identifier: Apache-2.0

//! Make build system implementation.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use super::{run_cmd, BuildContext, BuildResult, BuildSystem, BuildSystemType};
use crate::errors::Result;

/// Make-based build system (GNU make / nmake).
#[derive(Debug, Clone)]
pub struct MakeBuildSystem {
    pub working_dir: PathBuf,
    /// Path to make executable (defaults to "make")
    pub make_path: String,
}

impl MakeBuildSystem {
    pub fn new(working_dir: PathBuf) -> Self {
        Self {
            working_dir,
            make_path: "make".into(),
        }
    }

    pub fn is_valid_root(path: &Path) -> bool {
        path.join("Makefile").exists() || path.join("makefile").exists()
    }
}

impl BuildSystem for MakeBuildSystem {
    fn name(&self) -> &str {
        "make"
    }
    fn build_type(&self) -> BuildSystemType {
        BuildSystemType::Make
    }

    fn is_valid(&self) -> bool {
        Self::is_valid_root(&self.working_dir)
    }

    fn build(&self, ctx: &BuildContext) -> Result<BuildResult> {
        let start = Instant::now();

        let mut cmd = Command::new(&self.make_path);
        cmd.current_dir(&ctx.source_path)
            .arg(format!("-j{}", ctx.build_threads));
        if ctx.install {
            cmd.arg("install")
                .arg(format!("PREFIX={}", ctx.install_path.display()));
        }
        for arg in &ctx.build_args {
            cmd.arg(arg);
        }
        for (k, v) in &ctx.env_vars {
            cmd.env(k, v);
        }

        if let Err(e) = run_cmd(&mut cmd, "make") {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_make_valid_root() {
        let dir = std::env::temp_dir().join("rez_test_make_valid");
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::create_dir_all(&dir);

        assert!(!MakeBuildSystem::is_valid_root(&dir));

        // Both casings should work
        fs::write(dir.join("Makefile"), "all:\n\techo ok").ok();
        assert!(MakeBuildSystem::is_valid_root(&dir));
        fs::remove_file(dir.join("Makefile")).ok();

        fs::write(dir.join("makefile"), "all:").ok();
        assert!(MakeBuildSystem::is_valid_root(&dir));

        let _ = fs::remove_dir_all(&dir);
    }
}
