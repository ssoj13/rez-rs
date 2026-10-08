// SPDX-License-Identifier: Apache-2.0

//! Cargo builds and compiler-reported artifact installation for Rust packages.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde::Deserialize;

use super::native::{self, Artifact, ArtifactRoot};
use super::{run_cmd, BuildContext, BuildResult, BuildSystem, BuildSystemType};
use crate::errors::{Result, RezError};

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Options {
    manifest_path: PathBuf,
    profile: String,
    locked: bool,
    workspace: bool,
    features: Vec<String>,
    bins: Vec<String>,
    artifacts: Vec<Artifact>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            manifest_path: "Cargo.toml".into(),
            profile: "release".into(),
            locked: true,
            workspace: false,
            features: Vec::new(),
            bins: Vec::new(),
            artifacts: Vec::new(),
        }
    }
}

/// Rust Cargo build system.
#[derive(Debug, Clone)]
pub struct CargoBuildSystem {
    pub working_dir: PathBuf,
    /// Path to cargo executable (defaults to "cargo").
    pub cargo_path: String,
}
impl CargoBuildSystem {
    pub fn new(working_dir: PathBuf) -> Self {
        Self {
            working_dir,
            cargo_path: "cargo".into(),
        }
    }
    pub fn is_valid_root(path: &Path) -> bool {
        path.join("Cargo.toml").is_file()
    }

    fn command(&self, ctx: &BuildContext, options: &Options, manifest: &Path) -> Command {
        let mut command = Command::new(&self.cargo_path);
        command
            .current_dir(manifest.parent().expect("manifest has parent"))
            .arg("build")
            .arg("--manifest-path")
            .arg(manifest)
            .arg("--profile")
            .arg(&options.profile)
            .arg("--target-dir")
            .arg(&ctx.build_path)
            .arg("--message-format=json-render-diagnostics");
        if options.locked {
            command.arg("--locked");
        }
        if options.workspace {
            command.arg("--workspace");
        }
        if !options.features.is_empty() {
            command.arg("--features").arg(options.features.join(","));
        }
        for bin in &options.bins {
            command.arg("--bin").arg(bin);
        }
        command.args(&ctx.build_args).envs(&ctx.env_vars);
        command
    }
}

