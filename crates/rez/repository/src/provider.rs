// SPDX-License-Identifier: Apache-2.0
//! Repository candidates, provenance and providers shared with resolution and copying.

use crate::errors::{Result, RezError};
use crate::{log_debug, log_info};
use model::package::{Package, Variant};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use version::{Requirement, RequirementList, Version, VersionRange};

/// Rez resource handle for a filesystem package variant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceHandle {
    pub key: ResourceHandleKey,
    pub variables: ResourceHandleVariables,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceHandleKey {
    #[serde(rename = "filesystem.variant")]
    FilesystemVariant,
    #[serde(rename = "filesystem.variant.combined")]
    FilesystemVariantCombined,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceHandleVariables {
    pub repository_type: String,
    pub location: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ext: Option<String>,
}

impl ResourceHandle {
    /// Decode and validate the canonical resource schema before any path transformation.
    pub fn from_json(value: &serde_json::Value, scope: Option<&str>) -> Result<Self> {
        let result = (|| -> Result<Self> {
            let variables = value
                .get("variables")
                .and_then(serde_json::Value::as_object)
                .ok_or_else(|| RezError::ResolvedContext("variables must be an object".into()))?;
            if !variables.contains_key("index") {
                return Err(RezError::ResolvedContext(
                    "variables.index is required".into(),
                ));
            }
            match value.get("key").and_then(serde_json::Value::as_str) {
                Some("filesystem.variant") if variables.contains_key("ext") => {
                    return Err(RezError::ResolvedContext(
                        "variables.ext is not valid for filesystem.variant".into(),
                    ));
                }
                Some("filesystem.variant.combined")
                    if !variables
                        .get("ext")
                        .is_some_and(serde_json::Value::is_string) =>
                {
                    return Err(RezError::ResolvedContext(
                        "variables.ext is required for filesystem.variant.combined".into(),
                    ));
                }
                _ => {}
            }
            let handle: Self = serde_json::from_value(value.clone()).map_err(|error| {
                RezError::ResolvedContext(format!("invalid Rez resource handle: {error}"))
            })?;
            handle.validate()?;
            Ok(handle)
        })();
        result.map_err(|error| match (scope, error) {
            (Some(scope), RezError::ResolvedContext(message)) => {
                RezError::ResolvedContext(format!("{scope}.{message}"))
            }
            (Some(scope), error) => RezError::ResolvedContext(format!("{scope}: {error}")),
            (None, error) => error,
        })
    }

    /// Adjust only filesystem repository identity at the bundle file boundary.
    /// In-memory handles and external absolute locations remain unchanged.
    #[doc(hidden)]
    pub fn adjust_location(&mut self, bundle_path: &Path, out: bool) -> Result<()> {
        if self.variables.repository_type != "filesystem" {
            return Ok(());
        }
        let location = Path::new(&self.variables.location);
        if out {
            if !location.is_absolute() {
                return Err(RezError::ResolvedContext(format!(
                    "cannot save relative repository location {:?}",
                    self.variables.location
                )));
            }
            let canonical = match location.canonicalize() {
                Ok(path) => path,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    // Match realpath for deleted package repositories: canonicalize the
                    // existing ancestor and normalize only the nonexistent suffix.
                    let mut canonical = None;
                    for ancestor in location.ancestors().skip(1) {
                        match ancestor.canonicalize() {
                            Ok(mut base) => {
                                for component in location
                                    .strip_prefix(ancestor)
                                    .expect("path ancestor")
                                    .components()
                                {
                                    match component {
                                        std::path::Component::ParentDir => {
                                            base.pop();
                                        }
                                        std::path::Component::CurDir => {}
                                        _ => base.push(component),
                                    }
                                }
                                canonical = Some(base);
                                break;
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                            Err(error) => return Err(error.into()),
                        }
                    }
                    canonical.ok_or_else(|| {
                        RezError::ResolvedContext(format!(
                            "cannot canonicalize repository location {:?}",
                            self.variables.location
                        ))
                    })?
                }
                Err(error) => return Err(error.into()),
            };
            if canonical.starts_with(bundle_path) {
                let relative = pathdiff::diff_paths(canonical, bundle_path).ok_or_else(|| {
                    RezError::ResolvedContext(
                        "cannot make bundled repository location relative".into(),
                    )
                })?;
                self.variables.location = relative.to_string_lossy().into_owned();
            }
        } else if !location.is_absolute() {
            self.variables.location = bundle_path
                .join(location)
                .canonicalize()?
                .to_string_lossy()
                .into_owned();
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        let variables = &self.variables;
        if variables.repository_type != "filesystem" {
            return Err(RezError::ResolvedContext(format!(
                "unsupported repository_type {:?}",
                variables.repository_type
            )));
        }
        if variables.location.is_empty() || variables.name.is_empty() {
            return Err(RezError::ResolvedContext(
                "resource handle requires non-empty location and name".into(),
            ));
        }
        if let Some(version) = variables.version.as_deref() {
            if version.is_empty() {
                return Err(RezError::ResolvedContext(
                    "unversioned package handles must omit version".into(),
                ));
            }
            Version::new(version).map_err(|error| {
                RezError::ResolvedContext(format!(
                    "invalid package version {:?}: {}",
                    version, error
                ))
            })?;
        }
        crate::serialise::validate_rez_package_path(&variables.name, variables.version.as_deref())?;
        match self.key {
            ResourceHandleKey::FilesystemVariant if variables.ext.is_some() => Err(
                RezError::ResolvedContext("filesystem.variant handles must not contain ext".into()),
            ),
            ResourceHandleKey::FilesystemVariantCombined => match variables.ext.as_deref() {
                Some("py" | "yaml") => Ok(()),
                _ => Err(RezError::ResolvedContext(format!(
                    "filesystem.variant.combined requires ext 'py' or 'yaml', got {:?}",
                    variables.ext
                ))),
            },
            ResourceHandleKey::FilesystemVariant => Ok(()),
        }
    }
}

/// Explicit repository provenance for a package selected by a provider.
#[derive(Clone, Debug)]
pub struct PackageProvenance {
    pub repository_type: String,
    pub location: String,
    pub source: Option<PackageSource>,
}

/// A package and the repository source from which it was loaded.
#[derive(Clone, Debug)]
pub struct PackageCandidate {
    pub package: Package,
    pub provenance: Option<PackageProvenance>,
}

impl PackageCandidate {
    /// Materialize an exact variant while retaining its package and source provenance.
    pub fn into_variant(self, index: Option<usize>, building: bool) -> Result<PackageVariant> {
        let package = self.package;
        let mut variant = match (package.variants.is_empty(), index) {
            (true, None) => Variant::from_package(package),
            (true, Some(index)) => {
                return Err(RezError::ResolvedContext(format!(
                    "variant index {index} supplied for non-variant package {}",
                    package.name
                )));
            }
            (false, None) => {
                return Err(RezError::ResolvedContext(format!(
                    "variant index omitted for package {} with {} variants",
                    package.name,
                    package.variants.len()
                )));
            }
            (false, Some(index)) => Variant::new(package, index)?,
        };
        variant.root = variant.root();
        Ok(PackageVariant::new(variant, building, self.provenance))
    }

    fn without_provenance(package: Package) -> Self {
        Self {
            package,
            provenance: None,
        }
    }
}

impl PackageProvenance {
    pub fn resource_handle(
        &self,
        name: &str,
        version: &Version,
        index: Option<usize>,
    ) -> Option<ResourceHandle> {
        if self.repository_type != "filesystem" {
            return None;
        }
        let source = self.source.as_ref()?;
        let (key, ext) = match source.kind {
            PackageSourceKind::Directory => (ResourceHandleKey::FilesystemVariant, None),
            PackageSourceKind::Python | PackageSourceKind::Yaml => {
                let ext = source.path.extension()?.to_str()?.to_owned();
                (ResourceHandleKey::FilesystemVariantCombined, Some(ext))
            }
        };
        Some(ResourceHandle {
            key,
            variables: ResourceHandleVariables {
                repository_type: self.repository_type.clone(),
                location: self.location.clone(),
                name: name.to_owned(),
                version: (!version.is_empty()).then(|| version.to_string()),
                index,
                ext,
            },
        })
    }
}

/// Provides packages to the solver. Implement this for filesystem, memory, or
/// network-backed package repositories.
pub trait PackageProvider {
    /// Get all packages matching a name whose version falls within the range.
    /// Returns `Ok` with an empty vector when no matching packages exist.
    fn get_packages(&self, name: &str, range: &VersionRange) -> Result<Vec<Package>>;

    /// Get package candidates while preserving provider provenance where available.
    fn get_candidates(&self, name: &str, range: &VersionRange) -> Result<Vec<PackageCandidate>> {
        Ok(self
            .get_packages(name, range)?
            .into_iter()
            .map(PackageCandidate::without_provenance)
            .collect())
    }

    /// Get all packages for a name (any version), propagating provider errors.
    fn get_all_packages(&self, name: &str) -> Result<Vec<Package>> {
        let packages = self.get_packages(name, &VersionRange::any())?;
        if packages.is_empty() {
            return Err(RezError::PackageFamilyNotFound(format!(
                "package family not found: {}",
                name
            )));
        }
        Ok(packages)
    }

    /// Return a stable identity for persistent resolve-cache entries.
    ///
    /// Implementations may return `Some` only when the value changes whenever
    /// any repository metadata affecting package selection or resolved results
    /// changes. Providers without that guarantee must return `None`.
    fn cache_identity(&self) -> Option<String> {
        None
    }

    /// Get all packages and their source provenance for a family.
    fn get_all_candidates(&self, name: &str) -> Result<Vec<PackageCandidate>> {
        let candidates = self.get_candidates(name, &VersionRange::any())?;
        if candidates.is_empty() {
            return Err(RezError::PackageFamilyNotFound(format!(
                "package family not found: {}",
                name
            )));
        }
        Ok(candidates)
    }
}

/// In-memory package provider for testing.
#[derive(Clone, Debug, Default)]
pub struct MemoryPackageProvider {
    packages: HashMap<String, Vec<Package>>,
}

impl MemoryPackageProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a package to the provider.
    pub fn add(&mut self, pkg: Package) {
        self.packages.entry(pkg.name.clone()).or_default().push(pkg);
    }
}

impl PackageProvider for MemoryPackageProvider {
    fn get_packages(&self, name: &str, range: &VersionRange) -> Result<Vec<Package>> {
        Ok(self
            .packages
            .get(name)
            .map(|pkgs| {
                pkgs.iter()
                    .filter(|p| range.contains_version(&p.version))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }
}

// ---------------------------------------------------------------------------
// PackageVariant - wraps Variant with lazy RequirementList
// ---------------------------------------------------------------------------

/// A variant of a package within the solver context.
/// Wraps a Variant and lazily computes its RequirementList.
#[derive(Clone, Debug)]
pub struct PackageVariant {
    pub variant: Variant,
    pub building: bool,
    pub provenance: Option<PackageProvenance>,
    requires_list: Option<RequirementList>,
}

impl PackageVariant {
    pub fn new(variant: Variant, building: bool, provenance: Option<PackageProvenance>) -> Self {
        Self {
            variant,
            building,
            provenance,
            requires_list: None,
        }
    }

    #[doc(hidden)]
    pub fn cached_requirements(&self) -> Option<&RequirementList> {
        self.requires_list.as_ref()
    }

    pub fn name(&self) -> &str {
        self.variant.name()
    }

    pub fn version(&self) -> &Version {
        self.variant.version()
    }

    pub fn index(&self) -> Option<usize> {
        self.variant.index
    }

    /// Get the requirements list, computing it lazily.
    pub fn requires_list(&mut self) -> &RequirementList {
        if self.requires_list.is_none() {
            let mut requires = self.variant.parent.requires.clone();
            if self.building {
                requires.extend(self.variant.parent.build_requires.clone());
            }
            // Add variant-specific requires
            requires.extend(self.variant.variant_requires.clone());
            self.requires_list = Some(RequirementList::new(requires));
        }
        self.requires_list.as_ref().expect("just set")
    }

    /// Get requirement for a specific package family.
    pub fn get_req(&mut self, pkg_name: &str) -> Option<Requirement> {
        self.requires_list().get(pkg_name).cloned()
    }

    /// Get all non-conflict family names this variant requires.
    pub fn request_fams(&mut self) -> HashSet<String> {
        self.requires_list().names().clone()
    }

    /// Get all conflict family names this variant requires.
    pub fn conflict_request_fams(&mut self) -> HashSet<String> {
        self.requires_list().conflict_names().clone()
    }
}

impl PartialEq for PackageVariant {
    fn eq(&self, other: &Self) -> bool {
        self.name() == other.name()
            && self.version() == other.version()
            && self.index() == other.index()
    }
}

impl Eq for PackageVariant {}

impl fmt::Display for PackageVariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let idx = match self.index() {
            Some(i) => format!("{}", i),
            None => String::new(),
        };
        if self.version().is_truthy() {
            write!(f, "{}-{}[{}]", self.name(), self.version(), idx)
        } else {
            write!(f, "{}[{}]", self.name(), idx)
        }
    }
}

use crate::config::CONFIG;
use crate::repository::{
    FsRepoCached, FsRepoMemcached, MemcacheClient, PackageRepository, PackageSource,
    PackageSourceKind,
};

/// Filesystem-based package provider that reads from repository directories.
///
/// Uses FsRepoCached (or FsRepoMemcached when memcached_uri set) for caching.
/// Each path follows rez layout: `path/pkg_name/version/package.yaml`
pub struct FilesystemPackageProvider {
    repos: Vec<Box<dyn PackageRepository>>,
}

impl FilesystemPackageProvider {
    /// Create from a list of repository paths.
    /// Uses FsRepoMemcached when memcached_uri is set and cache_listdir/cache_package_files;
    /// otherwise FsRepoCached. Missing paths are absent repositories; invalid or
    /// unreadable configured paths are errors, including when a cache fallback fails.
    pub fn from_paths(paths: &[PathBuf]) -> Result<Self> {
        log_info!(
            "solver",
            "Creating package provider from {} path(s)",
            paths.len()
        );
        let use_memcache = !CONFIG.memcached_uri.is_empty()
            && (CONFIG.cache_listdir || CONFIG.cache_package_files);
        let create_if_missing = false;

        let mut repos: Vec<Box<dyn PackageRepository>> = Vec::new();
        for path in paths {
            let repo: Box<dyn PackageRepository> = if use_memcache {
                let memcache = MemcacheClient::new(&CONFIG.memcached_uri);
                match FsRepoMemcached::new(
                    path,
                    memcache,
                    CONFIG.cache_listdir,
                    CONFIG.cache_package_files,
                    create_if_missing,
                ) {
                    Ok(Some(repo)) => {
                        log_debug!("solver", "Added repo (memcached): {}", path.display());
                        Box::new(repo)
                    }
                    Ok(None) => continue,
                    Err(e) => {
                        log_info!(
                            "solver",
                            "Memcache repo failed for {}, fallback to cached: {}",
                            path.display(),
                            e
                        );
                        match FsRepoCached::new(path, create_if_missing) {
                            Ok(Some(repo)) => Box::new(repo),
                            Ok(None) => continue,
                            Err(error) => {
                                return Err(RezError::PackageRepository(format!(
                                    "Failed to open configured repository {}: {error}",
                                    path.display()
                                )));
                            }
                        }
                    }
                }
            } else {
                match FsRepoCached::new(path, create_if_missing) {
                    Ok(Some(repo)) => {
                        log_debug!("solver", "Added repo (cached): {}", path.display());
                        Box::new(repo)
                    }
                    Ok(None) => continue,
                    Err(error) => {
                        return Err(RezError::PackageRepository(format!(
                            "Failed to open configured repository {}: {error}",
                            path.display()
                        )));
                    }
                }
            };
            repos.push(repo);
        }
        log_info!(
            "solver",
            "Package provider ready with {} repos",
            repos.len()
        );
        Ok(Self { repos })
    }

    /// Create from a single repository path.
    pub fn from_path(path: &Path) -> Result<Self> {
        Self::from_paths(&[path.to_path_buf()])
    }
}

impl FilesystemPackageProvider {
    /// Resolve a serialized Rez variant handle against its exact repository and source.
    pub fn get_candidate_for_handle(&self, handle: &ResourceHandle) -> Result<PackageCandidate> {
        handle.validate()?;
        let variables = &handle.variables;
        let version = variables
            .version
            .as_deref()
            .map(Version::new)
            .transpose()
            .map_err(|error| {
                RezError::ResolvedContext(format!(
                    "invalid package version {:?}: {}",
                    variables.version, error
                ))
            })?
            .unwrap_or_else(Version::empty);
        let exact_provider;
        let repo = match self
            .repos
            .iter()
            .find(|repo| repo.location() == variables.location)
        {
            Some(repo) => repo,
            None => {
                exact_provider = Self::from_path(Path::new(&variables.location))?;
                exact_provider.repos.first().ok_or_else(|| {
                    RezError::ResolvedContext(format!(
                        "repository location {:?} does not exist",
                        variables.location
                    ))
                })?
            }
        };
        if repo.name() != variables.repository_type {
            return Err(RezError::ResolvedContext(format!(
                "repository at {:?} has type {:?}, expected {:?}",
                variables.location,
                repo.name(),
                variables.repository_type
            )));
        }

        let source = PackageSource {
            kind: match handle.key {
                ResourceHandleKey::FilesystemVariant => PackageSourceKind::Directory,
                ResourceHandleKey::FilesystemVariantCombined => match variables.ext.as_deref() {
                    Some("py") => PackageSourceKind::Python,
                    Some("yaml") => PackageSourceKind::Yaml,
                    _ => unreachable!("handle validation checks combined extension"),
                },
            },
            path: match handle.key {
                ResourceHandleKey::FilesystemVariant => Path::new(repo.location())
                    .join(&variables.name)
                    .join(version.to_string()),
                ResourceHandleKey::FilesystemVariantCombined => Path::new(repo.location()).join(
                    format!("{}.{}", variables.name, variables.ext.as_deref().unwrap()),
                ),
            },
        };
        let (info, source) = repo
            .get_package_with_source(&variables.name, &version, Some(&source))?
            .ok_or_else(|| {
                RezError::ResolvedContext(format!(
                    "package {:?}-{} is missing from repository {:?}",
                    variables.name,
                    variables.version.as_deref().unwrap_or(""),
                    variables.location
                ))
            })?;
        let source = source.ok_or_else(|| {
            RezError::ResolvedContext(format!(
                "repository {:?} did not provide source provenance for package {:?}-{}",
                variables.location,
                variables.name,
                variables.version.as_deref().unwrap_or("")
            ))
        })?;
        match handle.key {
            ResourceHandleKey::FilesystemVariant if source.kind != PackageSourceKind::Directory => {
                return Err(RezError::ResolvedContext(format!(
                    "handle key {:?} does not match source {:?} at {}",
                    handle.key,
                    source.kind,
                    source.path.display()
                )));
            }
            ResourceHandleKey::FilesystemVariantCombined
                if source.kind == PackageSourceKind::Directory =>
            {
                return Err(RezError::ResolvedContext(format!(
                    "handle key {:?} does not match directory source at {}",
                    handle.key,
                    source.path.display()
                )));
            }
            ResourceHandleKey::FilesystemVariantCombined => {
                let actual_ext = source
                    .path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .ok_or_else(|| {
                        RezError::ResolvedContext(format!(
                            "combined package source has no UTF-8 extension: {}",
                            source.path.display()
                        ))
                    })?;
                if variables.ext.as_deref() != Some(actual_ext) {
                    return Err(RezError::ResolvedContext(format!(
                        "combined handle extension {:?} does not match source {}",
                        variables.ext,
                        source.path.display()
                    )));
                }
            }
            ResourceHandleKey::FilesystemVariant => {}
        }

        let package = info.to_package()?;
        if package.name != variables.name || package.version != version {
            return Err(RezError::ResolvedContext(format!(
                "repository returned {:?}-{} for handle {:?}-{}",
                package.name,
                package.version,
                variables.name,
                variables.version.as_deref().unwrap_or("")
            )));
        }
        let provenance = PackageProvenance {
            repository_type: repo.name().to_owned(),
            location: repo.location().to_owned(),
            source: Some(source),
        };
        Ok(PackageCandidate {
            package,
            provenance: Some(provenance),
        })
    }
}

impl PackageProvider for FilesystemPackageProvider {
    fn get_packages(&self, name: &str, range: &VersionRange) -> Result<Vec<Package>> {
        Ok(self
            .get_candidates(name, range)?
            .into_iter()
            .map(|candidate| candidate.package)
            .collect())
    }

    fn get_candidates(&self, name: &str, range: &VersionRange) -> Result<Vec<PackageCandidate>> {
        let mut seen = HashSet::new();
        let mut result = Vec::new();

        for repo in &self.repos {
            // Get all versions for this package name
            let versions = repo.iter_versions(name)?;

            for version in &versions {
                // Filter by range and dedup (first repo wins)
                if !range.contains_version(version) {
                    continue;
                }
                let key = version.to_string();
                if seen.contains(&key) {
                    continue;
                }

                // Load package data and retain the exact repository source that won precedence.
                if let Some((info, source)) = repo.get_package_with_source(name, version, None)? {
                    let source = source.ok_or_else(|| {
                        RezError::PackageRepository(format!(
                            "filesystem repository {:?} omitted source provenance for {}-{}",
                            repo.location(),
                            name,
                            version
                        ))
                    })?;
                    result.push(PackageCandidate {
                        package: info.to_package()?,
                        provenance: Some(PackageProvenance {
                            repository_type: repo.name().to_owned(),
                            location: repo.location().to_owned(),
                            source: Some(source),
                        }),
                    });
                    seen.insert(key);
                }
            }
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fs_provider_missing_path_is_not_created() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("configured").join("repository");
        let provider = FilesystemPackageProvider::from_path(&missing).unwrap();

        assert!(!missing.exists());
        assert!(provider.repos.is_empty());
        assert!(matches!(
            provider.get_all_packages("foo"),
            Err(RezError::PackageFamilyNotFound(_))
        ));
    }
}
