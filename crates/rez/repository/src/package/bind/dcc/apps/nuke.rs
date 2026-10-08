// SPDX-License-Identifier: Apache-2.0

use crate::package::bind::detect::*;

pub const NUKE: AppDef = AppDef {
    name: "nuke",
    display: "Foundry Nuke",
    tools: &["nuke"],
    search: SearchMethod::Scan {
        windows: &[("C:/Program Files", "Nuke")],
        macos: &[("/Applications", "Nuke")],
        linux: &[("/usr/local", "Nuke")],
        min_version: None,
    },
    bin_sub: BinSub::PerPlatform {
        default: "",
        macos: "Contents/MacOS",
    },
    env_vars: &[("NUKE_PATH", "")],
    version: VersionDetect::DirName,
    bind_all_versions: false,
    try_command_first: false,
    env_append: &[],
    path_prepend_dirs: &[],
    env_prepend: &[],
};
