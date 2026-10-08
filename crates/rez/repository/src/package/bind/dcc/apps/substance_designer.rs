// SPDX-License-Identifier: Apache-2.0

use crate::package::bind::detect::*;

pub const SUBSTANCE_DESIGNER: AppDef = AppDef {
    name: "substance_designer",
    display: "Adobe Substance 3D Designer",
    tools: &["substance_designer"],
    search: SearchMethod::Fixed {
        windows: Some("C:/Program Files/Adobe/Adobe Substance 3D Designer"),
        macos: Some("/Applications/Adobe Substance 3D Designer.app"),
        linux: Some("/opt/Adobe/Adobe_Substance_3D_Designer"),
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
