// SPDX-License-Identifier: Apache-2.0

crate::dcc_scan!(
    MARI, "mari", "Foundry Mari", &["mari"],
    win: ("C:/Program Files", "Mari"),
    mac: ("/Applications", "Mari"),
    linux: ("/usr/local", "Mari"),
    linux_alt: ("/opt", "Mari"),
    bin: "bin",
    env: &[]
);
