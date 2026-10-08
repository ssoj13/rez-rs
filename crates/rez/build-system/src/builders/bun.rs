// SPDX-License-Identifier: Apache-2.0

//! Bun build system implementation.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use super::native;
use super::{run_cmd, BuildContext, BuildResult, BuildSystem, BuildSystemType};
use crate::errors::Result;

/// Bun build system (package.json + bunfig.toml or bun.lockb).
#[derive(Debug, Clone)]
pub struct BunBuildSystem {
    pub working_dir: PathBuf,
    /// Path to bun executable (defaults to "bun")
    pub bun_path: String,
}

impl BunBuildSystem {
    pub fn new(working_dir: PathBuf) -> Self {
        Self {
            working_dir,
            bun_path: "bun".into(),
        }
    }

    pub fn is_valid_root(path: &Path) -> bool {
        let has_package_json = path.join("package.json").exists();
        has_package_json && (path.join("bunfig.toml").exists() || path.join("bun.lockb").exists())
    }
}

impl BuildSystem for BunBuildSystem {
    fn name(&self) -> &str {
        "bun"
    }
    fn build_type(&self) -> BuildSystemType {
        BuildSystemType::Bun
    }

    fn is_valid(&self) -> bool {
        Self::is_valid_root(&self.working_dir)
    }

    fn build(&self, ctx: &BuildContext) -> Result<BuildResult> {
        let start = Instant::now();

        // bun install
        let mut install_cmd = Command::new(&self.bun_path);
        install_cmd.current_dir(&ctx.source_path).arg("install");

        for (k, v) in &ctx.env_vars {
            install_cmd.env(k, v);
        }

        if let Err(e) = run_cmd(&mut install_cmd, "bun install") {
            let elapsed = start.elapsed().as_secs_f64();
            return Ok(BuildResult::fail(
                ctx.build_path.clone(),
                elapsed,
                e.to_string(),
            ));
        }

        // bun run build
        let mut build_cmd = Command::new(&self.bun_path);
        build_cmd
            .current_dir(&ctx.source_path)
            .arg("run")
            .arg("build");

        for arg in &ctx.build_args {
            build_cmd.arg(arg);
        }
        for (k, v) in &ctx.env_vars {
            build_cmd.env(k, v);
        }

        if let Err(e) = run_cmd(&mut build_cmd, "bun run build") {
            let elapsed = start.elapsed().as_secs_f64();
            return Ok(BuildResult::fail(
                ctx.build_path.clone(),
                elapsed,
                e.to_string(),
            ));
        }

        let prepared_payload = if ctx.install {
            native::install(ctx, &native::artifacts(ctx, "bun")?)?
        } else {
            None
        };

        let elapsed = start.elapsed().as_secs_f64();

        let mut result = BuildResult::ok(ctx.build_path.clone(), elapsed);
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
    fn test_bun_valid_root() {
        let dir = std::env::temp_dir().join("rez_test_bun_root");
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::create_dir_all(&dir);

        // No files -> invalid
        assert!(!BunBuildSystem::is_valid_root(&dir));

        // package.json alone -> invalid (need bun marker)
        fs::write(dir.join("package.json"), "{}").ok();
        assert!(!BunBuildSystem::is_valid_root(&dir));

        // + bun.lockb -> valid
        fs::write(dir.join("bun.lockb"), "").ok();
        assert!(BunBuildSystem::is_valid_root(&dir));
        fs::remove_file(dir.join("bun.lockb")).ok();

        // + bunfig.toml -> valid
        fs::write(dir.join("bunfig.toml"), "").ok();
        assert!(BunBuildSystem::is_valid_root(&dir));

        let _ = fs::remove_dir_all(&dir);
    }
}
