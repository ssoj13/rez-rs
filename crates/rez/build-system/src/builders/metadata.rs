// SPDX-License-Identifier: Apache-2.0

//! Batch build metadata from the same lifecycle and resolver used by native builds.
//!
//! Prospective results are planning evidence. Installed-only results must be
//! refreshed before executing a driver; neither mode publishes or saves a context.

use crate::config::CONFIG;
use crate::errors::{Result, RezError};
use crate::package::DeveloperPackage;
use repository::provider::{
    FilesystemPackageProvider, PackageCandidate, PackageProvider, ResourceHandle,
};
use resolve::context::ResolvedContext;
use serde::Serialize;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use version::{Version, VersionRange};

#[derive(Debug, Serialize)]
pub struct BatchMetadata {
    pub producer: &'static str,
    pub schema: u32,
    pub mode: MetadataMode,
    pub packages: Vec<SourceMetadata>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetadataMode {
    Prospective,
    Installed,
}

#[derive(Debug, Serialize)]
pub struct SourceMetadata {
    pub source_path: PathBuf,
    pub aliases: Vec<PathBuf>,
    pub definition_path: Option<PathBuf>,
    pub name: Option<String>,
    pub version: Option<String>,
    pub error: Option<String>,
    pub variants: Vec<VariantMetadata>,
}

#[derive(Debug, Serialize)]
pub struct VariantMetadata {
    pub index: Option<usize>,
    pub requested: Vec<String>,
    pub resolve: ResolveMetadata,
}

#[derive(Debug, Default, Serialize)]
pub struct ResolveMetadata {
    pub success: bool,
    pub error: Option<String>,
    pub selected: Vec<SelectedPackage>,
    pub prerequisites: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageOrigin {
    Installed,
    Source,
}

#[derive(Debug, Serialize)]
pub struct SelectedPackage {
    pub name: String,
    pub version: String,
    pub index: Option<usize>,
    pub origin: PackageOrigin,
    pub source_path: Option<PathBuf>,
    pub resource_handle: Option<ResourceHandle>,
}

#[derive(Clone)]
struct Candidate {
    package: PackageCandidate,
    source_path: Option<PathBuf>,
}

/// Pins each family for one batch so selected identities retain their exact
/// winning origin. Actual repositories precede prospective source identities.
struct CompositePackageProvider<'a> {
    installed: &'a dyn PackageProvider,
    prospective: Vec<Candidate>,
    families: RefCell<HashMap<String, Vec<Candidate>>>,
}

impl<'a> CompositePackageProvider<'a> {
    fn new(installed: &'a dyn PackageProvider, prospective: Vec<Candidate>) -> Self {
        Self {
            installed,
            prospective,
            families: RefCell::new(HashMap::new()),
        }
    }

    fn family(&self, name: &str) -> Result<Vec<Candidate>> {
        if let Some(candidates) = self.families.borrow().get(name) {
            return Ok(candidates.clone());
        }
        let mut seen = HashSet::<Version>::new();
        let mut candidates = Vec::new();
        for package in self.installed.get_candidates(name, &VersionRange::any())? {
            if seen.insert(package.package.version.clone()) {
                candidates.push(Candidate {
                    package,
                    source_path: None,
                });
            }
        }
        for candidate in &self.prospective {
            if candidate.package.package.name == name
                && seen.insert(candidate.package.package.version.clone())
            {
                candidates.push(candidate.clone());
            }
        }
        self.families
            .borrow_mut()
            .insert(name.to_owned(), candidates.clone());
        Ok(candidates)
    }

