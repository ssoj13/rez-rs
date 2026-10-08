// SPDX-License-Identifier: Apache-2.0

//! CMake build system implementation.

use super::native::{self, Artifact, ArtifactRoot};
use path_slash::PathExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::{
    create_build_env_script, run_cmd, BuildContext, BuildResult, BuildSystem, BuildSystemType,
};
use crate::errors::{Result, RezError};

/// CMake-based build system.
///
/// Detects CMakeLists.txt, runs cmake configure + build, optionally install.
#[derive(Debug, Clone)]
pub struct CMakeBuildSystem {
    /// Working directory containing CMakeLists.txt
    pub working_dir: PathBuf,
    /// Path to cmake executable (defaults to "cmake")
    pub cmake_path: String,
}

impl CMakeBuildSystem {
    /// Create a CMake build system for the given source directory.
    pub fn new(working_dir: PathBuf) -> Self {
        Self {
            working_dir,
            cmake_path: "cmake".into(),
        }
    }

    /// Check if the directory has a CMakeLists.txt.
    pub fn is_valid_root(path: &Path) -> bool {
        path.join("CMakeLists.txt").is_file()
    }

    /// Run configure with a logical runtime prefix and a persistent owned staging root.
    fn configure(&self, ctx: &BuildContext, staged: &Path) -> Result<()> {
        let guarded = ctx
            .build_args
            .iter()
            .filter(|arg| arg.as_str() != "--")
            .cloned()
            .collect::<Vec<_>>();
        native::arguments(
            &guarded,
            &[
                "-S",
                "-B",
                "-H",
                "-P",
                "--build",
                "--install",
                "--workflow",
                "--find-package",
                "--install-prefix",
            ],
            Some(&["-S", "-B", "-H", "-P"]),
        )?;
        let mut arguments = ctx.build_args.iter();
        while let Some(argument) = arguments.next() {
            let definition = if argument == "-D" {
                arguments.next().map(String::as_str)
            } else {
                argument.strip_prefix("-D")
            };
            if let Some(definition) = definition {
                let name = definition
                    .split_once('=')
                    .map(|(name, _)| name)
                    .unwrap_or(definition);
                let name = name.split(':').next().unwrap_or(name);
                if matches!(
                    name,
                    "CMAKE_INSTALL_PREFIX"
                        | "CMAKE_STAGING_PREFIX"
                        | "CMAKE_ERROR_ON_ABSOLUTE_INSTALL_DESTINATION"
                ) {
                    return Err(RezError::BuildSystem(format!(
                        "CMake {name} is managed by the installation context"
                    )));
                }
            }
        }
        let logical = std::path::absolute(&ctx.install_path)?;
        let source = std::path::absolute(&ctx.source_path)?;
        let build = std::path::absolute(&ctx.build_path)?;
        let mut cmd = super::pip_utils::command(&self.cmake_path, ctx)?;
        cmd.current_dir(&source)
            .args(&ctx.build_args)
            .arg("-S")
            .arg(&source)
            .arg("-B")
            .arg(&build)
            .arg(format!(
                "-DCMAKE_INSTALL_PREFIX:PATH={}",
                logical.to_slash_lossy()
            ))
            .arg(format!(
                "-DCMAKE_STAGING_PREFIX:PATH={}",
                staged.to_slash_lossy()
            ));
        run_cmd(&mut cmd, "cmake configure")?;
        Ok(())
    }

    /// Run cmake build step.
    fn build_step(&self, ctx: &BuildContext) -> Result<()> {
        let mut cmd = super::pip_utils::command(&self.cmake_path, ctx)?;
        cmd.arg("--build")
            .arg(std::path::absolute(&ctx.build_path)?)
            .arg("--config")
            .arg("Release")
            .arg("--parallel")
            .arg(ctx.build_threads.to_string());
        if !ctx.child_build_args.is_empty() {
            cmd.arg("--");
            for arg in &ctx.child_build_args {
                cmd.arg(arg);
            }
        }
        run_cmd(&mut cmd, "cmake build")?;
        Ok(())
    }

