// SPDX-License-Identifier: Apache-2.0

//! Shared option decoding and artifact installation for native build adapters.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;

use super::BuildContext;
use crate::errors::{Result, RezError};

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArtifactRoot {
    #[default]
    Build,
    Source,
}

/// Explicit file or directory mapping relative to the build/source and install roots.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Artifact {
    pub source: PathBuf,
    pub destination: PathBuf,
    #[serde(default)]
    pub from: ArtifactRoot,
}

/// Reject passthrough flags that would redirect a managed native output or protocol.
pub(crate) fn arguments(
    args: &[String],
    reserved: &[&str],
    attached: Option<&[&str]>,
) -> Result<()> {
    if let Some(arg) = args
        .iter()
        .take_while(|arg| arg.as_str() != "--")
        .find(|arg| {
            reserved.iter().any(|flag| {
                arg.as_str() == *flag
                    || arg
                        .strip_prefix(*flag)
                        .is_some_and(|tail| tail.starts_with('='))
            }) || attached.is_some_and(|flags| {
                flags
                    .iter()
                    .any(|flag| arg.strip_prefix(*flag).is_some_and(|tail| !tail.is_empty()))
            })
        })
    {
        return Err(RezError::BuildSystem(format!(
            "Native output option {arg} must use package config instead of passthrough arguments"
        )));
    }
    Ok(())
}

pub(crate) fn options<T: serde::de::DeserializeOwned + Default>(
    ctx: &BuildContext,
    backend: &str,
) -> Result<T> {
    match ctx
        .package_config
        .as_ref()
        .and_then(|value| value.get(backend))
    {
        None => Ok(T::default()),
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|error| RezError::BuildSystem(format!("Invalid config.{backend}: {error}"))),
    }
}

/// Select explicit JS artifact mappings, or preserve the optional legacy dist layout.
/// Explicit mappings (including an empty list) replace inferred defaults and are strict.
pub(crate) fn artifacts(ctx: &BuildContext, backend: &str) -> Result<Vec<Artifact>> {
    #[derive(Default, Deserialize)]
    #[serde(default, deny_unknown_fields)]
    struct Options {
        artifacts: Option<Vec<Artifact>>,
    }
    let options: Options = options(ctx, backend)?;
    if let Some(artifacts) = options.artifacts {
        return Ok(artifacts);
    }
    let output = ctx.source_path.join("dist");
    let entries = match std::fs::read_dir(&output) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut entries = entries.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries
        .into_iter()
        .map(|entry| Artifact {
            source: Path::new("dist").join(entry.file_name()),
            destination: entry.file_name().into(),
            from: ArtifactRoot::Source,
        })
        .collect())
}

/// Resolve a required source manifest without allowing package-relative paths to escape.
pub(crate) fn manifest(ctx: &BuildContext, relative: &Path) -> Result<PathBuf> {
    relative_path(relative)?;
    let root = ctx.source_path.canonicalize()?;
    let path = root.join(relative).canonicalize()?;
    if !path.starts_with(&root) || !path.is_file() {
        return Err(RezError::BuildSystem(format!(
            "Native build manifest escapes source or is not a file: {}",
            relative.display()
        )));
    }
    Ok(path)
}

/// Start a fresh adapter-owned staging directory while preserving incremental compiler caches.
pub(crate) fn output(ctx: &BuildContext, name: &str) -> Result<PathBuf> {
    relative_path(Path::new(name))?;
    std::fs::create_dir_all(&ctx.build_path)?;
    let root = ctx.build_path.canonicalize()?;
    let path = root.join(name);
    if path.exists() && !path.canonicalize()?.starts_with(&root) {
        return Err(RezError::BuildSystem(format!(
            "Native staging directory escapes build root: {}",
            path.display()
        )));
    }
    super::prepare_build_dir(&path, true, Some(&ctx.source_path))?;
    Ok(path)
}

fn relative_path(path: &Path) -> Result<()> {
    if !foundation::path::is_safe_rez_path(&path.to_string_lossy(), false) {
        return Err(RezError::BuildSystem(format!(
            "Native artifact path must be a safe relative path: {}",
            path.display()
        )));
    }
    Ok(())
}

