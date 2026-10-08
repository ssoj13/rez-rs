// SPDX-License-Identifier: Apache-2.0

use crate::package::bind::detect::*;

pub const ILLUGEN: AppDef = AppDef {
    name: "illugen",
    display: "JangaFX IlluGen",
    tools: &["illugen"],
    search: SearchMethod::Scan {
        windows: &[("C:/Program Files/JangaFX", "IlluGen")],
        macos: &[],
        linux: &[("/opt/JangaFX", "IlluGen")],
        min_version: None,
    },
    bin_sub: BinSub::Same(""),
    env_vars: &[],
    version: VersionDetect::TextFile(&["version.txt", "VERSION"]),
    bind_all_versions: false,
    try_command_first: false,
    env_append: &[],
    path_prepend_dirs: &[],
    env_prepend: &[],
};
