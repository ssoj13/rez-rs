// SPDX-License-Identifier: Apache-2.0

//! Context bundling into relocatable directories.
//!
//! Rust port of Python rez bundle_context.py.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::{Result, RezError};
use crate::package::ops::{copy_package, CopyOptions};
use crate::resolve::context::ResolvedContext;
use foundation::filesystem::copy_dir_contents;
use repository::provider::{FilesystemPackageProvider, PackageCandidate, ResourceHandle};

#[cfg(target_os = "linux")]
use bin_patch_elf::ElfContainer;

#[cfg(target_os = "macos")]
use bin_patch_macho::MachoContainer;

// ============================================================================
// Types
// ============================================================================

/// Options controlling how a context is bundled.
#[derive(Debug, Clone, Default)]
pub struct BundleOptions {
    /// Skip packages marked non-relocatable instead of erroring.
    pub skip_non_relocatable: bool,
    /// Force relocate even non-relocatable packages.
    pub force: bool,
    /// Suppress informational output.
    pub quiet: bool,
    /// Enable verbose output (overridden by quiet).
    pub verbose: bool,
    /// Patch libs and executables to remap external paths to bundle-relative paths.
    pub patch_libs: bool,
}

/// Result of a successful bundle operation.
#[derive(Debug, Clone)]
pub struct BundleResult {
    /// Root directory of the bundle.
    pub bundle_path: PathBuf,
    /// Names of packages that were copied.
    pub packages_copied: Vec<String>,
    /// Total bytes copied across all packages.
    pub total_bytes: u64,
    /// Path to the retargeted context.rxt inside the bundle.
    pub context_path: PathBuf,
}

/// Serialized into bundle.yaml metadata file.
#[derive(Debug, Serialize, Deserialize)]
struct BundleMeta {
    #[serde(default)]
    logs: Vec<String>,
}

// ============================================================================
// BundleContext - main bundler
// ============================================================================

/// Bundles a resolved context (.rxt) with its package payloads into a
/// self-contained relocatable directory.
///
/// Layout produced:
/// ```text
///   dest_dir/
///     context.rxt          # retargeted context
///     bundle.yaml          # bundle metadata + logs
///     packages/
///       <name>/<version>/  # copied package payloads
/// ```
pub struct BundleContext {
    source_rxt: PathBuf,
    dest_dir: PathBuf,
    options: BundleOptions,
    logs: Vec<String>,
    /// Track copied variants: package_name -> (src_root, dest_root)
    copied_variants: HashMap<String, (PathBuf, PathBuf)>,
}

impl BundleContext {
    /// Create a new bundler.
    ///
    /// `source_rxt` - path to the resolved context .rxt (JSON or YAML) file.
    /// `dest_dir`   - destination directory (must not exist).
    pub fn new(source_rxt: impl Into<PathBuf>, dest_dir: impl Into<PathBuf>) -> Self {
        Self::with_options(source_rxt, dest_dir, BundleOptions::default())
    }

    /// Create a new bundler with explicit options.
    pub fn with_options(
        source_rxt: impl Into<PathBuf>,
        dest_dir: impl Into<PathBuf>,
        options: BundleOptions,
    ) -> Self {
        Self {
            source_rxt: source_rxt.into(),
            dest_dir: dest_dir.into(),
            options,
            logs: Vec::new(),
            copied_variants: HashMap::new(),
        }
    }

