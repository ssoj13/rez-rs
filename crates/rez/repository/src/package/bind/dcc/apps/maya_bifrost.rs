// SPDX-License-Identifier: Apache-2.0

crate::dcc_scan!(
    MAYA_BIFROST, "maya_bifrost", "Autodesk Bifrost", &["bifrost"],
    win: ("C:/Program Files/Autodesk", "Bifrost"),
    mac: ("/Applications/Autodesk", "Bifrost"),
    linux: ("/usr/autodesk", "bifrost"),
    bin: "bin",
    env: &[("BIFROST_LOCATION", "")],
    env_append: &[("MAYA_MODULE_PATH", "lib/maya/modules")]
);
