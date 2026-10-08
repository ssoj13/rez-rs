// SPDX-License-Identifier: Apache-2.0

//! `rez env` — resolve packages and spawn interactive shell.
//!
//! Parses requests → ResolvedContext::resolve → execute_shell. Uses `config_package_paths` (CONFIG.expanded_packages_path_os) or explicit --path.

use std::path::{Path, PathBuf};

use clap::Args;

use foundation::constants::ResolverStatus;
use foundation::errors::{Result, RezError};
use foundation::log_info;
use model::config::CONFIG;
use model::package::filter::{PackageFilter, PackageFilterList, Rule};
use repository::{FilesystemPackageProvider, PackageProvider};
use resolve::context::{ResolveOptions, ResolvedContext};
use rex::types::{detect_shell, ShellType};
use version::Requirement;

use crate::cli::util::{expand_tilde, view_graph};

// =============================================================================
// Args
// =============================================================================

#[derive(Args, Debug)]
pub struct EnvArgs {
    /// Packages to use in the target environment (e.g. "python-3.7+" "maya-2024")
    #[arg(value_name = "PKG")]
    pub packages: Vec<String>,

    /// Target shell type (bash, sh, csh, tcsh, cmd, powershell, gitbash)
    #[arg(long, short = 'S')]
    pub shell: Option<String>,

    /// Source this file instead of the shell's standard startup scripts
    #[arg(long)]
    pub rcfile: Option<String>,

    /// Skip loading of startup scripts
    #[arg(long)]
    pub norc: bool,

    /// Execute command within rez environment then exit
    #[arg(short = 'c', long)]
    pub command: Option<String>,

    /// Read commands from stdin
    #[arg(short = 's', long)]
    pub stdin: bool,

    /// Don't add implicit packages to the request
    #[arg(long = "ni", visible_alias = "no-implicit")]
    pub no_implicit: bool,

    /// Don't load local packages
    #[arg(long = "nl", visible_alias = "no-local")]
    pub no_local: bool,

    /// Create a build environment
    #[arg(short = 'b', long)]
    pub build: bool,

    /// Set package search paths (use platform path separator)
    #[arg(long)]
    pub paths: Option<String>,

    /// Ignore packages released after given time (epoch or relative e.g. -10s, -5m)
    #[arg(short = 't', long)]
    pub time: Option<String>,

    /// Abort if number of failed attempts exceeds N
    #[arg(long, default_value_t = -1)]
    pub max_fails: i32,

    /// Abort if resolve time exceeds SECS
    #[arg(long, default_value_t = -1)]
    pub time_limit: i32,

    /// Save context to .rxt file instead of spawning shell ("-" for stdout)
    #[arg(short = 'o', long, value_name = "FILE")]
    pub output: Option<String>,

    /// Load context from .rxt file instead of resolving
    #[arg(short = 'i', long, value_name = "FILE")]
    pub input: Option<String>,

    /// Add package exclusion filters (e.g. "*.beta")
    #[arg(long, num_args = 1..)]
    pub exclude: Vec<String>,

    /// Add package inclusion filters (e.g. "mypkg", "boost-*")
    #[arg(long, num_args = 1..)]
    pub include: Vec<String>,

    /// Turn off global package filters
    #[arg(long)]
    pub no_filters: bool,

    /// Patch the current context to create a new one
    #[arg(short = 'p', long)]
    pub patch: bool,

    /// Strict patching (only effective with --patch)
    #[arg(long)]
    pub strict: bool,

    /// Patch rank (only effective with --patch)
    #[arg(long, default_value_t = 0)]
    pub patch_rank: u32,

    /// Do not fetch cached resolves
    #[arg(long)]
    pub no_cache: bool,

    /// Quiet mode (hides welcome message)
    #[arg(short = 'q', long)]
    pub quiet: bool,

    /// Display resolve graph on failure
    #[arg(long)]
    pub fail_graph: bool,

    /// Start the shell in a new process group
    #[arg(long)]
    pub new_session: bool,

    /// Open a separate terminal
    #[arg(long)]
    pub detached: bool,

    /// Only print actions that affect the solve (with verbosity)
    #[arg(long)]
    pub no_passive: bool,

    /// Print advanced solver stats
    #[arg(long)]
    pub stats: bool,

    /// Disable package caching
    #[arg(long)]
    pub no_pkg_cache: bool,

    /// Override package cache async mode ("sync" or "async")
    #[arg(long, value_name = "MODE")]
    pub pkg_cache_mode: Option<String>,

    /// Start with an explicitly empty environment before applying package commands
    #[arg(long)]
    pub clear: bool,

    /// Inherit the parent environment (Rez default); --inherited=false requests an empty parent.
    #[arg(long, action = clap::ArgAction::Set, num_args = 0..=1,
          require_equals = true, default_value_t = true, default_missing_value = "true")]
    pub inherited: bool,

    /// Increase verbosity (-v, -vv, etc.)
    #[arg(from_global)]
    pub verbose: u8,

    /// Extra args after "--" treated as command
    #[arg(last = true)]
    pub extra: Vec<String>,
}

