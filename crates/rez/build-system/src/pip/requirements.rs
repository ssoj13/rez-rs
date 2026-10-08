// SPDX-License-Identifier: Apache-2.0

//! PEP 440 conversion shares Rez's range algebra rather than approximating compound bounds.

use crate::errors::{Result, RezError};
use serde::Deserialize;
use version::{Requirement, VersionRange};

#[derive(Debug, Deserialize)]
pub(super) struct Specifier {
    pub operator: String,
    /// Canonical Rez spelling, normalized by the selected pip's PEP 440 parser.
    pub version: String,
    pub wildcard: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct Dependency {
    pub name: String,
    pub specifiers: Vec<Specifier>,
    pub marker_names: Vec<String>,
    pub enabled: bool,
    pub url: Option<String>,
}

pub(super) fn name(value: &str) -> String {
    value.replace('-', "_")
}

pub(super) fn specifiers(values: &[Specifier]) -> Result<Option<VersionRange>> {
    let mut result: Option<VersionRange> = None;
    for spec in values {
        let version = &spec.version;
        let adjacent = adjacent(version)?;
        let following = next(version)?;
        let range = match (spec.operator.as_str(), spec.wildcard) {
            ("==", true) => version.clone(),
            ("==", false) => format!("{version}+<{adjacent}"),
            (">=", false) => format!("{version}+"),
            (">", false) => format!("{adjacent}+"),
            ("<=", false) => format!("<{adjacent}"),
            ("<", false) => format!("<{version}"),
            ("!=", true) => format!("<{version}|{following}+"),
            ("!=", false) => format!("<{version}|{adjacent}+"),
            ("~=", false) => {
                let (prefix, _) = version.rsplit_once('.').ok_or_else(|| {
                    RezError::PackageRequest(format!(
                        "Invalid compatible-release specifier ~= {version}"
                    ))
                })?;
                format!("{version}+<{}", next(prefix)?)
            }
            _ => {
                return Err(RezError::PackageRequest(format!(
                    "Cannot represent PEP 440 specifier {}{}{} in Rez",
                    spec.operator,
                    version,
                    if spec.wildcard { ".*" } else { "" }
                )));
            }
        };
        let range = VersionRange::new(&range)?;
        result = Some(match result {
            None => range,
            Some(previous) => previous.intersection(&range).ok_or_else(|| {
                RezError::PackageRequest("PEP 440 specifiers produce an empty Rez range".into())
            })?,
        });
    }
    Ok(result)
}

fn adjacent(value: &str) -> Result<String> {
    let (prefix, last) = value.rsplit_once('.').unwrap_or(("", value));
    if last.chars().all(|c| c.is_ascii_digit()) && !last.is_empty() {
        Ok(format!("{value}.1"))
    } else if prefix.is_empty() {
        Err(RezError::PackageRequest(format!(
            "Invalid normalized PEP 440 version {value}"
        )))
    } else {
        Ok(format!("{prefix}.0"))
    }
}

fn next(value: &str) -> Result<String> {
    let (prefix, last) = value.rsplit_once('.').unwrap_or(("", value));
    let last = if last.chars().all(|c| c.is_ascii_digit()) && !last.is_empty() {
        last.parse::<u128>()
            .ok()
            .and_then(|v| v.checked_add(1))
            .ok_or_else(|| {
                RezError::PackageRequest(format!("Version component overflow in {value}"))
            })?
            .to_string()
    } else {
        "0".into()
    };
    Ok(if prefix.is_empty() {
        last
    } else {
        format!("{prefix}.{last}")
    })
}

pub(super) fn requirement(
    dependency: &Dependency,
    installed_version: Option<&str>,
    resolved_name: Option<&str>,
) -> Result<String> {
    let name = resolved_name
        .map(str::to_owned)
        .unwrap_or_else(|| name(&dependency.name));
    let range = if dependency.url.is_some() {
        let version = installed_version.ok_or_else(|| RezError::PackageRequest(format!(
            "Direct URL dependency {} requires its installed distribution to preserve the selected version",
            dependency.name
        )))?;
        Some(VersionRange::new(&format!("=={version}"))?)
    } else {
        specifiers(&dependency.specifiers)?
    };
    let value = range.map(|range| format!("{name}-{range}")).unwrap_or(name);
    Requirement::new(&value)?;
    Ok(value)
}

pub(super) fn systems(markers: &[String]) -> Vec<&'static str> {
    let mut systems = Vec::new();
    for marker in markers {
        let values: &[&str] = match marker.as_str() {
            "python_version"
            | "python_full_version"
            | "implementation_name"
            | "implementation_version"
            | "platform_python_implementation" => &["python"],
            "platform_machine" => &["arch"],
            "os_name" | "sys_platform" | "platform_system" | "platform_release"
            | "platform_version" => &["platform"],
            "extra" => &[],
            _ => &[],
        };
        for &value in values {
            if !systems.contains(&value) {
                systems.push(value);
            }
        }
    }
    systems
}

#[cfg(test)]
mod tests {
    use super::*;
    fn spec(operator: &str, version: &str, wildcard: bool) -> Specifier {
        Specifier {
            operator: operator.into(),
            version: version.into(),
            wildcard,
        }
    }
    #[test]
    fn intersection_preserves_all_bounds_and_exclusions() {
        let range = specifiers(&[
            spec(">=", "1", false),
            spec(">=", "2", false),
            spec("<", "4", false),
            spec("!=", "3", true),
        ])
        .unwrap()
        .unwrap();
        assert!(range.contains_version(&version::Version::new("2.5").unwrap()));
        assert!(!range.contains_version(&version::Version::new("1.5").unwrap()));
        assert!(!range.contains_version(&version::Version::new("3.2").unwrap()));
    }
    #[test]
    fn contradictory_constraints_fail() {
        assert!(specifiers(&[spec(">=", "2", false), spec("<", "1", false)]).is_err());
        assert!(specifiers(&[spec("===", "1", false)]).is_err());
    }
    #[test]
    fn source_prerelease_adjacent_contract() {
        assert_eq!(adjacent("1.a2").unwrap(), "1.0");
        assert_eq!(next("1.a2").unwrap(), "1.0");
        assert_eq!(adjacent("1.2").unwrap(), "1.2.1");
        assert_eq!(next("1.2").unwrap(), "1.3");
    }
}