    fn selected(&self, context: &ResolvedContext) -> Result<ResolveMetadata> {
        let mut selected = Vec::new();
        let mut prerequisites = Vec::new();
        for package in context.resolved_packages().unwrap_or_default() {
            let candidate = self
                .family(&package.name)?
                .into_iter()
                .find(|candidate| candidate.package.package.version == package.version)
                .ok_or_else(|| {
                    RezError::ResolvedContext(format!(
                        "Resolved identity {}-{} has no pinned candidate",
                        package.name, package.version
                    ))
                })?;
            let expected = candidate
                .package
                .provenance
                .as_ref()
                .and_then(|provenance| {
                    provenance.resource_handle(
                        &package.name,
                        &package.version,
                        package.variant_index,
                    )
                });
            if expected != package.resource_handle {
                return Err(RezError::ResolvedContext(format!(
                    "Resolved source changed for {}-{}",
                    package.name, package.version
                )));
            }
            if let Some(source) = &candidate.source_path {
                if !prerequisites.contains(source) {
                    prerequisites.push(source.clone());
                }
            }
            selected.push(SelectedPackage {
                name: package.name.clone(),
                version: package.version.to_string(),
                index: package.variant_index,
                origin: if candidate.source_path.is_some() {
                    PackageOrigin::Source
                } else {
                    PackageOrigin::Installed
                },
                source_path: candidate.source_path,
                resource_handle: package.resource_handle.clone(),
            });
        }
        Ok(ResolveMetadata {
            success: true,
            error: None,
            selected,
            prerequisites,
        })
    }
}

impl PackageProvider for CompositePackageProvider<'_> {
    fn get_packages(
        &self,
        name: &str,
        range: &VersionRange,
    ) -> Result<Vec<crate::package::Package>> {
        Ok(self
            .get_candidates(name, range)?
            .into_iter()
            .map(|candidate| candidate.package)
            .collect())
    }

    fn get_candidates(&self, name: &str, range: &VersionRange) -> Result<Vec<PackageCandidate>> {
        Ok(self
            .family(name)?
            .into_iter()
            .filter(|candidate| range.contains_version(&candidate.package.package.version))
            .map(|candidate| candidate.package)
            .collect())
    }

    // The inherited None cache identity disables persistent resolve caching.
}

