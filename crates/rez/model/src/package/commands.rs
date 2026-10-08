// SPDX-License-Identifier: Apache-2.0
// Conversion behavior derived from Rez utils/backcompat.py.
// Copyright Contributors to the Rez Project

//! Normalize Rez 1 shell command lists into the shared Rex representation.

use crate::config::RezConfig;
use crate::errors::{Result, RezError};
use serde_json::Value;
use std::collections::HashMap;

/// Convert Bash-era package commands, retaining optional source annotations.
pub fn convert(
    commands: &[String],
    annotate: bool,
    separators: &HashMap<String, String>,
) -> String {
    let encode = |value: &str| {
        // Match the reference's immediately preceding backslash rule, including
        // unmatched quotes: only complete unescaped quoted spans are protected.
        let mut quoted_ranges = Vec::new();
        let mut offset = 0;
        // Python's reference regular expression does not match across newlines.
        for line in value.split_inclusive('\n') {
            let quotes: Vec<usize> = line
                .char_indices()
                .filter_map(|(index, ch)| {
                    (ch == '"' && !line[..index].ends_with('\\')).then_some(offset + index)
                })
                .collect();
            for pair in quotes.as_chunks::<2>().0 {
                quoted_ranges.push((pair[0], pair[1]));
            }
            offset += line.len();
        }
        let mut encoded = String::new();
        let mut start = 0;
        for (begin, end) in quoted_ranges {
            encoded.push_str(&value[start..begin].replace("\\\"", "\""));
            encoded.push_str(&value[begin..=end]);
            start = end + 1;
        }
        encoded.push_str(&value[start..].replace("\\\"", "\""));
        crate::serialise::python_repr(&Value::String(encoded))
    };
    let mut lines = Vec::new();
    for original in commands {
        if annotate {
            lines.push(format!(
                "comment({})",
                encode(&format!("OLD COMMAND: {original}"))
            ));
        }
        let mut command = original.clone();
        for (old, new) in [
            ("!VERSION!", "{version}"),
            ("!MAJOR_VERSION!", "{version.major}"),
            ("!MINOR_VERSION!", "{version.minor}"),
            ("!BASE!", "{base}"),
            ("!ROOT!", "{root}"),
            ("!USER!", "{system.user}"),
        ] {
            command = command.replace(old, new);
        }
        let tokens: Vec<_> = command.split_whitespace().collect();
        let converted = match tokens.first().copied() {
            Some("export") => command.split_once(' ').and_then(|(_, assignment)| {
                let (key, raw) = assignment.split_once('=')?;
                let mut value = raw.to_owned();
                for quote in ['"', '\''] {
                    if value.starts_with(quote) && value.ends_with(quote) {
                        value = if value.len() <= 1 {
                            String::new()
                        } else {
                            value[1..value.len() - 1].to_owned()
                        };
                        break;
                    }
                }
                let separator = separators.get(key).map(String::as_str).unwrap_or(":");
                if separator.is_empty() {
                    return None;
                }
                if key == "CMAKE_MODULE_PATH" {
                    value = value
                        .replace(&format!("'{separator}'"), separator)
                        .replace(&format!("\"{separator}\""), separator)
                        .replace(':', separator);
                }
                let parts: Vec<_> = value
                    .split(separator)
                    .filter(|part| !part.is_empty())
                    .collect();
                if parts.len() > 1 {
                    let plain = format!("${key}");
                    let braced = format!("${{{key}}}");
                    let position = parts
                        .iter()
                        .position(|part| *part == plain)
                        .or_else(|| parts.iter().position(|part| *part == braced));
                    if position == Some(0) {
                        return Some(format!(
                            "appendenv({}, {})",
                            encode(key),
                            encode(&parts[1..].join(separator))
                        ));
                    }
                    if position == Some(parts.len() - 1) {
                        return Some(format!(
                            "prependenv({}, {})",
                            encode(key),
                            encode(&parts[..parts.len() - 1].join(separator))
                        ));
                    }
                }
                Some(format!("setenv({}, {})", encode(key), encode(&value)))
            }),
            Some(token) if token.starts_with('#') => {
                Some(format!("comment({})", encode(&tokens[1..].join(" "))))
            }
            Some("alias") => command.split_once("alias ").and_then(|(_, assignment)| {
                let (key, value) = assignment.split_once('=')?;
                let key = key.trim();
                let mut value = value.trim();
                if value.len() >= 2
                    && ((value.starts_with('"') && value.ends_with('"'))
                        || (value.starts_with('\'') && value.ends_with('\'')))
                {
                    value = &value[1..value.len() - 1];
                }
                Some(format!("alias({}, {})", encode(key), encode(value)))
            }),
            _ => None,
        };
        lines.push(converted.unwrap_or_else(|| format!("command({})", encode(&command))));
    }
    lines.join("\n")
}

