// SPDX-License-Identifier: Apache-2.0

use crate::package::bind::detect::*;

pub const HOUDINI: AppDef = AppDef {
    name: "houdini",
    display: "SideFX Houdini",
    tools: &["houdini", "hython", "hescape", "hbatch"],
    search: SearchMethod::Scan {
        windows: &[("C:/Program Files/Side Effects Software", "Houdini ")],
        macos: &[
            ("/Applications/Houdini", "Houdini"),
            ("/Applications", "Houdini"),
        ],
        linux: &[("/opt", "hfs")],
        min_version: None,
    },
    bin_sub: BinSub::Same("bin"),
    env_vars: &[],
    version: VersionDetect::DirName,
    bind_all_versions: false,
    try_command_first: false,
    env_append: &[],
    // bin + dsolib + sbin in PATH
    path_prepend_dirs: &["dsolib", "houdini/sbin"],
    env_prepend: &[],
};
