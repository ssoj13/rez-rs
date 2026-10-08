// SPDX-License-Identifier: Apache-2.0

use crate::package::bind::detect::*;

pub const CURSOR: AppDef = AppDef {
    name: "cursor",
    display: "Cursor IDE",
    tools: &["cursor"],
    search: SearchMethod::Scan {
        windows: &[("C:/Program Files", "cursor")],
        macos: &[("/Applications", "Cursor")],
        linux: &[("/opt", "cursor"), ("/usr/local", "cursor")],
        min_version: None,
    },
    bin_sub: BinSub::PerPlatform {
        default: "",
        macos: "Contents/MacOS",
    },
    env_vars: &[("CURSOR_PATH", "")],
    version: VersionDetect::DirName,
    bind_all_versions: false,
    try_command_first: false,
    env_append: &[],
    path_prepend_dirs: &[],
    env_prepend: &[],
};