/// Apply the canonical compatibility policy once at package data boundaries.
pub fn normalize(data: &mut HashMap<String, Value>, config: &RezConfig) -> Result<()> {
    let mut pending = Vec::new();
    for field in ["commands", "pre_commands", "post_commands"] {
        let Some(Value::Array(values)) = data.get(field) else {
            continue;
        };
        let commands: Vec<String> = values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| RezError::PackageMetadata {
                        msg: format!("field '{field}[{index}]' must be a string"),
                        path: None,
                        resource_key: None,
                    })
            })
            .collect::<Result<_>>()?;
        let identity = data
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("<unnamed>");
        let message = format!("package {identity:?} is using old-style {field}.");
        if config.disable_rez_1_compatibility || config.error_old_commands {
            return Err(RezError::PackageMetadata {
                msg: message,
                path: None,
                resource_key: None,
            });
        }
        let source = convert(&commands, true, &config.env_var_separators);
        pending.push((field, commands, message, source));
    }
    for (field, commands, message, source) in pending {
        if config.is_warn_enabled("old_commands") {
            eprintln!("WARNING: {message}");
        }
        if config.is_debug_enabled("old_commands") {
            crate::logging::do_log(
                crate::logging::LogLevel::Debug,
                "old_commands",
                format_args!(
                    "OLD COMMANDS:\n{}\nNEW COMMANDS:\n{source}",
                    commands.join("\n")
                ),
                true,
            );
        }
        data.insert(field.into(), Value::String(source));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_conversion_edges() {
        // Expected strings were generated by the actual reference converter.
        let cases = [
            ("", "command('')"),
            ("export", "command('export')"),
            ("export A", "command('export A')"),
            ("export A=\"", "setenv('A', '')"),
            (" export A=B", "setenv('export A', 'B')"),
            ("export A=${A}:B", "appendenv('A', 'B')"),
            ("export A=B:${A}", "prependenv('A', 'B')"),
            ("export A=X:$A:Y", "setenv('A', 'X:$A:Y')"),
            ("export A=$A::B:", "appendenv('A', 'B')"),
            ("export CUSTOM=$CUSTOM|B|C", "appendenv('CUSTOM', 'B|C')"),
            (
                "export CMAKE_MODULE_PATH=\"A:\";\":B\"",
                "setenv('CMAKE_MODULE_PATH', 'A;;;B')",
            ),
            ("# hello world", "comment('hello world')"),
            ("alias hi=\"echo !ROOT!\"", "alias('hi', 'echo {root}')"),
            ("alias bad", "command('alias bad')"),
            (
                "echo !USER! !MAJOR_VERSION! !MINOR_VERSION! !VERSION! !BASE!",
                "command('echo {system.user} {version.major} {version.minor} {version} {base}')",
            ),
        ];
        let separators = HashMap::from([
            ("CMAKE_MODULE_PATH".into(), ";".into()),
            ("CUSTOM".into(), "|".into()),
        ]);
        for (command, expected) in cases {
            assert_eq!(
                convert(&[command.into()], false, &separators),
                expected,
                "{command:?}"
            );
        }
        assert_eq!(
            convert(&["export A=B".into()], true, &separators),
            "comment('OLD COMMAND: export A=B')\nsetenv('A', 'B')"
        );
        let empty_separator = HashMap::from([("A".into(), "".into())]);
        assert_eq!(
            convert(&["export A=B".into()], false, &empty_separator),
            "command('export A=B')"
        );
    }

    #[test]
    fn reference_conversion_vectors() {
        let cases = [
            ("export A=B", "setenv('A', 'B')"),
            ("export A=B:$C", "setenv('A', 'B:$C')"),
            ("export A=$A:B", "appendenv('A', 'B')"),
            ("export A=B:$A", "prependenv('A', 'B')"),
            ("export A=$A:B:$C", "appendenv('A', 'B:$C')"),
            ("export A=$C:B:$A", "prependenv('A', '$C:B')"),
            (
                "export A=\"hey \\\"there\\\"\"",
                "setenv('A', 'hey \"there\"')",
            ),
        ];
        assert_eq!(convert(&[], false, &HashMap::new()), "");
        for (command, expected) in cases {
            assert_eq!(convert(&[command.into()], false, &HashMap::new()), expected);
        }
    }

    #[test]
    fn configuration_policy_roundtrip() {
        let mut config = RezConfig::default();
        assert!(config.disable_rez_1_compatibility);
        assert!(!config.error_old_commands);
        assert!(config.is_warn_enabled("old_commands"));
        assert!(!config.is_debug_enabled("old_commands"));
        config.warn_none = true;
        config.warn_all = true;
        assert!(!config.is_warn_enabled("old_commands"));
        config.debug_all = true;
        assert!(config.is_debug_enabled("old_commands"));
        config.debug_none = true;
        assert!(!config.is_debug_enabled("old_commands"));
        let restored: RezConfig =
            serde_json::from_value(serde_json::to_value(&config).unwrap()).unwrap();
        assert!(restored.disable_rez_1_compatibility);
        assert!(restored.warn_old_commands);
        assert!(!restored.error_old_commands);
        assert!(!restored.debug_old_commands);
    }

    #[test]
    fn compatibility_policy_and_idempotence() {
        let data = HashMap::from([
            ("name".into(), Value::String("legacy".into())),
            ("commands".into(), serde_json::json!(["export A=B"])),
            ("pre_commands".into(), serde_json::json!(["# setup"])),
            (
                "post_commands".into(),
                serde_json::json!(["alias hello='echo hello'"]),
            ),
        ]);
        let mut config = RezConfig::default();
        assert!(normalize(&mut data.clone(), &config).is_err());
        config.disable_rez_1_compatibility = false;
        config.warn_none = true;
        let mut converted = data.clone();
        normalize(&mut converted, &config).unwrap();
        let snapshot = converted.clone();
        normalize(&mut converted, &config).unwrap();
        assert_eq!(converted, snapshot);
        let package = crate::package::Package::from_data(converted).unwrap();
        assert!(package.commands.unwrap().contains("setenv('A', 'B')"));
        assert!(package.pre_commands.unwrap().contains("comment('setup')"));
        assert!(package
            .post_commands
            .unwrap()
            .contains("alias('hello', 'echo hello')"));
        config.error_old_commands = true;
        assert!(normalize(&mut data.clone(), &config).is_err());
        config.error_old_commands = false;
        let mut invalid = data.clone();
        invalid.insert("post_commands".into(), serde_json::json!([42]));
        let snapshot = invalid.clone();
        assert!(normalize(&mut invalid, &config).is_err());
        assert_eq!(invalid, snapshot);
    }
}