    /// Execute the bundling process through the canonical context and package publisher.
    pub fn bundle(&mut self) -> Result<BundleResult> {
        if !self.source_rxt.try_exists()? {
            return Err(RezError::ContextBundle(format!(
                "Source context not found: {}",
                self.source_rxt.display()
            )));
        }
        match fs::symlink_metadata(&self.dest_dir) {
            Ok(_) => {
                return Err(RezError::ContextBundle(format!(
                    "Dest dir must not exist: {}",
                    self.dest_dir.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let context = ResolvedContext::load(&self.source_rxt, None)?;
        if !context.success() {
            return Err(RezError::ContextBundle(
                "Cannot bundle an unsolved context".into(),
            ));
        }
        let mut data = context.to_json()?;
        let provider = FilesystemPackageProvider::from_paths(&[])?;
        let mut selections = Vec::new();
        for raw_handle in data["resolved_packages"]
            .as_array()
            .ok_or_else(|| RezError::ContextBundle("Resolved packages must be an array".into()))?
        {
            let handle = ResourceHandle::from_json(raw_handle, None)?;
            let candidate = provider.get_candidate_for_handle(&handle)?;
            let config = candidate.package.config(None)?;
            let relocatable = candidate
                .package
                .is_relocatable(Some(&handle.variables.location), Some(&config));
            let copy = self.options.force || relocatable || !self.options.skip_non_relocatable;
            if copy {
                if !relocatable && !self.options.force {
                    return Err(RezError::ContextBundle(format!(
                        "Package {} is not relocatable. Use force or skip_non_relocatable.",
                        candidate.package.qualified_name()
                    )));
                }
                let variant = candidate
                    .clone()
                    .into_variant(handle.variables.index, false)?;
                let root = variant.variant.root.as_ref().ok_or_else(|| {
                    RezError::ContextBundle(format!(
                        "Package {} has no filesystem payload root",
                        candidate.package.qualified_name()
                    ))
                })?;
                if !root.is_dir() {
                    return Err(RezError::ContextBundle(format!(
                        "Source variant root is not a directory: {}",
                        root.display()
                    )));
                }
            }
            selections.push((handle, candidate, copy));
        }
        let destination = self.dest_dir.clone();
        let absolute = std::path::absolute(&destination)?;
        let parent = absolute
            .parent()
            .ok_or_else(|| RezError::ContextBundle("Destination has no parent directory".into()))?;
        fs::create_dir_all(parent)?;
        let stage = tempfile::Builder::new()
            .prefix(".rez-bundle-")
            .tempdir_in(parent)?;
        self.dest_dir = stage.path().to_path_buf();
        self.logs.clear();
        self.copied_variants.clear();
        let staged = (|| -> Result<(Vec<String>, u64)> {
            self.init_bundle()?;
            let (names, bytes) = self.copy_variants(&mut data, &selections)?;
            self.write_retargeted_context(&mut data, &absolute.join("packages"))?;
            if self.options.patch_libs {
                self.patch_libs()?;
            }
            if self
                .source_rxt
                .parent()
                .is_some_and(|parent| parent.join("bundle.yaml").is_file())
            {
                let post_commands = self
                    .source_rxt
                    .parent()
                    .expect("checked source parent")
                    .join("post_commands.py");
                if post_commands.try_exists()? {
                    fs::copy(post_commands, self.dest_dir.join("post_commands.py"))?;
                }
            }
            self.finalize_bundle()?;
            Ok((names, bytes))
        })();
        self.dest_dir = destination.clone();
        let (packages_copied, total_bytes) = staged?;
        crate::platform::rename(stage.path(), &destination, false)?;
        Ok(BundleResult {
            context_path: destination.join("context.rxt"),
            bundle_path: destination,
            packages_copied,
            total_bytes,
        })
    }

    /// Path to the packages repository inside the bundle.
    fn repo_path(&self) -> PathBuf {
        self.dest_dir.join("packages")
    }

    /// Record an info log entry.
    fn info(&mut self, msg: impl Into<String>) {
        self.logs.push(format!("INFO: {}", msg.into()));
    }

    /// Record a warning log entry.
    fn warning(&mut self, msg: impl Into<String>) {
        let m = msg.into();
        if !self.options.quiet {
            eprintln!("[WARNING] {}", m);
        }
        self.logs.push(format!("WARNING: {}", m));
    }

    /// Create the initial bundle directory structure.
    fn init_bundle(&self) -> Result<()> {
        fs::create_dir_all(&self.dest_dir)?;
        fs::create_dir_all(self.repo_path())?;

        // Write empty bundle.yaml marker (signals this is a bundle dir)
        let meta = BundleMeta { logs: Vec::new() };
        let yaml_str = serde_yaml::to_string(&meta).map_err(|e| {
            RezError::ContextBundle(format!("Failed to serialize bundle.yaml: {}", e))
        })?;
        fs::write(self.dest_dir.join("bundle.yaml"), &yaml_str)?;

        // Write settings.yaml disabling memcache (local bundle = fast access)
        let settings = serde_yaml::to_string(&serde_yaml::Mapping::from_iter([(
            serde_yaml::Value::String("disable_memcached".into()),
            serde_yaml::Value::Bool(true),
        )]))
        .map_err(|e| {
            RezError::ContextBundle(format!("Failed to serialize settings.yaml: {}", e))
        })?;
        fs::write(self.repo_path().join("settings.yaml"), &settings)?;

        Ok(())
    }

    /// Finalize the bundle by writing logs into bundle.yaml.
    fn finalize_bundle(&self) -> Result<()> {
        let meta = BundleMeta {
            logs: self.logs.clone(),
        };
        let yaml_str = serde_yaml::to_string(&meta).map_err(|e| {
            RezError::ContextBundle(format!("Failed to serialize bundle.yaml: {}", e))
        })?;
        fs::write(self.dest_dir.join("bundle.yaml"), &yaml_str)?;
        Ok(())
    }

    /// Publish selected variants and preserve their actual destination handles.
    fn copy_variants(
        &mut self,
        context_data: &mut Value,
        selections: &[(ResourceHandle, PackageCandidate, bool)],
    ) -> Result<(Vec<String>, u64)> {
        let mut names = Vec::new();
        let mut total_bytes = 0;
        let destination_provider = FilesystemPackageProvider::from_paths(&[])?;
        for (position, (handle, candidate, copy)) in selections.iter().enumerate() {
            if !copy {
                self.warning(format!(
                    "Skipped non-relocatable package: {}",
                    candidate.package.qualified_name()
                ));
                continue;
            }
            let source_index = handle.variables.index.unwrap_or(0);
            let result = copy_package(
                candidate,
                &self.repo_path(),
                &CopyOptions {
                    variants: Some(vec![source_index]),
                    force: self.options.force,
                    keep_timestamp: true,
                    ..CopyOptions::default()
                },
            )?;
            let copied_handle = result
                .handles
                .iter()
                .find(|(index, _)| *index == source_index)
                .map(|(_, handle)| handle)
                .ok_or_else(|| {
                    RezError::ContextBundle(
                        "Publisher did not return the selected destination handle".into(),
                    )
                })?;
            let source_variant = candidate
                .clone()
                .into_variant(handle.variables.index, false)?;
            let destination_variant = destination_provider
                .get_candidate_for_handle(copied_handle)?
                .into_variant(copied_handle.variables.index, false)?;
            let source_root = source_variant
                .variant
                .root
                .ok_or_else(|| RezError::ContextBundle("Source variant root missing".into()))?;
            let destination_root = destination_variant.variant.root.ok_or_else(|| {
                RezError::ContextBundle("Destination variant root missing".into())
            })?;
            self.copied_variants.insert(
                candidate.package.name.clone(),
                (source_root, destination_root.clone()),
            );
            for relative in &result.copied {
                self.info(format!(
                    "Published {}: {relative}",
                    candidate.package.qualified_name()
                ));
            }
            context_data["resolved_packages"][position] = serde_json::to_value(copied_handle)?;
            names.push(candidate.package.name.clone());
            // Count published files once, including common payload and include modules.
            let mut pending = vec![result.dest_path];
            while let Some(directory) = pending.pop() {
                for entry in fs::read_dir(directory)? {
                    let entry = entry?;
                    let metadata = entry.file_type()?;
                    if metadata.is_dir() {
                        pending.push(entry.path());
                    } else if metadata.is_file() {
                        total_bytes += entry.metadata()?.len();
                    }
                }
            }
            self.info(format!(
                "Copied {} to {}",
                candidate.package.qualified_name(),
                destination_root.display()
            ));
        }
        Ok((names, total_bytes))
    }

    /// Validate and save the retargeted canonical context with the bundle marker policy.
    fn write_retargeted_context(
        &self,
        context_data: &mut Value,
        destination_repository: &Path,
    ) -> Result<PathBuf> {
        remap_context(context_data, destination_repository, &[])?;
        let context = ResolvedContext::from_json(context_data, None)?;
        let path = self.dest_dir.join("context.rxt");
        context.save(&path)?;
        Ok(path)
    }

    /// Apply binary patching to bundle libs and executables.
    #[cfg(target_os = "linux")]
    fn patch_libs(&mut self) -> Result<()> {
        self.patch_libs_linux()
    }

    /// Apply binary patching to bundle libs and executables.
    #[cfg(target_os = "macos")]
    fn patch_libs(&mut self) -> Result<()> {
        self.patch_libs_macos()
    }

    /// Apply binary patching to bundle libs and executables.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn patch_libs(&mut self) -> Result<()> {
        self.info("Lib patching not supported on this platform (Windows/other), skipped");
        Ok(())
    }

    /// Patch ELF binaries on Linux (remap runpaths to $ORIGIN-relative).
    /// Matches Python: finds executables OR files with .so, .so., .so- in name.
    #[cfg(target_os = "linux")]
    fn patch_libs_linux(&mut self) -> Result<()> {
        let mut elfs = Vec::new();
        self.find_elf_files(&self.repo_path(), &mut elfs)?;

        if elfs.is_empty() {
            self.info("No ELF files found, thus no patching performed");
            return Ok(());
        }

        for elf_path in elfs {
            // Read ELF file
            let elf_data = match fs::read(&elf_path) {
                Ok(d) => d,
                Err(e) => {
                    self.warning(format!("Failed to read {}: {}", elf_path.display(), e));
                    continue;
                }
            };

            // Parse ELF
            let mut container = match ElfContainer::parse(&elf_data) {
                Ok(c) => c,
                Err(e) => {
                    // Skip non-ELF files (false positives from executable scripts)
                    if e.to_string().contains("Not an ELF")
                        || e.to_string().contains("Failed to read")
                    {
                        continue;
                    }
                    self.warning(format!("Failed to parse ELF {}: {}", elf_path.display(), e));
                    continue;
                }
            };

            // Get current rpaths
            let rpaths = container.get_rpath();
            if rpaths.is_empty() {
                continue; // Nothing to patch
            }

            // Build new rpaths
            let mut new_rpaths = Vec::new();
            let mut changed = false;

            for rpath in &rpaths {
                // Leave relative paths as-is (including $ORIGIN)
                if !Path::new(rpath).is_absolute() {
                    new_rpaths.push(rpath.clone());
                    continue;
                }

                // Check if rpath points into a bundled package
                let mut remapped = false;
                for (src_root, dest_root) in self.copied_variants.values() {
                    let rpath_path = Path::new(rpath);
                    if rpath_path.starts_with(src_root) {
                        // Remap to $ORIGIN-relative path
                        let relpath = rpath_path.strip_prefix(src_root).unwrap();
                        let new_rpath_abs = dest_root.join(relpath);
                        let elf_dir = elf_path.parent().unwrap();
                        let rel_to_elf =
                            pathdiff::diff_paths(&new_rpath_abs, elf_dir).ok_or_else(|| {
                                RezError::ContextBundle(format!(
                                    "Failed to compute relative path from {} to {}",
                                    elf_dir.display(),
                                    new_rpath_abs.display()
                                ))
                            })?;

                        // Use forward slashes for Unix paths
                        let rel_str = rel_to_elf.to_string_lossy().replace('\\', "/");
                        let new_rpath = format!("$ORIGIN/{}", rel_str);
                        new_rpaths.push(new_rpath.clone());
                        changed = true;
                        remapped = true;

                        self.info(format!(
                            "Remapped rpath {} in {} to {}",
                            rpath,
                            elf_path.display(),
                            new_rpath
                        ));
                        break;
                    }
                }

                if !remapped {
                    new_rpaths.push(rpath.clone());
                }
            }

            if !changed {
                self.info(format!(
                    "Left rpaths unchanged in {}: [{}]",
                    elf_path.display(),
                    rpaths.join(":")
                ));
                continue;
            }

            // Apply new runpath
            let new_runpath = new_rpaths.join(":");
            if let Err(e) = container.set_runpath(new_runpath.as_bytes()) {
                self.warning(format!(
                    "Failed to set runpath in {}: {}",
                    elf_path.display(),
                    e
                ));
                continue;
            }

            // Write patched ELF back
            if let Err(e) = container.write_to_path(&elf_path) {
                self.warning(format!(
                    "Failed to write patched ELF to {}: {}",
                    elf_path.display(),
                    e
                ));
                continue;
            }

            self.info(format!("Patched ELF {}", elf_path.display()));
        }

        Ok(())
    }

    /// Patch Mach-O binaries on macOS (remap install_name and rpaths to @loader_path-relative).
    #[cfg(target_os = "macos")]
    fn patch_libs_macos(&mut self) -> Result<()> {
        let mut machos = Vec::new();
        self.find_macho_files(&self.repo_path(), &mut machos)?;

        if machos.is_empty() {
            self.info("No Mach-O files found, thus no patching performed");
            return Ok(());
        }

        for macho_path in machos {
            // Read Mach-O file
            let macho_data = match fs::read(&macho_path) {
                Ok(d) => d,
                Err(e) => {
                    self.warning(format!("Failed to read {}: {}", macho_path.display(), e));
                    continue;
                }
            };

            // Parse Mach-O
            let mut container = match MachoContainer::parse(&macho_data) {
                Ok(c) => c,
                Err(e) => {
                    // Skip non-Mach-O files
                    if e.to_string().contains("Not a Mach-O") {
                        continue;
                    }
                    self.warning(format!(
                        "Failed to parse Mach-O {}: {}",
                        macho_path.display(),
                        e
                    ));
                    continue;
                }
            };

            // Extract actual paths from the binary (matches Python: only remap paths that exist)
            let (libs, rpaths) = container.get_paths();
            let macho_dir = macho_path.parent().unwrap();

            let mut install_name_remaps = Vec::new();
            let mut rpath_remaps = Vec::new();

            // Snapshot copied_variants so the closure doesn't capture self (allows self.info() later)
            let variant_pairs: Vec<_> = self
                .copied_variants
                .values()
                .map(|(s, d)| (s.clone(), d.clone()))
                .collect();

            let compute_remap = |old_path: &str| -> Option<String> {
                let old_path_p = Path::new(old_path);
                if !old_path_p.is_absolute() {
                    return None;
                }
                for (src_root, dest_root) in &variant_pairs {
                    if old_path_p.starts_with(src_root) {
                        let relpath = old_path_p.strip_prefix(src_root).ok()?;
                        let new_rpath_abs = dest_root.join(relpath);
                        let rel_to_macho = pathdiff::diff_paths(&new_rpath_abs, macho_dir)?;
                        let rel_str = rel_to_macho.to_string_lossy().replace('\\', "/");
                        return Some(format!("@loader_path/{}", rel_str));
                    }
                }
                None
            };

            for old_path in &libs {
                if let Some(ref new_path) = compute_remap(old_path) {
                    install_name_remaps.push((old_path.clone(), new_path.clone()));
                    self.info(format!(
                        "Remapped install_name {} in {} to {}",
                        old_path,
                        macho_path.display(),
                        new_path
                    ));
                }
            }
            for old_path in &rpaths {
                if let Some(ref new_path) = compute_remap(old_path) {
                    rpath_remaps.push((old_path.clone(), new_path.clone()));
                    self.info(format!(
                        "Remapped rpath {} in {} to {}",
                        old_path,
                        macho_path.display(),
                        new_path
                    ));
                }
            }

            if install_name_remaps.is_empty() && rpath_remaps.is_empty() {
                continue;
            }

            // Apply remaps
            if let Err(e) = container.remap_bundle_paths(&install_name_remaps, &rpath_remaps) {
                self.warning(format!(
                    "Failed to remap paths in Mach-O {}: {}",
                    macho_path.display(),
                    e
                ));
                continue;
            }

            // Write patched Mach-O back
            if let Err(e) = fs::write(&macho_path, &container.data) {
                self.warning(format!(
                    "Failed to write patched Mach-O to {}: {}",
                    macho_path.display(),
                    e
                ));
                continue;
            }

            self.info(format!("Patched Mach-O {}", macho_path.display()));
        }

        Ok(())
    }

    /// Find ELF candidates: executables OR files with .so, .so., .so- in name (matches Python).
    #[cfg(target_os = "linux")]
    fn find_elf_files(&self, dir: &Path, results: &mut Vec<PathBuf>) -> Result<()> {
        self.find_files_executable_or_ext(dir, results, &[".so", ".so.", ".so-"])?;
        Ok(())
    }

    /// Find Mach-O candidates: executables OR files with .dylib in name.
    #[cfg(target_os = "macos")]
    fn find_macho_files(&self, dir: &Path, results: &mut Vec<PathBuf>) -> Result<()> {
        self.find_files_executable_or_ext(dir, results, &[".dylib"])?;
        Ok(())
    }

    /// Find files that are executable OR have one of the extensions in filename.
    /// Used by platform-specific find_patchable_* methods (linux/macos only).
    #[cfg(unix)]
    fn find_files_executable_or_ext(
        &self,
        dir: &Path,
        results: &mut Vec<PathBuf>,
        ext_substrs: &[&str],
    ) -> Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                self.find_files_executable_or_ext(&path, results, ext_substrs)?;
            } else if path.is_file() {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default();
                #[cfg(unix)]
                let is_exec = {
                    use std::os::unix::fs::PermissionsExt;
                    fs::metadata(&path)
                        .ok()
                        .map(|m| m.permissions().mode() & 0o111 != 0)
                        .unwrap_or(false)
                };
                #[cfg(not(unix))]
                let is_exec = false;
                let matches_ext = ext_substrs.iter().any(|s| name.contains(s));
                if is_exec || matches_ext {
                    results.push(path);
                }
            }
        }
        Ok(())
    }
}

// ============================================================================
// Public free functions
// ============================================================================

/// Convenience function: bundle a context .rxt into a self-contained directory.
pub fn bundle_context(
    source_rxt: impl AsRef<Path>,
    dest_dir: impl AsRef<Path>,
    options: Option<BundleOptions>,
) -> Result<BundleResult> {
    let mut bundler = BundleContext::with_options(
        source_rxt.as_ref(),
        dest_dir.as_ref(),
        options.unwrap_or_default(),
    );
    bundler.bundle()
}

/// Retarget selected canonical handles to matching variants in an existing repository.
/// Unselected external handles remain unchanged; all metadata comes from typed candidates.
pub fn remap_context(
    data: &mut Value,
    new_packages_path: &Path,
    relocated_names: &[String],
) -> Result<()> {
    let provider = FilesystemPackageProvider::from_paths(&[])?;
    let handles = data
        .get_mut("resolved_packages")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| RezError::ContextBundle("resolved_packages must be an array".into()))?;
    for raw in handles {
        let handle = ResourceHandle::from_json(raw, None)?;
        if !relocated_names.contains(&handle.variables.name) {
            continue;
        }
        let source = provider
            .get_candidate_for_handle(&handle)?
            .into_variant(handle.variables.index, false)?;
        let mut destination_handle = handle.clone();
        destination_handle.key = repository::provider::ResourceHandleKey::FilesystemVariant;
        destination_handle.variables.ext = None;
        destination_handle.variables.location = new_packages_path.to_string_lossy().into_owned();
        let destination = provider.get_candidate_for_handle(&destination_handle)?;
        destination_handle.variables.index = if destination.package.variants.is_empty() {
            if !source.variant.variant_requires.is_empty() {
                return Err(RezError::ContextBundle(format!(
                    "Destination package {} has no matching variant",
                    handle.variables.name
                )));
            }
            None
        } else {
            Some(
                destination
                    .package
                    .variants
                    .iter()
                    .position(|requirements| requirements == &source.variant.variant_requires)
                    .ok_or_else(|| {
                        RezError::ContextBundle(format!(
                            "Destination package {} has no matching variant",
                            handle.variables.name
                        ))
                    })?,
            )
        };
        destination.into_variant(destination_handle.variables.index, false)?;
        *raw = serde_json::to_value(destination_handle)?;
    }
    data["package_paths"] = serde_json::json!([new_packages_path]);
    Ok(())
}

