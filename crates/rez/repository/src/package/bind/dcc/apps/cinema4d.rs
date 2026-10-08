// SPDX-License-Identifier: Apache-2.0

use crate::package::bind::detect::*;

pub const CINEMA4D: AppDef = AppDef {
    name: "cinema4d",
    display: "Maxon Cinema 4D",
    tools: &["Cinema 4D", "Commandline"],
    search: SearchMethod::Scan {
        windows: &[("C:/Program Files/Maxon", "Cinema 4D ")],
        macos: &[("/Applications/Maxon", "Cinema 4D ")],
        linux: &[("/opt/maxon", "cinema4dr")],
        min_version: None,
    },
    bin_sub: BinSub::PerPlatform {
        default: "",
        macos: "Contents/MacOS",
    },
    env_vars: &[("CINEMA4D_ROOT", "")],
    version: VersionDetect::DirName,
    bind_all_versions: false,
    try_command_first: false,
    env_append: &[],
    path_prepend_dirs: &[],
    env_prepend: &[],
};
