// SPDX-License-Identifier: Apache-2.0

//! Build systems - cmake, make, python, pip, cargo, nodejs, bun.
//!
//! Ported from Python rez build_system.py and build_process.py.

pub mod bun;
pub mod cargo_build;
pub mod cmake;
pub mod conan;
pub mod custom;
pub(crate) mod download;
pub mod extraction;
pub mod go;
pub mod make;
pub mod metadata;
pub(crate) mod native;
pub mod nodejs;
pub mod noop;
pub mod pip;
pub(crate) mod pip_utils;
pub mod python;
pub mod scons;
pub mod vcpkg;
pub mod zig;

// Re-export all builder structs for convenience
use crate::config::CONFIG;
use crate::shell::types::{detect_shell, ShellType};
use crate::shell::wrapper::{create_forwarding_script, ForwardingScriptOptions};
pub use bun::BunBuildSystem;
pub use cargo_build::CargoBuildSystem;
pub use cmake::CMakeBuildSystem;
pub use conan::ConanBuildSystem;
pub use custom::CustomBuildSystem;
pub use extraction::ExtractionBuildSystem;
pub use go::GoBuildSystem;
pub use make::MakeBuildSystem;
pub use nodejs::NodeJsBuildSystem;
pub use noop::NoOpBuildSystem;
pub use pip::PipBuildSystem;
pub use python::PythonBuildSystem;
pub use scons::SConsBuildSystem;
pub use vcpkg::VcpkgBuildSystem;
pub use zig::ZigBuildSystem;

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::constants::BuildType;
use crate::errors::{Result, RezError};
use crate::package::{DeveloperPackage, Package};
use crate::{log_debug, log_info};
use resolve::context::{ResolvedContext, RexExecutionCallback};

// ---------------------------------------------------------------------------
// BuildSystemType - enum of known build systems
// ---------------------------------------------------------------------------

/// Available build system types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuildSystemType {
    CMake,
    Make,
    Python,
    Pip,
    Cargo,
    Go,
    Zig,
    NodeJs,
    Bun,
    SCons,
    Vcpkg,
    Conan,
    Extraction,
    Custom,
    NoOp,
}

impl BuildSystemType {
    /// Human-readable name of the build system.
    pub fn name(&self) -> &str {
        match self {
            Self::CMake => "cmake",
            Self::Make => "make",
            Self::Python => "python",
            Self::Pip => "pip",
            Self::Cargo => "cargo",
            Self::Go => "go",
            Self::Zig => "zig",
            Self::NodeJs => "nodejs",
            Self::Bun => "bun",
            Self::SCons => "scons",
            Self::Vcpkg => "vcpkg",
            Self::Conan => "conan",
            Self::Extraction => "extraction",
            Self::Custom => "custom",
            Self::NoOp => "noop",
        }
    }

    /// Detect build system from files in the given directory.
    /// Returns None if no known build system is detected.
    pub fn detect(path: &Path) -> Option<Self> {
        // Detect in priority order: cmake, cargo, python, bun, nodejs, waf, scons, make
        if path.join("CMakeLists.txt").exists() {
            return Some(Self::CMake);
        }
        if path.join("Cargo.toml").exists() {
            return Some(Self::Cargo);
        }
        if GoBuildSystem::is_valid_root(path) {
            return Some(Self::Go);
        }
        if ZigBuildSystem::is_valid_root(path) {
            return Some(Self::Zig);
        }
        // PEP 517 projects and standalone requirements files use Pip.
        if PipBuildSystem::is_valid_root(path) {
            return Some(Self::Pip);
        }
        // Legacy Python projects use setup.py only when no valid PEP 517
        // build-system declaration takes precedence.
        if PythonBuildSystem::is_valid_root(path) {
            return Some(Self::Python);
        }
        // Bun: package.json + (bunfig.toml OR bun.lockb)
        let has_package_json = path.join("package.json").exists();
        if has_package_json {
            if path.join("bunfig.toml").exists() || path.join("bun.lockb").exists() {
                return Some(Self::Bun);
            }
            // Generic package.json -> NodeJs
            return Some(Self::NodeJs);
        }
        if SConsBuildSystem::is_valid_root(path) {
            return Some(Self::SCons);
        }
        if VcpkgBuildSystem::is_valid_root(path) {
            return Some(Self::Vcpkg);
        }
        if ConanBuildSystem::is_valid_root(path) {
            return Some(Self::Conan);
        }
        if ExtractionBuildSystem::is_valid_root(path) {
            return Some(Self::Extraction);
        }
        // Check both casings for Makefile
        if path.join("Makefile").exists() || path.join("makefile").exists() {
            return Some(Self::Make);
        }
        None
    }

    /// All known build system types.
    pub fn all() -> &'static [BuildSystemType] {
        &[
            Self::CMake,
            Self::Make,
            Self::Python,
            Self::Pip,
            Self::Cargo,
            Self::Go,
            Self::Zig,
            Self::NodeJs,
            Self::Bun,
            Self::SCons,
            Self::Vcpkg,
            Self::Conan,
            Self::Extraction,
            Self::Custom,
            Self::NoOp,
        ]
    }
}

impl fmt::Display for BuildSystemType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

// ---------------------------------------------------------------------------
// BuildContext - input parameters for a build
// ---------------------------------------------------------------------------

/// All inputs needed to perform a build.
#[derive(Debug, Clone)]
pub struct BuildContext {
    /// Path to package source directory
    pub source_path: PathBuf,
    /// Path to build output directory
    pub build_path: PathBuf,
    /// Path to install destination
    pub install_path: PathBuf,
    /// Local or central build
    pub build_type: BuildType,
    /// Which variant to build (None = default/only variant)
    pub variant_index: Option<usize>,
    /// Extra arguments passed to the build system
    pub build_args: Vec<String>,
    /// Extra arguments for child build system (e.g. make args under cmake)
    pub child_build_args: Vec<String>,
    /// Whether to actually install after building
    pub install: bool,
    /// Whether the selected build system should generate an interactive build launcher.
    pub write_build_scripts: bool,
    /// Serialized build context consumed by the generated launcher.
    pub build_context_path: Option<PathBuf>,
    /// Number of parallel build threads
    pub build_threads: usize,
    /// Extra environment variables to set during build
    pub env_vars: HashMap<String, String>,
    /// Package config (for extraction build system: config.extraction.downloads)
    pub package_config: Option<serde_json::Value>,
}

impl BuildContext {
    /// Create a minimal build context with source, build and install paths.
    pub fn new(source_path: PathBuf, build_path: PathBuf, install_path: PathBuf) -> Self {
        Self {
            source_path,
            build_path,
            install_path,
            build_type: BuildType::Local,
            variant_index: None,
            build_args: Vec::new(),
            child_build_args: Vec::new(),
            install: false,
            write_build_scripts: false,
            build_context_path: None,
            build_threads: num_build_threads(),
            env_vars: HashMap::new(),
            package_config: None,
        }
    }
}

// ---------------------------------------------------------------------------
// BuildResult - output from a build
// ---------------------------------------------------------------------------

/// Result of a build or install operation.
#[derive(Debug, Clone)]
pub struct BuildResult {
    /// Whether the build succeeded
    pub success: bool,
    /// Path where build artifacts were written
    pub build_path: PathBuf,
    /// Path where package was installed (if install was requested)
    pub install_path: Option<PathBuf>,
    /// Owned payload prepared for the shared locked publisher, not yet installed.
    pub prepared_payload: Option<std::sync::Arc<tempfile::TempDir>>,
    /// Optional launcher that opens the resolved build environment.
    pub build_env_script: Option<PathBuf>,
    /// Wall-clock seconds elapsed
    pub elapsed_secs: f64,
    /// Error message if build failed
    pub error: Option<String>,
    /// Extra files created during the build (e.g. build.rxt)
    pub extra_files: Vec<PathBuf>,
}

impl BuildResult {
    /// Create a successful result.
    pub fn ok(build_path: PathBuf, elapsed_secs: f64) -> Self {
        Self {
            success: true,
            build_path,
            install_path: None,
            prepared_payload: None,
            elapsed_secs,
            error: None,
            extra_files: Vec::new(),
            build_env_script: None,
        }
    }

    /// Create a failed result.
    pub fn fail(build_path: PathBuf, elapsed_secs: f64, error: String) -> Self {
        Self {
            success: false,
            build_path,
            install_path: None,
            prepared_payload: None,
            elapsed_secs,
            error: Some(error),
            build_env_script: None,
            extra_files: Vec::new(),
        }
    }

    /// Set install_path if the context requested an install.
    ///
    /// Called at the end of every builder's `build()` to avoid repeating
    /// the `if ctx.install { result.install_path = Some(...) }` block.
    pub fn mark_installed(&mut self, ctx: &BuildContext) {
        if ctx.install {
            self.install_path = Some(ctx.install_path.clone());
        }
    }
}

// ---------------------------------------------------------------------------
// Shared build command helper
// ---------------------------------------------------------------------------

/// Run a build command, check exit status, return error with stderr on failure.
/// Reduces boilerplate across all build system implementations.
pub(crate) fn run_cmd(cmd: &mut Command, label: &str) -> Result<std::process::Output> {
    let output = cmd
        .output()
        .map_err(|e| RezError::BuildSystem(format!("Failed to run {}: {}", label, e)))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(RezError::BuildSystem(format!(
            "{} failed (exit {}): {}",
            label,
            output.status.code().unwrap_or(-1),
            stderr.trim()
        )));
    }
    Ok(output)
}

// ---------------------------------------------------------------------------
// BuildSystem trait
// ---------------------------------------------------------------------------

/// Interface for build system implementations.
///
/// Each build system (cmake, make, custom, noop) implements this trait
/// to provide build and install functionality.
pub trait BuildSystem: fmt::Debug + Send + Sync {
    /// Name of this build system (e.g. "cmake").
    fn name(&self) -> &str;

    /// Type enum value.
    fn build_type(&self) -> BuildSystemType;

    /// Check if the working directory is valid for this build system.
    fn is_valid(&self) -> bool;

    /// Perform the build step.
    fn build(&self, ctx: &BuildContext) -> Result<BuildResult>;

    /// Whether this adapter implements Rez's build-script mode.
    fn supports_build_scripts(&self) -> bool {
        false
    }

    /// Create an interactive build-environment launcher.
    ///
    /// Adapters without an upstream script implementation return a typed error.
    fn write_build_scripts(&self, _ctx: &BuildContext) -> Result<PathBuf> {
        Err(RezError::BuildSystem(format!(
            "build scripts are not implemented for the '{}' build system",
            self.name()
        )))
    }

    /// Perform the install step. Default calls build with install=true.
    fn install(&self, ctx: &BuildContext) -> Result<BuildResult> {
        let mut install_ctx = ctx.clone();
        install_ctx.install = true;
        self.build(&install_ctx)
    }

    /// Child build system, if any. E.g. cmake generates Makefiles -> make is child.
    fn child_build_system(&self) -> Option<BuildSystemType> {
        None
    }
}

