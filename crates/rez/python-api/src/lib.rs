// SPDX-License-Identifier: Apache-2.0
//! CPython bridge. All package, version, Rex and solver behavior stays in its owner crate.
use foundation::errors::RezError as CoreError;
use model::config::CONFIG;
use model::package::Variant;
use pyo3::exceptions::{PyException, PyValueError};
use pyo3::prelude::*;
use repository::{FilesystemPackageProvider, PackageCandidate, PackageProvider, ResourceHandle};
use resolve::{ResolveOptions, ResolvedContext};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use version::{Requirement, Version, VersionRange, VersionedObject};

pyo3::create_exception!(rs, RezError, PyException);
pyo3::create_exception!(rs, VersionError, RezError);
pyo3::create_exception!(rs, PackageFamilyNotFoundError, RezError);
pyo3::create_exception!(rs, PackageNotFoundError, RezError);
pyo3::create_exception!(rs, PackageMetadataError, RezError);
pyo3::create_exception!(rs, ResolvedContextError, RezError);

fn error(error: CoreError) -> PyErr {
    let message = error.to_string();
    match error {
        CoreError::Version(_) | CoreError::Parse(_) => VersionError::new_err(message),
        CoreError::PackageFamilyNotFound(_) => PackageFamilyNotFoundError::new_err(message),
        CoreError::PackageNotFound(_) => PackageNotFoundError::new_err(message),
        CoreError::PackageMetadata { .. } | CoreError::InvalidPackage(_) => {
            PackageMetadataError::new_err(message)
        }
        CoreError::ResolvedContext(_) | CoreError::Resolve(_) => {
            ResolvedContextError::new_err(message)
        }
        _ => RezError::new_err(message),
    }
}

fn object(py: Python<'_>, value: Value) -> PyResult<Py<PyAny>> {
    Ok(py
        .import("json")?
        .call_method1("loads", (value.to_string(),))?
        .unbind())
}

fn parse_version(text: &str) -> PyResult<Version> {
    if text == "[INF]" {
        Ok(Version::inf())
    } else {
        Version::new(text).map_err(error)
    }
}

fn paths(paths: Option<Vec<PathBuf>>) -> PyResult<Vec<PathBuf>> {
    paths
        .unwrap_or_else(|| CONFIG.expanded_packages_path_os())
        .into_iter()
        .map(|path| std::path::absolute(path).map_err(|e| error(e.into())))
        .collect()
}

#[pyfunction]
fn version_info(py: Python<'_>, text: &str) -> PyResult<Py<PyAny>> {
    let version = parse_version(text)?;
    object(
        py,
        json!({"text": version.to_string(), "tokens": version.as_tuple(),
                      "is_inf": version.is_inf(), "is_empty": version.is_empty()}),
    )
}

#[pyfunction]
fn version_compare(left: &str, right: &str) -> PyResult<i8> {
    Ok(match parse_version(left)?.cmp(&parse_version(right)?) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    })
}

#[pyfunction]
#[pyo3(signature = (text, operation, length=0))]
fn version_transform(text: &str, operation: &str, length: usize) -> PyResult<String> {
    let version = parse_version(text)?;
    match operation {
        "next" => Ok(version.next().to_string()),
        "trim" => Ok(version.trim(length).to_string()),
        _ => Err(PyValueError::new_err("Unknown version operation")),
    }
}

#[pyfunction]
fn range_info(py: Python<'_>, text: &str) -> PyResult<Py<PyAny>> {
    let range = VersionRange::new(text).map_err(error)?;
    object(
        py,
        json!({"text": range.to_string(), "is_any": range.is_any(),
                      "lower_bounded": range.lower_bounded(), "upper_bounded": range.upper_bounded(),
                      "bounds": range.len()}),
    )
}

#[pyfunction]
#[pyo3(signature = (left, operation, right=""))]
fn range_operation(
    py: Python<'_>,
    left: &str,
    operation: &str,
    right: &str,
) -> PyResult<Py<PyAny>> {
    let left = VersionRange::new(left).map_err(error)?;
    let result = match operation {
        "contains" => json!(left.contains_version(&parse_version(right)?)),
        "inverse" => json!(left.inverse().map(|r| r.to_string())),
        "split" => json!(left
            .split()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()),
        "span" => json!(left.span().to_string()),
        _ => {
            let right = VersionRange::new(right).map_err(error)?;
            match operation {
                "union" => json!(left.union(&right).to_string()),
                "intersection" => json!(left.intersection(&right).map(|r| r.to_string())),
                "issuperset" => json!(left.issuperset(&right)),
                "issubset" => json!(left.issubset(&right)),
                "intersects" => json!(left.intersects(&right)),
                _ => return Err(PyValueError::new_err("Unknown range operation")),
            }
        }
    };
    object(py, result)
}

