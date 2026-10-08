// SPDX-License-Identifier: Apache-2.0

//! Package test runner for "tests" attribute.
//!
//! Ported from Python rez package_test.py.

use crate::config::CONFIG;
use crate::errors::{Result, RezError};
use crate::package::{DeveloperPackage, Package, Variant};
use crate::resolve::context::{ResolveOptions, ResolvedContext, RexExecutionCallback};
use crate::shell::types::{detect_shell, ShellType};
use repository::provider::FilesystemPackageProvider;
use version::{Requirement, RequirementList};

use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::time::Instant;

// ---------------------------------------------------------------------------
// TestRunOn - when a test should be executed
// ---------------------------------------------------------------------------

/// Specifies when a test should run in the package lifecycle.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TestRunOn {
    /// Run by default (when no explicit filter is given).
    Default,
    /// Run before package installation.
    PreInstall,
    /// Run after package installation.
    PostInstall,
    /// Run before package release.
    PreRelease,
    /// Run after package release.
    PostRelease,
    /// Only run when explicitly requested by name.
    Explicit,
    /// A user-defined Rez run_on tag.
    Custom(String),
}

impl TestRunOn {
    /// Parse standard Rez lifecycle tags and preserve custom tags verbatim.
    pub fn from_str_val(s: &str) -> Self {
        match s {
            "default" => Self::Default,
            "pre_install" => Self::PreInstall,
            "post_install" => Self::PostInstall,
            "pre_release" => Self::PreRelease,
            "post_release" => Self::PostRelease,
            "explicit" => Self::Explicit,
            _ => Self::Custom(s.to_owned()),
        }
    }

    /// Convert to string representation.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Default => "default",
            Self::PreInstall => "pre_install",
            Self::PostInstall => "post_install",
            Self::PreRelease => "pre_release",
            Self::PostRelease => "post_release",
            Self::Explicit => "explicit",
            Self::Custom(tag) => tag,
        }
    }
}

impl std::fmt::Display for TestRunOn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ---------------------------------------------------------------------------
// TestStatus - outcome of a single test execution
// ---------------------------------------------------------------------------

/// Result status of a test execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TestStatus {
    Passed,
    Failed,
    Skipped,
    Error(String),
}

impl TestStatus {
    pub fn is_passed(&self) -> bool {
        matches!(self, Self::Passed)
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, Self::Failed)
    }

    pub fn is_skipped(&self) -> bool {
        matches!(self, Self::Skipped)
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error(_))
    }
}

impl std::fmt::Display for TestStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Passed => write!(f, "passed"),
            Self::Failed => write!(f, "failed"),
            Self::Skipped => write!(f, "skipped"),
            Self::Error(msg) => write!(f, "error: {}", msg),
        }
    }
}

// ---------------------------------------------------------------------------
// OnVariants - variant selection mode for tests
// ---------------------------------------------------------------------------

/// Controls which variants a test runs on.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum OnVariants {
    /// Run test on only one (preferred) variant (Python: False).
    #[default]
    First,
    /// Run test on all variants (Python: True).
    All,
    /// Run test on variants matching a filter (Python: dict with "type"+"value").
    Filter {
        filter_type: String,
        value: Vec<String>,
    },
}

// ---------------------------------------------------------------------------
// TestCommand - command to execute
// ---------------------------------------------------------------------------

/// Test command: either a shell string or arguments joined for the configured shell.
#[derive(Clone, Debug, PartialEq)]
pub enum TestCommand {
    /// Shell command string (interpreted via shell).
    Shell(String),
    /// Command arguments joined and interpreted by the configured shell.
    Args(Vec<String>),
}

impl TestCommand {
    /// Expand {root} and other placeholders in the command.
    pub fn expand_vars(&self, vars: &HashMap<String, String>) -> Self {
        match self {
            Self::Shell(cmd) => {
                let mut expanded = cmd.clone();
                for (key, val) in vars {
                    expanded = expanded.replace(&format!("{{{}}}", key), val);
                }
                Self::Shell(expanded)
            }
            Self::Args(args) => {
                let expanded: Vec<String> = args
                    .iter()
                    .map(|arg| {
                        let mut s = arg.clone();
                        for (key, val) in vars {
                            s = s.replace(&format!("{{{}}}", key), val);
                        }
                        s
                    })
                    .collect();
                Self::Args(expanded)
            }
        }
    }

    /// Get display string for the command.
    pub fn display(&self) -> String {
        match self {
            Self::Shell(cmd) => cmd.clone(),
            Self::Args(args) => configured_shell().join_command(args, true, None),
        }
    }
}

impl std::fmt::Display for TestCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display())
    }
}

// ---------------------------------------------------------------------------
// TestSpec - parsed test specification from package data
// ---------------------------------------------------------------------------

/// A parsed test specification from a package's "tests" attribute.
///
/// Represents a single named test with its command, requirements,
/// run_on tags, and variant selection mode.
#[derive(Clone, Debug)]
pub struct TestSpec {
    /// Test name (key in the tests dict).
    pub name: String,
    /// Command to execute.
    pub command: TestCommand,
    /// Additional package requirements for the test environment.
    pub requires: Vec<String>,
    /// When this test should run.
    pub run_on: Vec<TestRunOn>,
    /// Which variants to run on.
    pub on_variants: OnVariants,
}

impl TestSpec {
    /// Check if this test should run for the given run_on tags.
    pub fn matches_run_on(&self, tags: &[TestRunOn]) -> bool {
        if tags.is_empty() {
            return true;
        }
        self.run_on.iter().any(|r| tags.contains(r))
    }
}

// ---------------------------------------------------------------------------
// TestResult - outcome of a single test execution
// ---------------------------------------------------------------------------

/// Result of executing a single test.
#[derive(Clone, Debug)]
pub struct TestResult {
    /// Name of the test.
    pub test_name: String,
    /// Execution status.
    pub status: TestStatus,
    /// Captured stdout output.
    pub stdout: String,
    /// Captured stderr output.
    pub stderr: String,
    /// Process exit code (None if test didn't run).
    pub exit_code: Option<i32>,
    /// Elapsed time in seconds.
    pub elapsed_secs: f64,
    /// Description / reason for the result.
    pub description: String,
}

// ---------------------------------------------------------------------------
// TestResults - aggregated results from test runs
// ---------------------------------------------------------------------------

/// Aggregated results from running multiple tests.
#[derive(Clone, Debug, Default)]
pub struct TestResults {
    /// Individual test results.
    pub results: Vec<TestResult>,
    /// Total elapsed time in seconds.
    pub total_elapsed: f64,
}

impl TestResults {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a test result.
    pub fn add(&mut self, result: TestResult) {
        self.total_elapsed += result.elapsed_secs;
        self.results.push(result);
    }

    /// Number of tests executed.
    pub fn num_tests(&self) -> usize {
        self.results.len()
    }

    /// Count of passed tests.
    pub fn num_passed(&self) -> usize {
        self.results.iter().filter(|r| r.status.is_passed()).count()
    }

    /// Count of failed tests.
    pub fn num_failed(&self) -> usize {
        self.results
            .iter()
            .filter(|r| r.status.is_failed() || r.status.is_error())
            .count()
    }

    /// Count of skipped tests.
    pub fn num_skipped(&self) -> usize {
        self.results
            .iter()
            .filter(|r| r.status.is_skipped())
            .count()
    }

    /// True if all non-skipped tests passed.
    pub fn all_passed(&self) -> bool {
        self.num_failed() == 0
    }

