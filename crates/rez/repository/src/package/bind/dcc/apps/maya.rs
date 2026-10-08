// SPDX-License-Identifier: Apache-2.0

crate::dcc_scan!(
    MAYA, "maya", "Autodesk Maya", &["maya", "mayapy", "mayabatch"],
    win: ("C:/Program Files/Autodesk", "Maya"),
    mac: ("/Applications/Autodesk", "maya"),
    linux: ("/usr/autodesk", "maya"),
    bin: "bin",
    env: &[("MAYA_LOCATION", "")],
    all_versions: true
);