#[pyfunction]
fn requirement_info(py: Python<'_>, text: &str) -> PyResult<Py<PyAny>> {
    let req = Requirement::new(text).map_err(error)?;
    object(
        py,
        json!({"text": req.to_string(), "name": req.name(),
                      "range": req.range().map(ToString::to_string),
                      "conflict": req.conflict(), "weak": req.weak()}),
    )
}

#[pyfunction]
fn requirement_conflicts(left: &str, right: &str) -> PyResult<bool> {
    Ok(Requirement::new(left)
        .map_err(error)?
        .conflicts_with_req(&Requirement::new(right).map_err(error)?))
}

#[pyfunction]
fn versioned_info(py: Python<'_>, text: &str) -> PyResult<Py<PyAny>> {
    let obj = VersionedObject::new(text).map_err(error)?;
    object(
        py,
        json!({"text": obj.to_string(), "name": obj.name(), "version": obj.version().to_string(),
                      "exact_requirement": obj.as_exact_requirement()}),
    )
}

fn package_value(candidate: &PackageCandidate) -> PyResult<Value> {
    let package = &candidate.package;
    let variants = if package.has_variants() {
        package
            .iter_variants()
            .map(|variant| variant_value(candidate, &variant))
            .collect()
    } else {
        vec![variant_value(
            candidate,
            &Variant::from_package(package.clone()),
        )]
    };
    Ok(
        json!({"data": package.to_data().map_err(error)?, "base": package.base,
              "qualified_name": package.qualified_name(), "variants": Value::Array(variants),
              "source": candidate.provenance.as_ref().and_then(|p| p.source.as_ref()).map(|s| &s.path)}),
    )
}

fn variant_value(candidate: &PackageCandidate, variant: &Variant) -> Value {
    json!({"index": variant.index, "root": variant.root(), "subpath": variant.subpath,
           "qualified_name": variant.qualified_name(),
           "requires": variant.requires().iter().map(ToString::to_string).collect::<Vec<_>>(),
           "handle": candidate.provenance.as_ref().and_then(|p|
               p.resource_handle(&candidate.package.name, &candidate.package.version, variant.index))})
}

#[pyfunction]
#[pyo3(signature = (name, range=None, package_paths=None))]
fn packages(
    py: Python<'_>,
    name: &str,
    range: Option<&str>,
    package_paths: Option<Vec<PathBuf>>,
) -> PyResult<Py<PyAny>> {
    let provider = FilesystemPackageProvider::from_paths(&paths(package_paths)?).map_err(error)?;
    let range = VersionRange::new(range.unwrap_or("")).map_err(error)?;
    let mut candidates = provider.get_candidates(name, &range).map_err(error)?;
    candidates.sort_by(|a, b| b.package.version.cmp(&a.package.version));
    let values = candidates
        .iter()
        .map(package_value)
        .collect::<PyResult<Vec<_>>>()?;
    object(py, Value::Array(values))
}

#[pyfunction]
#[pyo3(signature = (package_paths=None))]
fn package_families(package_paths: Option<Vec<PathBuf>>) -> PyResult<Vec<String>> {
    repository::package::discover::iter_package_families(Some(&paths(package_paths)?))
        .map_err(error)
}

#[pyfunction]
fn developer_package(py: Python<'_>, path: PathBuf) -> PyResult<Py<PyAny>> {
    let developer = repository::package::discover::get_developer_package(&path).map_err(error)?;
    object(
        py,
        package_value(&PackageCandidate {
            package: developer.package,
            provenance: None,
        })?,
    )
}

#[pyfunction]
fn package_from_handle(py: Python<'_>, handle_json: &str) -> PyResult<Py<PyAny>> {
    let handle = ResourceHandle::from_json(&parse_json(handle_json)?, None).map_err(error)?;
    let provider = FilesystemPackageProvider::from_paths(&[]).map_err(error)?;
    let candidate = provider.get_candidate_for_handle(&handle).map_err(error)?;
    object(py, package_value(&candidate)?)
}

fn parse_json(text: &str) -> PyResult<Value> {
    serde_json::from_str(text).map_err(|e| PyValueError::new_err(e.to_string()))
}

fn context_value(context: &ResolvedContext) -> PyResult<Value> {
    let provider = FilesystemPackageProvider::from_paths(&[]).map_err(error)?;
    let selected = context
        .resolved_packages()
        .unwrap_or_default()
        .iter()
        .map(|resolved| {
            let handle = resolved.resource_handle.as_ref().ok_or_else(|| {
                ResolvedContextError::new_err("Missing resolved package provenance")
            })?;
            let candidate = provider.get_candidate_for_handle(handle).map_err(error)?;
            let mut value = package_value(&candidate)?;
            value["selected_index"] = json!(resolved.variant_index);
            value["selected_root"] = json!(resolved.root);
            value["selected_handle"] = json!(handle);
            Ok(value)
        })
        .collect::<PyResult<Vec<_>>>()?;
    Ok(json!({"context": context.to_json().map_err(error)?, "packages": selected}))
}

