// SPDX-License-Identifier: Apache-2.0

//! Native pip-to-Rez orchestration. Selected Python performs packaging tools only;
//! canonical Rust package/variant models and the shared publisher own Rez behavior.

pub(crate) mod metadata;
mod python;
mod requirements;

use crate::config::RezConfig;
use crate::errors::{Result, RezError};
use crate::package::{Package, Variant};
use crate::repository::{publish_package, PackageRepositoryManager, PublicationPayload};
use python::Python;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use version::{Version, VersionRange};

/// How pip distributions are represented as Rez package variants.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VariantPolicy {
    /// Preserve Rez's interpreter-specific variants.
    #[default]
    Current,
    /// Publish a portable pure-Python distribution without variants.
    None,
}

impl std::str::FromStr for VariantPolicy {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "current" => Ok(Self::Current),
            "none" => Ok(Self::None),
            _ => Err(format!(
                "Unknown pip variant policy {value:?}; expected current or none"
            )),
        }
    }
}

#[derive(Debug, Default)]
pub struct Options {
    pub packages: Vec<String>,
    pub release: bool,
    pub local: bool,
    pub prefix: Option<PathBuf>,
    pub python_version: Option<String>,
    pub pip_version: Option<String>,
    pub no_deps: bool,
    pub deps: bool,
    pub pre_release: bool,
    pub build: bool,
    pub pypi_upload: bool,
    pub extra: Vec<String>,
    pub variant_policy: VariantPolicy,
    pub python_requires: Option<String>,
}