/// Load all definitions before resolving any target. Sources used as dependency
/// candidates retain ordinary (building=false) metadata; only each current build
/// target is reevaluated with building=true and its original variant bindings.
pub fn collect(
    sources: &[PathBuf],
    package_paths: Option<Vec<PathBuf>>,
    installed_only: bool,
) -> Result<BatchMetadata> {
    let paths = package_paths.unwrap_or_else(|| CONFIG.expanded_packages_path_os());
    let installed = FilesystemPackageProvider::from_paths(&paths)?;
    let working_directory = std::env::current_dir()?;
    let mut packages = Vec::<SourceMetadata>::new();
    let mut definitions = Vec::new();
    let mut seen_inputs = HashSet::<PathBuf>::new();
    let mut seen_definitions = HashMap::<PathBuf, usize>::new();
    let mut identities = HashMap::<(String, Version), Vec<usize>>::new();
    for source in sources {
        let canonical = std::fs::canonicalize(source);
        let source_path = canonical.as_ref().cloned().unwrap_or_else(|_| {
            if source.is_absolute() {
                source.clone()
            } else {
                working_directory.join(source)
            }
        });
        if !seen_inputs.insert(source_path.clone()) {
            continue;
        }
        // Discover through the same configured filename order as the developer
        // loader before execution, so directory/file aliases do not rerun Python.
        let located = canonical.map_err(RezError::from).and_then(|path| {
            let file = if path.is_dir() {
                crate::serialise::find_package_definition_file(
                    &path,
                    crate::serialise::PACKAGE_DEFINITION_EXTENSIONS,
                )?
                .ok_or_else(|| {
                    RezError::PackageNotFound(format!(
                        "No package definition found in {}",
                        path.display()
                    ))
                })?
            } else {
                path.clone()
            };
            Ok((path, std::fs::canonicalize(file)?))
        });
        if let Ok((_, definition)) = &located {
            if let Some(&existing) = seen_definitions.get(definition) {
                packages[existing].aliases.push(source_path);
                continue;
            }
        }
        let loaded = located.and_then(|(path, canonical_definition)| {
            let definition = DeveloperPackage::from_path(&path)?;
            if std::fs::canonicalize(&definition.filepath)? != canonical_definition {
                return Err(RezError::PackageMetadata {
                    msg: "Package definition changed during source loading".into(),
                    path: Some(definition.filepath.clone()),
                    resource_key: None,
                });
            }
            Ok((definition, canonical_definition))
        });
        let index = packages.len();
        match loaded {
            Ok((definition, canonical_definition)) => {
                seen_definitions.insert(canonical_definition.clone(), index);
                identities
                    .entry((
                        definition.package.name.clone(),
                        definition.package.version.clone(),
                    ))
                    .or_default()
                    .push(index);
                packages.push(SourceMetadata {
                    source_path,
                    aliases: Vec::new(),
                    definition_path: Some(canonical_definition),
                    name: Some(definition.package.name.clone()),
                    version: Some(definition.package.version.to_string()),
                    error: None,
                    variants: Vec::new(),
                });
                definitions.push(Some(definition));
            }
            Err(error) => {
                packages.push(SourceMetadata {
                    source_path,
                    aliases: Vec::new(),
                    definition_path: None,
                    name: None,
                    version: None,
                    error: Some(error.to_string()),
                    variants: Vec::new(),
                });
                definitions.push(None);
            }
        }
    }
    for ((name, version), indices) in identities {
        if indices.len() > 1 {
            for index in indices {
                packages[index].error = Some(format!(
                    "Multiple source definitions provide {name}-{version}; source ownership is ambiguous"
                ));
                definitions[index] = None;
            }
        }
    }
    let prospective = if installed_only {
        Vec::new()
    } else {
        definitions
            .iter()
            .enumerate()
            .filter_map(|(index, definition)| {
                definition.as_ref().map(|definition| Candidate {
                    package: PackageCandidate {
                        package: definition.package.clone(),
                        provenance: None,
                    },
                    source_path: Some(packages[index].source_path.clone()),
                })
            })
            .collect()
    };
    let provider = CompositePackageProvider::new(&installed, prospective);
    for (item, definition) in packages.iter_mut().zip(definitions) {
        let Some(definition) = definition else {
            continue;
        };
        for index in 0..definition.package.variants.len().max(1) {
            let variant_index = (!definition.package.variants.is_empty()).then_some(index);
            let mut variant = VariantMetadata {
                index: variant_index,
                requested: Vec::new(),
                resolve: ResolveMetadata::default(),
            };
            let result = (|| -> Result<ResolveMetadata> {
                let target = definition.build_variant(index)?;
                let target_variant = match variant_index {
                    Some(index) => crate::package::Variant::new(target.package.clone(), index)?,
                    None => crate::package::Variant::from_package(target.package.clone()),
                };
                variant.requested = target_variant
                    .build_request()
                    .iter()
                    .map(ToString::to_string)
                    .collect();
                let (context, saved) = ResolvedContext::create_build_context(
                    &target.package,
                    variant_index,
                    None,
                    Some(&provider),
                    Some(paths.clone()),
                    false,
                )?;
                debug_assert!(saved.is_none());
                variant.requested = context
                    .requested_packages(true)
                    .iter()
                    .map(ToString::to_string)
                    .collect();
                if context.success() {
                    provider.selected(&context)
                } else {
                    Ok(ResolveMetadata {
                        error: Some(
                            context
                                .failure_description
                                .clone()
                                .unwrap_or_else(|| "Build environment resolution failed".into()),
                        ),
                        ..ResolveMetadata::default()
                    })
                }
            })();
            variant.resolve = result.unwrap_or_else(|error| ResolveMetadata {
                error: Some(error.to_string()),
                ..ResolveMetadata::default()
            });
            item.variants.push(variant);
        }
    }
    Ok(BatchMetadata {
        producer: "rez-rs",
        schema: 1,
        mode: if installed_only {
            MetadataMode::Installed
        } else {
            MetadataMode::Prospective
        },
        packages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn source(root: &Path, directory: &str, data: serde_json::Value) -> PathBuf {
        let path = root.join(directory);
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join("package.yaml"),
            serde_json::to_string(&data).unwrap(),
        )
        .unwrap();
        path
    }

    fn item<'a>(metadata: &'a BatchMetadata, name: &str) -> &'a SourceMetadata {
        metadata
            .packages
            .iter()
            .find(|package| package.name.as_deref() == Some(name))
            .unwrap()
    }

    #[test]
    fn self_source_dependency_is_an_edge_until_an_installed_identity_wins() {
        let root = tempfile::tempdir().unwrap();
        let target = source(
            root.path(),
            "self_target",
            serde_json::json!({
                "name":"self_target", "version":"1", "private_build_requires":["self_target"]
            }),
        );
        let metadata = collect(std::slice::from_ref(&target), Some(Vec::new()), false).unwrap();
        let result = &item(&metadata, "self_target").variants[0].resolve;
        assert!(result.success, "{:?}", result.error);
        assert_eq!(
            result.prerequisites,
            vec![fs::canonicalize(&target).unwrap()]
        );
        assert_eq!(result.selected[0].origin, PackageOrigin::Source);
        let repo = tempfile::tempdir().unwrap();
        source(
            repo.path(),
            "self_target/1",
            serde_json::json!({
                "name":"self_target", "version":"1"
            }),
        );
        let metadata = collect(&[target], Some(vec![repo.path().to_path_buf()]), false).unwrap();
        let result = &item(&metadata, "self_target").variants[0].resolve;
        assert!(result.success, "{:?}", result.error);
        assert!(result.prerequisites.is_empty());
        assert_eq!(result.selected[0].origin, PackageOrigin::Installed);
    }

    #[test]
    fn repeated_input_and_same_definition_aliases_keep_one_candidate() {
        let root = tempfile::tempdir().unwrap();
        let path = source(
            root.path(),
            "definition",
            serde_json::json!({
                "name":"definition", "version":"1"
            }),
        );
        let file = path.join("package.yaml");
        let metadata = collect(
            &[path.clone(), path.clone(), file.clone()],
            Some(Vec::new()),
            false,
        )
        .unwrap();
        assert_eq!(metadata.producer, "rez-rs");
        assert_eq!(metadata.packages.len(), 1);
        let package = &metadata.packages[0];
        assert!(package.error.is_none());
        assert!(package.variants[0].resolve.success);
        assert_eq!(package.source_path, fs::canonicalize(path).unwrap());
        assert_eq!(package.aliases, vec![fs::canonicalize(&file).unwrap()]);
        assert_eq!(
            package.definition_path,
            Some(fs::canonicalize(file).unwrap())
        );
    }

    #[test]
    fn aliases_do_not_repeat_source_execution_before_target_reevaluation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("definition");
        fs::create_dir_all(&path).unwrap();
        let counter = root.path().join("loads.txt");
        let file = path.join("package.py");
        let counter_literal = serde_json::to_string(&counter.to_string_lossy()).unwrap();
        fs::write(&file, format!(
            "name='definition'\nversion='1'\nwith open({counter_literal}, 'a') as _counter:\n    _counter.write('load\\n')\n"
        )).unwrap();
        let metadata =
            collect(&[path.clone(), file.clone(), path], Some(Vec::new()), false).unwrap();
        assert_eq!(metadata.packages.len(), 1);
        assert!(metadata.packages[0].variants[0].resolve.success);
        assert_eq!(
            metadata.packages[0].aliases,
            vec![fs::canonicalize(file).unwrap()]
        );
        assert_eq!(
            fs::read_to_string(counter)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            vec!["load", "load"]
        );
    }

    #[test]
    fn joint_conflict_is_not_independent_dependency_availability() {
        let root = tempfile::tempdir().unwrap();
        let definitions = [
            source(
                root.path(),
                "target",
                serde_json::json!({"name":"target","version":"1","requires":["a","b"],"build_requires":["builder"],"private_build_requires":["private_dep"]}),
            ),
            source(
                root.path(),
                "a",
                serde_json::json!({"name":"a","version":"1","requires":["shared-1"]}),
            ),
            source(
                root.path(),
                "b",
                serde_json::json!({"name":"b","version":"1","requires":["shared-2"]}),
            ),
            source(
                root.path(),
                "shared1",
                serde_json::json!({"name":"shared","version":"1"}),
            ),
            source(
                root.path(),
                "shared2",
                serde_json::json!({"name":"shared","version":"2"}),
            ),
            source(
                root.path(),
                "builder",
                serde_json::json!({"name":"builder","version":"1"}),
            ),
            source(
                root.path(),
                "private_dep",
                serde_json::json!({"name":"private_dep","version":"1"}),
            ),
        ];
        let metadata = collect(&definitions, Some(Vec::new()), false).unwrap();
        assert!(item(&metadata, "a").variants[0].resolve.success);
        assert!(item(&metadata, "b").variants[0].resolve.success);
        let variant = &item(&metadata, "target").variants[0];
        let mut expected = vec![
            "a".to_owned(),
            "b".to_owned(),
            "builder".to_owned(),
            "private_dep".to_owned(),
        ];
        expected.extend(
            CONFIG
                .resolved_implicit_packages()
                .iter()
                .map(|request| version::Requirement::new(request).unwrap().to_string()),
        );
        assert_eq!(variant.requested, expected);
        let result = &variant.resolve;
        assert!(!result.success);
        assert!(result.error.is_some());
        assert!(result.prerequisites.is_empty());
    }

    #[test]
    fn selected_sources_form_one_prerequisite_set_and_no_saved_context() {
        let root = tempfile::tempdir().unwrap();
        let target = source(
            root.path(),
            "target",
            serde_json::json!({
                "name":"target","version":"1","requires":["dep"],"variants":[[],["missing"]]
            }),
        );
        let dep = source(
            root.path(),
            "dep",
            serde_json::json!({"name":"dep","version":"1"}),
        );
        let metadata = collect(&[target.clone(), dep.clone()], Some(Vec::new()), false).unwrap();
        let package = item(&metadata, "target");
        assert_eq!(package.variants.len(), 2);
        assert!(
            package.variants[0].resolve.success,
            "{:?}",
            package.variants[0].resolve.error
        );
        assert!(!package.variants[1].resolve.success);
        assert_eq!(
            package.variants[0].resolve.prerequisites,
            vec![fs::canonicalize(&dep).unwrap()]
        );
        let selected = &package.variants[0].resolve.selected[0];
        assert_eq!(selected.origin, PackageOrigin::Source);
        assert!(selected.resource_handle.is_none());
        assert_eq!(
            package.definition_path,
            Some(fs::canonicalize(target.join("package.yaml")).unwrap())
        );
        assert!(!target.join("build.rxt").exists());
        assert!(!target.join("build").exists());
        let live = collect(&[target], Some(Vec::new()), true).unwrap();
        assert!(item(&live, "target")
            .variants
            .iter()
            .all(|variant| !variant.resolve.success));
    }

    #[test]
    fn installed_identity_wins_and_has_real_resource_handle() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        source(
            repo.path(),
            "dep/1",
            serde_json::json!({"name":"dep","version":"1"}),
        );
        let target = source(
            root.path(),
            "target",
            serde_json::json!({"name":"target","version":"1","requires":["dep"]}),
        );
        let dep = source(
            root.path(),
            "dep",
            serde_json::json!({"name":"dep","version":"1","requires":["missing"]}),
        );
        let metadata = collect(
            &[target.clone(), dep],
            Some(vec![repo.path().to_path_buf()]),
            false,
        )
        .unwrap();
        let result = &item(&metadata, "target").variants[0].resolve;
        assert!(result.success, "{:?}", result.error);
        assert!(result.prerequisites.is_empty());
        assert_eq!(result.selected[0].origin, PackageOrigin::Installed);
        assert!(result.selected[0].resource_handle.is_some());
        assert!(result.selected[0].source_path.is_none());
        let live = collect(&[target], Some(vec![repo.path().to_path_buf()]), true).unwrap();
        assert!(item(&live, "target").variants[0].resolve.success);
    }

    #[test]
    fn only_target_is_reevaluated_with_building_true() {
        let root = tempfile::tempdir().unwrap();
        let target = source(
            root.path(),
            "target",
            serde_json::json!({"name":"target","version":"1","requires":["dep"]}),
        );
        let dep = root.path().join("dep");
        fs::create_dir_all(&dep).unwrap();
        fs::write(dep.join("package.py"), "name='dep'\nversion='1'\n@early()\ndef requires():\n    return [] if building else ['runtime']\n").unwrap();
        let runtime = source(
            root.path(),
            "runtime",
            serde_json::json!({"name":"runtime","version":"1"}),
        );
        let metadata = collect(
            &[target, dep.clone(), runtime.clone()],
            Some(Vec::new()),
            false,
        )
        .unwrap();
        let target = &item(&metadata, "target").variants[0].resolve;
        assert!(target.success, "{:?}", target.error);
        assert!(target
            .prerequisites
            .contains(&fs::canonicalize(dep).unwrap()));
        assert!(target
            .prerequisites
            .contains(&fs::canonicalize(runtime).unwrap()));
        let implicit: Vec<_> = CONFIG
            .resolved_implicit_packages()
            .iter()
            .map(|request| version::Requirement::new(request).unwrap().to_string())
            .collect();
        assert_eq!(item(&metadata, "dep").variants[0].requested, implicit);
    }

    #[test]
    fn source_errors_and_duplicate_identities_do_not_suppress_other_items() {
        let root = tempfile::tempdir().unwrap();
        let valid = source(
            root.path(),
            "valid",
            serde_json::json!({"name":"valid","version":"1"}),
        );
        let a = source(
            root.path(),
            "a",
            serde_json::json!({"name":"duplicate","version":"1"}),
        );
        let b = source(
            root.path(),
            "b",
            serde_json::json!({"name":"duplicate","version":"1"}),
        );
        let metadata = collect(
            &[valid, a, b, root.path().join("absent")],
            Some(Vec::new()),
            false,
        )
        .unwrap();
        assert!(item(&metadata, "valid").variants[0].resolve.success);
        for package in metadata.packages.iter().skip(1) {
            assert!(package.error.is_some());
            assert!(package.variants.is_empty());
        }
    }
}
