// SPDX-License-Identifier: Apache-2.0

//! Native Zig builds staged before shared Rez package installation.

use super::native::{self, Artifact, ArtifactRoot};
use super::{run_cmd, BuildContext, BuildResult, BuildSystem, BuildSystemType};
use crate::errors::{Result, RezError};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Options {
    manifest_path: PathBuf,
    steps: Vec<String>,
    optimize: String,
    artifacts: Vec<Artifact>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            manifest_path: "build.zig".into(),
            steps: vec!["install".into()],
            optimize: "ReleaseSafe".into(),
            artifacts: Vec::new(),
        }
    }
}
#[derive(Debug, Clone)]
pub struct ZigBuildSystem {
    pub working_dir: PathBuf,
    pub zig_path: String,
}
impl ZigBuildSystem {
    pub fn new(working_dir: PathBuf) -> Self {
        Self {
            working_dir,
            zig_path: "zig".into(),
        }
    }
    pub fn is_valid_root(path: &Path) -> bool {
        path.join("build.zig").is_file()
    }
    fn command(
        &self,
        ctx: &BuildContext,
        options: &Options,
        manifest: &Path,
        prefix: &Path,
    ) -> Result<Command> {
        if !matches!(
            options.optimize.as_str(),
            "Debug" | "ReleaseSafe" | "ReleaseFast" | "ReleaseSmall"
        ) {
            return Err(RezError::BuildSystem(format!(
                "Invalid Zig optimization mode: {}",
                options.optimize
            )));
        }
        let mut command = Command::new(&self.zig_path);
        command
            .current_dir(manifest.parent().expect("manifest has parent"))
            .arg("build")
            .arg("--build-file")
            .arg(manifest)
            .args(&options.steps)
            .arg(format!("-Doptimize={}", options.optimize))
            .arg("--prefix")
            .arg(prefix)
            .arg("--cache-dir")
            .arg(
                prefix
                    .parent()
                    .expect("prefix has parent")
                    .join("zig-cache"),
            )
            .arg("--global-cache-dir")
            .arg(
                prefix
                    .parent()
                    .expect("prefix has parent")
                    .join("zig-global-cache"),
            )
            .args(&ctx.build_args)
            .envs(&ctx.env_vars);
        Ok(command)
    }
}
impl BuildSystem for ZigBuildSystem {
    fn name(&self) -> &str {
        "zig"
    }
    fn build_type(&self) -> BuildSystemType {
        BuildSystemType::Zig
    }
    fn is_valid(&self) -> bool {
        Self::is_valid_root(&self.working_dir)
    }
    fn build(&self, ctx: &BuildContext) -> Result<BuildResult> {
        let start = Instant::now();
        let options: Options = native::options(ctx, "zig")?;
        native::arguments(
            &ctx.build_args,
            &[
                "--prefix",
                "-p",
                "--prefix-exe-dir",
                "--prefix-lib-dir",
                "--prefix-include-dir",
                "--cache-dir",
                "--global-cache-dir",
                "--build-file",
                "-Doptimize",
            ],
            None,
        )?;
        let manifest = native::manifest(ctx, &options.manifest_path)?;
        let prefix = native::output(ctx, "zig-out")?;
        if let Err(error) = run_cmd(
            &mut self.command(ctx, &options, &manifest, &prefix)?,
            "zig build",
        ) {
            return Ok(BuildResult::fail(
                ctx.build_path.clone(),
                start.elapsed().as_secs_f64(),
                error.to_string(),
            ));
        }
        let mut artifacts = options.artifacts;
        // Preserve the Zig install layout, including lib/include/share, rather than assuming bin only.
        let mut entries = std::fs::read_dir(&prefix)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            artifacts.push(Artifact {
                source: Path::new("zig-out").join(entry.file_name()),
                destination: entry.file_name().into(),
                from: ArtifactRoot::Build,
            });
        }
        let prepared_payload = native::install(ctx, &artifacts)?;
        let mut result = BuildResult::ok(ctx.build_path.clone(), start.elapsed().as_secs_f64());
        result.prepared_payload = prepared_payload;
        result.mark_installed(ctx);
        Ok(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn zig_prefix_and_caches_are_scratch_owned() {
        let ctx = BuildContext::new("/source".into(), "/scratch".into(), "/repo".into());
        let adapter = ZigBuildSystem::new(ctx.source_path.clone());
        let command = adapter
            .command(
                &ctx,
                &Options::default(),
                Path::new("/source/build.zig"),
                Path::new("/scratch/zig-out"),
            )
            .unwrap();
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"-Doptimize=ReleaseSafe".into()));
        assert!(args.contains(&"/scratch/zig-out".into()));
        assert!(!args.contains(&"/repo".into()));
        let invalid = Options {
            optimize: "invalid".into(),
            ..Options::default()
        };
        assert!(adapter
            .command(
                &ctx,
                &invalid,
                Path::new("/source/build.zig"),
                Path::new("/scratch/zig-out")
            )
            .is_err());
    }
}
