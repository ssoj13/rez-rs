// SPDX-License-Identifier: Apache-2.0

//! Native Go module builds using shared Rez build destinations.

use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use super::native::{self, Artifact, ArtifactRoot};
use super::{run_cmd, BuildContext, BuildResult, BuildSystem, BuildSystemType};
use crate::errors::Result;

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Options {
    manifest_path: PathBuf,
    targets: Vec<String>,
    ldflags: Option<String>,
    tags: Vec<String>,
    cgo: bool,
    artifacts: Vec<Artifact>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            manifest_path: "go.mod".into(),
            targets: vec!["./...".into()],
            ldflags: None,
            tags: Vec::new(),
            cgo: false,
            artifacts: Vec::new(),
        }
    }
}

/// Builds a local Go module. Ecosystem module installation is a separate operation.
#[derive(Debug, Clone)]
pub struct GoBuildSystem {
    pub working_dir: PathBuf,
    pub go_path: String,
}
impl GoBuildSystem {
    pub fn new(working_dir: PathBuf) -> Self {
        Self {
            working_dir,
            go_path: "go".into(),
        }
    }
    pub fn is_valid_root(path: &Path) -> bool {
        path.join("go.mod").is_file()
    }
    fn command(
        &self,
        ctx: &BuildContext,
        options: &Options,
        manifest: &Path,
        output: &Path,
    ) -> Command {
        let mut command = Command::new(&self.go_path);
        // Go requires an existing directory (or trailing separator) for multiple main packages.
        command
            .current_dir(manifest.parent().expect("manifest has parent"))
            .arg("build")
            .arg("-o")
            .arg(output);
        if let Some(flags) = &options.ldflags {
            command.arg("-ldflags").arg(flags);
        }
        if !options.tags.is_empty() {
            command.arg("-tags").arg(options.tags.join(","));
        }
        command
            .args(&ctx.build_args)
            .args(&options.targets)
            .envs(&ctx.env_vars)
            .env(
                "GOCACHE",
                output.parent().expect("output has parent").join("go-cache"),
            )
            .env(
                "GOPATH",
                output.parent().expect("output has parent").join("go-path"),
            );
        if !options.cgo
            && !ctx.env_vars.contains_key("CGO_ENABLED")
            && std::env::var_os("CGO_ENABLED").is_none()
        {
            command.env("CGO_ENABLED", "0");
        }
        command
    }
}
impl BuildSystem for GoBuildSystem {
    fn name(&self) -> &str {
        "go"
    }
    fn build_type(&self) -> BuildSystemType {
        BuildSystemType::Go
    }
    fn is_valid(&self) -> bool {
        Self::is_valid_root(&self.working_dir)
    }
    fn build(&self, ctx: &BuildContext) -> Result<BuildResult> {
        let start = Instant::now();
        let options: Options = native::options(ctx, "go")?;
        native::arguments(&ctx.build_args, &["-o"], None)?;
        let manifest = native::manifest(ctx, &options.manifest_path)?;
        let output = native::output(ctx, "go-bin")?;
        if let Err(error) = run_cmd(
            &mut self.command(ctx, &options, &manifest, &output),
            "go build",
        ) {
            return Ok(BuildResult::fail(
                ctx.build_path.clone(),
                start.elapsed().as_secs_f64(),
                error.to_string(),
            ));
        }
        let mut artifacts = options.artifacts;
        artifacts.push(Artifact {
            source: "go-bin".into(),
            destination: "bin".into(),
            from: ArtifactRoot::Build,
        });
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
    fn go_options_preserve_source_targets_and_payload() {
        let options: Options = serde_json::from_value(serde_json::json!({
            "manifest_path":"src/go.mod", "targets":["./..."], "tags":["desktop","release"],
            "artifacts":[{"from":"source","source":"payload/AMI.py","destination":"bin/AMI"}]
        }))
        .unwrap();
        let ctx = BuildContext::new("/source".into(), "/scratch".into(), "/repo".into());
        let command = GoBuildSystem::new(ctx.source_path.clone()).command(
            &ctx,
            &options,
            Path::new("/source/src/go.mod"),
            Path::new("/scratch/go-bin"),
        );
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "build",
                "-o",
                "/scratch/go-bin",
                "-tags",
                "desktop,release",
                "./..."
            ]
        );
        assert_eq!(command.get_current_dir(), Some(Path::new("/source/src")));
        assert_eq!(options.artifacts[0].destination, Path::new("bin/AMI"));
    }
}
