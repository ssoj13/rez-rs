// SPDX-License-Identifier: Apache-2.0

//! Node.js npm build system implementation.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use super::native;
use super::{run_cmd, BuildContext, BuildResult, BuildSystem, BuildSystemType};
use crate::errors::Result;

/// Node.js npm build system.
#[derive(Debug, Clone)]
pub struct NodeJsBuildSystem {
    pub working_dir: PathBuf,
    /// Path to npm executable (defaults to "npm")
    pub npm_path: String,
}

impl NodeJsBuildSystem {
    pub fn new(working_dir: PathBuf) -> Self {
        Self {
            working_dir,
            npm_path: "npm".into(),
        }
    }

    pub fn is_valid_root(path: &Path) -> bool {
        path.join("package.json").exists()
    }
}

impl BuildSystem for NodeJsBuildSystem {
    fn name(&self) -> &str {
        "nodejs"
    }
    fn build_type(&self) -> BuildSystemType {
        BuildSystemType::NodeJs
    }

    fn is_valid(&self) -> bool {
        Self::is_valid_root(&self.working_dir)
    }

    fn build(&self, ctx: &BuildContext) -> Result<BuildResult> {
        let start = Instant::now();

        // npm install
        let mut install_cmd = Command::new(&self.npm_path);
        install_cmd.current_dir(&ctx.source_path).arg("install");

        for (k, v) in &ctx.env_vars {
            install_cmd.env(k, v);
        }

        if let Err(e) = run_cmd(&mut install_cmd, "npm install") {
            let elapsed = start.elapsed().as_secs_f64();
            return Ok(BuildResult::fail(
                ctx.build_path.clone(),
                elapsed,
                e.to_string(),
            ));
        }

        // npm run build
        let mut build_cmd = Command::new(&self.npm_path);
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

        if let Err(e) = run_cmd(&mut build_cmd, "npm run build") {
            let elapsed = start.elapsed().as_secs_f64();
            return Ok(BuildResult::fail(
                ctx.build_path.clone(),
                elapsed,
                e.to_string(),
            ));
        }

        let prepared_payload = if ctx.install {
            native::install(ctx, &native::artifacts(ctx, "nodejs")?)?
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
    fn test_nodejs_valid_root() {
        let dir = std::env::temp_dir().join("rez_test_nodejs_root");
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::create_dir_all(&dir);

        assert!(!NodeJsBuildSystem::is_valid_root(&dir));
        fs::write(dir.join("package.json"), "{\"name\": \"test\"}").ok();
        assert!(NodeJsBuildSystem::is_valid_root(&dir));

        let _ = fs::remove_dir_all(&dir);
    }
}
