// SPDX-License-Identifier: Apache-2.0

use crate::package::bind::detect::*;

pub const DEADLINE: AppDef = AppDef {
    name: "deadline",
    display: "Thinkbox Deadline",
    tools: &["deadlinecommand", "deadline"],
    search: SearchMethod::Scan {
        windows: &[("C:/Programs", "D")],
        macos: &[("/Applications", "Deadline"), ("/opt/Thinkbox", "Deadline")],
        linux: &[
            ("/opt/Thinkbox", "Deadline"),
            ("/usr/local/Thinkbox", "Deadline"),
        ],
        min_version: None,
    },
    bin_sub: BinSub::Same("bin"),
    env_vars: &[("DEADLINE_PATH", "")],
    version: VersionDetect::DirName,
    bind_all_versions: false,
    try_command_first: false,
    env_append: &[],
    path_prepend_dirs: &[],
    env_prepend: &[],
};
