// SPDX-License-Identifier: Apache-2.0

use crate::package::bind::detect::*;

pub const MAYA_ARNOLD: AppDef = AppDef {
    name: "maya_arnold",
    display: "Autodesk Arnold (Maya)",
    tools: &["kick", "maketx", "noice", "kick_standalone"],
    search: SearchMethod::Scan {
        windows: &[("C:/Program Files/Autodesk", "Arnold")],
        macos: &[("/Applications/Autodesk", "Arnold")],
        linux: &[("/opt/solidangle", "mtoa"), ("/opt/autodesk", "arnold")],
        min_version: None,
    },
    bin_sub: BinSub::Same("bin"),
    env_vars: &[("ARNOLD_PATH", ""), ("solidangle_LICENSE", "")],
    version: VersionDetect::DirName,
    bind_all_versions: false,
    try_command_first: false,
    env_append: &[("MAYA_MODULE_PATH", "lib/maya/modules")],
    path_prepend_dirs: &[],
    env_prepend: &[],
};
