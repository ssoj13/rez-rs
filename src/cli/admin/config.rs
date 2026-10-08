// SPDX-License-Identifier: Apache-2.0

//! `rez config` - query and display rez configuration.

use std::path::PathBuf;

use clap::Args;

use foundation::errors::{Result, RezError};
use model::config::CONFIG;

// =============================================================================
// Args
// =============================================================================

/// Print current rez configuration settings.
#[derive(Args, Debug)]
pub struct ConfigArgs {
    /// Config field to query (e.g. "packages_path", "build_directory").
    /// Dot notation for nested fields (e.g. "critical_color.fore").
    #[arg(value_name = "FIELD")]
    pub field: Option<String>,

    /// Output dict/list values as JSON (useful for REZ_*_JSON env vars).
    /// Ignored if FIELD is not given.
    #[arg(long)]
    pub json: bool,

    /// List config files that are searched (not necessarily sourced).
    #[arg(long)]
    pub search_list: bool,

    /// List config files that were actually sourced.
    #[arg(long)]
    pub source_list: bool,
}

// =============================================================================
// Run
// =============================================================================

pub fn run(args: &ConfigArgs) -> Result<()> {
    // --search-list: print the config file search locations
    if args.search_list {
        for path in config_search_paths() {
            println!("{}", path);
        }
        return Ok(());
    }

    // --source-list: print actually sourced config files
    if args.source_list {
        for path in config_sourced_paths() {
            println!("{}", path);
        }
        return Ok(());
    }

    // Serialize entire config to a traversable JSON Value
    let data = serde_json::to_value(&*CONFIG)
        .map_err(|e| RezError::Config(format!("Failed to serialize config: {e}")))?;

    // Drill into a specific field if requested
    let value = if let Some(ref field) = args.field {
        let mut current = &data;
        for key in field.split('.') {
            current = current
                .get(key)
                .ok_or_else(|| RezError::Config(format!("No such setting: {field:?}")))?;
        }
        current.clone()
    } else {
        data
    };

    // Print the result
    print_value(&value, args.json);
    Ok(())
}

// =============================================================================
// Helpers
// =============================================================================

/// Print a serde_json Value in a human-friendly way.
fn print_value(value: &serde_json::Value, as_json: bool) {
    use serde_json::Value;
    match value {
        Value::Object(_) | Value::Array(_) => {
            if as_json {
                println!("{}", serde_json::to_string(value).unwrap_or_default());
            } else {
                // Pretty YAML-like output
                let pretty = serde_json::to_string_pretty(value).unwrap_or_default();
                println!("{}", pretty.trim());
            }
        }
        Value::String(s) => println!("{s}"),
        Value::Number(n) => println!("{n}"),
        Value::Bool(b) => println!("{b}"),
        Value::Null => println!("null"),
    }
}

/// Standard config file search paths (in order).
/// Uses PathBuf so paths use platform-native separators.
fn config_search_paths() -> Vec<String> {
    let mut paths = Vec::new();

    // System-level config
    if cfg!(windows) {
        if let Ok(pd) = std::env::var("PROGRAMDATA") {
            paths.push(
                PathBuf::from(pd)
                    .join("rez")
                    .join("rezconfig.toml")
                    .display()
                    .to_string(),
            );
        }
    } else {
        paths.push("/etc/rez/rezconfig.toml".into());
    }

    // User config
    if let Some(home) = model::platform::SystemInfo::home() {
        paths.push(
            PathBuf::from(home)
                .join(".rez")
                .join("rezconfig.toml")
                .display()
                .to_string(),
        );
    }

    // REZ_CONFIG_FILE env override
    if let Ok(f) = std::env::var("REZ_CONFIG_FILE") {
        paths.push(f);
    }

    paths
}

/// Config files that were actually loaded (exist on disk).
fn config_sourced_paths() -> Vec<String> {
    config_search_paths()
        .into_iter()
        .filter(|p| std::path::Path::new(p).exists())
        .collect()
}
