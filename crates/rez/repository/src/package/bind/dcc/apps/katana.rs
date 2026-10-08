// SPDX-License-Identifier: Apache-2.0

crate::dcc_scan!(
    KATANA, "katana", "Foundry Katana", &["katana"],
    win: ("C:/Program Files", "Katana"),
    mac: ("/Applications", "Katana"),
    linux: ("/opt", "Katana"),
    bin: "bin",
    env: &[("KATANA_ROOT", "")]
);