    /// Execute CMake's generated install script, rejecting absolute install destinations.
    /// Custom install(CODE/SCRIPT) commands retain their own external side effects.
    fn install_step(&self, ctx: &BuildContext) -> Result<()> {
        let mut cmd = super::pip_utils::command(&self.cmake_path, ctx)?;
        let source = std::path::absolute(&ctx.source_path)?;
        let script = std::path::absolute(ctx.build_path.join("cmake_install.cmake"))?;
        cmd.current_dir(&source)
            .arg("-DCMAKE_INSTALL_CONFIG_NAME=Release")
            .arg("-DCMAKE_ERROR_ON_ABSOLUTE_INSTALL_DESTINATION=ON")
            .arg("-P")
            .arg(script)
            .env_remove("DESTDIR");
        run_cmd(&mut cmd, "cmake install")?;
        Ok(())
    }
}

impl BuildSystem for CMakeBuildSystem {
    fn name(&self) -> &str {
        "cmake"
    }
    fn build_type(&self) -> BuildSystemType {
        BuildSystemType::CMake
    }

    fn is_valid(&self) -> bool {
        Self::is_valid_root(&self.working_dir)
    }

    fn supports_build_scripts(&self) -> bool {
        true
    }

    fn write_build_scripts(&self, ctx: &BuildContext) -> Result<PathBuf> {
        create_build_env_script(ctx)
    }

    fn build(&self, ctx: &BuildContext) -> Result<BuildResult> {
        let start = Instant::now();

        // Ensure build directory exists
        std::fs::create_dir_all(&ctx.build_path).map_err(|e| {
            RezError::BuildSystem(format!(
                "Failed to create build dir {}: {}",
                ctx.build_path.display(),
                e
            ))
        })?;

        // Keep staging persistent for the interactive launcher and cached configure.
        native::manifest(ctx, Path::new("CMakeLists.txt"))?;
        native::output(ctx, "cmake-install")?;
        let staged = std::path::absolute(ctx.build_path.join("cmake-install"))?;
        // Configure
        if let Err(e) = self.configure(ctx, &staged) {
            let elapsed = start.elapsed().as_secs_f64();
            return Ok(BuildResult::fail(
                ctx.build_path.clone(),
                elapsed,
                e.to_string(),
            ));
        }
        if ctx.write_build_scripts {
            let script = self.write_build_scripts(ctx)?;
            let mut result = BuildResult::ok(ctx.build_path.clone(), start.elapsed().as_secs_f64());
            result.build_env_script = Some(script);
            return Ok(result);
        }

        // Build
        if let Err(e) = self.build_step(ctx) {
            let elapsed = start.elapsed().as_secs_f64();
            return Ok(BuildResult::fail(
                ctx.build_path.clone(),
                elapsed,
                e.to_string(),
            ));
        }

        // Install (if requested)
        if ctx.install {
            if let Err(e) = self.install_step(ctx) {
                let elapsed = start.elapsed().as_secs_f64();
                return Ok(BuildResult::fail(
                    ctx.build_path.clone(),
                    elapsed,
                    e.to_string(),
                ));
            }
        }

        let prepared_payload = if ctx.install {
            let mut entries = std::fs::read_dir(&staged)?.collect::<std::io::Result<Vec<_>>>()?;
            entries.sort_by_key(|entry| entry.file_name());
            let artifacts = entries
                .into_iter()
                .map(|entry| Artifact {
                    source: Path::new("cmake-install").join(entry.file_name()),
                    destination: entry.file_name().into(),
                    from: ArtifactRoot::Build,
                })
                .collect::<Vec<_>>();
            native::install(ctx, &artifacts)?
        } else {
            None
        };
        let mut result = BuildResult::ok(ctx.build_path.clone(), start.elapsed().as_secs_f64());
        result.prepared_payload = prepared_payload;
        result.mark_installed(ctx);
        Ok(result)
    }

