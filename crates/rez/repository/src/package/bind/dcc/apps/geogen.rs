// SPDX-License-Identifier: Apache-2.0

use crate::package::bind::detect::*;

pub const GEOGEN: AppDef = AppDef {
    name: "geogen",
    display: "JangaFX GeoGen",
    tools: &["geogen"],
    search: SearchMethod::Scan {
        windows: &[("C:/Program Files/JangaFX", "GeoGen")],
        macos: &[],
        linux: &[("/opt/JangaFX", "GeoGen")],
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
