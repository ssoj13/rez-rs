//! `rez yaml2py` - convert package.yaml to package.py format.
//!
//! Loads YAML package definition and outputs Python equivalent.

use clap::Args;
use foundation::errors::{Result, RezError};
use std::fs;
use std::path::PathBuf;

use crate::cli::util::expand_tilde_path;
use model::package::Package;
use model::serialise::{dump_package_data_py, find_package_definition_file};
use std::collections::HashMap;

/// Convert package.yaml to package.py format.
#[derive(Args, Debug)]
pub struct Yaml2pyArgs {
    /// Path to a YAML package file or directory containing one (default: cwd)
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,
}

pub fn run(args: &Yaml2pyArgs) -> Result<()> {
    let path = match &args.path {
        Some(p) => {
            let expanded = expand_tilde_path(p);
            if expanded.is_dir() {
                find_package_definition_file(&expanded, &["yaml", "yml"])?.ok_or_else(|| {
                    RezError::Config(format!(
                        "Cannot find configured package YAML in '{}'",
                        expanded.display()
                    ))
                })?
            } else {
                expanded
            }
        }
        None => {
            let cwd = std::env::current_dir()
                .map_err(|e| RezError::Config(format!("Cannot get cwd: {}", e)))?;
            find_package_definition_file(&cwd, &["yaml", "yml"])?.ok_or_else(|| {
                RezError::Config(format!(
                    "Cannot find configured package YAML in '{}'",
                    cwd.display()
                ))
            })?
        }
    };

    if !path.is_file() {
        return Err(RezError::Config(format!(
            "Cannot find package YAML at '{}'",
            path.display()
        )));
    }

    let content = fs::read_to_string(&path)
        .map_err(|e| RezError::Config(format!("Cannot read {}: {}", path.display(), e)))?;

    let python_content = convert_yaml_to_python(&content).map_err(|e| {
        RezError::Config(format!("Invalid package YAML in {}: {}", path.display(), e))
    })?;
    println!("{python_content}");

    Ok(())
}

fn convert_yaml_to_python(content: &str) -> Result<String> {
    let yaml_value: serde_yaml::Value = serde_yaml::from_str(content)
        .map_err(|e| RezError::Config(format!("Invalid YAML: {e}")))?;
    let json_value = serde_json::to_value(yaml_value)
        .map_err(|e| RezError::Config(format!("Cannot convert YAML package data: {e}")))?;
    let object = json_value
        .as_object()
        .ok_or_else(|| RezError::Config("Package YAML must be a mapping".into()))?;
    let data: HashMap<String, serde_json::Value> = object.clone().into_iter().collect();

    // Match the package loading path so conversion rejects invalid package metadata.
    Package::from_data(data.clone())?;

    Ok(dump_package_data_py(&data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversion_serializes_newlines_quotes_and_nested_values_as_valid_python() {
        let yaml = r#"
name: example
version: "1.2.3"
description: |-
  first line
  a "quoted" line with 'single quotes' and a backslash \
custom_metadata:
  labels:
    - 'double "quoted"'
    - "single 'quoted'"
  enabled: true
  limit: 4
"#;
        let python = convert_yaml_to_python(yaml).unwrap();
        let parsed = model::serialise::exec_package_py(&python).unwrap();

        assert_eq!(
            parsed["description"],
            serde_json::json!(
                "first line\na \"quoted\" line with 'single quotes' and a backslash \\"
            )
        );
        assert_eq!(
            parsed["custom_metadata"],
            serde_json::json!({
                "labels": ["double \"quoted\"", "single 'quoted'"],
                "enabled": true,
                "limit": 4
            })
        );
    }

    #[test]
    fn conversion_rejects_malformed_non_mapping_and_invalid_package_data() {
        assert!(convert_yaml_to_python("name: [").is_err());
        assert!(convert_yaml_to_python("- item").is_err());
        assert!(convert_yaml_to_python("version: \"1.0\"").is_err());
        assert!(convert_yaml_to_python("name: example\nversion: 1").is_err());
    }
}