    fn child_build_system(&self) -> Option<BuildSystemType> {
        // cmake generates Makefiles by default on unix
        Some(BuildSystemType::Make)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_cmake_detection_and_build() {
        let owned = tempfile::tempdir().unwrap();
        let dir = owned.path().to_path_buf();

        // No CMakeLists.txt -> not valid
        assert!(!CMakeBuildSystem::is_valid_root(&dir));

        // With CMakeLists.txt -> valid, child is make
        fs::write(
            dir.join("CMakeLists.txt"),
            "cmake_minimum_required(VERSION 3.10)\nproject(test)",
        )
        .unwrap();
        let cmake = CMakeBuildSystem::new(dir.clone());
        assert!(cmake.is_valid());
        assert_eq!(cmake.child_build_system(), Some(BuildSystemType::Make));
    }

    #[test]
    fn test_cmake_script_mode_writes_build_launcher() {
        let owned = tempfile::tempdir().unwrap();
        let dir = owned.path().to_path_buf();

        let mut ctx = BuildContext::new(dir.clone(), dir.join("build"), dir.join("install"));
        fs::create_dir_all(&ctx.build_path).unwrap();
        ctx.write_build_scripts = true;
        ctx.build_context_path = Some(ctx.build_path.join("build.rxt"));
        ctx.env_vars
            .insert("REZ_BUILD_PROJECT_NAME".into(), "cmake-script-test".into());

        let cmake = CMakeBuildSystem::new(dir.clone());
        let script = cmake.write_build_scripts(&ctx).unwrap();
        assert!(script.exists());
        assert!(script
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("build-env"));
        assert!(fs::read_to_string(&script)
            .unwrap()
            .contains("REZ_BUILD_PROJECT_NAME"));
    }
    #[cfg(unix)]
    #[test]
    fn test_cmake_script_mode_configures_without_building_or_installing() {
        use std::os::unix::fs::PermissionsExt;

        let owned = tempfile::tempdir().unwrap();
        let dir = owned.path().to_path_buf();
        fs::write(
            dir.join("CMakeLists.txt"),
            "cmake_minimum_required(VERSION 3.10)\nproject(fixture NONE)",
        )
        .unwrap();
        let log = dir.join("cmake.log");
        let fake_cmake = dir.join("cmake");
        fs::write(
            &fake_cmake,
            format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", log.display()),
        )
        .unwrap();
        fs::set_permissions(&fake_cmake, fs::Permissions::from_mode(0o755)).unwrap();

        let mut ctx = BuildContext::new(dir.clone(), dir.join("build"), dir.join("install"));
        fs::create_dir_all(&ctx.build_path).unwrap();
        ctx.write_build_scripts = true;
        ctx.install = true;
        ctx.build_context_path = Some(ctx.build_path.join("build.rxt"));
        ctx.env_vars
            .insert("REZ_BUILD_PROJECT_NAME".into(), "cmake-order".into());

        let mut cmake = CMakeBuildSystem::new(dir.clone());
        cmake.cmake_path = fake_cmake.to_string_lossy().into_owned();
        let result = cmake.build(&ctx).unwrap();
        assert!(result.success);
        assert!(result.install_path.is_none());
        assert!(result.build_env_script.is_some());

        let invocation = fs::read_to_string(log).unwrap();
        assert!(invocation.contains("-S"));
        assert!(invocation.contains("-DCMAKE_INSTALL_PREFIX:PATH="));
        assert!(!invocation.contains("--build"));
        assert!(!invocation.contains("--install"));
    }
    #[test]
    fn managed_cmake_definitions_and_mode_redirects_cannot_override_owned_outputs() {
        let owned = tempfile::tempdir().unwrap();
        let source = owned.path().join("source");
        fs::create_dir(&source).unwrap();
        let mut ctx = BuildContext::new(
            source.clone(),
            owned.path().join("build"),
            owned.path().join("live"),
        );
        let builder = CMakeBuildSystem::new(source);
        let staged = owned.path().join("staged");
        for args in [
            vec!["-DCMAKE_INSTALL_PREFIX=foreign"],
            vec!["-DCMAKE_STAGING_PREFIX:PATH=foreign"],
            vec!["-D", "CMAKE_INSTALL_PREFIX:PATH=foreign"],
            vec!["-D", "CMAKE_ERROR_ON_ABSOLUTE_INSTALL_DESTINATION:BOOL=OFF"],
            vec!["-Bforeign"],
            vec!["-S", "foreign"],
            vec!["--install-prefix=foreign"],
            vec!["-Pforeign.cmake"],
            vec!["--", "-Bforeign"],
        ] {
            ctx.build_args = args.iter().map(|value| (*value).to_string()).collect();
            assert!(builder.configure(&ctx, &staged).is_err(), "{args:?}");
        }
        assert!(!ctx.install_path.exists());
        assert!(!staged.exists());
    }

    #[test]
    fn staged_cmake_payload_reuses_native_publication_boundary() {
        let owned = tempfile::tempdir().unwrap();
        let source = owned.path().join("source");
        fs::create_dir(&source).unwrap();
        let mut ctx = BuildContext::new(
            source,
            owned.path().join("build"),
            owned.path().join("live"),
        );
        let staged = native::output(&ctx, "cmake-install").unwrap();
        fs::create_dir(staged.join("share")).unwrap();
        fs::write(
            staged.join("share/prefix.txt"),
            ctx.install_path.to_slash_lossy().as_bytes(),
        )
        .unwrap();
        fs::create_dir(&ctx.install_path).unwrap();
        fs::write(ctx.install_path.join("valuable"), "original").unwrap();
        ctx.install = true;
        let prepared = native::install(
            &ctx,
            &[Artifact {
                source: "cmake-install/share".into(),
                destination: "share".into(),
                from: ArtifactRoot::Build,
            }],
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            fs::read_to_string(prepared.path().join("share/prefix.txt")).unwrap(),
            ctx.install_path.to_slash_lossy()
        );
        assert_eq!(
            fs::read_to_string(ctx.install_path.join("valuable")).unwrap(),
            "original"
        );
        assert!(!ctx.install_path.join("share").exists());
    }
    #[test]
    #[ignore = "requires native CMake and Ninja; runs in an owned child working directory"]
    fn relative_context_paths_resolve_once_for_every_cmake_phase() {
        const CHILD: &str = "REZ_RS_CMAKE_RELATIVE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let owned = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "builders::cmake::tests::relative_context_paths_resolve_once_for_every_cmake_phase", "--ignored", "--nocapture"])
                .env(CHILD, "1").current_dir(owned.path()).output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed"),
                "child fixture must execute exactly one test"
            );
            return;
        }
        fs::create_dir("source").unwrap();
        fs::create_dir("live").unwrap();
        fs::write("live/valuable", "original").unwrap();
        fs::write(
            "source/CMakeLists.txt",
            r#"cmake_minimum_required(VERSION 3.20)
project(relative NONE)
file(WRITE "${CMAKE_CURRENT_BINARY_DIR}/prefix.txt" "${CMAKE_INSTALL_PREFIX}")
install(FILES "${CMAKE_CURRENT_BINARY_DIR}/prefix.txt" DESTINATION share)
"#,
        )
        .unwrap();
        let mut ctx = BuildContext::new("source".into(), "build".into(), "live".into());
        ctx.install = true;
        ctx.build_args = vec!["-G".into(), "Ninja".into()];
        let mut builder = CMakeBuildSystem::new("source".into());
        builder.cmake_path = std::env::var("REZ_TEST_CMAKE").unwrap_or_else(|_| "cmake".into());
        let result = builder.build(&ctx).unwrap();
        assert!(result.success, "{:?}", result.error);
        assert!(Path::new("build/CMakeCache.txt").is_file());
        assert!(!Path::new("source/build").exists());
        let prepared = result.prepared_payload.unwrap();
        assert_eq!(
            fs::read_to_string(prepared.path().join("share/prefix.txt")).unwrap(),
            std::path::absolute("live").unwrap().to_slash_lossy()
        );
        assert_eq!(fs::read_to_string("live/valuable").unwrap(), "original");
        assert!(!Path::new("live/share").exists());
        assert!(!Path::new("source/source").exists());
    }
}