    /// Format summary as a string.
    pub fn summary(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "Test results: {} passed, {} failed, {} skipped (total: {:.2}s)\n",
            self.num_passed(),
            self.num_failed(),
            self.num_skipped(),
            self.total_elapsed
        ));
        out.push_str(&"-".repeat(80));
        out.push('\n');

        for r in &self.results {
            out.push_str(&format!(
                "  {:<30} {:<10} {}\n",
                r.test_name, r.status, r.description
            ));
        }
        out
    }

    /// Print summary to stdout.
    pub fn print_summary(&self) {
        print!("{}", self.summary());
    }
}

// ---------------------------------------------------------------------------
// parse_tests_from_data - parse test specs from package JSON data
// ---------------------------------------------------------------------------

/// Parse test specifications from a package's tests attribute value.
pub fn parse_tests_from_data(data: &Value) -> Result<Vec<TestSpec>> {
    let obj = data.as_object().ok_or_else(|| RezError::PackageMetadata {
        msg: format!("tests must be an object, got {}", data_type_name(data)),
        path: None,
        resource_key: None,
    })?;
    parse_test_entries(obj.iter().map(|(name, value)| (name.as_str(), value)))
}

/// Parse test specs from a Package's tests field directly.
pub fn parse_tests_from_package(pkg: &Package) -> Result<Vec<TestSpec>> {
    match &pkg.tests {
        Some(tests) => parse_test_entries(tests.iter().map(|(name, value)| (name.as_str(), value))),
        None => Ok(Vec::new()),
    }
}

fn parse_test_entries<'a>(
    entries: impl IntoIterator<Item = (&'a str, &'a Value)>,
) -> Result<Vec<TestSpec>> {
    let mut specs = entries
        .into_iter()
        .map(|(name, value)| parse_single_test(name, value))
        .collect::<Result<Vec<_>>>()?;
    specs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(specs)
}

/// Parse a single test entry from its name and JSON value.
fn parse_single_test(name: &str, val: &Value) -> Result<TestSpec> {
    crate::serialise::validate_test_entry(name, val)?;
    let (command, requires, run_on, on_variants) = match val {
        Value::String(command) => (
            TestCommand::Shell(command.clone()),
            Vec::new(),
            vec![TestRunOn::Default],
            OnVariants::First,
        ),
        Value::Array(args) => (
            TestCommand::Args(
                args.iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| malformed_test(name, "command"))
                    })
                    .collect::<Result<Vec<_>>>()?,
            ),
            Vec::new(),
            vec![TestRunOn::Default],
            OnVariants::First,
        ),
        Value::Object(fields) => {
            let command = fields
                .get("command")
                .ok_or_else(|| malformed_test(name, "command"))?;
            let command = match command {
                Value::String(command) => TestCommand::Shell(command.clone()),
                Value::Array(args) => TestCommand::Args(
                    args.iter()
                        .map(|value| {
                            value
                                .as_str()
                                .map(str::to_owned)
                                .ok_or_else(|| malformed_test(name, "command"))
                        })
                        .collect::<Result<Vec<_>>>()?,
                ),
                _ => return Err(malformed_test(name, "command")),
            };
            let requires = fields
                .get("requires")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .map(|value| {
                            value
                                .as_str()
                                .map(str::to_owned)
                                .ok_or_else(|| malformed_test(name, "requires"))
                        })
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default();
            let run_on = parse_run_on(fields.get("run_on"), name)?;
            let on_variants = parse_on_variants(fields.get("on_variants"), name)?;
            (command, requires, run_on, on_variants)
        }
        _ => return Err(malformed_test(name, "entry")),
    };
    Ok(TestSpec {
        name: name.to_owned(),
        command,
        requires,
        run_on,
        on_variants,
    })
}

fn malformed_test(name: &str, field: &str) -> RezError {
    let key = serde_json::to_string(name).unwrap_or_else(|_| format!("{name:?}"));
    RezError::PackageMetadata {
        msg: format!("tests[{key}].{field} is invalid"),
        path: None,
        resource_key: None,
    }
}

fn data_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Missing or empty run_on values have Rez's default tag; arbitrary strings are
/// preserved because Rez permits user-defined lifecycle tags.
fn parse_run_on(val: Option<&Value>, name: &str) -> Result<Vec<TestRunOn>> {
    let tags = match val {
        None => return Ok(vec![TestRunOn::Default]),
        Some(Value::String(tag)) if tag.is_empty() => return Ok(vec![TestRunOn::Default]),
        Some(Value::String(tag)) => return Ok(vec![TestRunOn::from_str_val(tag)]),
        Some(Value::Array(tags)) if tags.is_empty() => return Ok(vec![TestRunOn::Default]),
        Some(Value::Array(tags)) => tags,
        _ => return Err(malformed_test(name, "run_on")),
    };
    tags.iter()
        .map(|value| {
            value
                .as_str()
                .map(TestRunOn::from_str_val)
                .ok_or_else(|| malformed_test(name, "run_on"))
        })
        .collect()
}

fn parse_on_variants(val: Option<&Value>, name: &str) -> Result<OnVariants> {
    match val {
        None | Some(Value::Bool(false)) => Ok(OnVariants::First),
        Some(Value::Bool(true)) => Ok(OnVariants::All),
        Some(Value::Object(filter)) => {
            let filter_type = filter
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(|| malformed_test(name, "on_variants.type"))?;
            let values = filter
                .get("value")
                .and_then(Value::as_array)
                .ok_or_else(|| malformed_test(name, "on_variants.value"))?;
            let value = values
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| malformed_test(name, "on_variants.value"))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(OnVariants::Filter {
                filter_type: filter_type.to_owned(),
                value,
            })
        }
        _ => Err(malformed_test(name, "on_variants")),
    }
}

// ---------------------------------------------------------------------------
// PackageTestRunner - main test runner
// ---------------------------------------------------------------------------

/// Runner for executing tests defined in a package.
pub struct PackageTestRunner {
    pub package: Package,
    developer_package: Option<DeveloperPackage>,
    pub paths: Vec<PathBuf>,
    pub verbose: u8,
    pub dry_run: bool,
    pub stop_on_fail: bool,
    pub inplace: bool,
    pub extra_packages: Vec<String>,
    pub results: TestResults,
    pub stopped_on_fail: bool,
}

impl PackageTestRunner {
    pub fn new(package: Package, paths: Vec<PathBuf>, verbose: u8) -> Self {
        Self {
            package,
            developer_package: None,
            paths,
            verbose,
            dry_run: false,
            stop_on_fail: false,
            inplace: false,
            extra_packages: Vec::new(),
            results: TestResults::new(),
            stopped_on_fail: false,
        }
    }

    /// Create a test runner that retains developer source provenance for deferred values.
    pub fn from_developer_package(
        developer_package: DeveloperPackage,
        paths: Vec<PathBuf>,
        verbose: u8,
    ) -> Self {
        let mut runner = Self::new(developer_package.package.clone(), paths, verbose);
        runner.developer_package = Some(developer_package);
        runner
    }