#[pyfunction]
#[pyo3(signature = (requests, package_paths=None, add_implicit_packages=true, building=false,
                    caching=true, timestamp=None, max_fails=-1, time_limit=-1, verbosity=0))]
#[allow(clippy::too_many_arguments)]
fn resolve_context(
    py: Python<'_>,
    requests: Vec<String>,
    package_paths: Option<Vec<PathBuf>>,
    add_implicit_packages: bool,
    building: bool,
    caching: bool,
    timestamp: Option<u64>,
    max_fails: i32,
    time_limit: i32,
    verbosity: u32,
) -> PyResult<Py<PyAny>> {
    let paths = paths(package_paths)?;
    let requests = requests
        .iter()
        .map(|s| Requirement::new(s).map_err(error))
        .collect::<PyResult<Vec<_>>>()?;
    let provider = FilesystemPackageProvider::from_paths(&paths).map_err(error)?;
    let opts = ResolveOptions {
        package_paths: Some(paths),
        add_implicit: add_implicit_packages,
        building,
        caching,
        timestamp,
        max_fails,
        time_limit,
        verbosity,
        ..Default::default()
    };
    let context = ResolvedContext::resolve(requests, &provider, opts).map_err(error)?;
    object(py, context_value(&context)?)
}

#[pyfunction]
#[pyo3(signature = (context_json, parent_environ=None))]
fn context_environ(
    context_json: &str,
    parent_environ: Option<HashMap<String, String>>,
) -> PyResult<HashMap<String, String>> {
    let context = ResolvedContext::from_json(&parse_json(context_json)?, None).map_err(error)?;
    context.get_environ(parent_environ).map_err(error)
}

#[pyfunction]
fn context_save(context_json: &str, path: PathBuf) -> PyResult<()> {
    ResolvedContext::from_json(&parse_json(context_json)?, None)
        .map_err(error)?
        .save(Path::new(&path))
        .map_err(error)
}

#[pyfunction]
fn context_load(py: Python<'_>, path: PathBuf) -> PyResult<Py<PyAny>> {
    object(
        py,
        context_value(&ResolvedContext::load(&path, None).map_err(error)?)?,
    )
}

#[pyfunction]
fn config_snapshot(py: Python<'_>) -> PyResult<Py<PyAny>> {
    object(
        py,
        serde_json::to_value(&*CONFIG).map_err(|e| PyValueError::new_err(e.to_string()))?,
    )
}

#[pymodule]
fn rs(module: &Bound<'_, PyModule>) -> PyResult<()> {
    model::config::ensure_valid().map_err(error)?;
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    module.add("ABI_MINIMUM", "3.10")?;
    module.add("RezError", module.py().get_type::<RezError>())?;
    module.add("VersionError", module.py().get_type::<VersionError>())?;
    module.add(
        "PackageFamilyNotFoundError",
        module.py().get_type::<PackageFamilyNotFoundError>(),
    )?;
    module.add(
        "PackageNotFoundError",
        module.py().get_type::<PackageNotFoundError>(),
    )?;
    module.add(
        "PackageMetadataError",
        module.py().get_type::<PackageMetadataError>(),
    )?;
    module.add(
        "ResolvedContextError",
        module.py().get_type::<ResolvedContextError>(),
    )?;
    module.add_function(wrap_pyfunction!(version_info, module)?)?;
    module.add_function(wrap_pyfunction!(version_compare, module)?)?;
    module.add_function(wrap_pyfunction!(version_transform, module)?)?;
    module.add_function(wrap_pyfunction!(range_info, module)?)?;
    module.add_function(wrap_pyfunction!(range_operation, module)?)?;
    module.add_function(wrap_pyfunction!(requirement_info, module)?)?;
    module.add_function(wrap_pyfunction!(requirement_conflicts, module)?)?;
    module.add_function(wrap_pyfunction!(versioned_info, module)?)?;
    module.add_function(wrap_pyfunction!(packages, module)?)?;
    module.add_function(wrap_pyfunction!(package_families, module)?)?;
    module.add_function(wrap_pyfunction!(developer_package, module)?)?;
    module.add_function(wrap_pyfunction!(package_from_handle, module)?)?;
    module.add_function(wrap_pyfunction!(resolve_context, module)?)?;
    module.add_function(wrap_pyfunction!(context_environ, module)?)?;
    module.add_function(wrap_pyfunction!(context_save, module)?)?;
    module.add_function(wrap_pyfunction!(context_load, module)?)?;
    module.add_function(wrap_pyfunction!(config_snapshot, module)?)?;
    Ok(())
}