#[derive(Debug, Default)]
pub struct InstallResult {
    pub installed: Vec<PathBuf>,
    pub skipped: Vec<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct Normalized {
    name: String,
    version: String,
    dependencies: Vec<requirements::Dependency>,
    extras: Vec<String>,
    python_specifiers: Vec<requirements::Specifier>,
    portable: bool,
}

fn canonical_name(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .split(['-', '_', '.'])
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

pub fn install(options: &Options, config: &RezConfig) -> Result<InstallResult> {
    if options.packages.is_empty() {
        return Err(RezError::Build("No pip package was requested".into()));
    }
    let python_requires = match &options.python_requires {
        Some(_) if options.variant_policy == VariantPolicy::Current => {
            return Err(RezError::Build(
                "--python-requires requires --variant-policy none".into(),
            ));
        }
        Some(value) => Some(VersionRange::new(value)?),
        None => None,
    };
    let release = options.release || options.pre_release;
    if options.local && release {
        return Err(RezError::Build(
            "--local cannot be combined with --release or --pre-release".into(),
        ));
    }
    let no_deps = options.no_deps
        || (!options.deps && config.pip_default_no_deps)
        || config
            .pip_extra_args
            .iter()
            .chain(&options.extra)
            .any(|argument| argument == "--no-deps");
    let python = Python::select(
        config,
        options
            .python_version
            .as_deref()
            .or(config.pip_default_python_version.as_deref()),
        options.pip_version.as_deref(),
    )?;
    eprintln!(
        "Using pip-{} from {} (Python {})",
        python.pip_version,
        python.executable.display(),
        python.version
    );
    let work = tempfile::Builder::new().prefix("rez-pip-").tempdir()?;
    let target = work.path().join("target");
    fs::create_dir(&target)?;
    let mut sources = Vec::new();
    let mut built_wheels = Vec::new();
    let mut revisions = HashMap::new();
    for source in &options.packages {
        let path = Path::new(source);
        if path.is_dir() {
            if let Some(revision) = revision(path)? {
                revisions.insert(source.clone(), revision);
            }
        }
        let source = if path.is_dir() && (options.build || release) {
            let (wheel, owner) = build(&python, path, work.path())?;
            built_wheels.push(owner);
            if release && options.pypi_upload {
                upload(&python, &wheel)?;
            }
            wheel.to_string_lossy().into_owned()
        } else {
            if release && options.pypi_upload && path.extension().is_some_and(|ext| ext == "whl") {
                upload(&python, path)?;
            }
            source.clone()
        };
        sources.push(source);
    }
    let mut args = vec![
        "-m".into(),
        "pip".into(),
        "install".into(),
        "--target".into(),
        target.to_string_lossy().into_owned(),
        "--ignore-installed".into(),
    ];
    if no_deps {
        args.push("--no-deps".into());
    }
    let extra = config
        .pip_extra_args
        .iter()
        .chain(&options.extra)
        .cloned()
        .collect::<Vec<_>>();
    crate::builders::native::arguments(
        &extra,
        &["-t", "--target", "--prefix", "--root", "--user"],
        Some(&["-t"]),
    )?;
    args.extend(extra);
    args.extend(sources);
    let output = python.run(&args, None)?;
    if !output.stdout.is_empty() {
        eprintln!("{}", String::from_utf8_lossy(&output.stdout).trim());
    }
    let distributions = metadata::distributions(&target)?;
    if distributions.is_empty() {
        return Err(RezError::Build(
            "Pip installed no distribution metadata".into(),
        ));
    }
    let input = work.path().join("packaging.json");
    fs::write(
        &input,
        serde_json::to_vec(&json!({
            "environment": python.environment, "sources": options.packages, "distributions": distributions,
            "variant_policy": if options.variant_policy == VariantPolicy::None { "none" } else { "current" }
        }))?,
    )?;
    let output = python.run(
        &[
            "-E".into(),
            "-s".into(),
            "-c".into(),
            include_str!("packaging.py").into(),
            input.to_string_lossy().into_owned(),
        ],
        None,
    )?;
    let normalized: Vec<Normalized> = serde_json::from_slice(&output.stdout)?;
    if normalized.len() != distributions.len() {
        return Err(RezError::Build(
            "Packaging parser changed distribution identity/count".into(),
        ));
    }
    let mut casings = distributions
        .iter()
        .map(|dist| (canonical_name(&dist.name), requirements::name(&dist.name)))
        .collect::<HashMap<_, _>>();
    if casings.len() != distributions.len() {
        return Err(RezError::Build(
            "Multiple installed distributions share a canonical name".into(),
        ));
    }
    let versions = normalized
        .iter()
        .map(|dist| (canonical_name(&dist.name), dist.version.clone()))
        .collect::<HashMap<_, _>>();
    let base = if let Some(prefix) = &options.prefix {
        prefix.clone()
    } else if let Some(prefix) = &config.pip_install_prefix {
        RezConfig::expand_path(prefix).to_os()
    } else if release {
        config.expanded_release_path_for_pip().to_os()
    } else {
        config.expanded_local_packages_path().to_os()
    };
    // Pip identities are case/separator-insensitive, while Rez family names are not.
    // Reuse confirmed installed pip identities for both roots and dependencies.
    let needed = casings
        .keys()
        .cloned()
        .chain(normalized.iter().flat_map(|dist| {
            dist.dependencies
                .iter()
                .filter(|dependency| dependency.enabled)
                .map(|dependency| canonical_name(&dependency.name))
        }))
        .collect::<BTreeSet<_>>();
    let mut paths = config.expanded_packages_path_os();
    paths.push(base.clone());
    let paths = crate::install::category_paths(paths, config.rez_install_categories);
    let repositories = PackageRepositoryManager::from_paths(&paths)?;
    let mut installed_names = HashMap::<String, String>::new();
    for family in repositories.iter_family_names()? {
        let key = canonical_name(&family);
        if !needed.contains(&key) {
            continue;
        }
        let mut from_pip = false;
        for info in repositories.iter_packages(&family)? {
            if info.get("from_pip").and_then(Value::as_bool) == Some(true) {
                info.to_package()?;
                from_pip = true;
            }
        }
        if from_pip {
            if let Some(previous) = installed_names
                .insert(key.clone(), family.clone())
                .filter(|previous| previous != &family)
            {
                return Err(RezError::Build(format!(
                    "Pip identity {key:?} has ambiguous installed Rez families {previous:?} and {family:?}"
                )));
            }
        }
    }
    casings.extend(installed_names);
    // Build and validate every payload before any repository publication.
    let mut plans = Vec::new();
    let mut result = InstallResult::default();
    for (distribution, normalized) in distributions.iter().zip(&normalized) {
        if distribution.name != normalized.name {
            return Err(RezError::Build(
                "Packaging parser changed distribution identity/order".into(),
            ));
        }
        let name = casings
            .get(&canonical_name(&distribution.name))
            .cloned()
            .ok_or_else(|| RezError::Build("Missing pip distribution identity".into()))?;
        let version = &normalized.version;
        crate::serialise::validate_rez_package_path(&name, Some(version))?;
        if options.pre_release
            && version
                .chars()
                .all(|value| value.is_ascii_digit() || value == '.')
        {
            return Err(RezError::Build(format!(
                "Package version {version} is not a pre-release version"
            )));
        }
        let mut common = BTreeSet::new();
        let mut variant = BTreeSet::new();
        let mut systems = BTreeSet::from(["python"]);
        let marker_systems = normalized
            .dependencies
            .iter()
            .flat_map(|dependency| requirements::systems(&dependency.marker_names))
            .collect::<BTreeSet<_>>();
        let pure = distribution.pure
            && (options.variant_policy == VariantPolicy::None
                || !(config.pip_detect_system_markers
                    && (marker_systems.contains("arch") || marker_systems.contains("platform"))));
        let mut mapping = metadata::mapping(distribution, &target, config, None)?;
        let mut owners = vec![(distribution, mapping.clone())];
        // Keep the existing PySide6 layout extension required by its relative loader.
        if ["pyside6", "pyside6-essentials", "pyside6-addons"]
            .contains(&canonical_name(&name).as_str())
        {
            if let Some(shiboken) = distributions
                .iter()
                .find(|dist| canonical_name(&dist.name) == "shiboken6")
            {
                let auxiliary = metadata::mapping(shiboken, &target, config, None)?;
                for (source, destination) in &auxiliary {
                    mapping
                        .entry(source.clone())
                        .or_insert_with(|| destination.clone());
                }
                owners.push((shiboken, auxiliary));
            }
        }
        let entry_points = metadata::parse_entry_points(&distribution.directory)?;
        if options.variant_policy == VariantPolicy::None {
            if !distribution.pure || !normalized.portable {
                return Err(RezError::Build(format!(
                    "Distribution {} is not portable under --variant-policy none; use current",
                    distribution.name
                )));
            }
            // Replace only declared pip entry-point launchers, keeping every data asset.
            for destination in mapping.values() {
                if destination.starts_with("bin") {
                    let declared = entry_points.iter().any(|(name, _)| {
                        ["", ".exe", "-script.py", ".cmd", ".py"]
                            .iter()
                            .any(|suffix| {
                                destination == &Path::new("bin").join(format!("{name}{suffix}"))
                            })
                    });
                    if !declared {
                        return Err(RezError::Build(format!(
                            "Cannot port unknown script {} in distribution {}",
                            destination.display(),
                            distribution.name
                        )));
                    }
                }
            }
            mapping.retain(|_, destination| !destination.starts_with("bin"));
        }
        let mut tools = BTreeSet::new();
        for destination in mapping.values() {
            if destination.parent() == Some(Path::new("bin")) {
                if let Some(tool) = destination.file_name().and_then(|name| name.to_str()) {
                    tools.insert(
                        tool.trim_end_matches(".exe")
                            .trim_end_matches(".cmd")
                            .trim_end_matches(".bat")
                            .to_string(),
                    );
                }
            }
        }
        tools.extend(entry_points.iter().map(|(name, _)| name.clone()));
        if !pure || !tools.is_empty() {
            systems.extend(["platform", "arch"]);
        }
        for dependency in &normalized.dependencies {
            if !dependency.enabled {
                continue;
            }
            let text = requirements::requirement(
                dependency,
                versions
                    .get(&canonical_name(&dependency.name))
                    .map(String::as_str),
                casings
                    .get(&canonical_name(&dependency.name))
                    .map(String::as_str),
            )?;
            let marker_systems = requirements::systems(&dependency.marker_names);
            systems.extend(marker_systems.iter().copied());
            if marker_systems.is_empty() {
                common.insert(text);
            } else {
                variant.insert(text);
            }
        }
        let system = config
            .system_info
            .as_ref()
            .unwrap_or(&crate::platform::SYSTEM);
        let mut variant_requires = Vec::new();
        if systems.contains("platform") {
            variant_requires.push(format!("platform-{}", system.platform));
        }
        if systems.contains("arch") {
            variant_requires.push(format!("arch-{}", system.arch));
        }
        if systems.contains("os") {
            variant_requires.push(format!("os-{}", system.os));
        }
        if options.variant_policy == VariantPolicy::None {
            let metadata_range = requirements::specifiers(&normalized.python_specifiers)?;
            let range = match (metadata_range, python_requires.as_ref()) {
                (Some(metadata), Some(requested)) => {
                    Some(metadata.intersection(requested).ok_or_else(|| {
                        RezError::Build(format!(
                            "Python requirements for {} have an empty intersection",
                            distribution.name
                        ))
                    })?)
                }
                (Some(metadata), None) => Some(metadata),
                (None, Some(requested)) => Some(requested.clone()),
                (None, None) => None,
            };
            let selected_python = Version::new(&python.version)?;
            if range
                .as_ref()
                .is_some_and(|range| !range.contains_version(&selected_python))
            {
                return Err(RezError::Build(format!(
                    "Selected Python {} does not satisfy published requirements for {}",
                    python.version, distribution.name
                )));
            }
            common.insert(
                range
                    .map(|range| format!("python-{range}"))
                    .unwrap_or_else(|| "python".into()),
            );
        } else {
            variant_requires.push(format!("python-{}", Version::new(&python.version)?.trim(2)));
            variant_requires.extend(variant);
        }
        let mut data = HashMap::<String, Value>::from([
            ("name".into(), json!(name)),
            ("version".into(), json!(version)),
            ("requires".into(), json!(common)),
            (
                "variants".into(),
                if options.variant_policy == VariantPolicy::None {
                    json!([])
                } else {
                    json!([variant_requires])
                },
            ),
            (
                "hashed_variants".into(),
                json!(options.variant_policy == VariantPolicy::Current),
            ),
            ("tools".into(), json!(tools)),
            ("from_pip".into(), json!(true)),
            (
                "pip_name".into(),
                json!(format!("{}-{}", distribution.name, distribution.version)),
            ),
            ("is_pure_python".into(), json!(pure)),
            ("package_type".into(), json!("pip")),
            (
                "commands".into(),
                json!(if tools.is_empty() {
                    "env.PYTHONPATH.append('{root}/python')"
                } else {
                    "env.PYTHONPATH.append('{root}/python')\nenv.PATH.append('{root}/bin')"
                }),
            ),
        ]);
        if !distribution.summary.is_empty() {
            data.insert("description".into(), json!(distribution.summary));
        }
        if !distribution.author.is_empty() {
            data.insert(
                "authors".into(),
                json!([format!(
                    "{}{}{}",
                    distribution.author,
                    if distribution.author_email.is_empty() {
                        ""
                    } else {
                        " "
                    },
                    distribution.author_email
                )]),
            );
        }
        let mut help = Vec::new();
        if !distribution.home_page.is_empty() {
            help.push(vec!["Home Page".into(), distribution.home_page.clone()]);
        }
        if !distribution.download_url.is_empty() {
            help.push(vec![
                "Source Code".into(),
                distribution.download_url.clone(),
            ]);
        }
        if !help.is_empty() {
            data.insert("help".into(), json!(help));
        }
        if options.pre_release {
            data.insert("cachable".into(), json!(false));
        }
        if !normalized.extras.is_empty() {
            data.insert("pip_extras".into(), json!(normalized.extras));
        }
        if options.packages.len() == 1 {
            if let Some(revision) = revisions.get(&options.packages[0]) {
                data.insert("revision".into(), revision.clone());
            }
        }
        let package = Package::from_data(data)?;
        let variant = package
            .variants
            .first()
            .map(|requires| {
                Variant::compute_subpath(requires, true)
                    .ok_or_else(|| RezError::Build("Pip variant must have a subpath".into()))
            })
            .transpose()?;
        let staged = tempfile::Builder::new()
            .prefix("rez-pip-payload-")
            .tempdir()?;
        let payload = variant
            .as_ref()
            .map(|variant| staged.path().join(variant))
            .unwrap_or_else(|| staged.path().to_path_buf());
        fs::create_dir_all(&payload)?;
        metadata::copy(&mapping, &target, &payload)?;
        let missing_entry_points = entry_points
            .into_iter()
            .filter(|(name, _)| {
                options.variant_policy == VariantPolicy::None
                    || !mapping.values().any(|path| {
                        path.parent() == Some(Path::new("bin"))
                            && path.file_stem().and_then(|value| value.to_str())
                                == Some(name.as_str())
                    })
            })
            .collect::<Vec<_>>();
        let generated = metadata::generate_entry_point_launchers(
            &missing_entry_points,
            &payload.join("bin"),
            options.variant_policy == VariantPolicy::None,
        )?;
        let generated = generated
            .iter()
            .map(|path| {
                path.strip_prefix(&payload)
                    .map(Path::to_path_buf)
                    .map_err(|_| RezError::Build("Generated launcher outside payload".into()))
            })
            .collect::<Result<Vec<_>>>()?;
        for (owner, mut owned) in owners {
            owned.retain(|source, destination| mapping.get(source) == Some(destination));
            owner.finalize(
                &owned,
                &target,
                &payload,
                if std::ptr::eq(owner, distribution) {
                    &generated
                } else {
                    &[]
                },
            )?;
        }
        let destination =
            crate::install::route_install_path(&base, &package, config.rez_install_categories)
                .join(&name)
                .join(version);
        plans.push((package, destination, staged));
    }
    for (package, destination, staged) in plans {
        let backend = if release {
            config.pip_release_lock_backend.trim()
        } else {
            ""
        };
        if !backend.is_empty() {
            access(
                &python,
                backend,
                "unlock_path",
                destination
                    .parent()
                    .ok_or_else(|| RezError::Build("Pip package has no family path".into()))?,
            )?;
        }
        let published = publish_package(
            &package,
            &destination,
            &[0],
            Some(PublicationPayload::Owned {
                root: staged,
                replace: (options.pre_release || !release) && no_deps,
                paths: None,
                require_new: false,
            }),
            None,
        );
        let locked = if backend.is_empty() {
            Ok(())
        } else {
            access(
                &python,
                backend,
                "lock_path",
                destination
                    .parent()
                    .ok_or_else(|| RezError::Build("Pip package has no family path".into()))?,
            )
        };
        let published = match (published, locked) {
            (Ok(value), Ok(())) => value,
            (Err(error), Ok(())) => return Err(error),
            (Ok(_), Err(error)) => return Err(error),
            (Err(error), Err(lock)) => {
                return Err(RezError::Build(format!(
                    "{error}; repository re-lock failed: {lock}"
                )));
            }
        };
        if !published.installed_indices.is_empty() {
            result.installed.push(destination.clone());
        }
        if !published.skipped_indices.is_empty() {
            result.skipped.push(destination);
        }
    }
    eprintln!(
        "{} packages installed; {} already installed",
        result.installed.len(),
        result.skipped.len()
    );
    Ok(result)
}

fn build(python: &Python, source: &Path, work: &Path) -> Result<(PathBuf, tempfile::TempDir)> {
    let output = tempfile::Builder::new().prefix("wheel-").tempdir_in(work)?;
    let staged_source = crate::builders::pip_utils::source(source, output.path(), None)?;
    let wheels_dir = crate::util::directory(output.path(), Path::new("wheels"), true)?;
    python.run(
        &[
            "-m".into(),
            "build".into(),
            "--wheel".into(),
            "--outdir".into(),
            wheels_dir.to_string_lossy().into_owned(),
        ],
        Some(&staged_source),
    )?;
    let wheels = fs::read_dir(&wheels_dir)?
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "whl"))
        .collect::<Vec<_>>();
    if wheels.len() != 1 {
        return Err(RezError::Build(
            "Python build must produce exactly one wheel".into(),
        ));
    }
    Ok((wheels[0].clone(), output))
}