// =============================================================================
// Run
// =============================================================================

pub fn run(args: &EnvArgs) -> Result<std::process::ExitCode> {
    // Determine command: --command or args after "--"
    let command = if !args.extra.is_empty() {
        if args.command.is_some() {
            return Err(RezError::Config(
                "--command not allowed with arguments after '--'".into(),
            ));
        }
        None
    } else {
        args.command.clone()
    };

    // Parse timestamp
    let timestamp = match &args.time {
        Some(t) => Some(parse_epoch_time(t)?),
        None => None,
    };

    // Determine package search paths
    let pkg_paths = if let Some(ref paths_str) = args.paths {
        let sep = if cfg!(windows) { ';' } else { ':' };
        Some(
            paths_str
                .split(sep)
                .filter(|s| !s.is_empty())
                .map(|s| PathBuf::from(expand_tilde(s)))
                .collect::<Vec<_>>(),
        )
    } else if args.no_local {
        // Use nonlocal paths only (skip local_packages_path)
        let config = &*CONFIG;
        Some(
            config
                .packages_path
                .iter()
                .filter(|p| *p != &config.local_packages_path)
                .map(PathBuf::from)
                .collect(),
        )
    } else {
        None // use default from config
    };

    // Determine shell type
    let shell_type = match &args.shell {
        Some(name) => ShellType::from_name(name)
            .ok_or_else(|| RezError::Config(format!("Unknown shell type: '{name}'")))?,
        None => {
            let config = &*CONFIG;
            if !config.default_shell.is_empty() {
                ShellType::from_name(&config.default_shell).unwrap_or_else(detect_shell)
            } else {
                detect_shell()
            }
        }
    };

    // Rez treats tokens after -- as argv and quotes them for the selected shell.
    // --command remains shell source. Keep Rez's runtime variable expansion.
    let command = if args.extra.is_empty() {
        command
    } else {
        Some(shell_type.join_command(&args.extra, true, None))
    };

    // -- Load or resolve context --
    let cache_options = ResolveOptions {
        package_caching: args.no_pkg_cache.then_some(false),
        package_cache_async: args
            .pkg_cache_mode
            .as_deref()
            .map(|mode| match mode {
                "sync" => Ok(false),
                "async" => Ok(true),
                _ => Err(RezError::Config(format!(
                    "Invalid package cache mode: {mode}; expected sync or async"
                ))),
            })
            .transpose()?,
        ..Default::default()
    };
    let mut context: Option<ResolvedContext> = None;
    let mut request: Vec<String> = args.packages.clone();

    // Load from file if --input
    if let Some(ref input_path) = args.input {
        log_info!("env", "Loading context from {}", input_path);
        if !args.packages.is_empty() && !args.patch {
            return Err(RezError::Config(
                "Cannot use --input and provide packages, unless patching".into(),
            ));
        }
        context = Some(ResolvedContext::load(
            Path::new(input_path),
            Some(&cache_options),
        )?);
    }

    // Patching: modify request based on current context
    if args.patch {
        let patch_ctx = match context.take() {
            Some(c) => c,
            None => {
                // Get current context from environment
                match ResolvedContext::get_current(Some(&cache_options)) {
                    Some(Ok(c)) => c,
                    Some(Err(e)) => return Err(e),
                    None => {
                        return Err(RezError::ResolvedContext(
                            "cannot patch: not in a context".into(),
                        ));
                    }
                }
            }
        };

        // Apply patching: convert string requests to Requirements and call get_patched_request_simple
        let patch_reqs: Vec<Requirement> = request
            .iter()
            .map(|s| Requirement::new(s))
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let patched =
            patch_ctx.get_patched_request_simple(&patch_reqs, args.strict, args.patch_rank)?;

        request = patched.iter().map(|r| r.to_string()).collect();
        // context is now None, will be re-resolved below
    }

    // Resolve if no context loaded
    if context.is_none() {
        log_info!("env", "Resolving packages: {:?}", request);
        // Build package filter
        let mut filter = if args.no_filters {
            PackageFilterList::new()
        } else {
            load_config_filters()
        };

        // Add exclusion/inclusion rules
        if !args.exclude.is_empty() || !args.include.is_empty() {
            let mut pf = PackageFilter::new();
            for rule_str in &args.exclude {
                let rule = Rule::parse(rule_str)?;
                pf.add_exclusion(rule, None);
            }
            for rule_str in &args.include {
                let rule = Rule::parse(rule_str)?;
                pf.add_inclusion(rule, None);
            }
            filter.add_filter(pf);
        }

        // Parse package requests
        let reqs: Vec<Requirement> = request
            .iter()
            .map(|s| Requirement::new(s))
            .collect::<std::result::Result<Vec<_>, _>>()?;

        // Build resolve options
        let opts = ResolveOptions {
            timestamp,
            package_paths: pkg_paths,
            building: args.build,
            caching: !args.no_cache,
            add_implicit: !args.no_implicit,
            verbosity: args.verbose as u32,
            max_fails: args.max_fails,
            time_limit: args.time_limit,
            package_filter: Some(filter),
            ..cache_options
        };

        let provider = get_package_provider(&opts)?;
        context = Some(ResolvedContext::resolve(reqs, &*provider, opts)?);
    }

    let ctx = context.ok_or_else(|| {
        RezError::Resolve("no context: provide packages to resolve, or use --input".into())
    })?;
    let success = ctx.status == ResolverStatus::Solved;

    // Print info on failure
    if !success {
        ctx.print_info(args.verbose as u32);

        if args.fail_graph {
            if let Some(graph) = ctx.graph_string.as_deref() {
                let image_path = view_graph(graph)?;
                eprintln!("Resolve graph image: {}", image_path.display());
            } else {
                eprintln!("the failed resolve context did not generate a graph.");
            }
        }
    }

    // -- Output mode: save to file --
    if let Some(ref output_path) = args.output {
        log_info!("env", "Saving context to {}", output_path);
        if output_path == "-" {
            // Write to stdout
            let json = ctx.to_json()?;
            let formatted = serde_json::to_string_pretty(&json)?;
            println!("{formatted}");
        } else {
            ctx.save(Path::new(output_path))?;
        }
        if !success {
            return Err(RezError::Resolve("resolve failed".into()));
        }
        return Ok(std::process::ExitCode::SUCCESS);
    }

    if !success {
        return Err(RezError::Resolve("resolve failed".into()));
    }

    // -- Execute shell --
    log_info!("env", "Executing shell: {:?}", shell_type);
    let quiet = args.quiet || command.is_some();
    let inherited = args.inherited;
    let parent_environ = if args.clear || !inherited {
        Some(std::collections::HashMap::new())
    } else {
        None
    };
    let status = ctx.execute_shell(
        Some(shell_type),
        command.as_deref(),
        parent_environ,
        quiet,
        inherited,
    )?;

    Ok(rustpython_host_env::os::exit_code(
        status.code().unwrap_or(1) as u32,
    ))
}

