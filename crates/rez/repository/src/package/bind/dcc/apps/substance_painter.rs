// SPDX-License-Identifier: Apache-2.0

use crate::package::bind::detect::*;

pub const SUBSTANCE_PAINTER: AppDef = AppDef {
    name: "substance_painter",
    display: "Adobe Substance 3D Painter",
    tools: &["substance_painter"],
    search: SearchMethod::Fixed {
        windows: Some("C:/Program Files/Adobe/Adobe Substance 3D Painter"),
        macos: Some("/Applications/Adobe Substance 3D Painter.app"),
        linux: Some("/opt/Adobe/Adobe_Substance_3D_Painter"),
    },
    bin_sub: BinSub::PerPlatform {
        default: "",
        macos: "Contents/MacOS",
    },
    env_vars: &[],
    version: VersionDetect::XmlTag {
        file: "Version.xml",
        open_tag: "<ProductVersion>",
    },
    bind_all_versions: false,
    try_command_first: false,
    env_append: &[],
    path_prepend_dirs: &[],
    env_prepend: &[],
};
