// SPDX-License-Identifier: Apache-2.0

use crate::package::bind::detect::*;

pub const UNREAL: AppDef = AppDef {
    name: "unreal",
    display: "Epic Games Unreal Engine",
    tools: &["UnrealEditor", "UnrealEditor-Cmd"],
    search: SearchMethod::Scan {
        windows: &[("C:/Programs", "UE_")],
        macos: &[("/Users/Shared/Epic Games", "UE_"), ("/opt/Epic", "UE_")],
        linux: &[("/opt/Epic", "UE_"), ("/usr/local/Epic", "UE_")],
        min_version: None,
    },
    bin_sub: BinSub::FullPlatform {
        windows: "Engine/Binaries/Win64",
        macos: "Engine/Binaries/Mac/UnrealEditor.app/Contents/MacOS",
        linux: "Engine/Binaries/Linux",
    },
    env_vars: &[("UE_PATH", "")],
    version: VersionDetect::DirName,
    bind_all_versions: false,
    try_command_first: false,
    env_append: &[],
    path_prepend_dirs: &[],
    env_prepend: &[],
};
