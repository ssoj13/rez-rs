// SPDX-License-Identifier: Apache-2.0

crate::dcc_scan!(
    MAYA_LOOKDEVX, "maya_lookdevx", "Autodesk LookdevX", &["lookdevx"],
    win: ("C:/Program Files/Autodesk", "LookdevX"),
    mac: ("/Applications/Autodesk", "LookdevX"),
    linux: ("/usr/autodesk", "lookdevx"),
    bin: "bin",
    env: &[("LOOKDEVX_LOCATION", "")],
    env_append: &[("MAYA_MODULE_PATH", "lib/maya/modules")]
);
