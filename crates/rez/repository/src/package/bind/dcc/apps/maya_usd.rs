// SPDX-License-Identifier: Apache-2.0

crate::dcc_scan!(
    MAYA_USD, "maya_usd", "Autodesk Maya USD", &["mayausd"],
    win: ("C:/Program Files/Autodesk", "MayaUSD"),
    mac: ("/Applications/Autodesk", "mayaUSD"),
    linux: ("/usr/autodesk", "mayausd"),
    bin: "bin",
    env: &[("MAYAUSD_LOCATION", "")],
    env_append: &[("MAYA_MODULE_PATH", "lib/maya/modules")]
);
