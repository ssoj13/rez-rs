// SPDX-License-Identifier: Apache-2.0

//! `rez search` — search for packages by name pattern.
//!
//! Uses PackageSearcher, CONFIG.expanded_packages_path_os (or --no-local filtered paths). Optional validation via validate_package_data.

use clap::Args;
use foundation::errors::{Result, RezError};
use model::config::CONFIG;
use model::serialise::validate_package_data;
use repository::package::discover::get_package;
use repository::package::search::{
    get_reverse_dependencies, PackageSearcher, ResourceSearchResult,
};
use repository::PackageInfo;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Search for packages.
#[derive(Args, Debug)]
pub struct SearchArgs {
    /// Package search query (glob patterns supported).
    #[arg()]
    pub query: Option<String>,

    /// Type of resource to search for (auto/package/family/variant).
    #[arg(short = 't', long, default_value = "auto")]
    pub search_type: String,

    /// Only show the latest version of each package.
    #[arg(short, long)]
    pub latest: bool,

    /// Only show package family names (no versions).
    #[arg(long)]
    pub no_versions: bool,

    /// Show reverse dependencies of a package.
    #[arg(long, value_name = "PKG")]
    pub reverse: Option<String>,

    /// Package search paths (separated by OS path separator).
    #[arg(long, value_name = "PATHS")]
    pub paths: Option<String>,

    /// Validate packages during search.
    #[arg(long)]
    pub validate: bool,

    /// Custom format string (e.g., "{qualified_name} | {description}").
    #[arg(short = 'f', long)]
    pub format: Option<String>,

    /// Only show packages released before the given time.
    #[arg(long)]
    pub before: Option<String>,

    /// Only show packages released after the given time.
    #[arg(long)]
    pub after: Option<String>,

    /// Print newlines as '\\n' rather than actual newlines.
    #[arg(long)]
    pub no_newlines: bool,

    /// Only print packages containing errors (implies --validate).
    #[arg(short = 'e', long)]
    pub errors: bool,

    /// Output format (plain or json).
    #[arg(short = 'o', long, value_enum, default_value_t = OutputFormat::Plain)]
    pub output_format: OutputFormat,

    /// Do not search local packages.
    #[arg(long)]
    pub no_local: bool,
}

#[derive(clap::ValueEnum, Clone, Debug)]
pub enum OutputFormat {
    Plain,
    Json,
}

pub fn run(args: &SearchArgs) -> Result<()> {
    // Parse search paths
    let paths = parse_paths(args.paths.as_deref(), args.no_local);

    // Reverse dependency mode
    if let Some(ref pkg_name) = args.reverse {
        return run_reverse(pkg_name, paths.as_deref());
    }

    // Parse both bounds against one clock reading so the interval is stable.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let before_time = args
        .before
        .as_deref()
        .map(|value| parse_time_filter(value, now))
        .transpose()?
        .filter(|value| *value != 0.0);
    let after_time = args
        .after
        .as_deref()
        .map(|value| parse_time_filter(value, now))
        .transpose()?
        .filter(|value| *value != 0.0);

    if let (Some(after), Some(before)) = (after_time, before_time) {
        if after >= before {
            return Err(RezError::PackageRequest(
                "non-overlapping --before and --after".into(),
            ));
        }
    }

    let query = args.query.as_deref().unwrap_or("*");
    let searcher = PackageSearcher::new(paths.clone());

    // Effective paths for loading package metadata (timestamp filter, validation)
    let config_paths = CONFIG.expanded_packages_path_os();
    let effective_paths: &[std::path::PathBuf] =
        paths.as_deref().unwrap_or(config_paths.as_slice());

    // Handle search type (auto/family/package/variant)
    let force_families = args.search_type == "family" || args.no_versions;
    // Note: 'package' and 'variant' types use standard package search

    if force_families {
        // Family names only
        let families = searcher.search_families(query)?;
        if families.is_empty() {
            return Err(RezError::PackageNotFound(
                "No matching packages found".into(),
            ));
        }
        match args.output_format {
            OutputFormat::Plain => {
                for name in &families {
                    let output = if let Some(ref fmt) = args.format {
                        format_package(fmt, name, "", "")
                    } else {
                        name.clone()
                    };
                    println!("{}", escape_newlines(&output, args.no_newlines));
                }
            }
            OutputFormat::Json => {
                println!("{}", serde_json::to_string_pretty(&families)?);
            }
        }
        return Ok(());
    }

    // Full package search
    let results = searcher.search_packages(query, args.latest)?;

    let needs_metadata =
        args.validate || args.errors || before_time.is_some() || after_time.is_some();
    let mut results = load_search_packages(results, effective_paths, needs_metadata)?;

    // Apply validation if requested. The metadata lookup is shared with time filtering.
    let validation_errors = if args.validate || args.errors {
        validate_packages(&results)?
    } else {
        std::collections::HashSet::new()
    };

    // Filter by errors only
    if args.errors {
        if validation_errors.is_empty() {
            return Err(RezError::PackageNotFound(
                "No matching erroneous packages found".into(),
            ));
        }
        results.retain(|package| validation_errors.contains(&package.result.qualified_name()));
    }

    if results.is_empty() {
        return Err(RezError::PackageNotFound(
            "No matching packages found".into(),
        ));
    }

    // Apply time filters
    if before_time.is_some() || after_time.is_some() {
        results = filter_by_time(results, before_time, after_time)?;
        if results.is_empty() {
            return Err(RezError::PackageNotFound(
                "No packages match time filters".into(),
            ));
        }
    }

    let results: Vec<ResourceSearchResult> =
        results.into_iter().map(|package| package.result).collect();

    print_results(
        &results,
        &args.output_format,
        args.format.as_deref(),
        args.no_newlines,
    )
}