fn upload(python: &Python, wheel: &Path) -> Result<()> {
    // Source policy: an explicit upload failure is reported, while Rez publication continues.
    if let Err(error) = python.run(
        &[
            "-m".into(),
            "twine".into(),
            "upload".into(),
            wheel.to_string_lossy().into_owned(),
            "--non-interactive".into(),
        ],
        None,
    ) {
        eprintln!("Warning: wheel upload failed: {error}");
    }
    Ok(())
}

fn access(python: &Python, backend: &str, operation: &str, path: &Path) -> Result<()> {
    python.run(&["-s".into(), "-c".into(),
        "import importlib,sys; getattr(importlib.import_module(sys.argv[1]),sys.argv[2])(sys.argv[3],recursive=True)".into(),
        backend.into(), operation.into(), path.to_string_lossy().into_owned()], None)?;
    Ok(())
}

fn revision(source: &Path) -> Result<Option<Value>> {
    let run = |args: &[&str]| -> Result<Option<String>> {
        let output = match std::process::Command::new("git")
            .args(args)
            .current_dir(source)
            .output()
        {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        Ok(output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().into()))
    };
    let Some(commit) = run(&["rev-parse", "HEAD"])? else {
        return Ok(None);
    };
    let mut result = serde_json::Map::from_iter([("commit".into(), json!(commit))]);
    if let Some(branch) = run(&["branch", "--show-current"])? {
        result.insert("branch".into(), json!(branch));
    }
    if let Some(tracking) = run(&[
        "rev-parse",
        "--abbrev-ref",
        "--symbolic-full-name",
        "@{upstream}",
    ])? {
        if let Some((remote, _)) = tracking.split_once('/') {
            if let Some(url) = run(&["remote", "get-url", remote])? {
                result.insert("fetch_url".into(), json!(url));
            }
            if let Some(url) = run(&["remote", "get-url", "--push", remote])? {
                result.insert("push_url".into(), json!(url));
            }
        }
        result.insert("tracking_branch".into(), json!(tracking));
    }
    Ok(Some(Value::Object(result)))
}