    fn package_for_test_variant(&self, variant: Option<&Variant>) -> Result<Package> {
        let mut package = self.package.clone();
        let Some(source) = package
            .attributes
            .get("tests")
            .and_then(Value::as_str)
            .filter(|_| {
                crate::serialise::is_late_binding_source(&package.attributes["tests"], "tests")
            })
        else {
            return Ok(package);
        };
        let mut data = package.attributes.clone();
        data.insert("is_package".into(), Value::Bool(variant.is_none()));
        data.insert("is_variant".into(), Value::Bool(variant.is_some()));
        data.insert(
            "index".into(),
            variant
                .and_then(|variant| variant.index)
                .map_or(Value::Null, Value::from),
        );
        data.insert(
            "variant_requires".into(),
            serde_json::json!(variant
                .map(|variant| variant
                    .variant_requires
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>())
                .unwrap_or_default()),
        );
        if let Some(developer) = &self.developer_package {
            data.remove("base");
            if let Some(root) = developer.root() {
                data.insert(
                    "root".into(),
                    Value::String(root.to_string_lossy().into_owned()),
                );
            }
        } else if let Some(base) = &package.base {
            data.insert(
                "base".into(),
                Value::String(base.to_string_lossy().into_owned()),
            );
        }
        let value = crate::serialise::eval_late_binding_for_package(source, "tests", &data, false)?;
        if value.is_null() {
            package.tests = None;
        } else {
            let tests = value.as_object().ok_or_else(|| {
                RezError::PackageTest("Late tests must return a mapping or None".into())
            })?;
            for (name, value) in tests {
                crate::serialise::validate_test_entry(name, value)?;
            }
            package.tests = Some(
                tests
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect(),
            );
        }
        Ok(package)
    }

    pub fn test_specs(&self) -> Result<Vec<TestSpec>> {
        let package = self.package_for_test_variant(None)?;
        parse_tests_from_package(&package)
    }

    pub fn test_names(&self, run_on: Option<&[TestRunOn]>) -> Result<Vec<String>> {
        Ok(self
            .test_specs()?
            .into_iter()
            .filter(|spec| run_on.is_none_or(|tags| spec.matches_run_on(tags)))
            .map(|spec| spec.name)
            .collect())
    }

    pub fn find_test(&self, name: &str) -> Result<Option<TestSpec>> {
        Ok(self
            .test_specs()?
            .into_iter()
            .find(|spec| spec.name == name))
    }

    pub fn run_tests(&mut self, run_on: Option<&[TestRunOn]>) -> Result<&TestResults> {
        let specs = self.test_specs()?;
        for spec in specs
            .iter()
            .filter(|spec| run_on.is_none_or(|tags| spec.matches_run_on(tags)))
        {
            if self.stopped_on_fail {
                break;
            }
            self.run_test(&spec.name)?;
        }
        Ok(&self.results)
    }

    /// Run a single test by name. Variant-specific tests add one result per target variant.
    pub fn run_test(&mut self, name: &str) -> Result<&TestResult> {
        let test_package = self.package_for_test_variant(None)?;
        let spec = parse_tests_from_package(&test_package)?
            .into_iter()
            .find(|spec| spec.name == name)
            .ok_or_else(|| {
                RezError::PackageTest(format!(
                    "Test '{}' not found in package {}",
                    name,
                    test_package.qualified_name()
                ))
            })?;

        let start_index = self.results.results.len();
        let mut variants: Vec<Variant> = test_package.iter_variants().collect();
        if variants.is_empty() {
            variants.push(Variant::from_package(test_package.clone()));
        }

        if self.inplace {
            let current = match ResolvedContext::get_current(None) {
                Some(context) => context?,
                None => {
                    self.results
                        .add(skipped_test(name, "No current Rez context"));
                    return Ok(self.results.results.last().expect("just pushed"));
                }
            };
            let Some(resolved) = current.get_resolved_package(&test_package.name) else {
                self.results.add(skipped_test(
                    name,
                    "The current environment does not contain the package",
                ));
                return Ok(self.results.results.last().expect("just pushed"));
            };
            variants.retain(|variant| variant.index == resolved.variant_index);
        }
        let run_once = matches!(&spec.on_variants, OnVariants::First);
        if run_once && !self.inplace && !variants.is_empty() {
            let common_requirements = self.test_requirements(&test_package, &spec, None);
            if let Ok(Some(context)) = self.resolve_context(&common_requirements) {
                if context.success() {
                    if let Some(resolved) = context.get_resolved_package(&test_package.name) {
                        if let Some(preferred) = variants
                            .iter()
                            .find(|variant| variant.index == resolved.variant_index)
                            .cloned()
                        {
                            variants = vec![preferred];
                        }
                    }
                }
            }
        }

        if variants.is_empty() {
            self.results.add(skipped_test(
                name,
                "No matching package variant is available",
            ));
        }

        for variant in variants {
            if self.stopped_on_fail {
                break;
            }

            let variant_package = self.package_for_test_variant(Some(&variant))?;
            let variant_spec = parse_tests_from_package(&variant_package)?
                .into_iter()
                .find(|spec| spec.name == name);
            let Some(variant_spec) = variant_spec else {
                self.results.add(skipped_test(
                    name,
                    "The test is not declared for this package variant",
                ));
                continue;
            };

            if let OnVariants::Filter { filter_type, value } = &variant_spec.on_variants {
                if filter_type != "requires" {
                    return Err(RezError::PackageTest(format!(
                        "Unsupported on_variants filter type '{}'; upstream Rez supports 'requires'",
                        filter_type
                    )));
                }
                if !variant_matches_requirements(&variant, value)? {
                    self.results.add(skipped_test(
                        name,
                        "Test skipped by on_variants requires filter",
                    ));
                    continue;
                }
            }

            let requirements =
                self.test_requirements(&variant_package, &variant_spec, Some(&variant));
            let context = match self.resolve_context(&requirements) {
                Ok(Some(context)) if context.success() => context,
                Ok(Some(context)) => {
                    self.results.add(test_failure(
                        name,
                        format!(
                            "Test environment failed to resolve: {}",
                            context
                                .failure_description
                                .as_deref()
                                .unwrap_or("unknown resolve error")
                        ),
                        TestStatus::Failed,
                    ));
                    if self.stop_on_fail {
                        self.stopped_on_fail = true;
                    }
                    continue;
                }
                Ok(None) => {
                    self.results.add(skipped_test(
                        name,
                        "The current environment does not meet test requirements",
                    ));
                    continue;
                }
                Err(error) => {
                    self.results.add(test_failure(
                        name,
                        format!("Test environment failed to resolve: {error}"),
                        TestStatus::Failed,
                    ));
                    if self.stop_on_fail {
                        self.stopped_on_fail = true;
                    }
                    continue;
                }
            };

            let Some(resolved) = context.get_resolved_package(&variant_package.name) else {
                self.results.add(skipped_test(
                    name,
                    "Resolved environment does not contain the tested package",
                ));
                continue;
            };
            if resolved.variant_index != variant.index {
                self.results.add(skipped_test(
                    name,
                    "Could not resolve to the requested package variant",
                ));
                continue;
            }

            let result = self.execute_test(&variant_package, &variant_spec, &variant, &context);
            let failed = result.status.is_failed() || result.status.is_error();
            let passed = result.status.is_passed();
            self.results.add(result);
            if failed && self.stop_on_fail {
                self.stopped_on_fail = true;
            }
            if passed && run_once {
                break;
            }
        }

        let results = &self.results.results[start_index..];
        let result_index = results
            .iter()
            .position(|result| result.status.is_failed() || result.status.is_error())
            .unwrap_or(results.len().saturating_sub(1));
        Ok(&self.results.results[start_index + result_index])
    }