/// Print search results in the chosen format.
fn print_results(
    results: &[ResourceSearchResult],
    output_format: &OutputFormat,
    custom_format: Option<&str>,
    no_newlines: bool,
) -> Result<()> {
    match output_format {
        OutputFormat::Plain => {
            for r in results {
                let output = if let Some(fmt) = custom_format {
                    let version_str = r
                        .version
                        .as_ref()
                        .map(|v| v.to_string())
                        .unwrap_or_default();
                    format_package(fmt, &r.package_name, &version_str, "")
                } else {
                    r.qualified_name()
                };
                println!("{}", escape_newlines(&output, no_newlines));
            }
        }
        OutputFormat::Json => {
            let items: Vec<serde_json::Value> = results
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "name": r.package_name,
                        "version": r.version.as_ref().map(|v| v.to_string()),
                        "match": format!("{:?}", r.match_type),
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&items)?);
        }
    }
    Ok(())
}

/// Show reverse dependencies.
fn run_reverse(name: &str, paths: Option<&[PathBuf]>) -> Result<()> {
    let deps = get_reverse_dependencies(name, None, None, paths)?;
    if deps.is_empty() {
        println!("No reverse dependencies found for '{}'.", name);
        return Ok(());
    }

    let mut names: Vec<&String> = deps.keys().collect();
    names.sort();
    for dep_name in names {
        let reqs = &deps[dep_name];
        let req_strs: Vec<String> = reqs.iter().map(|r| r.to_string()).collect();
        println!("{}: {}", dep_name, req_strs.join(", "));
    }
    Ok(())
}

/// Parse --paths option and apply --no-local filtering.
fn parse_paths(paths_str: Option<&str>, no_local: bool) -> Option<Vec<PathBuf>> {
    if let Some(s) = paths_str {
        let paths: Vec<PathBuf> = std::env::split_paths(s)
            .filter(|p| !p.as_os_str().is_empty())
            .collect();
        if paths.is_empty() {
            None
        } else {
            Some(paths)
        }
    } else if no_local {
        // Filter out local packages path from config
        let all = CONFIG.expanded_packages_path();
        let local = CONFIG.expanded_local_packages_path();
        let filtered: Vec<PathBuf> = all
            .into_iter()
            .filter(|p| *p != local)
            .map(|p| p.to_os())
            .collect();
        Some(filtered)
    } else {
        None // use config default
    }
}

/// Format package info using template string.
fn format_package(fmt: &str, name: &str, version: &str, desc: &str) -> String {
    let qualified = if version.is_empty() {
        name.to_string()
    } else {
        format!("{}-{}", name, version)
    };
    fmt.replace("{name}", name)
        .replace("{version}", version)
        .replace("{qualified_name}", &qualified)
        .replace("{description}", desc)
}

