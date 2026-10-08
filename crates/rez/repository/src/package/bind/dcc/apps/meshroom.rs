// SPDX-License-Identifier: Apache-2.0

crate::dcc_scan!(
    MESHROOM, "meshroom", "AliceVision Meshroom", &["meshroom", "Meshroom"],
    win: ("C:/Programs", "Meshroom"),
    mac: ("/Applications", "Meshroom"),
    linux: ("/opt", "Meshroom"),
    linux_alt: ("/usr/local", "Meshroom"),
    bin: "",
    env: &[("MESHROOM_ROOT", "")]
);