    fn test_requirements(
        &self,
        package: &Package,
        spec: &TestSpec,
        variant: Option<&Variant>,
    ) -> Vec<String> {
        let mut requirements = vec![format!("{}=={}", package.name, package.version)];
        requirements.extend(spec.requires.iter().cloned());
        requirements.extend(self.extra_packages.iter().cloned());
        if let Some(variant) = variant {
            requirements.extend(variant.variant_requires.iter().map(ToString::to_string));
        }
        requirements
    }

    fn resolve_context(&self, requests: &[String]) -> Result<Option<ResolvedContext>> {
        if self.inplace {
            let Some(context) = ResolvedContext::get_current(None) else {
                return Ok(None);
            };
            let context = context?;
            if !context.success() || !context_meets_requirements(&context, requests)? {
                return Ok(None);
            }
            return Ok(Some(context));
        }

        let requests = requests
            .iter()
            .map(|request| Requirement::new(request))
            .collect::<Result<Vec<_>>>()?;
        let provider = FilesystemPackageProvider::from_paths(&self.paths)?;
        ResolvedContext::resolve(
            requests,
            &provider,
            ResolveOptions::new()
                .with_paths(self.paths.clone())
                .with_testing(true),
        )
        .map(Some)
    }

    fn execute_test(
        &self,
        package: &Package,
        spec: &TestSpec,
        variant: &Variant,
        context: &ResolvedContext,
    ) -> TestResult {
        if self.verbose >= 1 {
            eprintln!("Running test: {}", spec.name);
        }
        if self.dry_run {
            return skipped_test(&spec.name, "Dry run mode");
        }

        let mut vars = HashMap::new();
        if let Some(root) = variant.root.clone().or_else(|| {
            package.base.as_ref().map(|base| {
                variant
                    .subpath
                    .as_ref()
                    .map_or_else(|| base.clone(), |subpath| base.join(subpath))
            })
        }) {
            vars.insert("root".to_string(), root.display().to_string());
        }
        vars.insert("name".to_string(), package.name.clone());
        vars.insert("version".to_string(), package.version.to_string());

        let command = spec.command.expand_vars(&vars);
        if self.verbose >= 2 {
            eprintln!("  Command: {command}");
            if !spec.requires.is_empty() {
                eprintln!("  Requires: {}", spec.requires.join(", "));
            }
            if !self.extra_packages.is_empty() {
                eprintln!("  Extra packages: {}", self.extra_packages.join(", "));
            }
        }

        let callback = package.pre_test_commands.as_deref().and_then(|code| {
            (!code.trim().is_empty()).then(|| RexExecutionCallback {
                package_name: variant.parent.name.clone(),
                name: "pre_test_commands".into(),
                code: code.to_string(),
                package: Some(variant.clone()),
                bindings: HashMap::from([("test".into(), serde_json::json!({"name": spec.name}))]),
                developer: self.developer_package.is_some(),
            })
        });

        let start = Instant::now();
        match exec_command(&command, Some(context), callback.as_ref()) {
            Ok((exit_code, stdout, stderr)) => TestResult {
                test_name: spec.name.clone(),
                status: if exit_code == 0 {
                    TestStatus::Passed
                } else {
                    TestStatus::Failed
                },
                stdout,
                stderr,
                exit_code: Some(exit_code),
                elapsed_secs: start.elapsed().as_secs_f64(),
                description: if exit_code == 0 {
                    "Test succeeded".to_string()
                } else {
                    format!("Test failed with exit code {exit_code}")
                },
            },
            Err(error) => {
                let reason = format!("Failed to execute test command: {error}");
                test_failure(&spec.name, reason.clone(), TestStatus::Error(reason))
            }
        }
    }

    pub fn print_summary(&self) {
        self.results.print_summary();
    }
}