/// Freeze build controls and explicitly allow them through clean-shell policy.
fn build_launcher_environment(
    ctx: &BuildContext,
    config: &crate::config::RezConfig,
) -> HashMap<String, String> {
    let mut environment: HashMap<String, String> = ctx
        .env_vars
        .iter()
        .filter(|(name, _)| {
            name.starts_with("REZ_BUILD_")
                || crate::config::RezConfig::is_recipe_environment_variable(name)
                || matches!(
                    name.as_str(),
                    "CARGO_NET_OFFLINE" | "GOPROXY" | "GOSUMDB" | "NPM_CONFIG_OFFLINE"
                )
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let mut parent_variables = config.parent_variables.clone();
    parent_variables.extend(environment.keys().cloned());
    parent_variables.sort();
    parent_variables.dedup();
    // The plain override takes precedence over an inherited JSON form.
    environment.insert("REZ_PARENT_VARIABLES".into(), parent_variables.join(","));
    environment
}

/// Create the shared build-environment launcher using the resolved build context.
pub(crate) fn create_build_env_script(ctx: &BuildContext) -> Result<PathBuf> {
    let context_path = ctx.build_context_path.as_deref().ok_or_else(|| {
        RezError::BuildSystem(
            "cannot write build scripts without a serialized build context".into(),
        )
    })?;
    let rez_executable = std::env::current_exe().map_err(|error| {
        RezError::BuildSystem(format!("cannot locate the running rez executable: {error}"))
    })?;
    let args = vec![
        "env".to_string(),
        "--input".to_string(),
        context_path.to_string_lossy().into_owned(),
        "--inherited".to_string(),
    ];
    let build_env = build_launcher_environment(ctx, &CONFIG);
    let shell = if CONFIG.default_shell.is_empty() {
        detect_shell()
    } else {
        ShellType::from_name(&CONFIG.default_shell).unwrap_or_else(detect_shell)
    };

    create_forwarding_script(ForwardingScriptOptions {
        bin_dir: &ctx.build_path,
        tool_name: "build-env",
        program: &rez_executable.to_string_lossy(),
        args: &args,
        env_vars: &build_env,
        working_dir: Some(&ctx.build_path),
        shell,
        extensionless_unix: true,
    })
}

// ---------------------------------------------------------------------------
// Build system detection / creation
// ---------------------------------------------------------------------------

/// Detect which build system should be used for the given directory.
///
/// Checks for known build files (CMakeLists.txt, Makefile) and optionally
/// the package's build_command field.
pub fn detect_build_system(path: &Path) -> Option<BuildSystemType> {
    BuildSystemType::detect(path)
}

/// Detect valid build systems for a directory, filtering out child systems.
///
/// E.g. if both CMakeLists.txt and Makefile exist, cmake is the primary and
/// make is filtered out as a child build system.
/// Bun is preferred over NodeJs when both are detected.
pub fn get_valid_build_systems(
    working_dir: &Path,
    build_command: Option<&str>,
) -> Vec<BuildSystemType> {
    // If package has a custom build command, that takes precedence
    if build_command.is_some() {
        return vec![BuildSystemType::Custom];
    }

    // Detect from files
    let mut found: Vec<BuildSystemType> = Vec::new();
    if CMakeBuildSystem::is_valid_root(working_dir) {
        found.push(BuildSystemType::CMake);
    }
    if MakeBuildSystem::is_valid_root(working_dir) {
        found.push(BuildSystemType::Make);
    }
    if PythonBuildSystem::is_valid_root(working_dir) {
        found.push(BuildSystemType::Python);
    }
    if PipBuildSystem::is_valid_root(working_dir) {
        found.push(BuildSystemType::Pip);
    }
    if CargoBuildSystem::is_valid_root(working_dir) {
        found.push(BuildSystemType::Cargo);
    }
    if GoBuildSystem::is_valid_root(working_dir) {
        found.push(BuildSystemType::Go);
    }
    if ZigBuildSystem::is_valid_root(working_dir) {
        found.push(BuildSystemType::Zig);
    }
    if BunBuildSystem::is_valid_root(working_dir) {
        found.push(BuildSystemType::Bun);
    }
    if NodeJsBuildSystem::is_valid_root(working_dir) {
        found.push(BuildSystemType::NodeJs);
    }
    if SConsBuildSystem::is_valid_root(working_dir) {
        found.push(BuildSystemType::SCons);
    }
    if VcpkgBuildSystem::is_valid_root(working_dir) {
        found.push(BuildSystemType::Vcpkg);
    }
    if ConanBuildSystem::is_valid_root(working_dir) {
        found.push(BuildSystemType::Conan);
    }
    if ExtractionBuildSystem::is_valid_root(working_dir) {
        found.push(BuildSystemType::Extraction);
    }

    // Filter out child build systems and overlapping systems
    let children: Vec<BuildSystemType> = found
        .iter()
        .filter_map(|bs| match bs {
            BuildSystemType::CMake => Some(BuildSystemType::Make),
            // Bun is more specific than NodeJs - filter out NodeJs if Bun is present
            BuildSystemType::Bun => Some(BuildSystemType::NodeJs),
            _ => None,
        })
        .collect();

    found.retain(|bs| !children.contains(bs));
    found
}

/// Create a build system instance for the given directory and type.
pub fn create_build_system(
    working_dir: &Path,
    sys_type: BuildSystemType,
    build_command: Option<String>,
) -> Box<dyn BuildSystem> {
    match sys_type {
        BuildSystemType::CMake => Box::new(CMakeBuildSystem::new(working_dir.to_path_buf())),
        BuildSystemType::Make => Box::new(MakeBuildSystem::new(working_dir.to_path_buf())),
        BuildSystemType::Python => Box::new(PythonBuildSystem::new(working_dir.to_path_buf())),
        BuildSystemType::Pip => Box::new(PipBuildSystem::new(working_dir.to_path_buf())),
        BuildSystemType::Cargo => Box::new(CargoBuildSystem::new(working_dir.to_path_buf())),
        BuildSystemType::Go => Box::new(GoBuildSystem::new(working_dir.to_path_buf())),
        BuildSystemType::Zig => Box::new(ZigBuildSystem::new(working_dir.to_path_buf())),
        BuildSystemType::NodeJs => Box::new(NodeJsBuildSystem::new(working_dir.to_path_buf())),
        BuildSystemType::Bun => Box::new(BunBuildSystem::new(working_dir.to_path_buf())),
        BuildSystemType::SCons => Box::new(SConsBuildSystem::new(working_dir.to_path_buf())),
        BuildSystemType::Vcpkg => Box::new(VcpkgBuildSystem::new(working_dir.to_path_buf())),
        BuildSystemType::Conan => Box::new(ConanBuildSystem::new(working_dir.to_path_buf())),
        BuildSystemType::Extraction => {
            Box::new(ExtractionBuildSystem::new(working_dir.to_path_buf()))
        }
        BuildSystemType::Custom => Box::new(CustomBuildSystem::new(
            working_dir.to_path_buf(),
            build_command.unwrap_or_default(),
        )),
        BuildSystemType::NoOp => Box::new(NoOpBuildSystem::new(working_dir.to_path_buf())),
    }
}

// ---------------------------------------------------------------------------
// BuildProcess - orchestrates the full build pipeline
// ---------------------------------------------------------------------------

/// Orchestrates the full build process for a package.
///
/// Handles build directory preparation, build system detection,
/// variant iteration, and the actual build invocation.
///
/// Mirrors Python's BuildProcess + BuildProcessHelper.
#[derive(Debug)]
pub struct BuildProcess {
    /// Package source directory
    pub working_dir: PathBuf,
    /// Build output directory
    pub build_path: PathBuf,
    /// Configured build directory before package/version isolation.
    build_directory: PathBuf,
    /// The build system to use
    pub build_system: Box<dyn BuildSystem>,
    /// Package name (for logging)
    pub package_name: String,
    /// Package version (for env vars)
    pub package_version: String,
    /// Number of variants in the package
    pub num_variants: usize,
    /// Extra arguments passed to the build system
    pub build_args: Vec<String>,
    /// Extra arguments for child build system (e.g. make args under cmake)
    pub child_build_args: Vec<String>,
    /// Verbose logging
    pub verbose: bool,
    /// Full package metadata (for env vars and pre_build_commands)
    pub package: Option<Package>,
    /// Original developer source, retained only for build-context reevaluation.
    developer_package: Option<DeveloperPackage>,
}

impl BuildProcess {
    /// Create a build process for a package directory.
    ///
    /// Detects the build system from source files and package metadata.
    /// The `build_dir` is typically "{working_dir}/build" or an absolute path
    /// from config.build_directory.
    pub fn new(
        working_dir: &Path,
        build_command: Option<String>,
        build_system_name: Option<&str>,
        config: Option<&crate::config::RezConfig>,
    ) -> Result<Self> {
        // Determine build system type
        let sys_type = if let Some(name) = build_system_name {
            BuildSystemType::all()
                .iter()
                .copied()
                .find(|system| system.name() == name)
                .ok_or_else(|| RezError::BuildSystem(format!("Unknown build system: {name}")))?
        } else if build_command.is_some() {
            BuildSystemType::Custom
        } else {
            let valid = get_valid_build_systems(working_dir, None);
            match valid.len() {
                0 => BuildSystemType::NoOp,
                1 => valid[0],
                _ => {
                    let names: Vec<&str> = valid.iter().map(|v| v.name()).collect();
                    return Err(RezError::BuildSystem(format!(
                        "Multiple build systems detected ({}), please specify one",
                        names.join(", ")
                    )));
                }
            }
        };

        let build_system = create_build_system(working_dir, sys_type, build_command);
        let config = config.unwrap_or(&CONFIG);
        let build_directory =
            crate::config::RezConfig::expand_path(&config.build_directory).to_os();
        let build_path = working_dir.join(&build_directory);

        Ok(Self {
            working_dir: working_dir.to_path_buf(),
            build_path,
            build_directory,
            build_system,
            package_name: String::new(),
            package_version: String::new(),
            num_variants: 0,
            build_args: Vec::new(),
            child_build_args: Vec::new(),
            verbose: false,
            package: None,
            developer_package: None,
        })
    }

    /// Set package metadata (name, version, num_variants) for logging and env vars.
    pub fn set_package_info(
        &mut self,
        name: &str,
        version: &str,
        num_variants: usize,
        build_directory: Option<&Path>,
    ) -> Result<()> {
        crate::serialise::validate_rez_package_path(
            name,
            (!version.is_empty()).then_some(version),
        )?;
        let directory = build_directory.unwrap_or(&self.build_directory);
        let build_path = if directory.is_absolute() {
            directory.join(name).join(version)
        } else {
            self.working_dir.join(directory)
        };
        self.package_name = name.to_string();
        self.package_version = version.to_string();
        self.num_variants = num_variants;
        self.build_path = build_path;
        Ok(())
    }

    /// Set full package metadata for enhanced env vars and pre_build_commands.
    ///
    /// This enables the additional REZ_BUILD_* variables that require package data:
    /// VARIANT_REQUIRES, PROJECT_DESCRIPTION, PROJECT_FILE, REQUIRES, etc.
    pub fn set_package(
        &mut self,
        package: Package,
        developer_package: Option<DeveloperPackage>,
    ) -> Result<()> {
        if developer_package.as_ref().is_some_and(|source| {
            source.package.name != package.name || source.package.version != package.version
        }) {
            return Err(RezError::Build(
                "Developer source does not match package identity".into(),
            ));
        }
        let directory = package
            .config
            .as_ref()
            .and_then(|config| config.get("build_directory"))
            .map(|value| {
                value
                    .as_str()
                    .map(|path| crate::config::RezConfig::expand_path(path).to_os())
                    .ok_or_else(|| {
                        RezError::Config("package config.build_directory must be a string".into())
                    })
            })
            .transpose()?;
        self.set_package_info(
            &package.name,
            &package.version.to_string(),
            package.num_variants().max(1),
            directory.as_deref(),
        )?;
        self.package = Some(package);
        self.developer_package = developer_package;
        Ok(())
    }

    /// Perform the build, optionally installing and cleaning first.
    ///
    /// If variant_indices is None, builds all variants.
    /// Returns a Vec of BuildResults, one per variant built.
    /// When force=false and install=true, skips variants already installed.
    pub fn build(
        &self,
        install_path: &Path,
        clean: bool,
        install: bool,
        variant_indices: Option<&[usize]>,
        force: bool,
        write_build_scripts: bool,
    ) -> Result<Vec<BuildResult>> {
        crate::config::ensure_valid()?;
        if self.build_directory.is_absolute() && self.package_name.is_empty() {
            return Err(RezError::Build(
                "An absolute build_directory requires package metadata".into(),
            ));
        }
        if write_build_scripts && !self.build_system.supports_build_scripts() {
            return Err(RezError::BuildSystem(format!(
                "build scripts are not implemented for the '{}' build system",
                self.build_system.name()
            )));
        }
        if write_build_scripts && self.package.is_none() {
            return Err(RezError::BuildSystem(
                "build scripts require validated package metadata".into(),
            ));
        }
        let indices: Vec<usize> = match variant_indices {
            Some(v) => v.to_vec(),
            None => {
                let n = if self.num_variants == 0 {
                    1
                } else {
                    self.num_variants
                };
                (0..n).collect()
            }
        };

        if let Some(package) = &self.package {
            let variant_count = package.variants.len().max(1);
            let mut seen_indices = std::collections::HashSet::new();
            for &index in &indices {
                if index >= variant_count {
                    return Err(RezError::Build(format!(
                        "Package {} has no variant at index {}",
                        package.name, index
                    )));
                }
                if !seen_indices.insert(index) {
                    return Err(RezError::Build(format!(
                        "Variant index {} was selected more than once",
                        index
                    )));
                }
            }
        }

        log_info!(
            "build",
            "Starting build, install_path={}, clean={}, install={}, force={}",
            install_path.display(),
            clean,
            install,
            force
        );
        let mut results = Vec::new();

        for &variant_idx in &indices {
            let variant_subpath = self
                .package
                .as_ref()
                .map(|pkg| get_variant_subpath(pkg, variant_idx))
                .unwrap_or_default();
            let variant_build_path = if variant_subpath.is_empty() {
                self.build_path.clone()
            } else {
                self.build_path.join(&variant_subpath)
            };
            let variant_install_path = self.package.as_ref().map_or_else(
                || Ok(install_path.to_path_buf()),
                |pkg| {
                    variant_install_dir(
                        install_path,
                        &pkg.name,
                        &pkg.version.to_string(),
                        &variant_subpath,
                    )
                },
            )?;

            // Script mode must produce a launcher even when the package is already installed.
            if install && !force && !write_build_scripts {
                if let Some(package) = &self.package {
                    let variant_dir = variant_install_path.clone();
                    let version_dir = variant_install_dir(
                        install_path,
                        &package.name,
                        &package.version.to_string(),
                        "",
                    )?;
                    if crate::repository::is_variant_installed(
                        package,
                        &version_dir,
                        variant_idx,
                        None,
                    )? {
                        log_info!(
                            "build",
                            "Skipping variant {}: already installed at {}",
                            variant_idx,
                            variant_dir.display()
                        );
                        if self.verbose {
                            eprintln!(
                                "[rez] Skipping variant {}: already installed at {}",
                                variant_idx,
                                variant_dir.display()
                            );
                        }
                        let mut skip_result = BuildResult::ok(variant_build_path.clone(), 0.0);
                        skip_result.install_path = Some(variant_dir);
                        results.push(skip_result);
                        continue;
                    }
                }
            }

            log_debug!(
                "build",
                "Building variant {} of {}",
                variant_idx,
                indices.len()
            );
            // Clean if requested
            if clean {
                prepare_build_dir(&variant_build_path, true, Some(&self.working_dir))?;
            }

            // Ensure build dir exists
            std::fs::create_dir_all(&variant_build_path).map_err(|e| {
                RezError::Build(format!(
                    "Failed to create build dir {}: {}",
                    variant_build_path.display(),
                    e
                ))
            })?;

            if install && !write_build_scripts {
                std::fs::create_dir_all(&variant_install_path).map_err(|e| {
                    RezError::Build(format!(
                        "Failed to create install dir {}: {}",
                        variant_install_path.display(),
                        e
                    ))
                })?;
            }

            let variant_index = self
                .package
                .as_ref()
                .and_then(|pkg| (!pkg.variants.is_empty()).then_some(variant_idx));

            // Upstream reevaluates the developer definition only for resolving
            // build requirements. Paths, adapters and installed metadata continue
            // to use the original non-building package.
            let reevaluated_package = self
                .developer_package
                .as_ref()
                .map(|source| source.build_variant(variant_idx))
                .transpose()?;
            let context_package = reevaluated_package
                .as_ref()
                .map(|source| &source.package)
                .or(self.package.as_ref());
            let (build_context_path, resolved_context) = if let Some(pkg) = context_package {
                if self.developer_package.is_some()
                    || !pkg.build_requires.is_empty()
                    || !pkg.private_build_requires.is_empty()
                    || pkg.pre_build_commands.is_some()
                    || write_build_scripts
                {
                    match ResolvedContext::create_build_context(
                        pkg,
                        variant_index,
                        Some(&variant_build_path),
                        None,
                        None,
                        true,
                    ) {
                        Ok((build_ctx, rxt_path)) => (rxt_path, Some(build_ctx)),
                        Err(error @ RezError::BuildContextResolve { .. }) => return Err(error),
                        Err(error) => {
                            return Err(RezError::Build(format!(
                                "Failed to create build context: {}",
                                error
                            )));
                        }
                    }
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            };

            let mut env_vars = self.standard_env_vars(
                variant_idx,
                &variant_build_path,
                &variant_install_path,
                install,
                resolved_context.as_ref(),
            )?;

            env_vars.extend(CONFIG.recipe_environment());
            env_vars.extend(std::env::vars().filter(|(name, _)| name.starts_with("REZ_PBS_")));
            if CONFIG.offline {
                env_vars.extend([
                    ("CARGO_NET_OFFLINE".into(), "true".into()),
                    ("GOPROXY".into(), "off".into()),
                    ("GOSUMDB".into(), "off".into()),
                    ("NPM_CONFIG_OFFLINE".into(), "true".into()),
                ]);
            }

            if let Some(context) = resolved_context.as_ref() {
                let mut parent = std::env::vars().collect::<HashMap<_, _>>();
                parent.extend(env_vars.clone());
                let callback = if write_build_scripts {
                    None
                } else {
                    self.package
                        .as_ref()
                        .and_then(|package| {
                            package
                                .pre_build_commands
                                .as_ref()
                                .map(|code| (package, code))
                        })
                        .map(|(package, code)| -> Result<RexExecutionCallback> {
                            let variant = if package.variants.is_empty() {
                                crate::package::Variant::from_package(package.clone())
                            } else {
                                crate::package::Variant::new(package.clone(), variant_idx)?
                            };
                            Ok(RexExecutionCallback {
                                package_name: package.name.clone(),
                                name: "pre_build_commands".into(),
                                code: code.clone(),
                                package: Some(variant),
                                bindings: HashMap::from([(
                                    "build".into(),
                                    serde_json::json!({
                                        "build_type": "local",
                                        "install": install,
                                        "build_path": variant_build_path.to_string_lossy(),
                                        "install_path": variant_install_path.to_string_lossy(),
                                    }),
                                )]),
                                developer: self.developer_package.is_some(),
                            })
                        })
                        .transpose()?
                };
                // Rex returns assignments relative to its parent, not the parent itself.
                // Keep the shared build variables for every adapter and allow Rex/hook overrides.
                env_vars
                    .extend(context.get_environ_with_callback(Some(parent), callback.as_ref())?);
            }

            let ctx = BuildContext {
                source_path: self.working_dir.clone(),
                build_path: variant_build_path.clone(),
                install_path: variant_install_path.clone(),
                build_type: BuildType::Local,
                variant_index,
                build_args: self.build_args.clone(),
                child_build_args: self.child_build_args.clone(),
                install,
                write_build_scripts,
                build_context_path,
                build_threads: num_build_threads(),
                env_vars,
                package_config: self.package.as_ref().and_then(|p| p.config.clone()),
            };

            if self.verbose {
                eprintln!(
                    "[rez] Building{} with {}...",
                    if self.num_variants > 1 {
                        format!(" variant {}/{}", variant_idx + 1, self.num_variants)
                    } else {
                        String::new()
                    },
                    self.build_system.name()
                );
            }

            let result = self.build_system.build(&ctx)?;
            results.push(result);
        }

        if install
            && !write_build_scripts
            && !results.is_empty()
            && results.iter().all(|result| result.success)
        {
            if self.package.is_none()
                && results
                    .iter()
                    .any(|result| result.prepared_payload.is_some())
            {
                return Err(RezError::BuildSystem(
                    "Publishing a prepared installation requires validated package metadata".into(),
                ));
            }
            if let Some(package) = &self.package {
                let version_dir = variant_install_dir(
                    install_path,
                    &package.name,
                    &package.version.to_string(),
                    "",
                )?;
                let prepared_count = results
                    .iter()
                    .filter(|result| result.prepared_payload.is_some())
                    .count();
                if prepared_count == 0 {
                    crate::repository::publish_package(
                        package,
                        &version_dir,
                        &indices,
                        Some(crate::repository::PublicationPayload::Existing(&|| {
                            self.install_include_modules(&version_dir)
                        })),
                        None,
                    )?;
                } else {
                    for (&index, result) in indices.iter().zip(&results) {
                        if result.prepared_payload.is_none()
                            && !crate::repository::is_variant_installed(
                                package,
                                &version_dir,
                                index,
                                None,
                            )?
                        {
                            return Err(RezError::BuildSystem(
                                "Build results mix staged and unverified live payloads".into(),
                            ));
                        }
                    }
                    let staged = tempfile::tempdir()?;
                    let prepared_indices = indices
                        .iter()
                        .zip(&results)
                        .filter_map(|(&index, result)| {
                            result.prepared_payload.as_ref().map(|_| index)
                        })
                        .collect::<Vec<_>>();
                    let mut prepared = indices
                        .iter()
                        .zip(&results)
                        .filter(|(_, result)| result.prepared_payload.is_some())
                        .collect::<Vec<_>>();
                    prepared.sort_by_key(|(index, _)| {
                        Path::new(&get_variant_subpath(package, **index))
                            .components()
                            .count()
                    });
                    for (&index, result) in prepared {
                        let payload = result.prepared_payload.as_ref().ok_or_else(|| {
                            RezError::BuildSystem("Missing prepared build payload".into())
                        })?;
                        let relative = PathBuf::from(get_variant_subpath(package, index));
                        foundation::filesystem::copy_dir_contents(
                            payload.path(),
                            &staged.path().join(&relative),
                            false,
                            true,
                            Some(staged.path()),
                            None,
                        )?;
                    }
                    self.install_include_modules(staged.path())?;
                    crate::repository::publish_package(
                        package,
                        &version_dir,
                        &prepared_indices,
                        Some(crate::repository::PublicationPayload::Owned {
                            root: staged,
                            replace: true,
                            paths: None,
                            require_new: false,
                        }),
                        None,
                    )?;
                    for result in &mut results {
                        result.prepared_payload = None;
                    }
                }
            }
        }

        Ok(results)
    }

    /// Install deferred include modules before publishing package metadata.
    fn install_include_modules(&self, version_dir: &Path) -> Result<()> {
        let Some(source) = &self.developer_package else {
            return Ok(());
        };
        if source.includes.is_empty() {
            return Ok(());
        }
        let directory = crate::serialise::include_directory(None, source.package.config.as_ref())?;
        // Read every module first: a missing/invalid later module must not leave
        // a partially updated include set or publish unusable metadata.
        let modules = source
            .includes
            .iter()
            .map(|name| {
                let path = crate::serialise::include_module_path(name, &directory, false)?;
                let content = std::fs::read(&path)?;
                let hash = python_runtime::include_module_hash(&content);
                Ok((name, content, hash))
            })
            .collect::<Result<Vec<_>>>()?;
        let destination = version_dir.join(".rez/include");
        for (name, content, hash) in modules {
            crate::serialise::atomic_write(&destination.join(format!("{name}.py")), &content)?;
            crate::serialise::atomic_write(&destination.join(format!("{name}.sha1")), &hash)?;
        }
        Ok(())
    }

    /// Generate standard REZ_BUILD_* env vars for the build.
    fn standard_env_vars(
        &self,
        variant_index: usize,
        build_path: &Path,
        install_path: &Path,
        install: bool,
        context: Option<&ResolvedContext>,
    ) -> Result<HashMap<String, String>> {
        let mut vars = HashMap::new();

        // Basic vars (always present)
        vars.insert("REZ_BUILD_ENV".into(), "1".into());
        vars.insert("REZ_BUILD_PROJECT_NAME".into(), self.package_name.clone());
        vars.insert(
            "REZ_BUILD_PROJECT_VERSION".into(),
            self.package_version.clone(),
        );
        vars.insert("REZ_BUILD_VARIANT_INDEX".into(), variant_index.to_string());
        vars.insert(
            "REZ_BUILD_SOURCE_PATH".into(),
            self.working_dir.to_string_lossy().into_owned(),
        );
        vars.insert(
            "REZ_BUILD_PATH".into(),
            build_path.to_string_lossy().into_owned(),
        );
        vars.insert(
            "REZ_BUILD_INSTALL_PATH".into(),
            install_path.to_string_lossy().into_owned(),
        );
        vars.insert(
            "REZ_BUILD_THREAD_COUNT".into(),
            num_build_threads().to_string(),
        );
        vars.insert("REZ_BUILD_TYPE".into(), "local".into());
        vars.insert(
            "REZ_BUILD_INSTALL".into(),
            if install { "1" } else { "0" }.into(),
        );

        // Enhanced vars (require full Package object)
        if let Some(ref pkg) = self.package {
            vars.insert(
                "REZ_BUILD_VARIANT_REQUIRES".into(),
                get_variant_requires_str(pkg, variant_index),
            );
            vars.insert(
                "REZ_BUILD_VARIANT_SUBPATH".into(),
                get_variant_subpath(pkg, variant_index),
            );
            vars.insert(
                "REZ_BUILD_PROJECT_DESCRIPTION".into(),
                pkg.description.as_deref().unwrap_or("").trim().into(),
            );
            vars.insert(
                "REZ_BUILD_PROJECT_FILE".into(),
                if let Some(source) = self.developer_package.as_ref() {
                    source.filepath.to_string_lossy().into_owned()
                } else {
                    get_package_file_path(pkg)?
                },
            );
            let requests = if let Some(context) = context {
                context.requested_packages(true)
            } else {
                let variant = if pkg.variants.is_empty() {
                    crate::package::Variant::from_package(pkg.clone())
                } else {
                    crate::package::Variant::new(pkg.clone(), variant_index)?
                };
                variant.build_request()
            };
            vars.insert(
                "REZ_BUILD_REQUIRES".into(),
                requests
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            vars.insert(
                "REZ_BUILD_REQUIRES_UNVERSIONED".into(),
                requests
                    .iter()
                    .map(|request| request.name())
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }

        Ok(vars)
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Prepare (and optionally clean) the build directory.
///
/// If clean=true, removes the build dir first. Always ensures it exists.
pub fn prepare_build_dir(build_path: &Path, clean: bool, source_path: Option<&Path>) -> Result<()> {
    if clean && build_path.exists() {
        if let Some(source_path) = source_path {
            let target = build_path.canonicalize().map_err(|error| {
                RezError::Build(format!(
                    "Cannot validate build directory {}: {error}",
                    build_path.display()
                ))
            })?;
            let source = source_path.canonicalize().map_err(|error| {
                RezError::Build(format!(
                    "Cannot validate source directory {}: {error}",
                    source_path.display()
                ))
            })?;
            if source.starts_with(&target) {
                return Err(RezError::Build(format!(
                    "Cannot clean build directory {}: it contains the source directory",
                    build_path.display()
                )));
            }
        }
        std::fs::remove_dir_all(build_path).map_err(|e| {
            RezError::Build(format!(
                "Failed to clean build dir {}: {}",
                build_path.display(),
                e
            ))
        })?;
    }
    std::fs::create_dir_all(build_path).map_err(|e| {
        RezError::Build(format!(
            "Failed to create build dir {}: {}",
            build_path.display(),
            e
        ))
    })?;
    Ok(())
}

/// Resolve the number of build threads from config.
fn num_build_threads() -> usize {
    use crate::config::CONFIG;
    CONFIG.build_thread_count.resolve()
}

/// Compute the install directory for a variant.
/// Handles both: install_path as base repo (build) or as version dir (release).
fn variant_install_dir(
    install_path: &Path,
    name: &str,
    version: &str,
    variant_subpath: &str,
) -> Result<PathBuf> {
    crate::serialise::validate_rez_package_path(name, (!version.is_empty()).then_some(version))?;
    if !foundation::path::is_safe_rez_path(variant_subpath, true) {
        return Err(RezError::PackageRequest(format!(
            "Not a valid package variant path component: {variant_subpath:?}"
        )));
    }
    let version_dir = if version.is_empty() {
        if install_path.file_name().and_then(|f| f.to_str()) == Some(name) {
            install_path.to_path_buf()
        } else {
            install_path.join(name)
        }
    } else if install_path.file_name().and_then(|f| f.to_str()) == Some(version)
        && install_path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|f| f.to_str())
            == Some(name)
    {
        install_path.to_path_buf()
    } else {
        install_path.join(name).join(version)
    };
    Ok(if variant_subpath.is_empty() {
        version_dir
    } else {
        version_dir.join(variant_subpath)
    })
}

/// Get variant requires as space-separated string.
fn get_variant_requires_str(package: &Package, variant_index: usize) -> String {
    if variant_index < package.variants.len() {
        package.variants[variant_index]
            .iter()
            .map(|r| r.to_string())
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        String::new()
    }
}

/// Get variant subpath (e.g. "python-3.10" or SHA1 hash).
fn get_variant_subpath(package: &Package, variant_index: usize) -> String {
    package
        .variants
        .get(variant_index)
        .and_then(|requires| {
            crate::package::Variant::compute_subpath(requires, package.hashed_variants)
        })
        .unwrap_or_default()
}

/// Find the configured package definition file for a package.
fn get_package_file_path(package: &Package) -> Result<String> {
    if let Some(base) = package.base.as_deref() {
        if let Some(path) = crate::serialise::find_package_definition_file(
            base,
            crate::serialise::PACKAGE_DEFINITION_EXTENSIONS,
        )? {
            return Ok(path.to_string_lossy().into_owned());
        }
        return Ok(base.to_string_lossy().into_owned());
    }
    Ok(String::new())
}

/// Set standard REZ_BUILD_* environment variables on a Command.
///
/// This mirrors Python BuildSystem.set_standard_vars() for use when
/// spawning build subprocesses.
// Not yet called by any builder — retained for future custom build systems.
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub fn set_standard_build_vars(
    cmd: &mut Command,
    package_name: &str,
    package_version: &str,
    source_path: &Path,
    build_path: &Path,
    install_path: Option<&Path>,
    build_type: BuildType,
    install: bool,
    variant_index: usize,
) {
    cmd.env("REZ_BUILD_ENV", "1");
    cmd.env("REZ_BUILD_PATH", build_path);
    cmd.env("REZ_BUILD_SOURCE_PATH", source_path);
    cmd.env("REZ_BUILD_PROJECT_NAME", package_name);
    cmd.env("REZ_BUILD_PROJECT_VERSION", package_version);
    cmd.env("REZ_BUILD_TYPE", build_type.to_string());
    cmd.env("REZ_BUILD_INSTALL", if install { "1" } else { "0" });
    cmd.env("REZ_BUILD_VARIANT_INDEX", variant_index.to_string());
    cmd.env("REZ_BUILD_THREAD_COUNT", num_build_threads().to_string());

    if let Some(ip) = install_path {
        cmd.env("REZ_BUILD_INSTALL_PATH", ip);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[derive(Debug)]
    struct CaptureBuildSystem(std::sync::Arc<std::sync::Mutex<Vec<BuildContext>>>);

    impl BuildSystem for CaptureBuildSystem {
        fn name(&self) -> &str {
            "capture"
        }
        fn build_type(&self) -> BuildSystemType {
            BuildSystemType::NoOp
        }
        fn is_valid(&self) -> bool {
            true
        }
        fn build(&self, context: &BuildContext) -> Result<BuildResult> {
            self.0.lock().unwrap().push(context.clone());
            Ok(BuildResult::ok(context.build_path.clone(), 0.0))
        }
    }

    #[test]
    fn frozen_launcher_controls_survive_a_clean_shell_without_host_leaks() {
        let mut ctx = BuildContext::new("/source".into(), "/build".into(), "/install".into());
        ctx.env_vars = HashMap::from([
            ("REZ_BUILD_PROJECT_NAME".into(), "frozen-project".into()),
            ("REZ_PBS_RELEASE_TAG".into(), "frozen-release".into()),
            ("REZ_USER_PATH".into(), "frozen-user".into()),
            ("REZ_OFFLINE".into(), "true".into()),
            ("CARGO_NET_OFFLINE".into(), "true".into()),
            ("GOPROXY".into(), "off".into()),
            ("GOSUMDB".into(), "off".into()),
            ("NPM_CONFIG_OFFLINE".into(), "true".into()),
            ("UNRELATED".into(), "must-not-be-frozen".into()),
        ]);
        let config = crate::config::RezConfig {
            parent_variables: vec!["KEEP_EXISTING".into()],
            ..crate::config::RezConfig::default()
        };
        let frozen = build_launcher_environment(&ctx, &config);
        assert!(!frozen.contains_key("UNRELATED"));
        let mut parent = std::env::vars().collect::<HashMap<_, _>>();
        parent.extend([
            ("REZ_PBS_RELEASE_TAG".into(), "changed-host-release".into()),
            ("REZ_USER_PATH".into(), "changed-host-user".into()),
            ("KEEP_EXISTING".into(), "kept".into()),
            ("UNRELATED".into(), "host-leak".into()),
            ("REZ_PARENT_VARIABLES_JSON".into(), "[\"UNRELATED\"]".into()),
        ]);
        parent.extend(frozen);
        let allowlist = parent["REZ_PARENT_VARIABLES"]
            .split(',')
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let clean = model::environment::clean_environ(
            model::platform::Platform::current(),
            &parent,
            &allowlist,
        );
        assert_eq!(clean["KEEP_EXISTING"], "kept");
        assert_eq!(clean["CARGO_NET_OFFLINE"], "true");
        assert_eq!(clean["GOPROXY"], "off");
        assert_eq!(clean["GOSUMDB"], "off");
        assert_eq!(clean["NPM_CONFIG_OFFLINE"], "true");
        assert!(!clean.contains_key("UNRELATED"));
        #[cfg(windows)]
        let (shell, args) = (
            crate::shell::types::find_executable("cmd.exe", None).unwrap(),
            vec![
                "/D",
                "/c",
                "echo %REZ_BUILD_PROJECT_NAME%:%REZ_PBS_RELEASE_TAG%:%REZ_USER_PATH%:%REZ_OFFLINE%",
            ],
        );
        #[cfg(not(windows))]
        let (shell, args) = (
            "/bin/sh".to_owned(),
            vec!["-c", "printf '%s:%s:%s:%s' \"$REZ_BUILD_PROJECT_NAME\" \"$REZ_PBS_RELEASE_TAG\" \"$REZ_USER_PATH\" \"$REZ_OFFLINE\""],
        );
        let output = Command::new(shell)
            .args(args)
            .env_clear()
            .envs(clean)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "frozen-project:frozen-release:frozen-user:true"
        );
    }

    #[test]
    fn test_pre_build_commands_include_and_rex_environment_execute_once() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source 'quoted'");
        let modules = temp.path().join("modules");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&modules).unwrap();
        fs::write(modules.join("build_helper.py"), "value = 'from include'\n").unwrap();
        let module_directory = serde_json::to_string(&modules.to_string_lossy()).unwrap();
        fs::write(source.join("package.py"), format!(
            "name = 'build_hook'\nversion = '1.0'\nconfig = {{'package_definition_python_path': {module_directory}}}\n@include('build_helper')\ndef pre_build_commands():\n    import os\n    assert this.name == 'build_hook'\n    assert str(this.version) == '1.0'\n    assert build.build_type == 'local'\n    assert build.install is False\n    assert str(env.REZ_BUILD_PATH) == build.build_path\n    env.BUILD_HOOK_VALUE = build_helper.value\n    with open(os.path.join(build.build_path, 'hook.txt'), 'a') as output:\n        output.write('once\\n')\n",
        )).unwrap();
        let developer = DeveloperPackage::from_path(&source).unwrap();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut process = BuildProcess::new(&source, None, Some("noop"), None).unwrap();
        process
            .set_package(developer.package.clone(), Some(developer))
            .unwrap();
        process.build_system = Box::new(CaptureBuildSystem(captured.clone()));
        process
            .build(&temp.path().join("repo"), false, false, None, true, false)
            .unwrap();
        let contexts = captured.lock().unwrap();
        assert_eq!(contexts.len(), 1);
        let environment = &contexts[0].env_vars;
        assert_eq!(environment["REZ_BUILD_PROJECT_NAME"], "build_hook");
        assert_eq!(environment["REZ_BUILD_PROJECT_VERSION"], "1.0");
        assert_eq!(environment["REZ_BUILD_VARIANT_INDEX"], "0");
        assert_eq!(environment["REZ_BUILD_ENV"], "1");
        assert_eq!(
            environment["REZ_BUILD_SOURCE_PATH"],
            source.to_string_lossy()
        );
        assert_eq!(
            environment["REZ_BUILD_PROJECT_FILE"],
            source.join("package.py").to_string_lossy()
        );
        assert_eq!(
            environment["REZ_BUILD_PATH"],
            contexts[0].build_path.to_string_lossy()
        );
        assert_eq!(
            environment["REZ_BUILD_INSTALL_PATH"],
            contexts[0].install_path.to_string_lossy()
        );
        assert_eq!(
            environment["REZ_BUILD_THREAD_COUNT"],
            contexts[0].build_threads.to_string()
        );
        assert_eq!(environment["REZ_BUILD_TYPE"], "local");
        assert_eq!(environment["REZ_BUILD_INSTALL"], "0");
        assert_eq!(
            contexts[0]
                .env_vars
                .get("BUILD_HOOK_VALUE")
                .map(String::as_str),
            Some("from include")
        );
        assert_eq!(
            fs::read_to_string(contexts[0].build_path.join("hook.txt"))
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            vec!["once"]
        );
    }

    #[test]
    fn test_custom_child_receives_standard_build_variables_after_resolve() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("package.py"),
            "name = 'build_environment'\nversion = '3.10'\n",
        )
        .unwrap();
        let developer = DeveloperPackage::from_path(temp.path()).unwrap();
        let command = if cfg!(windows) {
            "echo %REZ_BUILD_PROJECT_NAME%:%REZ_BUILD_PROJECT_VERSION%:%REZ_BUILD_ENV% > child-environment.txt"
        } else {
            "echo \"$REZ_BUILD_PROJECT_NAME:$REZ_BUILD_PROJECT_VERSION:$REZ_BUILD_ENV\" > child-environment.txt"
        };
        let mut process =
            BuildProcess::new(temp.path(), Some(command.into()), Some("custom"), None).unwrap();
        process
            .set_package(developer.package.clone(), Some(developer))
            .unwrap();
        let results = process
            .build(&temp.path().join("repo"), false, false, None, true, false)
            .unwrap();
        assert!(results[0].success, "{:?}", results[0].error);
        assert_eq!(
            fs::read_to_string(temp.path().join("child-environment.txt"))
                .unwrap()
                .trim(),
            "build_environment:3.10:1"
        );
    }

    #[test]
    fn test_pre_build_commands_errors_stop_before_adapter() {
        for hook in [
            "@include('missing_module')\ndef pre_build_commands():\n    pass\n",
            "def pre_build_commands():\n    raise RuntimeError('hook failure')\n",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let modules = temp.path().join("modules");
            fs::create_dir_all(&modules).unwrap();
            fs::write(
                modules.join("missing_module.py"),
                "value = 'initially present'\n",
            )
            .unwrap();
            let module_directory = serde_json::to_string(&modules.to_string_lossy()).unwrap();
            fs::write(temp.path().join("package.py"), format!(
                "name = 'build_hook'\nversion = '1.0'\nconfig = {{'package_definition_python_path': {module_directory}}}\n{hook}"
            )).unwrap();
            let developer = DeveloperPackage::from_path(temp.path()).unwrap();
            fs::remove_file(modules.join("missing_module.py")).unwrap();
            let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let mut process = BuildProcess::new(temp.path(), None, Some("noop"), None).unwrap();
            process
                .set_package(developer.package.clone(), Some(developer))
                .unwrap();
            process.build_system = Box::new(CaptureBuildSystem(captured.clone()));
            let error = process
                .build(&temp.path().join("repo"), false, false, None, true, false)
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("missing_module") || error.contains("hook failure"),
                "{error}"
            );
            assert!(captured.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn test_pre_build_commands_raw_body_and_installed_include_provenance() {
        for installed in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let mut package = Package::new("build_hook", version::Version::new("1.0").unwrap());
            package.base = Some(temp.path().to_path_buf());
            package.config = Some(
                serde_json::json!({"package_definition_python_path": temp.path().join("wrong_source_path")}),
            );
            package.pre_build_commands = if installed {
                let modules = temp.path().join(".rez/include");
                fs::create_dir_all(&modules).unwrap();
                fs::write(
                    modules.join("installed_hook.py"),
                    "value = 'installed payload'\n",
                )
                .unwrap();
                Some("@include('installed_hook')\ndef pre_build_commands():\n    env.HOOK_RESULT = installed_hook.value\n".into())
            } else {
                Some("env.HOOK_RESULT = 'raw body'\n".into())
            };
            let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let mut process = BuildProcess::new(temp.path(), None, Some("noop"), None).unwrap();
            process.set_package(package, None).unwrap();
            process.build_system = Box::new(CaptureBuildSystem(captured.clone()));
            process
                .build(&temp.path().join("repo"), false, false, None, true, false)
                .unwrap();
            let contexts = captured.lock().unwrap();
            assert_eq!(contexts.len(), 1);
            assert_eq!(
                contexts[0].env_vars.get("HOOK_RESULT").map(String::as_str),
                Some(if installed {
                    "installed payload"
                } else {
                    "raw body"
                })
            );
        }
    }

    #[test]
    fn test_variant_install_dir_validates_package_identity() {
        let base = Path::new("packages");
        assert_eq!(
            variant_install_dir(base, "foo", "1.0", "").unwrap(),
            PathBuf::from("packages/foo/1.0")
        );
        assert!(variant_install_dir(base, "..", "1.0", "").is_err());
        assert!(variant_install_dir(base, "foo", "1/0", "").is_err());
        assert!(variant_install_dir(base, "foo", "1.0", "../outside").is_err());
    }

    #[test]
    fn native_build_system_registry_and_detection_agree() {
        for (kind, manifest) in [
            (BuildSystemType::Go, "go.mod"),
            (BuildSystemType::Zig, "build.zig"),
        ] {
            let temp = tempfile::tempdir().unwrap();
            fs::write(temp.path().join(manifest), "fixture").unwrap();
            assert_eq!(BuildSystemType::detect(temp.path()), Some(kind));
            assert_eq!(get_valid_build_systems(temp.path(), None), vec![kind]);
            assert!(BuildSystemType::all().contains(&kind));
            let process = BuildProcess::new(temp.path(), None, Some(kind.name()), None).unwrap();
            assert_eq!(process.build_system.build_type(), kind);
        }
    }

    // -- Detection: each build system type from files --

    #[test]
    fn test_detect_all_types() {
        // CMake
        let d = tempdir("detect_cmake");
        fs::write(
            d.join("CMakeLists.txt"),
            "cmake_minimum_required(VERSION 3.10)",
        )
        .ok();
        assert_eq!(BuildSystemType::detect(&d), Some(BuildSystemType::CMake));
        cleanup(&d);

        // Cargo
        let d = tempdir("detect_cargo");
        fs::write(d.join("Cargo.toml"), "[package]\nname = \"test\"").ok();
        assert_eq!(BuildSystemType::detect(&d), Some(BuildSystemType::Cargo));
        cleanup(&d);

        // Python (setup.py)
        let d = tempdir("detect_setup_py");
        fs::write(d.join("setup.py"), "from setuptools import setup").ok();
        assert_eq!(BuildSystemType::detect(&d), Some(BuildSystemType::Python));
        cleanup(&d);

        // PEP 517 projects use Pip, while a generic pyproject is not enough
        // to select a Python build backend.
        let d = tempdir("detect_pep517");
        fs::write(
            d.join("pyproject.toml"),
            "[build-system]\nrequires = [\"setuptools\"]\nbuild-backend = \"setuptools.build_meta\"",
        )
        .unwrap();
        assert_eq!(BuildSystemType::detect(&d), Some(BuildSystemType::Pip));
        assert_eq!(
            get_valid_build_systems(&d, None),
            vec![BuildSystemType::Pip]
        );
        cleanup(&d);

        let d = tempdir("detect_generic_pyproject");
        fs::write(d.join("pyproject.toml"), "[project]\nname = \"example\"").unwrap();
        assert_eq!(BuildSystemType::detect(&d), None);
        cleanup(&d);

        // Make
        let d = tempdir("detect_make");
        fs::write(d.join("Makefile"), "all:\n\techo hello").ok();
        assert_eq!(BuildSystemType::detect(&d), Some(BuildSystemType::Make));
        cleanup(&d);

        // NodeJs
        let d = tempdir("detect_nodejs");
        fs::write(d.join("package.json"), "{}").ok();
        assert_eq!(BuildSystemType::detect(&d), Some(BuildSystemType::NodeJs));
        cleanup(&d);

        // Bun (lockb)
        let d = tempdir("detect_bun_lockb");
        fs::write(d.join("package.json"), "{}").ok();
        fs::write(d.join("bun.lockb"), "").ok();
        assert_eq!(BuildSystemType::detect(&d), Some(BuildSystemType::Bun));
        cleanup(&d);

        // Bun (bunfig)
        let d = tempdir("detect_bun_bunfig");
        fs::write(d.join("package.json"), "{}").ok();
        fs::write(d.join("bunfig.toml"), "").ok();
        assert_eq!(BuildSystemType::detect(&d), Some(BuildSystemType::Bun));
        cleanup(&d);

        // SCons
        let d = tempdir("detect_scons");
        fs::write(d.join("SConstruct"), "import glob").ok();
        assert_eq!(BuildSystemType::detect(&d), Some(BuildSystemType::SCons));
        cleanup(&d);

        // Vcpkg
        let d = tempdir("detect_vcpkg");
        fs::write(d.join("vcpkg.json"), r#"{"name":"pkg","version":"1.0"}"#).ok();
        assert_eq!(BuildSystemType::detect(&d), Some(BuildSystemType::Vcpkg));
        cleanup(&d);

        // Conan
        let d = tempdir("detect_conan");
        fs::write(d.join("conanfile.py"), "from conan import ConanFile").ok();
        assert_eq!(BuildSystemType::detect(&d), Some(BuildSystemType::Conan));
        cleanup(&d);

        // Empty dir -> None
        let d = tempdir("detect_none");
        assert_eq!(BuildSystemType::detect(&d), None);
        cleanup(&d);
    }

    // -- Detection priority: cmake > make, bun > nodejs --

    #[test]
    fn test_detect_priority() {
        // cmake wins over make
        let d = tempdir("priority_cmake_make");
        fs::write(d.join("CMakeLists.txt"), "").ok();
        fs::write(d.join("Makefile"), "").ok();
        assert_eq!(BuildSystemType::detect(&d), Some(BuildSystemType::CMake));
        cleanup(&d);

        // cargo wins over make (detected earlier in priority)
        let d = tempdir("priority_cargo_make");
        fs::write(d.join("Cargo.toml"), "[package]").ok();
        fs::write(d.join("Makefile"), "").ok();
        assert_eq!(BuildSystemType::detect(&d), Some(BuildSystemType::Cargo));
        cleanup(&d);
    }

    // -- get_valid_build_systems: filtering and priority --

    #[test]
    fn test_valid_systems_filtering() {
        // Custom build command overrides everything
        let d = tempdir("valid_custom");
        let result = get_valid_build_systems(&d, Some("python setup.py build"));
        assert_eq!(result, vec![BuildSystemType::Custom]);
        cleanup(&d);

        // cmake + Makefile -> cmake only (make is child)
        let d = tempdir("valid_cmake_make");
        fs::write(d.join("CMakeLists.txt"), "").ok();
        fs::write(d.join("Makefile"), "").ok();
        let result = get_valid_build_systems(&d, None);
        assert_eq!(result, vec![BuildSystemType::CMake]);
        cleanup(&d);

        // bun + package.json -> bun only (nodejs filtered)
        let d = tempdir("valid_bun_nodejs");
        fs::write(d.join("package.json"), "{}").ok();
        fs::write(d.join("bun.lockb"), "").ok();
        let result = get_valid_build_systems(&d, None);
        assert_eq!(result, vec![BuildSystemType::Bun]);
        cleanup(&d);

        // package.json only -> nodejs
        let d = tempdir("valid_nodejs_only");
        fs::write(d.join("package.json"), "{}").ok();
        let result = get_valid_build_systems(&d, None);
        assert_eq!(result, vec![BuildSystemType::NodeJs]);
        cleanup(&d);

        // Empty -> empty
        let d = tempdir("valid_empty");
        assert!(get_valid_build_systems(&d, None).is_empty());
        cleanup(&d);
    }

    // -- BuildProcess: detection, creation, execution --

    #[test]
    fn test_unsupported_build_system_rejects_script_mode() {
        let source = tempdir("bp_scripts_unsupported");
        let process = BuildProcess::new(&source, None, Some("noop"), None).unwrap();
        let error = process
            .build(&source.join("install"), false, false, None, false, true)
            .unwrap_err();
        assert!(matches!(error, RezError::BuildSystem(_)));
        cleanup(&source);
    }

    #[test]
    fn test_build_process_detection() {
        // Detect from CMakeLists.txt
        let d = tempdir("bp_cmake");
        fs::write(
            d.join("CMakeLists.txt"),
            "cmake_minimum_required(VERSION 3.10)",
        )
        .ok();
        let bp = BuildProcess::new(&d, None, None, None).unwrap();
        assert_eq!(bp.build_system.name(), "cmake");
        cleanup(&d);

        // Detect from Cargo.toml
        let d = tempdir("bp_cargo");
        fs::write(d.join("Cargo.toml"), "[package]\nname = \"test\"").ok();
        let bp = BuildProcess::new(&d, None, None, None).unwrap();
        assert_eq!(bp.build_system.name(), "cargo");
        cleanup(&d);

        // Custom command -> custom system
        let d = tempdir("bp_custom");
        let bp = BuildProcess::new(&d, Some("echo build".into()), None, None).unwrap();
        assert_eq!(bp.build_system.name(), "custom");
        cleanup(&d);

        // Unknown system name -> error
        let d = tempdir("bp_unknown");
        assert!(BuildProcess::new(&d, None, Some("zig_build"), None).is_err());
        cleanup(&d);

        // Empty dir, no hint -> noop
        let d = tempdir("bp_noop_auto");
        let bp = BuildProcess::new(&d, None, None, None).unwrap();
        assert_eq!(bp.build_system.name(), "noop");
        cleanup(&d);
    }

    #[test]
    fn test_build_reevaluates_only_context_with_original_variant_objects() {
        let source = tempfile::tempdir().unwrap();
        fs::write(
            source.path().join("package.py"),
            "name = 'lifecycle_app'\nversion = '1.0'\nvariants = [['python-3.11'], ['python-3.12']]\nhashed_variants = False\n@early\ndef requires():\n    if building:\n        raise RuntimeError('build-context-%s-%s' % (build_variant_index, build_variant_requires[0].name))\n    return []\n",
        ).unwrap();
        let developer = DeveloperPackage::from_path(source.path()).unwrap();
        let original = developer.package.to_data().unwrap();
        let mut process = BuildProcess::new(source.path(), None, Some("noop"), None).unwrap();
        process
            .set_package(developer.package.clone(), Some(developer))
            .unwrap();
        let error = process
            .build(
                &source.path().join("repo"),
                false,
                false,
                Some(&[1]),
                false,
                false,
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("build-context-1-python"), "{error}");
        assert_eq!(
            process.package.as_ref().unwrap().to_data().unwrap(),
            original
        );
        assert_eq!(process.build_system.name(), "noop");
        assert_eq!(process.build_path, source.path().join("build"));
        assert!(!source.path().join("repo").exists());
    }

    #[test]
    fn test_build_installs_all_deferred_includes_before_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let modules = temp.path().join("modules");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&modules).unwrap();
        fs::write(modules.join("shared_a.py"), "value = 'first'\n").unwrap();
        fs::write(modules.join("shared_b.py"), "value = 'second'\n").unwrap();
        let module_directory = serde_json::to_string(&modules.to_string_lossy()).unwrap();
        fs::write(source.join("package.py"), format!(
            "name = 'include_app'\nversion = '1.0'\nconfig = {{'package_definition_python_path': {module_directory}}}\n@include('shared_a', 'shared_b')\ndef commands():\n    env.INCLUDE_A = shared_a.value\n    env.INCLUDE_B = shared_b.value\n",
        )).unwrap();
        let developer = DeveloperPackage::from_path(&source).unwrap();
        assert_eq!(developer.includes, vec!["shared_a", "shared_b"]);
        let mut process = BuildProcess::new(&source, None, Some("noop"), None).unwrap();
        process
            .set_package(developer.package.clone(), Some(developer))
            .unwrap();
        let repo = temp.path().join("repo");
        let results = process
            .build(&repo, false, true, None, true, false)
            .unwrap();
        assert!(results.iter().all(|result| result.success));
        let version_dir = repo.join("include_app/1.0");
        for name in ["shared_a", "shared_b"] {
            assert_eq!(
                fs::read(version_dir.join(format!(".rez/include/{name}.py"))).unwrap(),
                fs::read(modules.join(format!("{name}.py"))).unwrap(),
            );
            assert!(version_dir
                .join(format!(".rez/include/{name}.sha1"))
                .is_file());
        }
        assert!(crate::serialise::find_package_definition_file(
            &version_dir,
            crate::serialise::PACKAGE_DEFINITION_EXTENSIONS,
        )
        .unwrap()
        .is_some());
    }

    #[test]
    fn test_include_install_preserves_bytes_and_writes_rez_hashes() {
        use sha1::{Digest, Sha1};
        let temp = tempfile::tempdir().unwrap();
        let modules = temp.path().join("modules");
        fs::create_dir_all(&modules).unwrap();
        let payload = b"\x0b \r\n# coding: latin-1\nvalue = '\xff'\n\x0c";
        fs::write(modules.join("first.py"), payload).unwrap();
        fs::write(modules.join("second.py"), b"value = 2\n").unwrap();
        let mut package = Package::new("includes", version::Version::new("1.0").unwrap());
        package.config = Some(serde_json::json!({"package_definition_python_path": modules}));
        let mut developer = DeveloperPackage::new(package.clone(), temp.path().join("package.py"));
        developer.includes = vec!["first".into(), "second".into()];
        let mut process = BuildProcess::new(temp.path(), None, Some("noop"), None).unwrap();
        process.set_package(package, Some(developer)).unwrap();
        let version_dir = temp.path().join("repo/includes/1.0");
        process.install_include_modules(&version_dir).unwrap();
        let destination = version_dir.join(".rez/include");
        assert_eq!(fs::read(destination.join("first.py")).unwrap(), payload);
        let expected = crate::util::hex_encode(Sha1::digest(b"# coding: latin-1\nvalue = '\xff'"));
        assert_eq!(
            fs::read_to_string(destination.join("first.sha1")).unwrap(),
            expected
        );
        assert!(destination.join("second.py").is_file());
        assert!(destination.join("second.sha1").is_file());
        assert!(!version_dir.join("package.yaml").exists());
    }

    #[test]
    fn test_include_install_validates_all_sources_before_writing() {
        let temp = tempfile::tempdir().unwrap();
        let modules = temp.path().join("modules");
        fs::create_dir_all(&modules).unwrap();
        fs::write(modules.join("first.py"), "value = 1\n").unwrap();
        let mut package = Package::new("includes", version::Version::new("1.0").unwrap());
        package.config = Some(serde_json::json!({"package_definition_python_path": modules}));
        let mut developer = DeveloperPackage::new(package.clone(), temp.path().join("package.py"));
        developer.includes = vec!["first".into(), "missing".into()];
        let mut process = BuildProcess::new(temp.path(), None, Some("noop"), None).unwrap();
        process.set_package(package, Some(developer)).unwrap();
        let version_dir = temp.path().join("repo/includes/1.0");
        assert!(process.install_include_modules(&version_dir).is_err());
        assert!(!version_dir.exists());
        process.developer_package.as_mut().unwrap().includes = vec!["../outside".into()];
        assert!(process.install_include_modules(&version_dir).is_err());
        assert!(!version_dir.exists());
    }

    #[test]
    fn test_build_source_identity_mismatch_preserves_existing_state() {
        let source = tempfile::tempdir().unwrap();
        let mut process = BuildProcess::new(source.path(), None, Some("noop"), None).unwrap();
        let original = Package::new("original", version::Version::new("1.0").unwrap());
        process.set_package(original.clone(), None).unwrap();
        let other = Package::new("other", version::Version::new("1.0").unwrap());
        let developer = DeveloperPackage::new(other, source.path().join("package.py"));
        assert!(process
            .set_package(original.clone(), Some(developer))
            .is_err());
        assert_eq!(process.package.as_ref().unwrap().name, original.name);
        assert!(process.developer_package.is_none());
    }

    #[test]
    fn test_build_installs_into_package_and_variant_directories() {
        let source = tempdir("variant_install_source");
        let repo = tempdir("variant_install_repo");
        let version = version::Version::new("1.0.0").unwrap();
        let mut package = Package::new("my_pkg", version);
        package.variants = vec![
            vec![version::Requirement::new("python-3.11").unwrap()],
            vec![version::Requirement::new("python-3.12").unwrap()],
        ];
        package.hashed_variants = false;

        let mut process = BuildProcess::new(&source, None, Some("noop"), None).unwrap();
        process.set_package(package.clone(), None).unwrap();
        let results = process
            .build(&repo, false, true, None, true, false)
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].build_path,
            source.join("build").join("python-3.11")
        );
        assert_eq!(
            results[1].build_path,
            source.join("build").join("python-3.12")
        );
        assert_eq!(
            results[0].install_path,
            Some(repo.join("my_pkg").join("1.0.0").join("python-3.11"))
        );
        assert_eq!(
            results[1].install_path,
            Some(repo.join("my_pkg").join("1.0.0").join("python-3.12"))
        );

        let hashed_repo = repo.join("hashed");
        package.hashed_variants = true;
        let mut hashed_process = BuildProcess::new(&source, None, Some("noop"), None).unwrap();
        hashed_process.set_package(package, None).unwrap();
        let hashed_results = hashed_process
            .build(&hashed_repo, false, true, None, true, false)
            .unwrap();
        let hashed_install_paths: Vec<_> = hashed_results
            .iter()
            .map(|result| result.install_path.as_ref().unwrap())
            .collect();
        assert_eq!(hashed_install_paths.len(), 2);
        assert_eq!(
            hashed_install_paths[0],
            &hashed_repo
                .join("my_pkg")
                .join("1.0.0")
                .join("b99a49e4ec48ad4d9833734782ee775813473768")
        );
        assert_eq!(
            hashed_install_paths[1],
            &hashed_repo
                .join("my_pkg")
                .join("1.0.0")
                .join("a2588a16ab802913232ccb76fd69640200385572")
        );
        assert!(hashed_install_paths.iter().all(|path| {
            path.parent() == Some(hashed_repo.join("my_pkg").join("1.0.0").as_path())
        }));

        // Skipped variants still report their own variant-specific build path.
        let mut skip_package = Package::new("skip_pkg", version::Version::new("2.0.0").unwrap());
        skip_package.variants = vec![vec![version::Requirement::new("python-3.11").unwrap()]];
        skip_package.hashed_variants = false;
        let skipped_install_dir = repo.join("skip_pkg").join("2.0.0").join("python-3.11");

        let mut skip_process = BuildProcess::new(&source, None, Some("noop"), None).unwrap();
        skip_process.set_package(skip_package, None).unwrap();
        skip_process
            .build(&repo, false, true, None, true, false)
            .unwrap();
        let skipped_results = skip_process
            .build(&repo, false, true, None, false, false)
            .unwrap();
        assert_eq!(skipped_results.len(), 1);
        assert_eq!(skipped_results[0].elapsed_secs, 0.0);
        assert_eq!(
            skipped_results[0].build_path,
            source.join("build").join("python-3.11")
        );
        assert_eq!(skipped_results[0].install_path, Some(skipped_install_dir));

        let release_version_dir = repo.join("release").join("my_pkg").join("1.0.0");
        let package = Package::new("my_pkg", version::Version::new("1.0.0").unwrap());
        let mut release_process = BuildProcess::new(&source, None, Some("noop"), None).unwrap();
        release_process.set_package(package, None).unwrap();
        let release_results = release_process
            .build(&release_version_dir, false, true, None, true, false)
            .unwrap();

        assert_eq!(release_results[0].install_path, Some(release_version_dir));

        cleanup(&source);
        cleanup(&repo);
    }

    #[test]
    fn installation_publishes_only_successful_selected_variants() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir_all(&source).unwrap();
        let repo = temp.path().join("repo");
        let mut package = Package::from_yaml(
            "name: partial_probe\nversion: '1.0'\npackage_type: tool\ncustom: [one, two]\nvariants: [[python-3.11], [python-3.12]]\n",
        ).unwrap();
        package.hashed_variants = false;
        let mut process = BuildProcess::new(&source, None, Some("noop"), None).unwrap();
        process.set_package(package.clone(), None).unwrap();
        assert!(process
            .build(&repo, false, true, Some(&[2]), true, false)
            .is_err());
        assert!(
            !repo.exists(),
            "invalid variant must fail before installing anything"
        );
        process
            .build(&repo, false, true, Some(&[0]), true, false)
            .unwrap();
        let definition = repo.join("partial_probe/1.0/package.yaml");
        let first_data = crate::repository::load_package_data(&definition).unwrap();
        let first = Package::from_data(first_data).unwrap();
        assert_eq!(first.variants, vec![package.variants[0].clone()]);
        assert_eq!(
            first.attributes["custom"],
            serde_json::json!(["one", "two"])
        );
        let old_bytes = fs::read(&definition).unwrap();

        let mut failing =
            BuildProcess::new(&source, Some("exit 1".into()), Some("custom"), None).unwrap();
        failing.set_package(package.clone(), None).unwrap();
        let failed = failing
            .build(&repo, false, true, Some(&[1]), true, false)
            .unwrap();
        assert!(!failed[0].success);
        assert_eq!(fs::read(&definition).unwrap(), old_bytes);
        process
            .build(&repo, false, true, Some(&[1]), true, false)
            .unwrap();
        let manager = crate::repository::PackageRepositoryManager::from_paths(&[repo]).unwrap();
        let installed = manager
            .get_package("partial_probe", &package.version)
            .unwrap()
            .unwrap()
            .to_package()
            .unwrap();
        assert_eq!(installed.variants, package.variants);
        assert_eq!(
            installed.attributes["package_type"],
            serde_json::json!("tool")
        );
        let base = installed.base.as_ref().unwrap();
        assert!(installed
            .iter_variants()
            .all(|variant| base.join(variant.subpath.as_deref().unwrap_or("")).is_dir()));
    }

    #[test]
    fn test_noop_build_succeeds() {
        let d = tempdir("bp_noop_exec");
        let install = d.join("install");
        let bp = BuildProcess::new(&d, None, Some("noop"), None).unwrap();
        let results = bp
            .build(&install, false, false, None, false, false)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].success);
        assert_eq!(results[0].elapsed_secs, 0.0);
        cleanup(&d);
    }

    #[test]
    fn test_custom_build_executes() {
        let d = tempdir("bp_custom_exec");
        let install = d.join("install");
        fs::create_dir_all(&install).ok();

        let bp = BuildProcess::new(&d, Some("echo ok".into()), None, None).unwrap();
        let results = bp
            .build(&install, false, false, None, false, false)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].success);
        cleanup(&d);
    }

    #[test]
    fn test_custom_build_fails_on_bad_command() {
        let d = tempdir("bp_custom_fail");
        let install = d.join("install");

        let bp = BuildProcess::new(&d, Some("exit 1".into()), None, None).unwrap();
        let results = bp
            .build(&install, false, false, None, false, false)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(!results[0].success);
        assert!(results[0].error.is_some());
        cleanup(&d);
    }

    // -- prepare_build_dir --

    #[test]
    fn test_prepare_build_dir() {
        let d = tempdir("prepare");
        let build = d.join("build");
        fs::create_dir_all(&build).ok();
        fs::write(build.join("artifact.o"), "data").ok();

        // clean=true removes contents
        prepare_build_dir(&build, true, Some(&d)).unwrap();
        assert!(build.exists());
        assert!(!build.join("artifact.o").exists());

        // clean=false preserves contents
        fs::write(build.join("artifact.o"), "data2").ok();
        prepare_build_dir(&build, false, Some(&d)).unwrap();
        assert!(build.join("artifact.o").exists());
        cleanup(&d);
    }

    #[test]
    fn configured_build_directories_share_package_and_variant_layout() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir_all(&source).unwrap();
        let scratch = temp.path().join("scratch");
        let config = crate::config::RezConfig {
            build_directory: scratch.to_string_lossy().into_owned(),
            ..crate::config::RezConfig::default()
        };
        let mut process = BuildProcess::new(&source, None, Some("noop"), Some(&config)).unwrap();
        // A shared root cannot be built or cleaned before package identity is known.
        assert!(process
            .build(&temp.path().join("repo"), true, false, None, false, false)
            .is_err());
        assert!(!scratch.exists());

        let mut package = Package::new("app", version::Version::new("1.0").unwrap());
        package.hashed_variants = false;
        package.variants = vec![vec![version::Requirement::new("python-3.11").unwrap()]];
        process.set_package(package.clone(), None).unwrap();
        let expected = scratch.join("app").join("1.0");
        assert_eq!(process.build_path, expected);
        // Metadata setup is idempotent: never append identity twice.
        process.set_package(package, None).unwrap();
        assert_eq!(process.build_path, expected);
        let results = process
            .build(&temp.path().join("repo"), false, false, None, false, false)
            .unwrap();
        assert_eq!(results[0].build_path, expected.join("python-3.11"));
        assert!(results[0].build_path.is_dir());
        let env = process
            .standard_env_vars(0, &results[0].build_path, temp.path(), false, None)
            .unwrap();
        assert_eq!(
            env["REZ_BUILD_PATH"],
            results[0].build_path.to_string_lossy()
        );

        process.set_package_info("other", "2.0", 1, None).unwrap();
        assert_eq!(process.build_path, scratch.join("other").join("2.0"));
        process
            .set_package_info("unversioned", "", 1, None)
            .unwrap();
        assert_eq!(process.build_path, scratch.join("unversioned"));
        let before = process.build_path.clone();
        assert!(process
            .set_package_info("../escape", "2.0", 1, None)
            .is_err());
        assert_eq!(process.build_path, before);
    }

    #[test]
    fn relative_and_package_build_directory_overrides_use_one_policy() {
        let temp = tempfile::tempdir().unwrap();
        let config = crate::config::RezConfig {
            build_directory: "out/debug".into(),
            ..crate::config::RezConfig::default()
        };
        let mut process =
            BuildProcess::new(temp.path(), None, Some("noop"), Some(&config)).unwrap();
        let mut package = Package::new("app", version::Version::new("1.0").unwrap());
        process.set_package(package.clone(), None).unwrap();
        assert_eq!(process.build_path, temp.path().join("out/debug"));
        package.config = Some(serde_json::json!({"build_directory": "custom"}));
        process.set_package(package.clone(), None).unwrap();
        assert_eq!(process.build_path, temp.path().join("custom"));
        let root = temp.path().join("scratch");
        package.config = Some(serde_json::json!({"build_directory": root}));
        process.set_package(package.clone(), None).unwrap();
        assert_eq!(process.build_path, root.join("app/1.0"));
        package.config = Some(serde_json::json!({"build_directory": 42}));
        assert!(process.set_package(package.clone(), None).is_err());
        assert_eq!(process.build_path, root.join("app/1.0"));
        package.config = None;
        process.set_package(package, None).unwrap();
        assert_eq!(process.build_path, temp.path().join("out/debug"));
    }

    #[test]
    fn clean_build_rejects_source_and_ancestor_directories() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir_all(&source).unwrap();
        let sentinel = source.join("package.py");
        fs::write(&sentinel, "name = 'app'").unwrap();
        for directory in [".", ".."] {
            let config = crate::config::RezConfig {
                build_directory: directory.into(),
                ..crate::config::RezConfig::default()
            };
            let mut process =
                BuildProcess::new(&source, None, Some("noop"), Some(&config)).unwrap();
            process.set_package_info("app", "1.0", 1, None).unwrap();
            assert!(process
                .build(&temp.path().join("repo"), true, false, None, false, false)
                .is_err());
            assert_eq!(fs::read_to_string(&sentinel).unwrap(), "name = 'app'");
        }
        let build = source.join("build");
        fs::create_dir_all(&build).unwrap();
        fs::write(build.join("old.o"), "old").unwrap();
        prepare_build_dir(&build, true, Some(&source)).unwrap();
        assert!(!build.join("old.o").exists());
        assert!(sentinel.exists());
    }

    // -- Standard env vars --

    #[test]
    fn test_standard_env_vars() {
        let d = tempdir("env_vars");
        let build_path = d.join("build").join("variant");
        let install = d.join("install").join("variant");
        let mut bp = BuildProcess::new(&d, None, Some("noop"), None).unwrap();
        bp.set_package_info("my_pkg", "1.0.0", 2, None).unwrap();

        let vars = bp
            .standard_env_vars(0, &build_path, &install, false, None)
            .unwrap();
        assert_eq!(vars["REZ_BUILD_ENV"], "1");
        assert_eq!(vars["REZ_BUILD_PROJECT_NAME"], "my_pkg");
        assert_eq!(vars["REZ_BUILD_PROJECT_VERSION"], "1.0.0");
        assert_eq!(vars["REZ_BUILD_VARIANT_INDEX"], "0");
        assert_eq!(vars["REZ_BUILD_PATH"], build_path.to_string_lossy());
        assert_eq!(vars["REZ_BUILD_INSTALL"], "0");
        assert_eq!(vars["REZ_BUILD_TYPE"], "local");

        // install=true flips the flag
        let vars2 = bp
            .standard_env_vars(1, &build_path, &install, true, None)
            .unwrap();
        assert_eq!(vars2["REZ_BUILD_INSTALL"], "1");
        assert_eq!(vars2["REZ_BUILD_VARIANT_INDEX"], "1");
        cleanup(&d);
    }

    #[test]
    fn test_standard_env_uses_context_requests_and_exact_source() {
        use repository::provider::MemoryPackageProvider;
        use resolve::context::ResolveOptions;
        use version::Requirement;
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("package.py"), "name = 'other'\n").unwrap();
        let definition = temp.path().join("chosen.yaml");
        fs::write(&definition,
            "name: metadata_env\nversion: '1.0'\nrequires: [runtime-1]\nbuild_requires: [builder-2]\nprivate_build_requires: [private-3]\nvariants: [[variant-1]]\n").unwrap();
        let source = DeveloperPackage::from_path(&definition).unwrap();
        let mut process = BuildProcess::new(temp.path(), None, Some("noop"), None).unwrap();
        process
            .set_package(source.package.clone(), Some(source))
            .unwrap();
        let fallback = process
            .standard_env_vars(
                0,
                &temp.path().join("build"),
                &temp.path().join("install"),
                false,
                None,
            )
            .unwrap();
        assert_eq!(
            fallback["REZ_BUILD_PROJECT_FILE"],
            definition.to_string_lossy()
        );
        assert_eq!(
            fallback["REZ_BUILD_REQUIRES"],
            "runtime-1 variant-1 builder-2 private-3"
        );
        assert_eq!(
            fallback["REZ_BUILD_REQUIRES_UNVERSIONED"],
            "runtime variant builder private"
        );

        let mut provider = MemoryPackageProvider::new();
        for (name, version) in [("build_runtime", "2"), ("variant", "1"), ("implicit", "3")] {
            provider.add(
                Package::from_data(HashMap::from([
                    ("name".into(), serde_json::json!(name)),
                    ("version".into(), serde_json::json!(version)),
                ]))
                .unwrap(),
            );
        }
        let context = ResolvedContext::resolve(
            ["build_runtime-2", "variant-1"]
                .into_iter()
                .map(|request| Requirement::new(request).unwrap())
                .collect(),
            &provider,
            ResolveOptions {
                building: true,
                implicit_packages: Some(vec![Requirement::new("implicit-3").unwrap()]),
                ..ResolveOptions::default()
            },
        )
        .unwrap();
        assert!(context.success());
        let actual = process
            .standard_env_vars(
                0,
                &temp.path().join("build"),
                &temp.path().join("install"),
                false,
                Some(&context),
            )
            .unwrap();
        assert_eq!(
            actual["REZ_BUILD_REQUIRES"],
            "build_runtime-2 variant-1 implicit-3"
        );
        assert_eq!(
            actual["REZ_BUILD_REQUIRES_UNVERSIONED"],
            "build_runtime variant implicit"
        );
        assert_eq!(
            actual["REZ_BUILD_PROJECT_FILE"],
            definition.to_string_lossy()
        );
    }

    // -- BuildResult --

    #[test]
    fn extraction_publishes_staged_payload_and_preserves_existing_payload_on_failure() {
        let owner = tempfile::tempdir().unwrap();
        let source = owner.path().join("source");
        let repo = owner.path().join("repo");
        fs::create_dir_all(&source).unwrap();
        let mut archive = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(3);
        header.set_mode(0o644);
        header.set_cksum();
        archive
            .append_data(&mut header, "outer/payload.txt", b"new".as_slice())
            .unwrap();
        fs::write(source.join("payload.tar"), archive.into_inner().unwrap()).unwrap();
        fs::write(
            source.join("sources.yaml"),
            "sources:\n  - path: payload.tar\n",
        )
        .unwrap();
        let package = Package::new("staged_probe", version::Version::new("1.0").unwrap());
        let mut process = BuildProcess::new(&source, None, Some("extraction"), None).unwrap();
        process.set_package(package, None).unwrap();
        let result = process
            .build(&repo, false, true, None, true, false)
            .unwrap();
        assert!(result[0].success);
        let installed = repo.join("staged_probe/1.0");
        assert_eq!(fs::read(installed.join("payload.txt")).unwrap(), b"new");
        let definition = crate::serialise::find_package_definition_file(
            &installed,
            crate::serialise::PACKAGE_DEFINITION_EXTENSIONS,
        )
        .unwrap()
        .unwrap();
        let original_metadata = fs::read(&definition).unwrap();
        fs::write(installed.join("payload.txt"), b"valuable").unwrap();
        fs::write(source.join("broken.zip"), b"invalid archive").unwrap();
        fs::write(
            source.join("sources.yaml"),
            "sources:\n  - path: payload.tar\n  - path: broken.zip\n",
        )
        .unwrap();
        let error = process
            .build(&repo, false, true, None, true, false)
            .unwrap_err();
        assert!(error.to_string().contains("Invalid zip"), "{error}");
        assert_eq!(
            fs::read(installed.join("payload.txt")).unwrap(),
            b"valuable"
        );
        assert_eq!(fs::read(definition).unwrap(), original_metadata);
    }

    #[test]
    fn test_build_result() {
        let ok = BuildResult::ok(PathBuf::from("/b"), 1.5);
        assert!(ok.success);
        assert!(ok.error.is_none());

        let fail = BuildResult::fail(PathBuf::from("/b"), 0.5, "oops".into());
        assert!(!fail.success);
        assert_eq!(fail.error.as_deref(), Some("oops"));
    }

    // -- Helpers --

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rez_test_{}", name));
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::create_dir_all(&dir);
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }
}