// =============================================================================
// Helpers
// =============================================================================

/// Parse epoch time from string: epoch seconds or relative time (-10s, -5m, -0.5h, -10d).
fn parse_epoch_time(s: &str) -> Result<u64> {
    // Try direct epoch parse
    if let Ok(epoch) = s.parse::<u64>() {
        return Ok(epoch);
    }

    // Relative time: "-Ns", "-Nm", "-Nh", "-Nd"
    let trimmed = s.trim_start_matches('-');
    if trimmed.is_empty() {
        return Err(RezError::Config(format!("Invalid time format: '{s}'")));
    }

    let (num_str, unit) = trimmed.split_at(trimmed.len() - 1);
    let value: f64 = num_str
        .parse()
        .map_err(|_| RezError::Config(format!("Invalid time format: '{s}'")))?;

    let secs = match unit {
        "s" => value,
        "m" => value * 60.0,
        "h" => value * 3600.0,
        "d" => value * 86400.0,
        _ => {
            return Err(RezError::Config(format!(
                "Invalid time unit in '{s}': expected s/m/h/d"
            )));
        }
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    Ok(now.saturating_sub(secs as u64))
}
/// Build PackageProvider backed by filesystem repositories.
fn get_package_provider(opts: &ResolveOptions) -> Result<Box<dyn PackageProvider>> {
    let paths = if let Some(ref p) = opts.package_paths {
        if !p.is_empty() {
            p.clone()
        } else {
            config_package_paths()
        }
    } else {
        config_package_paths()
    };

    Ok(Box::new(FilesystemPackageProvider::from_paths(&paths)?))
}

/// Get package search paths from CONFIG, with ~ expansion.
fn config_package_paths() -> Vec<PathBuf> {
    CONFIG.expanded_packages_path_os()
}

/// Load global package filters from CONFIG.package_filter.
fn load_config_filters() -> PackageFilterList {
    let mut list = PackageFilterList::new();

    // CONFIG.package_filter is Option<serde_json::Value>
    // Expected format: { "excludes": ["pattern", ...], "includes": ["pattern", ...] }
    if let Some(ref val) = CONFIG.package_filter {
        let mut pf = PackageFilter::new();
        let mut has_rules = false;

        if let Some(excludes) = val.get("excludes").and_then(|v| v.as_array()) {
            for item in excludes {
                if let Some(s) = item.as_str() {
                    if let Ok(rule) = Rule::parse(s) {
                        pf.add_exclusion(rule, None);
                        has_rules = true;
                    }
                }
            }
        }

        if let Some(includes) = val.get("includes").and_then(|v| v.as_array()) {
            for item in includes {
                if let Some(s) = item.as_str() {
                    if let Ok(rule) = Rule::parse(s) {
                        pf.add_inclusion(rule, None);
                        has_rules = true;
                    }
                }
            }
        }

        if has_rules {
            list.add_filter(pf);
        }
    }

    list
}