fn collect_files(
    source: &Path,
    source_root: &Path,
    destination: &Path,
    files: &mut Vec<(PathBuf, PathBuf, bool)>,
    active: &mut HashSet<PathBuf>,
) -> Result<()> {
    let resolved = source.canonicalize()?;
    if !resolved.starts_with(source_root) {
        return Err(RezError::BuildSystem(format!(
            "Native artifact escapes its root: {}",
            source.display()
        )));
    }
    if resolved.is_file() {
        files.push((resolved, destination.to_path_buf(), false));
    } else if resolved.is_dir() {
        if !active.insert(resolved.clone()) {
            return Err(RezError::BuildSystem(format!(
                "Native artifact directory cycle: {}",
                source.display()
            )));
        }
        files.push((resolved.clone(), destination.to_path_buf(), true));
        let mut entries = std::fs::read_dir(&resolved)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            collect_files(
                &entry.path(),
                source_root,
                &destination.join(entry.file_name()),
                files,
                active,
            )?;
        }
        active.remove(&resolved);
    } else {
        return Err(RezError::BuildSystem(format!(
            "Native artifact is not a regular file or directory: {}",
            source.display()
        )));
    }
    Ok(())
}

/// Preflight and prepare an owned payload; only BuildProcess publishes the final installation.
pub(crate) fn install(
    ctx: &BuildContext,
    artifacts: &[Artifact],
) -> Result<Option<Arc<tempfile::TempDir>>> {
    if !ctx.install {
        return Ok(None);
    }
    let mut files = Vec::new();
    for artifact in artifacts {
        relative_path(&artifact.source)?;
        relative_path(&artifact.destination)?;
        let root = match artifact.from {
            ArtifactRoot::Build => &ctx.build_path,
            ArtifactRoot::Source => &ctx.source_path,
        }
        .canonicalize()?;
        collect_files(
            &root.join(&artifact.source),
            &root,
            &artifact.destination,
            &mut files,
            &mut HashSet::new(),
        )?;
    }
    let mut destinations = HashMap::new();
    for (_, path, is_dir) in &files {
        relative_path(path)?;
        #[cfg(windows)]
        let key = {
            use std::os::windows::ffi::{OsStrExt, OsStringExt};
            let mut lowered = Vec::new();
            for character in char::decode_utf16(path.as_os_str().encode_wide()) {
                match character {
                    Ok(character) => {
                        for lower in character.to_lowercase() {
                            lowered.extend_from_slice(lower.encode_utf16(&mut [0; 2]));
                        }
                    }
                    Err(error) => lowered.push(error.unpaired_surrogate()),
                }
            }
            PathBuf::from(std::ffi::OsString::from_wide(&lowered))
        };
        #[cfg(not(windows))]
        let key = path.clone();
        if destinations
            .insert(key, *is_dir)
            .is_some_and(|previous| !previous || !is_dir)
        {
            return Err(RezError::BuildSystem(format!(
                "Duplicate native artifact destination: {}",
                path.display()
            )));
        }
    }
    // A file destination cannot also be an ancestor of another artifact.
    for path in destinations.keys() {
        let mut ancestor = path.parent();
        while let Some(parent) = ancestor {
            if destinations.get(parent) == Some(&false) {
                return Err(RezError::BuildSystem(format!(
                    "Native artifact file/directory collision: {}",
                    parent.display()
                )));
            }
            ancestor = parent.parent();
        }
    }

    let staged = Arc::new(tempfile::tempdir()?);
    for (source, relative, is_dir) in &files {
        if *is_dir {
            crate::util::directory(staged.path(), relative, true)?;
        } else {
            let parent = relative.parent().unwrap_or_else(|| Path::new(""));
            crate::util::directory(staged.path(), parent, true)?;
            foundation::filesystem::copy_file(source, &staged.path().join(relative), true)?;
        }
    }
    // Restore directory stats after child creation, through the same copy policy.
    for (source, relative, is_dir) in files.iter().rev() {
        if *is_dir {
            foundation::filesystem::copy_timestamps(source, &staged.path().join(relative))?;
        }
    }
    Ok(Some(staged))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_mapping_preflights_missing_and_duplicate_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let build = temp.path().join("build");
        let install_path = temp.path().join("install");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&build).unwrap();
        std::fs::write(build.join("tool"), "binary").unwrap();
        let mut ctx = BuildContext::new(source, build, install_path.clone());
        ctx.install = true;
        let artifact = Artifact {
            source: "tool".into(),
            destination: "bin/tool".into(),
            from: ArtifactRoot::Build,
        };
        let missing = Artifact {
            source: "missing".into(),
            destination: "bin/missing".into(),
            from: ArtifactRoot::Build,
        };
        assert!(install(&ctx, &[artifact.clone(), missing]).is_err());
        assert!(!install_path.exists());
        assert!(install(&ctx, &[artifact.clone(), artifact.clone()]).is_err());
        assert!(!install_path.exists());
        let staged = install(&ctx, &[artifact]).unwrap().unwrap();
        assert!(!install_path.exists());
        assert_eq!(
            std::fs::read_to_string(staged.path().join("bin/tool")).unwrap(),
            "binary"
        );
    }

    #[test]
    fn native_mapping_merges_directories_preserving_empty_children() {
        let temp = tempfile::tempdir().unwrap();
        let build = temp.path().join("build");
        std::fs::create_dir_all(build.join("one/empty")).unwrap();
        std::fs::create_dir_all(build.join("two")).unwrap();
        std::fs::write(build.join("two/tool"), "payload").unwrap();
        let mut ctx = BuildContext::new(temp.path().into(), build, temp.path().join("install"));
        ctx.install = true;
        let artifacts = ["one", "two"].map(|source| Artifact {
            source: source.into(),
            destination: "bin".into(),
            from: ArtifactRoot::Build,
        });
        let staged = install(&ctx, &artifacts).unwrap().unwrap();
        assert!(!ctx.install_path.exists());
        assert!(staged.path().join("bin/empty").is_dir());
        assert_eq!(
            std::fs::read_to_string(staged.path().join("bin/tool")).unwrap(),
            "payload"
        );
    }

    #[test]
    fn native_preparation_preserves_live_payload_and_owned_lifetime() {
        let temp = tempfile::tempdir().unwrap();
        let build = temp.path().join("build");
        let live = temp.path().join("install");
        std::fs::create_dir_all(&build).unwrap();
        std::fs::create_dir_all(live.join("bin")).unwrap();
        std::fs::write(build.join("tool"), "new").unwrap();
        std::fs::write(live.join("bin/tool"), "old").unwrap();
        let mut ctx = BuildContext::new(temp.path().into(), build, live.clone());
        ctx.install = true;
        let artifacts = [Artifact {
            source: "tool".into(),
            destination: "bin/tool".into(),
            from: ArtifactRoot::Build,
        }];
        let staged = install(&ctx, &artifacts).unwrap().unwrap();
        let stage_path = staged.path().to_path_buf();
        assert_eq!(
            std::fs::read_to_string(live.join("bin/tool")).unwrap(),
            "old"
        );
        assert_eq!(
            std::fs::read_to_string(stage_path.join("bin/tool")).unwrap(),
            "new"
        );
        let retained = Arc::clone(&staged);
        drop(staged);
        assert!(stage_path.exists());
        drop(retained);
        assert!(!stage_path.exists());
        ctx.install = false;
        assert!(install(&ctx, &artifacts).unwrap().is_none());
        assert_eq!(ctx.install_path, live);
    }

    #[cfg(unix)]
    #[test]
    fn native_mapping_preserves_distinct_non_utf8_names() {
        use std::os::unix::ffi::OsStringExt;
        let temp = tempfile::tempdir().unwrap();
        let build = temp.path().join("build");
        std::fs::create_dir_all(build.join("payload")).unwrap();
        let names = [0xfe, 0xff].map(|byte| std::ffi::OsString::from_vec(vec![b'f', byte]));
        for (index, name) in names.iter().enumerate() {
            std::fs::write(build.join("payload").join(name), [index as u8]).unwrap();
        }
        let mut ctx = BuildContext::new(temp.path().into(), build, temp.path().join("install"));
        ctx.install = true;
        let staged = install(
            &ctx,
            &[Artifact {
                source: "payload".into(),
                destination: "bin".into(),
                from: ArtifactRoot::Build,
            }],
        )
        .unwrap()
        .unwrap();
        for (index, name) in names.iter().enumerate() {
            assert_eq!(
                std::fs::read(staged.path().join("bin").join(name)).unwrap(),
                [index as u8]
            );
        }
    }

    #[test]
    fn javascript_artifacts_keep_optional_defaults_and_strict_explicit_mappings() {
        let temp = tempfile::tempdir().unwrap();
        let mut ctx = BuildContext::new(
            temp.path().into(),
            temp.path().join("build"),
            temp.path().join("live"),
        );
        ctx.install = true;
        assert!(artifacts(&ctx, "nodejs").unwrap().is_empty());
        std::fs::create_dir(temp.path().join("dist")).unwrap();
        std::fs::write(temp.path().join("dist/output"), "built").unwrap();
        let selected = artifacts(&ctx, "bun").unwrap();
        assert_eq!(selected[0].destination, Path::new("output"));
        ctx.package_config = Some(serde_json::json!({"nodejs":{"artifacts":[]}}));
        assert!(artifacts(&ctx, "nodejs").unwrap().is_empty());
        ctx.package_config = Some(serde_json::json!({"nodejs":{"artifacts":[
            {"from":"source","source":"absent","destination":"bin/tool"}
        ]}}));
        assert!(install(&ctx, &artifacts(&ctx, "nodejs").unwrap()).is_err());
        assert!(!ctx.install_path.exists());
    }

    #[test]
    fn javascript_adapters_prepare_payload_without_touching_live_installation() {
        use crate::builders::{BuildSystem, BunBuildSystem, NodeJsBuildSystem};
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let live = temp.path().join("live");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&live).unwrap();
        std::fs::write(
            source.join("package.json"),
            r#"{"scripts":{"build":"fixture"}}"#,
        )
        .unwrap();
        std::fs::write(live.join("output"), "old").unwrap();
        let tool = temp
            .path()
            .join(if cfg!(windows) { "mock.cmd" } else { "mock" });
        #[cfg(windows)]
        std::fs::write(&tool, "@echo off\r\nif \"%REZ_FIXTURE_FAIL%\"==\"1\" exit /b 1\r\nif \"%1\"==\"run\" (\r\nif not exist dist mkdir dist\r\necho built>dist\\output\r\n)\r\nexit /b 0\r\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(&tool, "#!/bin/sh\nif [ \"$REZ_FIXTURE_FAIL\" = 1 ]; then exit 1; fi\nif [ \"$1\" = run ]; then mkdir -p dist; printf built >dist/output; fi\n").unwrap();
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let adapters: Vec<Box<dyn BuildSystem>> = vec![
            Box::new(NodeJsBuildSystem {
                working_dir: source.clone(),
                npm_path: tool.to_string_lossy().into_owned(),
            }),
            Box::new(BunBuildSystem {
                working_dir: source.clone(),
                bun_path: tool.to_string_lossy().into_owned(),
            }),
        ];
        for adapter in adapters {
            let mut ctx = BuildContext::new(
                source.clone(),
                temp.path().join(adapter.name()),
                live.clone(),
            );
            ctx.install = true;
            let result = adapter.build(&ctx).unwrap();
            assert!(result.success);
            assert_eq!(result.install_path, Some(live.clone()));
            let prepared = result.prepared_payload.unwrap();
            assert_eq!(
                std::fs::read_to_string(prepared.path().join("output"))
                    .unwrap()
                    .trim(),
                "built"
            );
            assert_eq!(std::fs::read_to_string(live.join("output")).unwrap(), "old");
            ctx.env_vars.insert("REZ_FIXTURE_FAIL".into(), "1".into());
            let failed = adapter.build(&ctx).unwrap();
            assert!(!failed.success);
            assert!(failed.prepared_payload.is_none());
            assert_eq!(std::fs::read_to_string(live.join("output")).unwrap(), "old");
        }
    }

    #[test]
    fn native_output_clears_stale_payload_without_clearing_caches() {
        let temp = tempfile::tempdir().unwrap();
        let ctx = BuildContext::new(
            temp.path().into(),
            temp.path().join("build"),
            temp.path().join("install"),
        );
        let staging = output(&ctx, "go-bin").unwrap();
        std::fs::write(staging.join("stale"), "old").unwrap();
        std::fs::write(ctx.build_path.join("cache"), "cached").unwrap();
        output(&ctx, "go-bin").unwrap();
        assert!(!staging.join("stale").exists());
        assert_eq!(
            std::fs::read_to_string(ctx.build_path.join("cache")).unwrap(),
            "cached"
        );
    }

    #[test]
    fn native_arguments_reject_owned_output_options() {
        assert!(arguments(&["--prefix=/outside".into()], &["--prefix"], None).is_err());
        assert!(arguments(
            &["--message-format".into(), "human".into()],
            &["--message-format"],
            None,
        )
        .is_err());
        assert!(arguments(&["--offline".into()], &["--message-format"], None).is_ok());
    }

    #[test]
    fn native_arguments_only_reject_backend_supported_attached_options() {
        assert!(arguments(&["-t/outside".into()], &["-t"], Some(&["-t", "-w"])).is_err());
        assert!(arguments(&["-w/outside".into()], &["-w"], Some(&["-t", "-w"])).is_err());
        assert!(arguments(&["-overlay".into(), "overlay.json".into()], &["-o"], None).is_ok());
        assert!(arguments(
            &["--".into(), "-t/outside".into()],
            &["-t"],
            Some(&["-t", "-w"])
        )
        .is_ok());
    }

    #[test]
    fn native_mapping_rejects_traversal() {
        assert!(relative_path(Path::new("../escape")).is_err());
        assert!(relative_path(Path::new("/escape")).is_err());
        assert!(relative_path(Path::new("bin/tool")).is_ok());
    }
}