/// Cargo's JSON stream is authoritative: stale output-directory entries are never installed.
fn artifacts(output: &[u8], ctx: &BuildContext, explicit: Vec<Artifact>) -> Result<Vec<Artifact>> {
    let build_root = ctx.build_path.canonicalize()?;
    let mut result = explicit;
    let mut sources: HashSet<PathBuf> = result
        .iter()
        .filter(|artifact| matches!(artifact.from, ArtifactRoot::Build))
        .map(|artifact| artifact.source.clone())
        .collect();
    for line in output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let value: serde_json::Value = serde_json::from_slice(line).map_err(|error| {
            RezError::BuildSystem(format!("Invalid Cargo JSON output: {error}"))
        })?;
        if value.get("reason").and_then(|value| value.as_str()) != Some("compiler-artifact") {
            continue;
        }
        let executable = value.get("executable").and_then(|value| value.as_str());
        let dynamic = value
            .pointer("/target/crate_types")
            .and_then(|value| value.as_array())
            .is_some_and(|types| types.iter().any(|kind| kind == "cdylib" || kind == "dylib"));
        let paths: Vec<&str> = if let Some(executable) = executable {
            vec![executable]
        } else if dynamic {
            value
                .get("filenames")
                .and_then(|value| value.as_array())
                .into_iter()
                .flatten()
                .filter_map(|value| value.as_str())
                .filter(|path| {
                    matches!(
                        Path::new(path).extension().and_then(|value| value.to_str()),
                        Some("dll" | "so" | "dylib")
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        for path in paths {
            let path = Path::new(path).canonicalize()?;
            let relative = path
                .strip_prefix(&build_root)
                .map_err(|_| {
                    RezError::BuildSystem(format!(
                        "Cargo artifact is outside target directory: {}",
                        path.display()
                    ))
                })?
                .to_path_buf();
            if sources.insert(relative.clone()) {
                let name = path.file_name().ok_or_else(|| {
                    RezError::BuildSystem("Cargo artifact has no filename".into())
                })?;
                result.push(Artifact {
                    source: relative,
                    destination: Path::new(if executable.is_some() { "bin" } else { "lib" })
                        .join(name),
                    from: ArtifactRoot::Build,
                });
            }
        }
    }
    Ok(result)
}

impl BuildSystem for CargoBuildSystem {
    fn name(&self) -> &str {
        "cargo"
    }
    fn build_type(&self) -> BuildSystemType {
        BuildSystemType::Cargo
    }
    fn is_valid(&self) -> bool {
        Self::is_valid_root(&self.working_dir)
    }
    fn build(&self, ctx: &BuildContext) -> Result<BuildResult> {
        let start = Instant::now();
        let options: Options = native::options(ctx, "cargo")?;
        native::arguments(
            &ctx.build_args,
            &[
                "--target-dir",
                "--message-format",
                "--manifest-path",
                "--profile",
                "--release",
            ],
            None,
        )?;
        let manifest = native::manifest(ctx, &options.manifest_path)?;
        std::fs::create_dir_all(&ctx.build_path)?;
        let output = match run_cmd(&mut self.command(ctx, &options, &manifest), "cargo build") {
            Ok(output) => output,
            Err(error) => {
                return Ok(BuildResult::fail(
                    ctx.build_path.clone(),
                    start.elapsed().as_secs_f64(),
                    error.to_string(),
                ));
            }
        };
        let prepared_payload = if ctx.install {
            native::install(ctx, &artifacts(&output.stdout, ctx, options.artifacts)?)?
        } else {
            None
        };
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
    fn cargo_command_uses_typed_workspace_release_options() {
        let ctx = BuildContext::new("/source".into(), "/scratch".into(), "/repo".into());
        let options: Options = serde_json::from_value(serde_json::json!({
            "workspace":true, "features":["desktop","python"], "bins":["idle-rs"]
        }))
        .unwrap();
        let command = CargoBuildSystem::new(ctx.source_path.clone()).command(
            &ctx,
            &options,
            Path::new("/source/Cargo.toml"),
        );
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"--locked".into()));
        assert!(args.contains(&"--workspace".into()));
        assert!(args.contains(&"release".into()));
        assert!(args.contains(&"desktop,python".into()));
        assert!(args.contains(&"idle-rs".into()));
    }
    #[test]
    fn cargo_installs_only_reported_artifacts_with_explicit_library_mapping() {
        let temp = tempfile::tempdir().unwrap();
        let build = temp.path().join("build");
        std::fs::create_dir_all(build.join("release")).unwrap();
        let exe = build.join("release/idle-rs.exe");
        let dll = build.join("release/idle_rs.dll");
        std::fs::write(&exe, "exe").unwrap();
        std::fs::write(&dll, "dll").unwrap();
        std::fs::write(build.join("release/stale.exe"), "stale").unwrap();
        let ctx = BuildContext::new(temp.path().into(), build, temp.path().join("install"));
        let messages = [
            serde_json::json!({"reason":"compiler-artifact","executable":exe,"target":{"crate_types":["bin"]}}),
            serde_json::json!({"reason":"compiler-artifact","executable":null,"target":{"crate_types":["cdylib"]},"filenames":[dll]})
        ].into_iter().map(|value|value.to_string()).collect::<Vec<_>>().join("\n");
        let explicit = Artifact {
            source: "release/idle_rs.dll".into(),
            destination: "site-packages/idle_rs.pyd".into(),
            from: ArtifactRoot::Build,
        };
        let artifacts = artifacts(messages.as_bytes(), &ctx, vec![explicit]).unwrap();
        assert_eq!(artifacts.len(), 2);
        assert_eq!(
            artifacts[0].destination,
            Path::new("site-packages/idle_rs.pyd")
        );
        assert_eq!(artifacts[1].destination, Path::new("bin/idle-rs.exe"));
    }
    #[test]
    fn cargo_valid_root() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!CargoBuildSystem::is_valid_root(temp.path()));
        std::fs::write(temp.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
        assert!(CargoBuildSystem::is_valid_root(temp.path()));
    }
}