/// Escape newlines if --no-newlines flag is set.
fn escape_newlines(text: &str, should_escape: bool) -> String {
    if should_escape {
        text.replace('\n', "\\n")
    } else {
        text.to_string()
    }
}

/// One package search result and its optional, single-load repository metadata.
struct SearchPackage {
    result: ResourceSearchResult,
    info: Option<PackageInfo>,
}

/// Load metadata once for the validation and timestamp filters that share it.
fn load_search_packages(
    results: Vec<ResourceSearchResult>,
    paths: &[PathBuf],
    load_metadata: bool,
) -> Result<Vec<SearchPackage>> {
    results
        .into_iter()
        .map(|result| {
            let info = if load_metadata {
                if let Some(version) = &result.version {
                    get_package(&result.package_name, version, Some(paths))?
                } else {
                    None
                }
            } else {
                None
            };

            Ok(SearchPackage { result, info })
        })
        .collect()
}

/// Parse an epoch time or relative duration. Both negative Rez syntax (-5m)
/// and the existing positive form (5m) mean five minutes before now.
fn parse_time_filter(time_str: &str, now: f64) -> Result<f64> {
    let s = time_str.trim();

    // Rez treats zero as an unset bound.
    if s == "0" {
        return Ok(0.0);
    }

    // Preserve the existing numeric epoch syntax, including fractional epochs.
    if let Ok(epoch) = s.parse::<f64>() {
        if epoch.is_finite() {
            return Ok(epoch);
        }
    }

    let invalid = || {
        RezError::PackageRequest(format!(
            "invalid time filter {time_str:?}; expected epoch seconds or a relative duration such as -5m (units: s/m/h/d, with w supported as an extension)"
        ))
    };

    let Some(unit) = s.chars().last() else {
        return Err(invalid());
    };
    let number_end = s.len() - unit.len_utf8();
    let number = s[..number_end].parse::<f64>().map_err(|_| invalid())?;
    if !number.is_finite() || !now.is_finite() {
        return Err(invalid());
    }

    let multiplier = match unit {
        's' => 1.0,
        'm' => 60.0,
        'h' => 3600.0,
        'd' => 86400.0,
        'w' => 604800.0,
        _ => return Err(invalid()),
    };
    let seconds = number.abs() * multiplier;
    if !seconds.is_finite() {
        return Err(invalid());
    }

    Ok((now - seconds).max(0.0))
}

/// Filter by optional timestamp bounds. Rez's upper bound is exclusive; an
/// absent/zero package timestamp is not filtered.
fn filter_by_time(
    results: Vec<SearchPackage>,
    before: Option<f64>,
    after: Option<f64>,
) -> Result<Vec<SearchPackage>> {
    results
        .into_iter()
        .filter_map(|package| {
            package.result.version.as_ref()?;
            let info = package.info.as_ref()?;
            let timestamp = info.get("timestamp").map(|value| {
                value
                    .as_i64()
                    .map(|value| value as f64)
                    .or_else(|| value.as_u64().map(|value| value as f64))
                    .ok_or_else(|| RezError::PackageMetadata {
                        msg: format!(
                            "timestamp for package {} must be an integer",
                            package.result.qualified_name()
                        ),
                        path: None,
                        resource_key: Some("timestamp".into()),
                    })
            });

            let timestamp = match timestamp {
                Some(Ok(timestamp)) if timestamp != 0.0 => timestamp,
                Some(Ok(_)) | None => return Some(Ok(package)),
                Some(Err(error)) => return Some(Err(error)),
            };
            let matches = before.is_none_or(|bound| timestamp < bound)
                && after.is_none_or(|bound| timestamp >= bound);
            matches.then_some(Ok(package))
        })
        .collect()
}