/// Copy a single package's payload into the bundle repository.
///
/// Source layout: `src_root/` (flat contents)
/// Dest layout:   `dest_root/<name>/<version>/` (contents copied here)
///
/// Returns total bytes copied.
pub fn copy_package_payload(
    src_root: &Path,
    dest_root: &Path,
    name: &str,
    version: &str,
) -> Result<u64> {
    crate::serialise::validate_rez_package_path(name, Some(version))?;
    let dest_pkg = dest_root.join(name).join(version);
    let copied_files = copy_dir_contents(src_root, &dest_pkg, false, true, Some(dest_root), None)?;

    // Calculate total bytes
    let mut total: u64 = 0;
    for rel_path in &copied_files {
        let full = dest_pkg.join(rel_path);
        if full.is_file() {
            total += fs::metadata(&full).map(|m| m.len()).unwrap_or(0);
        }
    }

    Ok(total)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use repository::provider::{ResourceHandleKey, ResourceHandleVariables};

    fn definition(repo: &Path, name: &str, version: &str, metadata: &str) -> PathBuf {
        let base = if version.is_empty() {
            repo.join(name)
        } else {
            repo.join(name).join(version)
        };
        fs::create_dir_all(&base).unwrap();
        let version_data = if version.is_empty() {
            String::new()
        } else {
            format!("version: '{version}'\n")
        };
        fs::write(
            base.join("package.yaml"),
            format!("name: {name}\n{version_data}{metadata}"),
        )
        .unwrap();
        fs::write(base.join("payload.dat"), "payload").unwrap();
        base
    }

    fn handle(repo: &Path, name: &str, version: &str, index: Option<usize>) -> ResourceHandle {
        ResourceHandle {
            key: ResourceHandleKey::FilesystemVariant,
            variables: ResourceHandleVariables {
                repository_type: "filesystem".into(),
                location: repo.to_string_lossy().into_owned(),
                name: name.into(),
                version: (!version.is_empty()).then(|| version.into()),
                index,
                ext: None,
            },
        }
    }

    fn context(path: &Path, handles: &[ResourceHandle], yaml: bool) -> PathBuf {
        let mut data = ResolvedContext::empty().to_json().unwrap();
        data["status"] = serde_json::json!("solved");
        data["resolved_packages"] = serde_json::to_value(handles).unwrap();
        data["package_paths"] = serde_json::json!([path.join("unavailable_historical_repository")]);
        let file = path.join("source.rxt");
        fs::create_dir_all(path).unwrap();
        if yaml {
            fs::write(&file, serde_yaml::to_string(&data).unwrap()).unwrap();
        } else {
            ResolvedContext::from_json(&data, None)
                .unwrap()
                .save(&file)
                .unwrap();
        }
        file
    }

    #[test]
    fn canonical_bundle_copies_real_payload_and_loads_json_and_yaml() {
        for yaml in [false, true] {
            let temporary = tempfile::tempdir().unwrap();
            let repo = temporary.path().join("repo");
            definition(
                &repo,
                "foo",
                "1",
                "custom: retained\ncommands: |\n  env.FOO_ROOT = '{this.root}'\n",
            );
            definition(&repo, "bar", "2", "");
            let rxt = context(
                &temporary.path().join("source"),
                &[
                    handle(&repo, "foo", "1", None),
                    handle(&repo, "bar", "2", None),
                ],
                yaml,
            );
            let destination = temporary.path().join("bundle");
            let result = bundle_context(rxt, &destination, None).unwrap();
            assert_eq!(result.packages_copied, ["foo", "bar"]);
            assert_eq!(result.bundle_path, destination);
            assert_eq!(result.context_path, destination.join("context.rxt"));
            assert!(result.total_bytes > 0);
            for (name, version) in [("foo", "1"), ("bar", "2")] {
                let root = destination.join("packages").join(name).join(version);
                assert!(crate::serialise::find_package_definition_file(
                    &root,
                    &["py", "yaml", "toml", "yml"]
                )
                .unwrap()
                .is_some());
                assert_eq!(
                    fs::read_to_string(root.join("payload.dat")).unwrap(),
                    "payload"
                );
            }
            let data: Value =
                serde_json::from_str(&fs::read_to_string(&result.context_path).unwrap()).unwrap();
            assert_eq!(
                data["resolved_packages"][0]["variables"]["location"],
                "packages"
            );
            let loaded = ResolvedContext::load(&result.context_path, None).unwrap();
            assert_eq!(
                loaded.get_environ(Some(HashMap::new())).unwrap()["FOO_ROOT"],
                destination
                    .join("packages")
                    .join("foo")
                    .join("1")
                    .canonicalize()
                    .unwrap()
                    .display()
                    .to_string()
            );
            let meta: BundleMeta =
                serde_yaml::from_str(&fs::read_to_string(destination.join("bundle.yaml")).unwrap())
                    .unwrap();
            assert!(!meta.logs.is_empty());
            assert!(destination.join("packages/settings.yaml").is_file());
        }
    }

    #[test]
    fn selected_nested_variant_uses_published_index_and_include_payload_after_move() {
        let temporary = tempfile::tempdir().unwrap();
        let repo = temporary.path().join("repo");
        let base = definition(
            &repo,
            "nested",
            "1",
            "variants: [[], ['dep-a'], ['dep-a', 'dep-b']]\nhashed_variants: false\ncustom: kept\ncommands: |\n  env.NESTED_ROOT = '{this.root}'\n",
        );
        fs::create_dir_all(base.join("dep-a/dep-b")).unwrap();
        fs::write(base.join("dep-a/other.dat"), "not selected").unwrap();
        fs::write(base.join("dep-a/dep-b/selected.dat"), "selected").unwrap();
        fs::create_dir_all(base.join(".rez/include")).unwrap();
        fs::write(base.join(".rez/include/helper.py"), "VALUE = 'included'\n").unwrap();
        let rxt = context(
            &temporary.path().join("source"),
            &[handle(&repo, "nested", "1", Some(2))],
            false,
        );
        let bundle = temporary.path().join("bundle");
        bundle_context(rxt, &bundle, None).unwrap();
        let serialized: Value =
            serde_json::from_str(&fs::read_to_string(bundle.join("context.rxt")).unwrap()).unwrap();
        assert_eq!(serialized["resolved_packages"][0]["variables"]["index"], 0);
        assert!(bundle
            .join("packages/nested/1/dep-a/dep-b/selected.dat")
            .is_file());
        assert!(!bundle.join("packages/nested/1/dep-a/other.dat").exists());
        assert!(bundle
            .join("packages/nested/1/.rez/include/helper.py")
            .is_file());
        let moved = temporary.path().join("moved");
        fs::rename(&bundle, &moved).unwrap();
        let loaded = ResolvedContext::load(&moved.join("context.rxt"), None).unwrap();
        let env = loaded.get_environ(Some(HashMap::new())).unwrap();
        assert_eq!(
            env["NESTED_ROOT"],
            moved
                .join("packages")
                .join("nested")
                .join("1")
                .join("dep-a")
                .join("dep-b")
                .canonicalize()
                .unwrap()
                .display()
                .to_string(),
            "{env:?}"
        );
        let provider = FilesystemPackageProvider::from_paths(&[]).unwrap();
        let copied = provider
            .get_candidate_for_handle(
                loaded.resolved_packages().unwrap()[0]
                    .resource_handle
                    .as_ref()
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(copied.package.attributes["custom"], "kept");
        #[cfg(windows)]
        {
            let shell = crate::shell::ShellType::Cmd;
            let output = loaded
                .execute_shell_output(Some(shell), "echo %NESTED_ROOT%", None, true, false, None)
                .unwrap();
            assert!(String::from_utf8_lossy(&output.stdout).contains("nested"));
            assert!(output.status.success());
        }
    }

    #[test]
    fn skip_preserves_external_handle_and_force_overrides_skip() {
        let temporary = tempfile::tempdir().unwrap();
        let repo = temporary.path().join("repo");
        definition(&repo, "yes", "1", "relocatable: true\n");
        definition(&repo, "no", "1", "relocatable: false\n");
        let external = handle(&repo, "no", "1", None);
        let rxt = context(
            &temporary.path().join("source"),
            &[handle(&repo, "yes", "1", None), external.clone()],
            false,
        );
        let destination = temporary.path().join("skip");
        let result = bundle_context(
            &rxt,
            &destination,
            Some(BundleOptions {
                skip_non_relocatable: true,
                ..Default::default()
            }),
        )
        .unwrap();
        assert_eq!(result.packages_copied, ["yes"]);
        let loaded = ResolvedContext::load(&result.context_path, None).unwrap();
        assert_eq!(
            loaded.resolved_packages().unwrap()[1]
                .resource_handle
                .as_ref(),
            Some(&external)
        );
        assert!(!destination.join("packages/no").exists());
        let forced = bundle_context(
            &rxt,
            temporary.path().join("force"),
            Some(BundleOptions {
                force: true,
                skip_non_relocatable: true,
                ..Default::default()
            }),
        )
        .unwrap();
        assert_eq!(forced.packages_copied, ["yes", "no"]);
    }

    #[test]
    fn policy_preflight_leaves_no_partial_bundle_and_preserves_foreign_destination() {
        let temporary = tempfile::tempdir().unwrap();
        let repo = temporary.path().join("repo");
        definition(&repo, "first", "1", "");
        definition(&repo, "no", "1", "config: {default_relocatable: false}\n");
        let rxt = context(
            &temporary.path().join("source"),
            &[
                handle(&repo, "first", "1", None),
                handle(&repo, "no", "1", None),
            ],
            false,
        );
        let destination = temporary.path().join("bundle");
        assert!(bundle_context(&rxt, &destination, None).is_err());
        assert!(!destination.exists());
        assert!(!fs::read_dir(temporary.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".rez-bundle-")
        }));
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("keep"), "foreign").unwrap();
        assert!(bundle_context(&rxt, &destination, None)
            .unwrap_err()
            .to_string()
            .contains("must not exist"));
        assert_eq!(
            fs::read_to_string(destination.join("keep")).unwrap(),
            "foreign"
        );
    }

    #[test]
    fn source_bundle_post_commands_remain_runtime_rex_and_survive_rebundle() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source_bundle");
        let repo = source.join("packages");
        definition(&repo, "foo", "1", "");
        let rxt = context(&source, &[handle(&repo, "foo", "1", None)], false);
        fs::write(source.join("bundle.yaml"), "{}\n").unwrap();
        fs::write(
            source.join("post_commands.py"),
            "setenv('BUNDLE_POST', 'runtime')\n",
        )
        .unwrap();
        let destination = temporary.path().join("new_bundle");
        let result = bundle_context(rxt, &destination, None).unwrap();
        assert!(destination.join("post_commands.py").is_file());
        assert_eq!(
            ResolvedContext::load(&result.context_path, None)
                .unwrap()
                .get_environ(Some(HashMap::new()))
                .unwrap()["BUNDLE_POST"],
            "runtime"
        );
    }

    #[test]
    fn remap_context_matches_variant_requirements_instead_of_source_index() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let destination = temporary.path().join("destination");
        definition(&source, "foo", "1", "variants: [['dep-a'], ['dep-b']]\n");
        definition(&destination, "foo", "1", "variants: [['dep-b']]\n");
        let mut data = ResolvedContext::empty().to_json().unwrap();
        data["resolved_packages"] = serde_json::json!([handle(&source, "foo", "1", Some(1))]);
        remap_context(&mut data, &destination, &["foo".into()]).unwrap();
        assert_eq!(data["resolved_packages"][0]["variables"]["index"], 0);
        assert_eq!(
            data["resolved_packages"][0]["variables"]["location"],
            destination.to_string_lossy().as_ref()
        );
        assert_eq!(data["package_paths"], serde_json::json!([destination]));
    }

    #[test]
    fn empty_canonical_context_bundles() {
        let temporary = tempfile::tempdir().unwrap();
        let rxt = context(&temporary.path().join("source"), &[], false);
        let result = bundle_context(rxt, temporary.path().join("bundle"), None).unwrap();
        assert!(result.packages_copied.is_empty());
        assert_eq!(result.total_bytes, 0);
        assert!(ResolvedContext::load(&result.context_path, None)
            .unwrap()
            .success());
    }

    #[test]
    fn missing_source_has_contextual_error() {
        let temporary = tempfile::tempdir().unwrap();
        assert!(bundle_context(
            temporary.path().join("missing.rxt"),
            temporary.path().join("bundle"),
            None
        )
        .unwrap_err()
        .to_string()
        .contains("not found"));
    }

    #[test]
    fn raw_payload_helper_preserves_data_and_rejects_unsafe_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let destination = temporary.path().join("destination");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("payload"), [0u8; 256]).unwrap();
        assert!(copy_package_payload(&source, &destination, "..", "1").is_err());
        assert!(copy_package_payload(&source, &destination, "foo", "1/0").is_err());
        assert!(!destination.exists());
        assert_eq!(
            copy_package_payload(&source, &destination, "foo", "1").unwrap(),
            256
        );
        assert!(destination.join("foo/1/payload").is_file());
    }
}
