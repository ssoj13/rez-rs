// SPDX-License-Identifier: Apache-2.0

crate::dcc_scan!(
    BLENDER, "blender", "Blender", &["blender"],
    win: &[
        ("C:/Program Files/Blender Foundation", "Blender "),
        ("C:/Programs", "Blender"),  // Blender5, Blender4, etc.
    ],
    mac: &[],
    linux: &[("/opt", "blender"), ("/usr/local", "blender")],
    bin: per_platform("", "Contents/MacOS"),
    env: &[],
    version: command("blender", &["--version"], "Blender "),
    try_command_first: true
);