/// Validate loaded package data and return qualified names with model errors.
fn validate_packages(results: &[SearchPackage]) -> Result<std::collections::HashSet<String>> {
    let mut with_errors = std::collections::HashSet::new();
    for package in results {
        if let Some(info) = &package.info {
            if validate_package_data(&info.data).is_err()
                || model::package::Package::from_data(info.data.clone()).is_err()
            {
                with_errors.insert(package.result.qualified_name());
            }
        }
    }
    Ok(with_errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use repository::package::search::MatchType;
    use serde_json::{json, Value};
    use std::collections::HashMap;

    fn search_result() -> ResourceSearchResult {
        ResourceSearchResult {
            package_name: "foo".into(),
            version: Some("1.0.0".parse().unwrap()),
            match_type: MatchType::Exact,
        }
    }

    fn package_info(data: Value) -> PackageInfo {
        let data: HashMap<String, Value> = serde_json::from_value(data).unwrap();
        PackageInfo::from_data(data).unwrap()
    }

    #[test]
    fn time_filter_accepts_absolute_epoch() {
        assert_eq!(
            parse_time_filter("1700000000", 2_000_000_000.0).unwrap(),
            1_700_000_000.0
        );
        assert_eq!(
            parse_time_filter("1700000000.5", 2_000_000_000.0).unwrap(),
            1_700_000_000.5
        );
    }

    #[test]
    fn time_filter_relative_durations_are_before_now_and_clamped() {
        let now = 10_000.0;
        assert_eq!(parse_time_filter("-5m", now).unwrap(), 9_700.0);
        assert_eq!(parse_time_filter("5m", now).unwrap(), 9_700.0);
        assert_eq!(parse_time_filter("2h", now).unwrap(), 2_800.0);
        assert_eq!(parse_time_filter("1w", now).unwrap(), 0.0);
        assert_eq!(parse_time_filter("-999999d", now).unwrap(), 0.0);
        assert_eq!(parse_time_filter("0", now).unwrap(), 0.0);
    }

    #[test]
    fn time_filter_rejects_invalid_numbers_units_and_non_finite_values() {
        for input in ["not-a-numberh", "5q", "NaNs", "inf", "1e999"] {
            let error = parse_time_filter(input, 10_000.0).unwrap_err();
            assert!(
                error.to_string().contains(input),
                "error for {input:?} should include the supplied value: {error}"
            );
        }
    }

    #[test]
    fn missing_or_zero_package_timestamp_is_not_filtered() {
        for data in [
            json!({"name": "foo", "version": "1.0.0"}),
            json!({"name": "foo", "version": "1.0.0", "timestamp": 0}),
        ] {
            let packages = vec![SearchPackage {
                result: search_result(),
                info: Some(package_info(data)),
            }];
            let filtered = filter_by_time(packages, Some(200.0), Some(100.0)).unwrap();
            assert_eq!(filtered.len(), 1);
        }
    }

    #[test]
    fn timestamp_filter_uses_inclusive_after_and_exclusive_before_bounds() {
        let package = SearchPackage {
            result: search_result(),
            info: Some(package_info(json!({
                "name": "foo",
                "version": "1.0.0",
                "timestamp": 100
            }))),
        };
        assert_eq!(
            filter_by_time(vec![package], Some(101.0), Some(100.0))
                .unwrap()
                .len(),
            1
        );

        let package = SearchPackage {
            result: search_result(),
            info: Some(package_info(json!({
                "name": "foo",
                "version": "1.0.0",
                "timestamp": 100
            }))),
        };
        assert!(filter_by_time(vec![package], Some(100.0), None)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn malformed_package_timestamp_is_an_error() {
        let package = SearchPackage {
            result: search_result(),
            info: Some(package_info(json!({
                "name": "foo",
                "version": "1.0.0",
                "timestamp": "yesterday"
            }))),
        };
        let error = filter_by_time(vec![package], Some(200.0), None)
            .err()
            .expect("malformed timestamp must fail");
        assert!(error.to_string().contains("foo-1.0.0"));
        assert!(error.to_string().contains("timestamp"));
    }

    #[test]
    fn package_provider_errors_propagate_from_shared_metadata_load() {
        let repository_file = tempfile::NamedTempFile::new().unwrap();
        let paths = vec![repository_file.path().to_path_buf()];
        let error = load_search_packages(vec![search_result()], &paths, true)
            .err()
            .expect("invalid repository must fail");
        assert!(
            error.to_string().contains("repository"),
            "provider error should retain repository context: {error}"
        );
    }
}
