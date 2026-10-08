//! System status reporting for rez.
//!
//! Ported from Python rez status module.

use std::env;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde_json::{json, Value};

use crate::config::CONFIG;
use crate::errors::Result;
use crate::platform::{self, SystemInfo, SYSTEM};
use crate::resolve::context::ResolvedContext;
use crate::suite::Suite;
use version::Requirement;

// ---------------------------------------------------------------------------
// Version
// ---------------------------------------------------------------------------

/// Current rez-rs version from Cargo.toml
pub fn rez_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

// ---------------------------------------------------------------------------
// Status struct
// ---------------------------------------------------------------------------

/// Snapshot of the current rez system status.
#[derive(Debug, Clone)]
pub struct Status {
    pub rez_version: String,
    pub rez_path: PathBuf,
    pub python_version: Option<String>,
    pub platform: String,
    pub arch: String,
    pub os_version: String,
    pub shell: String,
    pub user: String,
    pub hostname: String,
    pub config_file: Option<PathBuf>,
    pub packages_path: Vec<crate::rez_path::RezPath>,
    pub package_cache_path: Option<PathBuf>,
    pub implicit_packages: Vec<String>,
    pub plugin_paths: Vec<PathBuf>,
}

impl Status {
    /// Gather current system status.
    pub fn new() -> Self {
        let config = &*CONFIG;

        // Path to current executable
        let rez_path = env::current_exe().unwrap_or_default();

        // Config file from REZ_CONFIG_FILE env var
        let config_file = env::var("REZ_CONFIG_FILE").ok().map(PathBuf::from);

        // Package cache path
        let package_cache_path = config.cache_packages_path.as_ref().map(PathBuf::from);

        // Plugin paths
        let plugin_paths: Vec<PathBuf> = config.plugin_path.iter().map(PathBuf::from).collect();

        Self {
            rez_version: rez_version().to_string(),
            rez_path,
            python_version: None, // rez-rs has no Python dependency
            platform: SYSTEM.platform.name().to_string(),
            arch: SYSTEM.arch.to_string(),
            os_version: SYSTEM.os.to_string(),
            shell: platform::default_shell().to_string(),
            user: SystemInfo::user(),
            hostname: SystemInfo::hostname(),
            config_file,
            packages_path: config.expanded_packages_path(),
            package_cache_path,
            implicit_packages: config.resolved_implicit_packages(),
            plugin_paths,
        }
    }

    /// Format status as human-readable string.
    pub fn format(&self) -> String {
        let mut out = String::with_capacity(1024);

        // Header
        out.push_str(&format!("rez-rs {}\n", self.rez_version));
        out.push('\n');

        // System section
        let sys_items = [
            ("Platform", self.platform.as_str()),
            ("Arch", self.arch.as_str()),
            ("OS", self.os_version.as_str()),
            ("Shell", self.shell.as_str()),
            ("User", self.user.as_str()),
            ("Hostname", self.hostname.as_str()),
        ];
        out.push_str(&format_section("System", &sys_items));

        // Paths section
        out.push('\n');
        out.push_str("Paths:\n");
        out.push_str(&format!("  rez binary:   {}\n", self.rez_path.display()));
        if let Some(ref cf) = self.config_file {
            out.push_str(&format!("  config file:  {}\n", cf.display()));
        } else {
            out.push_str("  config file:  (none)\n");
        }
        if let Some(ref cp) = self.package_cache_path {
            out.push_str(&format!("  pkg cache:    {}\n", cp.display()));
        }

        // Packages path
        out.push('\n');
        out.push_str("Packages path:\n");
        if self.packages_path.is_empty() {
            out.push_str("  (none)\n");
        } else {
            for p in &self.packages_path {
                out.push_str(&format!("  - {}\n", p));
            }
        }

        // Implicit packages
        out.push('\n');
        out.push_str("Implicit packages:\n");
        if self.implicit_packages.is_empty() {
            out.push_str("  (none)\n");
        } else {
            for pkg in &self.implicit_packages {
                out.push_str(&format!("  - {}\n", pkg));
            }
        }

        // Plugin paths
        if !self.plugin_paths.is_empty() {
            out.push('\n');
            out.push_str("Plugin paths:\n");
            for p in &self.plugin_paths {
                out.push_str(&format!("  - {}\n", p.display()));
            }
        }

        out
    }

