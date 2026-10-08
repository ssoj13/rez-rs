// SPDX-License-Identifier: Apache-2.0

//! Platform, architecture, and OS bind detectors (always succeed).

use super::BindInfo;
use crate::environment;
use crate::platform::{Platform, SYSTEM};
use std::collections::HashMap;

/// Rez system requirements used by system-dependent bind modules.
pub fn system_variant(include_os: bool) -> Vec<String> {
    let mut requirements = SYSTEM.variant();
    if !include_os {
        requirements.truncate(2);
    }
    requirements
}

/// Detect current platform and produce BindInfo.
///
/// The platform package is the single Rez package boundary that applies the
/// canonical system PATH and baseline OS environment to resolved contexts.
pub fn detect_platform() -> BindInfo {
    let platform = SYSTEM.platform;
    let mut info = BindInfo::new("platform", platform.name());
    info.description = format!("System platform: {}", platform.name());
    info.commands = Some(
        r#"
for _rez_system_path in system.paths:
    env.PATH.append(_rez_system_path)
for _rez_system_key, _rez_system_value in system.environ.items():
    if _rez_system_key.upper() != 'PATH':
        env[_rez_system_key].set(_rez_system_value)
"#
        .trim()
        .to_string(),
    );
    info
}

/// Detect current CPU architecture and produce BindInfo.
pub fn detect_arch() -> BindInfo {
    let a = &SYSTEM.arch;
    let mut info = BindInfo::new("arch", &a.to_string());
    info.description = format!("System architecture: {}", a);
    info
}

/// Detect current OS and produce BindInfo.
///
/// Rez's OS package depends on platform and architecture. The platform package
/// owns system PATH and baseline environment commands.
pub fn detect_os() -> BindInfo {
    let os = &SYSTEM.os;
    let mut info = BindInfo::new("os", os.name());
    info.description = format!("Operating system: {}", os);
    info.requires = system_variant(false);
    info
}

/// Discover canonical system paths for callers that need the platform baseline.
pub fn discover_sys_paths() -> Vec<String> {
    let parent: HashMap<String, String> = std::env::vars_os()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect();
    environment::system_paths(Platform::current(), &parent)
}