fn context_meets_requirements(context: &ResolvedContext, requests: &[String]) -> Result<bool> {
    let resolved = context.resolved_packages().unwrap_or_default();
    for request in requests {
        let requirement = Requirement::new(request)?;
        let package = resolved
            .iter()
            .find(|package| package.name == requirement.name());
        if requirement.conflict() {
            if package.is_some() {
                return Ok(false);
            }
        } else {
            let Some(package) = package else {
                return Ok(false);
            };
            if requirement
                .range()
                .is_some_and(|range| !range.contains_version(&package.version))
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn variant_matches_requirements(variant: &Variant, filters: &[String]) -> Result<bool> {
    let filters = filters
        .iter()
        .map(|filter| Requirement::new(filter))
        .collect::<Result<Vec<_>>>()?;
    let combined = RequirementList::new(
        variant
            .variant_requires
            .iter()
            .cloned()
            .chain(filters)
            .collect(),
    );
    if combined.conflict().is_some() {
        return Ok(false);
    }

    let actual = RequirementList::new(variant.variant_requires.clone())
        .requirements()
        .iter()
        .filter(|requirement| !requirement.conflict())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let selected = combined
        .requirements()
        .iter()
        .filter(|requirement| !requirement.conflict())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    Ok(actual == selected)
}

fn skipped_test(name: &str, reason: &str) -> TestResult {
    TestResult {
        test_name: name.to_string(),
        status: TestStatus::Skipped,
        stdout: String::new(),
        stderr: String::new(),
        exit_code: None,
        elapsed_secs: 0.0,
        description: reason.to_string(),
    }
}

fn test_failure(name: &str, reason: String, status: TestStatus) -> TestResult {
    TestResult {
        test_name: name.to_string(),
        status,
        stdout: String::new(),
        stderr: reason.clone(),
        exit_code: None,
        elapsed_secs: 0.0,
        description: reason,
    }
}

// ---------------------------------------------------------------------------
// Command execution helper
// ---------------------------------------------------------------------------

/// Execute a test command inside a resolved package environment.
fn configured_shell() -> ShellType {
    if CONFIG.default_shell.is_empty() {
        detect_shell()
    } else {
        ShellType::from_name(&CONFIG.default_shell).unwrap_or_else(detect_shell)
    }
}

fn exec_command(
    cmd: &TestCommand,
    context: Option<&ResolvedContext>,
    callback: Option<&RexExecutionCallback>,
) -> Result<(i32, String, String)> {
    let shell_type = configured_shell();
    let command = match cmd {
        TestCommand::Shell(command) => command.clone(),
        TestCommand::Args(args) => shell_type.join_command(args, true, None),
    };
    let output = if let Some(context) = context {
        context.execute_shell_output(
            Some(shell_type),
            &command,
            Some(env::vars().collect()),
            true,
            true,
            callback,
        )?
    } else {
        shell_type.command(&command).output()?
    };
    Ok((
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn runner_with_repository(mut package: Package) -> (PackageTestRunner, tempfile::TempDir) {
        let repository = tempfile::tempdir().unwrap();
        let package_path = repository
            .path()
            .join(&package.name)
            .join(package.version.to_string());
        std::fs::create_dir_all(&package_path).unwrap();
        std::fs::write(
            package_path.join("package.yaml"),
            format!("name: {}\nversion: '{}'\n", package.name, package.version),
        )
        .unwrap();
        package.base = Some(package_path);
        let runner = PackageTestRunner::new(package, vec![repository.path().to_path_buf()], 0);
        (runner, repository)
    }

    // -- TestRunOn --

    #[test]
    fn test_run_on_from_str() {
        assert_eq!(TestRunOn::from_str_val("default"), TestRunOn::Default);
        assert_eq!(
            TestRunOn::from_str_val("pre_install"),
            TestRunOn::PreInstall
        );
        assert_eq!(
            TestRunOn::from_str_val("post_install"),
            TestRunOn::PostInstall
        );
        assert_eq!(
            TestRunOn::from_str_val("pre_release"),
            TestRunOn::PreRelease
        );
        assert_eq!(
            TestRunOn::from_str_val("post_release"),
            TestRunOn::PostRelease
        );
        assert_eq!(TestRunOn::from_str_val("explicit"), TestRunOn::Explicit);
        assert_eq!(
            TestRunOn::from_str_val("DEFAULT"),
            TestRunOn::Custom("DEFAULT".into())
        );
        assert_eq!(
            TestRunOn::from_str_val("unknown"),
            TestRunOn::Custom("unknown".into())
        );
    }

    #[test]
    fn test_run_on_display() {
        assert_eq!(TestRunOn::Default.to_string(), "default");
        assert_eq!(TestRunOn::PreInstall.to_string(), "pre_install");
        assert_eq!(TestRunOn::Explicit.to_string(), "explicit");
    }

    // -- TestStatus --

    #[test]
    fn test_status_predicates() {
        assert!(TestStatus::Passed.is_passed());
        assert!(!TestStatus::Passed.is_failed());

        assert!(TestStatus::Failed.is_failed());
        assert!(!TestStatus::Failed.is_passed());

        assert!(TestStatus::Skipped.is_skipped());
        assert!(!TestStatus::Skipped.is_error());

        let err = TestStatus::Error("boom".into());
        assert!(err.is_error());
        assert!(!err.is_passed());
    }

    #[test]
    fn test_status_display() {
        assert_eq!(TestStatus::Passed.to_string(), "passed");
        assert_eq!(TestStatus::Failed.to_string(), "failed");
        assert_eq!(TestStatus::Skipped.to_string(), "skipped");
        assert_eq!(TestStatus::Error("oops".into()).to_string(), "error: oops");
    }

    // -- TestCommand --

    #[test]
    fn test_command_shell_display() {
        let cmd = TestCommand::Shell("python -m pytest".into());
        assert_eq!(cmd.display(), "python -m pytest");
        assert_eq!(cmd.to_string(), "python -m pytest");
    }

    #[test]
    fn test_command_args_display() {
        let cmd = TestCommand::Args(vec!["python".into(), "-m".into(), "pytest".into()]);
        assert_eq!(cmd.display(), "python -m pytest");
    }

    #[test]
    fn test_command_args_expansion_and_backticks() {
        let args = vec![
            "echo".to_string(),
            "a b".to_string(),
            "$HOME".to_string(),
            "a\"b".to_string(),
            "`x`".to_string(),
            String::new(),
        ];
        assert_eq!(
            ShellType::Bash.join_command(&args, true, None),
            "echo \"a b\" \"$HOME\" \"a\"'\"'\"b\" \"\\\x60x\\\x60\" ''"
        );
        assert_eq!(
            ShellType::Cmd.join_command(&args, true, None),
            r#"echo "a b" $HOME "a\"b" `x` """#
        );
        let powershell = ShellType::PowerShell.join_command(&args, true, None);
        if cfg!(windows) {
            assert!(powershell.contains(
                "$__rez_rs_argv = @(\"a b\", \"$HOME\", \"a\x60\"b\", \"\x60\x60x\x60\x60\", \"\")"
            ));
        } else {
            assert_eq!(
                powershell,
                "& \"echo\" \"a b\" \"$HOME\" \"a\x60\"b\" \"\x60\x60x\x60\x60\" \"\""
            );
        }
        assert_eq!(ShellType::Bash.join_command(&[], true, None), "");
    }

    #[test]
    fn test_command_expand_vars() {
        let mut vars = HashMap::new();
        vars.insert("root".to_string(), "/pkg/foo/1.0".to_string());
        vars.insert("name".to_string(), "foo".to_string());

        let cmd = TestCommand::Shell("python {root}/tests/run.py --pkg {name}".into());
        let expanded = cmd.expand_vars(&vars);
        assert_eq!(
            expanded,
            TestCommand::Shell("python /pkg/foo/1.0/tests/run.py --pkg foo".into())
        );

        let cmd2 = TestCommand::Args(vec![
            "python".into(),
            "{root}/test.py".into(),
            "{name}".into(),
        ]);
        let expanded2 = cmd2.expand_vars(&vars);
        assert_eq!(
            expanded2,
            TestCommand::Args(vec![
                "python".into(),
                "/pkg/foo/1.0/test.py".into(),
                "foo".into(),
            ])
        );
    }

    #[test]
    fn test_command_expand_no_vars() {
        let vars = HashMap::new();
        let cmd = TestCommand::Shell("echo hello".into());
        let expanded = cmd.expand_vars(&vars);
        assert_eq!(expanded, TestCommand::Shell("echo hello".into()));
    }

    // -- parse_tests_from_data --

    #[test]
    fn test_parse_empty() {
        let data = json!({});
        let specs = parse_tests_from_data(&data).unwrap();
        assert!(specs.is_empty());
    }

    #[test]
    fn test_parse_non_object_returns_metadata_error() {
        let data = json!(null);
        let error = parse_tests_from_data(&data).unwrap_err().to_string();
        assert!(error.contains("tests must be an object"));
    }

    #[test]
    fn test_parse_string_command() {
        let data = json!({
            "unit": "python -m unittest discover"
        });
        let specs = parse_tests_from_data(&data).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "unit");
        assert_eq!(
            specs[0].command,
            TestCommand::Shell("python -m unittest discover".into())
        );
        assert_eq!(specs[0].run_on, vec![TestRunOn::Default]);
        assert_eq!(specs[0].on_variants, OnVariants::First);
        assert!(specs[0].requires.is_empty());
    }

    #[test]
    fn test_parse_args_command() {
        let data = json!({
            "lint": ["python", "-m", "flake8", "."]
        });
        let specs = parse_tests_from_data(&data).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "lint");
        assert_eq!(
            specs[0].command,
            TestCommand::Args(vec![
                "python".into(),
                "-m".into(),
                "flake8".into(),
                ".".into(),
            ])
        );
    }

    #[test]
    fn test_parse_dict_command() {
        let data = json!({
            "CI": {
                "command": "python {root}/ci_tests/main.py",
                "requires": ["maya-2017", "pymel"],
                "run_on": "explicit",
                "on_variants": true
            }
        });
        let specs = parse_tests_from_data(&data).unwrap();
        assert_eq!(specs.len(), 1);

        let spec = &specs[0];
        assert_eq!(spec.name, "CI");
        assert_eq!(
            spec.command,
            TestCommand::Shell("python {root}/ci_tests/main.py".into())
        );
        assert_eq!(spec.requires, vec!["maya-2017", "pymel"]);
        assert_eq!(spec.run_on, vec![TestRunOn::Explicit]);
        assert_eq!(spec.on_variants, OnVariants::All);
    }

    #[test]
    fn test_parse_dict_with_list_run_on() {
        let data = json!({
            "integration": {
                "command": "run_tests.sh",
                "run_on": ["pre_release", "post_install"]
            }
        });
        let specs = parse_tests_from_data(&data).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(
            specs[0].run_on,
            vec![TestRunOn::PreRelease, TestRunOn::PostInstall]
        );
    }

    #[test]
    fn test_parse_dict_with_on_variants_filter() {
        let data = json!({
            "targeted": {
                "command": "run_test.sh",
                "on_variants": {
                    "type": "requires",
                    "value": ["maya", "houdini"]
                }
            }
        });
        let specs = parse_tests_from_data(&data).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(
            specs[0].on_variants,
            OnVariants::Filter {
                filter_type: "requires".into(),
                value: vec!["maya".into(), "houdini".into()],
            }
        );
    }

    #[test]
    fn test_parse_multiple_tests_sorted() {
        let data = json!({
            "z_test": "echo z",
            "a_test": "echo a",
            "m_test": "echo m"
        });
        let specs = parse_tests_from_data(&data).unwrap();
        assert_eq!(specs.len(), 3);
        assert_eq!(specs[0].name, "a_test");
        assert_eq!(specs[1].name, "m_test");
        assert_eq!(specs[2].name, "z_test");
    }

    #[test]
    fn test_parse_dict_missing_command_returns_path_error() {
        let data = json!({
            "bad": {
                "requires": ["foo"]
            }
        });
        let error = parse_tests_from_data(&data).unwrap_err().to_string();
        assert!(error.contains("tests[\"bad\"].command"));
    }

    #[test]
    fn test_parse_dict_command_as_list() {
        let data = json!({
            "direct": {
                "command": ["python", "-c", "print('hello')"],
                "requires": ["numpy"]
            }
        });
        let specs = parse_tests_from_data(&data).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(
            specs[0].command,
            TestCommand::Args(vec!["python".into(), "-c".into(), "print('hello')".into()])
        );
        assert_eq!(specs[0].requires, vec!["numpy"]);
    }

    #[test]
    fn test_parse_on_variants_false() {
        let data = json!({
            "t": {
                "command": "echo",
                "on_variants": false
            }
        });
        let specs = parse_tests_from_data(&data).unwrap();
        assert_eq!(specs[0].on_variants, OnVariants::First);
    }

    #[test]
    fn test_parse_null_run_on_returns_path_error() {
        let data = json!({
            "t": {
                "command": "echo",
                "run_on": null
            }
        });
        let error = parse_tests_from_data(&data).unwrap_err().to_string();
        assert!(error.contains("tests[\"t\"].run_on"));
    }

    #[test]
    fn test_parse_custom_run_on_tag_is_preserved() {
        let data = json!({
            "custom": {
                "command": "echo",
                "run_on": "custom_stage"
            }
        });
        let specs = parse_tests_from_data(&data).unwrap();
        assert_eq!(
            specs[0].run_on,
            vec![TestRunOn::Custom("custom_stage".into())]
        );
    }

    #[test]
    fn test_runner_propagates_malformed_test_metadata() {
        use version::Version;

        let mut package = Package::new("foo", Version::new("1.0").unwrap());
        package.tests = Some(HashMap::from([
            ("valid".to_owned(), json!("echo valid")),
            ("invalid".to_owned(), json!({"requires": ["foo"]})),
        ]));
        let mut runner = PackageTestRunner::new(package, Vec::new(), 0);

        let names_error = runner.test_names(None).unwrap_err().to_string();
        assert!(names_error.contains("tests[\"invalid\"].command"));
        let run_error = runner.run_tests(None).unwrap_err().to_string();
        assert!(run_error.contains("tests[\"invalid\"].command"));
        assert_eq!(runner.results.num_tests(), 0);
    }

    // -- TestSpec::matches_run_on --

    #[test]
    fn test_spec_matches_run_on_empty() {
        let spec = TestSpec {
            name: "t".into(),
            command: TestCommand::Shell("echo".into()),
            requires: Vec::new(),
            run_on: vec![TestRunOn::Explicit],
            on_variants: OnVariants::First,
        };
        // Empty filter matches everything
        assert!(spec.matches_run_on(&[]));
    }

    #[test]
    fn test_spec_matches_run_on_hit() {
        let spec = TestSpec {
            name: "t".into(),
            command: TestCommand::Shell("echo".into()),
            requires: Vec::new(),
            run_on: vec![TestRunOn::Default, TestRunOn::PreRelease],
            on_variants: OnVariants::First,
        };
        assert!(spec.matches_run_on(&[TestRunOn::Default]));
        assert!(spec.matches_run_on(&[TestRunOn::PreRelease]));
        assert!(!spec.matches_run_on(&[TestRunOn::Explicit]));
    }

    // -- TestResults --

    #[test]
    fn test_results_counts() {
        let mut results = TestResults::new();
        assert_eq!(results.num_tests(), 0);
        assert!(results.all_passed());

        results.add(TestResult {
            test_name: "a".into(),
            status: TestStatus::Passed,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
            elapsed_secs: 1.0,
            description: "ok".into(),
        });
        results.add(TestResult {
            test_name: "b".into(),
            status: TestStatus::Failed,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(1),
            elapsed_secs: 0.5,
            description: "fail".into(),
        });
        results.add(TestResult {
            test_name: "c".into(),
            status: TestStatus::Skipped,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
            elapsed_secs: 0.0,
            description: "skip".into(),
        });
        results.add(TestResult {
            test_name: "d".into(),
            status: TestStatus::Error("boom".into()),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
            elapsed_secs: 0.1,
            description: "err".into(),
        });

        assert_eq!(results.num_tests(), 4);
        assert_eq!(results.num_passed(), 1);
        assert_eq!(results.num_failed(), 2); // Failed + Error
        assert_eq!(results.num_skipped(), 1);
        assert!(!results.all_passed());
        assert!((results.total_elapsed - 1.6).abs() < 0.001);
    }

    #[test]
    fn test_results_all_passed() {
        let mut results = TestResults::new();
        results.add(TestResult {
            test_name: "a".into(),
            status: TestStatus::Passed,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
            elapsed_secs: 0.1,
            description: "ok".into(),
        });
        results.add(TestResult {
            test_name: "b".into(),
            status: TestStatus::Skipped,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
            elapsed_secs: 0.0,
            description: "skip".into(),
        });
        // Passed + Skipped = all_passed is true
        assert!(results.all_passed());
    }

    #[test]
    fn test_results_summary_format() {
        let mut results = TestResults::new();
        results.add(TestResult {
            test_name: "unit".into(),
            status: TestStatus::Passed,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
            elapsed_secs: 2.5,
            description: "Test succeeded".into(),
        });
        let summary = results.summary();
        assert!(summary.contains("1 passed"));
        assert!(summary.contains("0 failed"));
        assert!(summary.contains("unit"));
        assert!(summary.contains("passed"));
    }

    // -- parse_tests_from_package --

    #[test]
    fn test_parse_from_package_none() {
        use version::Version;
        let pkg = Package::new("foo", Version::new("1.0").unwrap());
        let specs = parse_tests_from_package(&pkg).unwrap();
        assert!(specs.is_empty());
    }

    #[test]
    fn test_parse_from_package_with_tests() {
        use version::Version;

        let mut tests = HashMap::new();
        tests.insert("unit".to_string(), json!("python -m pytest"));
        tests.insert(
            "integration".to_string(),
            json!({
                "command": "run_integration.sh",
                "requires": ["test_framework"],
                "run_on": "explicit"
            }),
        );

        let mut pkg = Package::new("bar", Version::new("2.0").unwrap());
        pkg.tests = Some(tests);

        let specs = parse_tests_from_package(&pkg).unwrap();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].name, "integration");
        assert_eq!(specs[1].name, "unit");
    }

    // -- PackageTestRunner --

    #[test]
    fn test_runner_evaluates_late_tests_per_variant_without_early_reevaluation() {
        use std::fs;
        use std::sync::atomic::{AtomicUsize, Ordering};

        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "rez_test_runner_developer_package_{}_{}",
            std::process::id(),
            id
        ));
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("package.py");
        fs::write(
            &source,
            "name = 'source_tests'\nversion = '1.0'\nvariants = [['python-3.11'], ['python-3.12']]\n@early\ndef description():\n    return str(testing)\n@late\ndef tests():\n    return {'smoke': 'echo test-%s' % (this.index if this.is_variant else 'package')}\n",
        )
        .unwrap();

        let developer_package = DeveloperPackage::from_path(&source).unwrap();
        let runner = PackageTestRunner::from_developer_package(developer_package, Vec::new(), 0);
        assert_eq!(runner.package.description.as_deref(), Some("False"));
        let specs = runner.test_specs().unwrap();
        assert_eq!(
            specs
                .iter()
                .map(|spec| spec.name.as_str())
                .collect::<Vec<_>>(),
            ["smoke"]
        );

        let test_package = runner.package_for_test_variant(None).unwrap();
        let variants = test_package.iter_variants().collect::<Vec<_>>();
        assert_eq!(variants.len(), 2);
        let variant_package = runner.package_for_test_variant(Some(&variants[1])).unwrap();
        let spec = parse_tests_from_package(&variant_package)
            .unwrap()
            .into_iter()
            .find(|spec| spec.name == "smoke")
            .unwrap();
        assert_eq!(spec.command.display(), "echo test-1");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_runner_new() {
        use version::Version;
        let pkg = Package::new("foo", Version::new("1.0").unwrap());
        let runner = PackageTestRunner::new(pkg, vec![], 0);
        assert_eq!(runner.verbose, 0);
        assert!(!runner.dry_run);
        assert!(!runner.stop_on_fail);
        assert_eq!(runner.results.num_tests(), 0);
    }

    #[test]
    fn test_runner_test_names() {
        use version::Version;

        let mut tests = HashMap::new();
        tests.insert("alpha".to_string(), json!("echo alpha"));
        tests.insert(
            "beta".to_string(),
            json!({"command": "echo beta", "run_on": "explicit"}),
        );

        let mut pkg = Package::new("foo", Version::new("1.0").unwrap());
        pkg.tests = Some(tests);

        let runner = PackageTestRunner::new(pkg, vec![], 0);

        // All names
        let all = runner.test_names(None).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.contains(&"alpha".to_string()));
        assert!(all.contains(&"beta".to_string()));

        // Filtered by default
        let defaults = runner.test_names(Some(&[TestRunOn::Default])).unwrap();
        assert_eq!(defaults.len(), 1);
        assert_eq!(defaults[0], "alpha");

        // Filtered by explicit
        let explicit = runner.test_names(Some(&[TestRunOn::Explicit])).unwrap();
        assert_eq!(explicit.len(), 1);
        assert_eq!(explicit[0], "beta");
    }

    #[test]
    fn test_runner_find_test() {
        use version::Version;

        let mut tests = HashMap::new();
        tests.insert("unit".to_string(), json!("echo test"));

        let mut pkg = Package::new("foo", Version::new("1.0").unwrap());
        pkg.tests = Some(tests);

        let runner = PackageTestRunner::new(pkg, vec![], 0);

        assert!(runner.find_test("unit").unwrap().is_some());
        assert!(runner.find_test("nonexistent").unwrap().is_none());
    }

    #[test]
    fn test_runner_dry_run() {
        use version::Version;

        let mut tests = HashMap::new();
        tests.insert("unit".to_string(), json!("echo hello"));

        let mut pkg = Package::new("foo", Version::new("1.0").unwrap());
        pkg.tests = Some(tests);

        let (mut runner, _repository) = runner_with_repository(pkg);
        runner.dry_run = true;

        let result = runner.run_test("unit").unwrap();
        assert!(result.status.is_skipped());
        assert_eq!(result.description, "Dry run mode");
    }

    #[test]
    fn test_on_variants_first_falls_back_when_common_environment_fails() {
        let repository = tempfile::tempdir().unwrap();
        let package_path = repository.path().join("foo").join("1.0");
        std::fs::create_dir_all(&package_path).unwrap();
        std::fs::write(
            package_path.join("package.yaml"),
            "name: foo\nversion: '1.0'\nvariants:\n  - [missing_dependency]\n  - [available_dependency]\ntests:\n  unit:\n    command: echo ok\n    on_variants: false\n",
        )
        .unwrap();

        let dependency_path = repository.path().join("available_dependency").join("1.0");
        std::fs::create_dir_all(&dependency_path).unwrap();
        std::fs::write(
            dependency_path.join("package.yaml"),
            "name: available_dependency\nversion: '1.0'\n",
        )
        .unwrap();

        let mut package = Package::from_yaml(
            &std::fs::read_to_string(package_path.join("package.yaml")).unwrap(),
        )
        .unwrap();
        package.base = Some(package_path);
        let mut runner = PackageTestRunner::new(package, vec![repository.path().to_path_buf()], 0);

        runner.run_test("unit").unwrap();

        assert_eq!(runner.results.results.len(), 2);
        assert!(runner.results.results[0].status.is_failed());
        assert!(runner.results.results[1].status.is_passed());
    }

    #[test]
    fn test_on_variants_first_does_not_try_later_after_preferred_failure() {
        let repository = tempfile::tempdir().unwrap();
        let package_path = repository.path().join("foo").join("1.0");
        std::fs::create_dir_all(&package_path).unwrap();
        std::fs::write(
            package_path.join("package.yaml"),
            "name: foo\nversion: '1.0'\nvariants:\n  - []\n  - []\ntests:\n  unit:\n    command: exit 1\n    on_variants: false\n",
        )
        .unwrap();

        let mut package = Package::from_yaml(
            &std::fs::read_to_string(package_path.join("package.yaml")).unwrap(),
        )
        .unwrap();
        package.base = Some(package_path);
        let mut runner = PackageTestRunner::new(package, vec![repository.path().to_path_buf()], 0);

        runner.run_test("unit").unwrap();

        assert_eq!(runner.results.results.len(), 1);
        assert!(runner.results.results[0].status.is_failed());
    }

    #[test]
    fn test_runner_run_test_not_found() {
        use version::Version;
        let pkg = Package::new("foo", Version::new("1.0").unwrap());
        let (mut runner, _repository) = runner_with_repository(pkg);

        let err = runner.run_test("nonexistent");
        assert!(err.is_err());
        match err.unwrap_err() {
            RezError::PackageTest(msg) => {
                assert!(msg.contains("nonexistent"));
            }
            other => panic!("Expected PackageTest error, got: {:?}", other),
        }
    }

    #[test]
    fn pre_test_commands_use_resolved_rex_bindings_before_test_command() {
        use version::Version;

        let mut package = Package::new("testpkg", Version::new("1.0").unwrap());
        package.pre_test_commands = Some(
            "assert this.name == 'testpkg'\nassert str(this.version) == '1.0'\nassert test.name == 'pre_test'\nassert defined('TRACE')\nenv.TRACE.append('_' + this.name + '_' + test.name)".into(),
        );
        #[cfg(windows)]
        {
            package.tests = Some(HashMap::from([(
                "pre_test".to_owned(),
                json!("echo %TRACE%"),
            )]));
        }
        #[cfg(not(windows))]
        {
            package.tests = Some(HashMap::from([(
                "pre_test".to_owned(),
                json!("echo $TRACE"),
            )]));
        }

        let (runner, repository) = runner_with_repository(package);
        let package_definition = repository
            .path()
            .join("testpkg")
            .join("1.0")
            .join("package.yaml");
        std::fs::write(
            package_definition,
            "name: testpkg\nversion: '1.0'\ncommands: |\n  env.TRACE.set('resolved')\n",
        )
        .unwrap();

        let mut runner = runner;
        let result = runner.run_test("pre_test").unwrap();

        assert!(result.status.is_passed(), "{result:?}");
        let output = result.stdout.trim();
        assert!(output.starts_with("resolved"), "stdout was {output:?}");
        assert!(
            output.ends_with("_testpkg_pre_test"),
            "stdout was {output:?}"
        );
    }

    #[test]
    fn test_runner_echo_command() {
        use version::Version;

        let mut tests = HashMap::new();
        tests.insert("echo_test".to_string(), json!("echo hello_rez_test"));

        let mut pkg = Package::new("testpkg", Version::new("1.0").unwrap());
        pkg.tests = Some(tests);

        let (mut runner, _repository) = runner_with_repository(pkg);
        let result = runner.run_test("echo_test").unwrap();

        assert!(result.status.is_passed());
        assert_eq!(result.exit_code, Some(0));
        assert!(result.stdout.contains("hello_rez_test"));
    }

    #[test]
    fn test_runner_failing_command() {
        use version::Version;

        let mut tests = HashMap::new();
        // Use a command that will fail with a known exit code
        #[cfg(windows)]
        tests.insert("fail_test".to_string(), json!("cmd /C exit 42"));
        #[cfg(not(windows))]
        tests.insert("fail_test".to_string(), json!("sh -c 'exit 42'"));

        let mut pkg = Package::new("testpkg", Version::new("1.0").unwrap());
        pkg.tests = Some(tests);

        let (mut runner, _repository) = runner_with_repository(pkg);
        let result = runner.run_test("fail_test").unwrap();

        assert!(result.status.is_failed());
        assert_eq!(result.exit_code, Some(42));
    }

    #[test]
    fn test_runner_run_tests_default() {
        use version::Version;

        let mut tests = HashMap::new();
        tests.insert("a".to_string(), json!("echo a"));
        tests.insert(
            "b".to_string(),
            json!({"command": "echo b", "run_on": "explicit"}),
        );

        let mut pkg = Package::new("foo", Version::new("1.0").unwrap());
        pkg.tests = Some(tests);

        let (mut runner, _repository) = runner_with_repository(pkg);
        runner.run_tests(Some(&[TestRunOn::Default])).unwrap();

        // Only "a" should have run (default), not "b" (explicit)
        assert_eq!(runner.results.num_tests(), 1);
        assert_eq!(runner.results.results[0].test_name, "a");
    }

    #[test]
    fn test_runner_stop_on_fail() {
        use version::Version;

        let mut tests = HashMap::new();
        // a_fail runs first (alphabetical), should fail
        #[cfg(windows)]
        tests.insert("a_fail".to_string(), json!("cmd /C exit 1"));
        #[cfg(not(windows))]
        tests.insert("a_fail".to_string(), json!("sh -c 'exit 1'"));
        tests.insert("b_pass".to_string(), json!("echo ok"));

        let mut pkg = Package::new("foo", Version::new("1.0").unwrap());
        pkg.tests = Some(tests);

        let (mut runner, _repository) = runner_with_repository(pkg);
        runner.stop_on_fail = true;
        runner.run_tests(None).unwrap();

        // Should have stopped after first failure
        assert_eq!(runner.results.num_tests(), 1);
        assert!(runner.stopped_on_fail);
    }

    // -- OnVariants --

    #[test]
    fn test_on_variants_default() {
        assert_eq!(OnVariants::default(), OnVariants::First);
    }

    #[test]
    fn test_parse_on_variants_values() {
        let data = json!({
            "first": {"command": "echo", "on_variants": false},
            "all": {"command": "echo", "on_variants": true},
            "filtered": {
                "command": "echo",
                "on_variants": {"type": "requires", "value": ["foo"]}
            }
        });
        let specs = parse_tests_from_data(&data).unwrap();
        assert_eq!(specs[0].on_variants, OnVariants::All);
        assert_eq!(
            specs[1].on_variants,
            OnVariants::Filter {
                filter_type: "requires".into(),
                value: vec!["foo".into()],
            }
        );
        assert_eq!(specs[2].on_variants, OnVariants::First);
    }

    #[test]
    fn test_parse_run_on_values() {
        let data = json!({
            "default": {"command": "echo"},
            "explicit": {"command": "echo", "run_on": "explicit"},
            "stages": {"command": "echo", "run_on": ["pre_release", "post_release"]},
            "empty": {"command": "echo", "run_on": []}
        });
        let specs = parse_tests_from_data(&data).unwrap();
        assert_eq!(specs[0].run_on, vec![TestRunOn::Default]);
        assert_eq!(specs[1].run_on, vec![TestRunOn::Default]);
        assert_eq!(specs[2].run_on, vec![TestRunOn::Explicit]);
        assert_eq!(
            specs[3].run_on,
            vec![TestRunOn::PreRelease, TestRunOn::PostRelease]
        );
    }

    // -- exec_command --

    #[test]
    fn test_exec_shell_command() {
        let cmd = TestCommand::Shell("echo exec_test_output".into());
        let (code, stdout, _stderr) = exec_command(&cmd, None, None).unwrap();
        assert_eq!(code, 0);
        assert!(stdout.contains("exec_test_output"));
    }

    #[test]
    fn test_exec_args_command() {
        let cmd = TestCommand::Args(vec!["echo".into(), "args_output".into()]);

        let (code, stdout, _stderr) = exec_command(&cmd, None, None).unwrap();
        assert_eq!(code, 0);
        assert!(stdout.contains("args_output"));
    }

    #[test]
    fn test_exec_empty_args() {
        let cmd = TestCommand::Args(vec![]);
        let (exit_code, stdout, stderr) = exec_command(&cmd, None, None).unwrap();
        assert_eq!(exit_code, 0, "stderr was {stderr:?}");
        assert!(stdout.is_empty());
    }

    #[test]
    fn test_runner_executes_empty_args_in_resolved_context_non_interactively() {
        use version::Version;

        let mut package = Package::new("empty_command", Version::new("1.0").unwrap());
        package.tests = Some(HashMap::from([("empty".to_string(), json!([]))]));
        let (mut runner, _repository) = runner_with_repository(package);

        let result = runner.run_test("empty").unwrap();

        assert!(result.status.is_passed(), "{result:?}");
        assert_eq!(result.exit_code, Some(0));
    }

    // -- Variable expansion in runner context --

    #[test]
    fn test_runner_var_expansion() {
        use version::Version;
        let mut tests = HashMap::new();
        #[cfg(windows)]
        tests.insert(
            "var_test".to_string(),
            json!("echo name={name} version={version}"),
        );
        #[cfg(not(windows))]
        tests.insert(
            "var_test".to_string(),
            json!("echo name={name} version={version}"),
        );

        let mut pkg = Package::new("mypkg", Version::new("3.2.1").unwrap());
        pkg.tests = Some(tests);
        let (mut runner, _repository) = runner_with_repository(pkg);
        let result = runner.run_test("var_test").unwrap();

        assert!(result.status.is_passed());
        assert!(result.stdout.contains("name=mypkg"));
        assert!(result.stdout.contains("version=3.2.1"));
    }
}