    /// Print status to stdout.
    pub fn print(&self) {
        print!("{}", self.format());
    }

    /// Convert to JSON value.
    pub fn to_json(&self) -> Value {
        let paths_str: Vec<String> = self
            .packages_path
            .iter()
            .map(|p| p.as_str().to_string())
            .collect();
        let plugin_str: Vec<String> = self
            .plugin_paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();

        json!({
            "rez_version": self.rez_version,
            "rez_path": self.rez_path.to_string_lossy(),
            "python_version": self.python_version,
            "platform": self.platform,
            "arch": self.arch,
            "os_version": self.os_version,
            "shell": self.shell,
            "user": self.user,
            "hostname": self.hostname,
            "config_file": self.config_file.as_ref().map(|p| p.to_string_lossy().into_owned()),
            "packages_path": paths_str,
            "package_cache_path": self.package_cache_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
            "implicit_packages": self.implicit_packages,
            "plugin_paths": plugin_str,
        })
    }
}

impl Default for Status {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Convenience functions
// ---------------------------------------------------------------------------

/// Print full system status to stdout.
pub fn print_status() {
    Status::new().print();
}

/// List available rez tools found in PATH.
pub fn print_tools() {
    let tools = [
        "rez-env",
        "rez-build",
        "rez-release",
        "rez-search",
        "rez-view",
        "rez-status",
        "rez-context",
        "rez-suite",
        "rez-interpret",
        "rez-config",
        "rez-bind",
        "rez-selftest",
        "rez-benchmark",
        "rez-bundle",
        "rez-cp",
        "rez-mv",
        "rez-rm",
        "rez-pkg-cache",
        "rez-memcache",
        "rez-pip",
        "rez-python",
        "rez-yaml2py",
        "rez-diff",
        "rez-gui",
        "rez-help",
        "rez-test",
    ];

    println!("Available rez tools:");
    println!("{:<24} STATUS", "TOOL");
    println!("{:<24} ------", "----");

    for tool in &tools {
        let found = find_in_path(tool);
        let status = if let Some(ref path) = found {
            format!("found ({})", path.display())
        } else {
            "not found".to_string()
        };
        println!("{:<24} {}", tool, status);
    }
}

/// Report whether a file contains a loadable resolved context.
pub fn print_context_info(context_file: &Path) -> Result<()> {
    let _context = ResolvedContext::load(context_file, None)?;
    println!(
        "'{}' is a context. Use 'rez-context' for more information.",
        absolute_path(context_file).display()
    );
    Ok(())
}

/// Check if rez is configured (config file or packages_path set).
pub fn is_configured() -> bool {
    // Config file explicitly set
    if env::var("REZ_CONFIG_FILE").is_ok() {
        return true;
    }
    // Check if any standard config file exists
    let home = env::var("HOME")
        .or_else(|_| env::var("USERPROFILE"))
        .unwrap_or_default();
    if !home.is_empty() {
        let user_config = PathBuf::from(&home).join(".rez").join("rezconfig.toml");
        if user_config.exists() {
            return true;
        }
    }
    // Check if packages_path has non-default entries
    !CONFIG.packages_path.is_empty()
}

/// Get current context file path from REZ_RXT_FILE env var.
pub fn context_file() -> Option<PathBuf> {
    env::var("REZ_RXT_FILE").ok().map(PathBuf::from)
}

/// Check if we are currently inside a resolved context.
pub fn in_context() -> bool {
    context_file().is_some()
}

/// Print Rez-compatible status information for an object or the current environment.
pub fn print_info(object: Option<&str>) -> Result<bool> {
    let paths = CONFIG.expanded_packages_path_os();
    let stdout = std::io::stdout();
    print_info_with_paths(object, &paths, &mut stdout.lock())
}

/// Query status using explicit package paths and an output sink.
pub fn print_info_with_paths<W: Write>(
    object: Option<&str>,
    package_paths: &[PathBuf],
    output: &mut W,
) -> Result<bool> {
    let (context, mut suites) = current_status()?;
    let Some(object) = object.filter(|value| !value.is_empty()) else {
        print_environment_info(output, context.as_ref(), &suites)?;
        return Ok(true);
    };

    let mut matched = false;

    if let Some(tool_path) = find_tool_path(object) {
        let tool_name = tool_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(object);
        if let Some(context) = context.as_ref() {
            let providers: Vec<_> = context
                .get_tools(false)?
                .into_iter()
                .filter(|(_, (_, tools))| tools.iter().any(|tool| tool == tool_name))
                .map(|(package, (variant, _))| (package, variant))
                .collect();
            if !providers.is_empty() {
                writeln!(
                    output,
                    "'{}' {} a tool in the active context:",
                    tool_name,
                    match_word(matched)
                )?;
                writeln!(output, "Tool:     {tool_name}")?;
                if let Some(path) = &context.load_path {
                    writeln!(output, "Context:  {}", path.display())?;
                }
                if providers.len() > 1 {
                    let names = providers
                        .iter()
                        .map(|(_, variant)| variant.as_str())
                        .collect::<Vec<_>>()
                        .join(" ");
                    writeln!(output, "Packages (in conflict): {names}")?;
                } else {
                    writeln!(output, "Package:  {}", providers[0].1)?;
                }
                matched = true;
            }
        }

        let explicit_path =
            Path::new(object).components().count() > 1 || Path::new(object).is_absolute();
        for suite in &mut suites {
            let tools_path = suite.tools_path().unwrap_or_default();
            let expected_path = tools_path.join(tool_name);
            let same_suite_executable = tool_path.parent() == Some(tools_path.as_path())
                && tool_path.file_stem() == expected_path.file_stem();
            if same_suite_executable || !explicit_path {
                if let Some(tool) = suite.get_tools().get(tool_name).cloned() {
                    writeln!(
                        output,
                        "'{}' {} a suite tool:",
                        tool_name,
                        match_word(matched)
                    )?;
                    writeln!(output, "Tool:     {}", tool.tool_name)?;
                    writeln!(output, "Exec:     {}", tool.tool_alias)?;
                    writeln!(
                        output,
                        "Suite:    {}",
                        suite
                            .load_path
                            .as_deref()
                            .unwrap_or(Path::new("."))
                            .display()
                    )?;
                    writeln!(output, "Context:  {}", tool.context_name)?;
                    matched = true;
                    break;
                }
            }
        }
    }

    if Path::new(object).file_name().and_then(|name| name.to_str()) == Some(object) {
        if let Ok(request) = Requirement::from_str(object) {
            if !request.conflict() {
                let name = request.name();
                let range = request.range();
                let package_in_context = context.as_ref().and_then(|context| {
                    context
                        .get_resolved_package(name)
                        .filter(|package| {
                            range.is_none_or(|range| range.contains_version(&package.version))
                        })
                        .map(|package| (context, package))
                });
                if let Some((context, package)) = package_in_context {
                    writeln!(
                        output,
                        "'{}' {} a package in the active context:",
                        name,
                        match_word(matched)
                    )?;
                    writeln!(output, "Package:  {}", package.qualified_name())?;
                    if let Some(path) = &package.repo_path {
                        writeln!(output, "Path:     {}", path.display())?;
                    }
                    if let Some(path) = &context.load_path {
                        writeln!(output, "Context:  {}", path.display())?;
                    }
                    matched = true;
                } else {
                    let mut packages =
                        crate::package::discover::iter_packages(name, range, Some(package_paths))?;
                    packages.sort_by(|left, right| left.version.cmp(&right.version));
                    if let Some(package) = packages.last() {
                        let mut heading = format!(
                            "'{}' {} a package. The latest version",
                            name,
                            match_word(matched)
                        );
                        if let Some(range) = range.filter(|range| !range.is_any()) {
                            heading.push_str(&format!(" in the range '{range}'"));
                        }
                        writeln!(output, "{heading} is:")?;
                        writeln!(output, "Package:  {}-{}", package.name, package.version)?;
                        if let Some(path) = package.data.get("base").and_then(Value::as_str) {
                            writeln!(output, "Path:     {path}")?;
                        }
                        matched = true;
                    }
                }
            }
        }
    }

    let object_path = absolute_path(Path::new(object));
    if object_path.is_dir() && Suite::load(&object_path).is_ok() {
        writeln!(
            output,
            "'{}' {} a suite. Use 'rez-suite' for more information.",
            object_path.display(),
            match_word(matched)
        )?;
        matched = true;
    }

    for suite in &suites {
        if suite.has_context(object) {
            let suite_path = suite.load_path.as_deref().unwrap_or(Path::new("."));
            writeln!(
                output,
                "'{}' {} a context in suite '{}'. Use 'rez-suite' for more information.",
                object,
                match_word(matched),
                suite_path.display()
            )?;
            matched = true;
        }
    }

    if object_path.is_file() && ResolvedContext::load(&object_path, None).is_ok() {
        writeln!(
            output,
            "'{}' {} a context. Use 'rez-context' for more information.",
            object_path.display(),
            match_word(matched)
        )?;
        matched = true;
    }

    if !matched {
        writeln!(output, "Rez does not know what '{object}' is")?;
    }
    Ok(matched)
}

/// Print currently available tools from the active context and visible suites.
pub fn print_visible_tools(pattern: Option<&str>) -> Result<bool> {
    let stdout = std::io::stdout();
    print_visible_tools_to(pattern, &mut stdout.lock())
}

fn print_visible_tools_to<W: Write>(pattern: Option<&str>, output: &mut W) -> Result<bool> {
    let (context, suites) = current_status()?;
    print_visible_tools_with_status(pattern, context.as_ref(), suites, output)
}

fn print_visible_tools_with_status<W: Write>(
    pattern: Option<&str>,
    context: Option<&ResolvedContext>,
    suites: Vec<Suite>,
    output: &mut W,
) -> Result<bool> {
    let pattern = pattern
        .filter(|pattern| !pattern.is_empty())
        .map(|pattern| foundation::patterns::fnmatch_regex(pattern, cfg!(windows)))
        .transpose()
        .map_err(|error| {
            crate::errors::RezError::Parse(format!("invalid tools pattern: {error}"))
        })?;
    let mut rows: Vec<(String, String, String, String, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    if let Some(context) = context.as_ref() {
        let (tools, conflicts) = context.get_tools_with_conflicts(false)?;
        for (_, (variant, tools)) in tools {
            for tool in tools {
                if pattern
                    .as_ref()
                    .is_some_and(|pattern| !pattern.is_match(&tool))
                    || !seen.insert(tool.clone())
                {
                    continue;
                }
                let label = if conflicts.contains_key(&tool) {
                    "(in conflict)"
                } else {
                    ""
                };
                rows.push((
                    tool,
                    "-".to_string(),
                    variant.clone(),
                    "active context".to_string(),
                    label.to_string(),
                ));
            }
        }
    }

    for mut suite in suites {
        let tools_path = suite.tools_path().unwrap_or_default();
        let suite_path = suite.load_path.clone().unwrap_or_default();
        for (alias, tool) in suite.get_tools() {
            if !seen.insert(alias.clone())
                || pattern
                    .as_ref()
                    .is_some_and(|pattern| !pattern.is_match(alias))
            {
                continue;
            }
            let label = match find_in_path(alias) {
                Some(path) if path != tools_path.join(alias) => {
                    format!("(hidden by unknown tool '{}')", path.display())
                }
                _ => String::new(),
            };
            rows.push((
                alias.clone(),
                if tool.tool_name == *alias {
                    "-".to_string()
                } else {
                    tool.tool_name.clone()
                },
                tool.package.clone().unwrap_or_default(),
                format!(
                    "context '{}' in suite '{}'",
                    tool.context_name,
                    suite_path.display()
                ),
                label,
            ));
        }
    }

    if rows.is_empty() {
        writeln!(output, "No matching tools.")?;
        return Ok(false);
    }
    rows.sort_by_key(|row| row.0.to_lowercase());
    writeln!(
        output,
        "{:<24} {:<12} {:<28} {:<40} STATUS",
        "TOOL", "ALIASING", "PACKAGE", "SOURCE"
    )?;
    writeln!(
        output,
        "{:<24} {:<12} {:<28} {:<40} ------",
        "----", "--------", "-------", "------"
    )?;
    for (tool, alias, package, source, label) in rows {
        writeln!(
            output,
            "{tool:<24} {alias:<12} {package:<28} {source:<40} {label}"
        )?;
    }
    Ok(true)
}

fn print_environment_info<W: Write>(
    output: &mut W,
    context: Option<&ResolvedContext>,
    suites: &[Suite],
) -> Result<()> {
    writeln!(output, "Using rez-rs v{}", rez_version())?;
    if let Some(context) = &context {
        if let Some(path) = &context.load_path {
            writeln!(output, "\nActive Context: {}", path.display())?;
        } else {
            writeln!(output, "\nIn Active Context.")?;
        }
    } else {
        writeln!(output, "\nNo active context.")?;
    }
    if suites.is_empty() {
        writeln!(output, "\nNo visible suites.")?;
    } else {
        writeln!(output, "\n{} visible suites:", suites.len())?;
        for suite in suites {
            if let Some(path) = &suite.load_path {
                writeln!(output, "{}", path.display())?;
            }
        }
    }
    if let Some(context) = &context {
        if let (Some(suite_path), Some(context_name)) =
            (&context.parent_suite_path, &context.suite_context_name)
        {
            writeln!(
                output,
                "\nCurrently within context '{context_name}' in suite at {suite_path}"
            )?;
        }
    }
    Ok(())
}

fn current_status() -> Result<(Option<ResolvedContext>, Vec<Suite>)> {
    let context_path = context_file();
    let suite_paths = Suite::visible_suite_paths(None);
    load_status_environment(context_path.as_deref(), &suite_paths)
}

fn load_status_environment(
    context_path: Option<&Path>,
    suite_paths: &[PathBuf],
) -> Result<(Option<ResolvedContext>, Vec<Suite>)> {
    let context = context_path
        .map(|path| ResolvedContext::load(path, None))
        .transpose()?;
    let suites = suite_paths
        .iter()
        .map(|path| Suite::load(path))
        .collect::<Result<Vec<_>>>()?;
    Ok((context, suites))
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir().unwrap_or_default().join(path)
    }
}

fn find_tool_path(value: &str) -> Option<PathBuf> {
    let path = Path::new(value);
    let candidate = if path.components().count() > 1 || path.is_absolute() {
        Some(absolute_path(path))
    } else {
        find_in_path(value)
    }?;
    candidate.is_file().then_some(candidate)
}

fn match_word(previous_match: bool) -> &'static str {
    if previous_match {
        "is also"
    } else {
        "is"
    }
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

/// Format a titled section with key-value pairs.
fn format_section(title: &str, items: &[(&str, &str)]) -> String {
    let mut out = String::new();
    out.push_str(title);
    out.push_str(":\n");
    // Find max key length for alignment
    let max_key = items.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    for (key, val) in items {
        out.push_str(&format!("  {:<width$}  {}\n", key, val, width = max_key));
    }
    out
}

/// Format a list of paths as indented bullet list.
pub fn format_paths(paths: &[PathBuf]) -> String {
    let mut out = String::new();
    for p in paths {
        out.push_str(&format!("  - {}\n", p.display()));
    }
    if out.is_empty() {
        out.push_str("  (none)\n");
    }
    out
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Search for an executable in PATH.
fn find_in_path(name: &str) -> Option<PathBuf> {
    let path_var = env::var("PATH").unwrap_or_default();
    let sep = if cfg!(windows) { ';' } else { ':' };
    let extensions: Vec<&str> = if cfg!(windows) {
        vec![".exe", ".cmd", ".bat", ""]
    } else {
        vec![""]
    };

    for dir in path_var.split(sep) {
        let dir_path = Path::new(dir);
        for ext in &extensions {
            let candidate = dir_path.join(format!("{}{}", name, ext));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rez_version() {
        let ver = rez_version();
        assert!(!ver.is_empty());
        // Should be semver-like
        assert!(ver.contains('.'), "version should contain dots: {}", ver);
    }

    #[test]
    fn test_status_new() {
        let status = Status::new();
        assert_eq!(status.rez_version, rez_version());
        assert!(!status.platform.is_empty());
        assert!(!status.arch.is_empty());
        assert!(!status.os_version.is_empty());
        assert!(!status.shell.is_empty());
        assert!(!status.user.is_empty());
        assert!(!status.hostname.is_empty());
        // rez-rs has no Python
        assert!(status.python_version.is_none());
    }

    #[test]
    fn test_status_format() {
        let status = Status::new();
        let text = status.format();
        assert!(text.contains("rez-rs"));
        assert!(text.contains("System:"));
        assert!(text.contains("Platform"));
        assert!(text.contains("Packages path:"));
        assert!(text.contains("Implicit packages:"));
    }

    #[test]
    fn test_status_json() {
        let status = Status::new();
        let json = status.to_json();
        assert!(json.is_object());
        assert_eq!(json["rez_version"], rez_version());
        assert!(json["python_version"].is_null());
        assert!(json["platform"].is_string());
        assert!(json["packages_path"].is_array());
        assert!(json["implicit_packages"].is_array());
    }

    #[test]
    fn test_format_section() {
        let items = [("Key1", "Value1"), ("LongerKey", "Value2")];
        let out = format_section("Test", &items);
        assert!(out.starts_with("Test:\n"));
        assert!(out.contains("Key1"));
        assert!(out.contains("LongerKey"));
        assert!(out.contains("Value1"));
        assert!(out.contains("Value2"));
    }

    #[test]
    fn test_format_paths() {
        let paths = vec![PathBuf::from("/a/b"), PathBuf::from("/c/d")];
        let out = format_paths(&paths);
        assert!(out.contains("/a/b"));
        assert!(out.contains("/c/d"));
        assert!(out.starts_with("  - "));
    }

    #[test]
    fn test_format_paths_empty() {
        let out = format_paths(&[]);
        assert_eq!(out, "  (none)\n");
    }

    #[test]
    fn test_is_configured() {
        // With default config, packages_path is non-empty => configured
        assert!(is_configured());
    }

    #[test]
    fn test_context_file() {
        // Unless REZ_RXT_FILE is set, should be None
        if env::var("REZ_RXT_FILE").is_err() {
            assert!(context_file().is_none());
            assert!(!in_context());
        }
    }

    #[test]
    fn test_find_in_path_nonexistent() {
        assert!(find_in_path("rez_nonexistent_tool_12345").is_none());
    }

    #[test]
    fn test_print_context_info_missing() {
        let result = print_context_info(Path::new("/nonexistent/context.rxt"));
        assert!(result.is_err());
    }

    #[test]
    fn test_status_default() {
        // Default trait delegates to new()
        let status = Status::default();
        assert_eq!(status.rez_version, rez_version());
    }

    #[test]
    fn test_package_request_hit_and_miss() {
        let repository = tempfile::tempdir().expect("temporary repository");
        let package_dir = repository.path().join("status_fixture").join("1.2.3");
        std::fs::create_dir_all(&package_dir).expect("create package directory");
        std::fs::write(
            package_dir.join("package.yaml"),
            "name: status_fixture\nversion: 1.2.3\n",
        )
        .expect("write package definition");
        let paths = vec![repository.path().to_path_buf()];

        let mut output = Vec::new();
        assert!(
            print_info_with_paths(Some("status_fixture-1.2+"), &paths, &mut output)
                .expect("query package")
        );
        let output = String::from_utf8(output).expect("UTF-8 output");
        assert!(output.contains("status_fixture-1.2.3"));

        let mut output = Vec::new();
        assert!(
            !print_info_with_paths(Some("missing_status_fixture"), &paths, &mut output)
                .expect("query missing package")
        );
        assert!(String::from_utf8(output)
            .expect("UTF-8 output")
            .contains("Rez does not know what 'missing_status_fixture' is"));
    }

    #[test]
    fn test_context_object_requires_valid_context_data() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let missing_context = directory.path().join("missing.rxt");
        let invalid_context = directory.path().join("invalid.rxt");
        std::fs::write(&invalid_context, "not a context").expect("write invalid context");
        let paths = [];

        for path in [&missing_context, &invalid_context] {
            let mut output = Vec::new();
            assert!(
                !print_info_with_paths(Some(path.to_str().unwrap()), &paths, &mut output)
                    .expect("query invalid context")
            );
        }

        let valid_context = directory.path().join("valid.rxt");
        ResolvedContext::empty()
            .save(&valid_context)
            .expect("save context");
        let mut output = Vec::new();
        assert!(
            print_info_with_paths(Some(valid_context.to_str().unwrap()), &paths, &mut output)
                .expect("query valid context")
        );
        assert!(String::from_utf8(output)
            .expect("UTF-8 output")
            .contains("is a context"));
    }

    #[test]
    fn test_suite_object_and_glob_filtered_tools() {
        let directory = tempfile::tempdir().expect("temporary directory");
        std::fs::write(directory.path().join("suite.yaml"), "contexts: {}\n")
            .expect("write suite metadata");
        let paths = [];
        let mut output = Vec::new();
        assert!(print_info_with_paths(
            Some(directory.path().to_str().unwrap()),
            &paths,
            &mut output
        )
        .expect("query suite"));
        assert!(String::from_utf8(output)
            .expect("UTF-8 output")
            .contains("is a suite"));

        let character_class = foundation::patterns::fnmatch_regex("maya[0-9]", false)
            .expect("compile character class");
        assert!(character_class.is_match("maya7"));
        assert!(!character_class.is_match("mayaX"));

        let invalid_range = foundation::patterns::fnmatch_regex("[z-a]", false)
            .expect("compile invalid range as a no-match pattern");
        assert!(!invalid_range.is_match("z"));
        assert!(!invalid_range.is_match("a"));

        let empty_range = foundation::patterns::fnmatch_regex("[a--]", false)
            .expect("compile empty range as a no-match pattern");
        assert!(!empty_range.is_match("a"));
        assert!(!empty_range.is_match("-"));

        let normalized_range = foundation::patterns::fnmatch_regex("[a--b]", false)
            .expect("compile overlapping range");
        assert!(!normalized_range.is_match("a"));
        assert!(normalized_range.is_match("b"));

        let initial_closing_bracket = foundation::patterns::fnmatch_regex("[]]", false)
            .expect("compile class with initial closing bracket");
        assert!(initial_closing_bracket.is_match("]"));

        let negated_class = foundation::patterns::fnmatch_regex("maya[!a]", false)
            .expect("compile negated character class");
        assert!(negated_class.is_match("maya7"));
        assert!(!negated_class.is_match("mayaa"));

        let unmatched_bracket =
            foundation::patterns::fnmatch_regex("maya[", false).expect("compile unmatched bracket");
        assert!(unmatched_bracket.is_match("maya["));

        let unicode =
            foundation::patterns::fnmatch_regex("é?", false).expect("compile Unicode pattern");
        assert!(unicode.is_match("é🙂"));

        let newline =
            foundation::patterns::fnmatch_regex("a?b", false).expect("compile newline pattern");
        assert!(newline.is_match("a\nb"));

        let backslash = foundation::patterns::fnmatch_regex(r"foo\*", false)
            .expect("compile backslash pattern");
        assert!(backslash.is_match(r"foo\bar"));

        let escaped_regex_meta = foundation::patterns::fnmatch_regex("a.b", false)
            .expect("compile escaped regex metacharacter");
        assert!(escaped_regex_meta.is_match("a.b"));
        assert!(!escaped_regex_meta.is_match("axb"));

        let platform_case = foundation::patterns::fnmatch_regex("a", cfg!(windows))
            .expect("compile platform-specific pattern");
        assert_eq!(platform_case.is_match("A"), cfg!(windows));
    }
    #[test]
    fn test_tools_filter_reports_no_match_without_environment_state() {
        let mut output = Vec::new();
        assert!(!print_visible_tools_with_status(
            Some("rez_status_tool_that_does_not_exist_93821"),
            None,
            Vec::new(),
            &mut output
        )
        .expect("query unmatched tool"));
        assert!(String::from_utf8(output)
            .expect("UTF-8 output")
            .contains("No matching tools."));
    }

    #[test]
    fn test_status_environment_propagates_context_and_suite_errors() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let missing_context = directory.path().join("missing.rxt");
        let invalid_context = directory.path().join("invalid.rxt");
        std::fs::write(&invalid_context, "not a context").expect("write invalid context");
        let no_suites = [];

        assert!(load_status_environment(Some(&missing_context), &no_suites).is_err());
        assert!(load_status_environment(Some(&invalid_context), &no_suites).is_err());

        let valid_context = directory.path().join("valid.rxt");
        ResolvedContext::empty()
            .save(&valid_context)
            .expect("save context");
        assert!(load_status_environment(Some(&valid_context), &no_suites).is_ok());

        let invalid_suite = directory.path().join("invalid_suite");
        std::fs::create_dir_all(&invalid_suite).expect("create invalid suite");
        std::fs::write(invalid_suite.join("suite.yaml"), "contexts: [")
            .expect("write invalid suite");
        assert!(load_status_environment(None, &[invalid_suite]).is_err());
    }
}
