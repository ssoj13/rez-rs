// SPDX-License-Identifier: Apache-2.0

//! Custom build system implementation for user-defined builders.

use std::path::PathBuf;
use std::time::Instant;

use super::{create_build_env_script, BuildContext, BuildResult, BuildSystem, BuildSystemType};
use crate::errors::{Result, RezError};
use crate::shell::ShellType;

/// Custom build system using a user-specified command string.
///
/// The command is executed via the platform shell (sh -c / cmd /c).
/// Standard rez env vars (REZ_BUILD_PATH, etc.) are set before execution.
#[derive(Debug, Clone)]
pub struct CustomBuildSystem {
    pub working_dir: PathBuf,
    /// The build command string from package definition
    pub command: String,
}

impl CustomBuildSystem {
    pub fn new(working_dir: PathBuf, command: String) -> Self {
        Self {
            working_dir,
            command,
        }
    }

    /// Run the custom command in a shell.
    /// Expands {root} -> source path, {install} -> install path.
    fn run_command(&self, ctx: &BuildContext) -> Result<std::process::Output> {
        let command = self
            .command
            .replace("{root}", ctx.source_path.to_string_lossy().as_ref())
            .replace("{install}", ctx.install_path.to_string_lossy().as_ref());
        let shell = if cfg!(windows) {
            ShellType::Cmd
        } else {
            ShellType::Sh
        };
        let mut cmd = shell.command(&command);

        cmd.current_dir(&ctx.source_path);

        // Set standard build env vars
        cmd.env("REZ_BUILD_PATH", &ctx.build_path);
        cmd.env("REZ_BUILD_SOURCE_PATH", &ctx.source_path);
        cmd.env("REZ_BUILD_INSTALL_PATH", &ctx.install_path);
        cmd.env("REZ_BUILD_TYPE", ctx.build_type.to_string());
        cmd.env("REZ_BUILD_INSTALL", if ctx.install { "1" } else { "0" });
        cmd.env("REZ_BUILD_THREAD_COUNT", ctx.build_threads.to_string());

        if let Some(idx) = ctx.variant_index {
            cmd.env("REZ_BUILD_VARIANT_INDEX", idx.to_string());
        }

        for (k, v) in &ctx.env_vars {
            cmd.env(k, v);
        }

        cmd.output().map_err(|e| {
            RezError::BuildSystem(format!("Failed to run custom '{}': {}", self.command, e))
        })
    }
}

impl BuildSystem for CustomBuildSystem {
    fn name(&self) -> &str {
        "custom"
    }
    fn build_type(&self) -> BuildSystemType {
        BuildSystemType::Custom
    }

    fn is_valid(&self) -> bool {
        !self.command.is_empty()
    }
    fn supports_build_scripts(&self) -> bool {
        true
    }

    fn write_build_scripts(&self, ctx: &BuildContext) -> Result<PathBuf> {
        create_build_env_script(ctx)
    }

    fn build(&self, ctx: &BuildContext) -> Result<BuildResult> {
        let start = Instant::now();

        // Ensure build dir exists
        std::fs::create_dir_all(&ctx.build_path).map_err(|e| {
            RezError::BuildSystem(format!(
                "Failed to create build dir {}: {}",
                ctx.build_path.display(),
                e
            ))
        })?;

        if ctx.write_build_scripts {
            let script = self.write_build_scripts(ctx)?;
            let mut result = BuildResult::ok(ctx.build_path.clone(), start.elapsed().as_secs_f64());
            result.build_env_script = Some(script);
            return Ok(result);
        }

        let output = self.run_command(ctx)?;
        let elapsed = start.elapsed().as_secs_f64();

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Ok(BuildResult::fail(
                ctx.build_path.clone(),
                elapsed,
                format!(
                    "Custom build command failed (exit {}): {}",
                    output.status.code().unwrap_or(-1),
                    stderr.trim()
                ),
            ));
        }

        let mut result = BuildResult::ok(ctx.build_path.clone(), elapsed);
        if ctx.install {
            result.install_path = Some(ctx.install_path.clone());
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_custom_validity() {
        let custom = CustomBuildSystem::new(PathBuf::from("/tmp"), "echo build".into());
        assert!(custom.is_valid());
        let empty = CustomBuildSystem::new(PathBuf::from("/tmp"), String::new());
        assert!(!empty.is_valid());
    }

    #[test]
    fn test_custom_build_success_and_failure() {
        let dir = std::env::temp_dir().join("rez_test_custom_run");
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::create_dir_all(&dir);

        // Successful command
        let custom = CustomBuildSystem::new(dir.clone(), "echo ok".into());
        let ctx = BuildContext::new(dir.clone(), dir.join("build"), dir.join("install"));
        let result = custom.build(&ctx).unwrap();
        assert!(result.success);

        // Failed command
        let fail_cmd = if cfg!(windows) { "exit /b 1" } else { "exit 1" };
        let custom = CustomBuildSystem::new(dir.clone(), fail_cmd.into());
        let result = custom.build(&ctx).unwrap();
        assert!(!result.success);
        assert!(result.error.is_some());

        let _ = fs::remove_dir_all(&dir);
    }
    #[test]
    fn test_custom_script_mode_does_not_run_build_command() {
        let dir = std::env::temp_dir().join("rez_test_custom_scripts");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let custom = CustomBuildSystem::new(dir.clone(), "rez-custom-command-must-not-run".into());
        let mut ctx = BuildContext::new(dir.clone(), dir.join("build"), dir.join("install"));
        fs::create_dir_all(&ctx.build_path).unwrap();
        ctx.write_build_scripts = true;
        ctx.build_context_path = Some(ctx.build_path.join("build.rxt"));

        let result = custom.build(&ctx).unwrap();
        assert!(result.success);
        assert!(result.install_path.is_none());
        assert!(result
            .build_env_script
            .as_ref()
            .is_some_and(|path| path.exists()));

        let _ = fs::remove_dir_all(&dir);
    }
}
