// SPDX-License-Identifier: Apache-2.0

//! Unified DCC app definitions via macro.
//!
//! Paths: (base, prefix) for single. Use linux_alt, mac_alt, win_alt for extra paths per platform.
//!
//! ## Examples
//! ```ignore
//! dcc_scan!(NUKE, "nuke", "Nuke", &["nuke"]; win: (...), mac: (...), linux: (...), bin: "bin", env: &[]);
//! dcc_scan!(MAYA, "maya", "Maya", &["maya"]; ..., all_versions: true);
//! dcc_scan!(MAYA_ARNOLD, ...; env_append: &[("MAYA_MODULE_PATH", "lib/maya/modules")]);
//! ```

#[macro_export]
macro_rules! dcc_scan {
    // Basic: single path per platform
    ($NAME:ident, $name:expr, $display:expr, $tools:expr,
     win: $win:expr, mac: $mac:expr, linux: $linux:expr,
     bin: $bin:expr, env: $env:expr) => {
        $crate::dcc_scan!($NAME, $name, $display, $tools,
            win: &[$win], mac: &[$mac], linux: &[$linux],
            bin: $bin, env: $env, all_versions: false, min_version: None, try_command_first: false, env_append: &[]);
    };
    // With all_versions (optional min_version filter)
    ($NAME:ident, $name:expr, $display:expr, $tools:expr,
     win: $win:expr, mac: $mac:expr, linux: $linux:expr,
     bin: $bin:expr, env: $env:expr, all_versions: $all:expr) => {
        $crate::dcc_scan!($NAME, $name, $display, $tools,
            win: &[$win], mac: &[$mac], linux: &[$linux],
            bin: $bin, env: $env, all_versions: $all, min_version: None, try_command_first: false, env_append: &[]);
    };
    ($NAME:ident, $name:expr, $display:expr, $tools:expr,
     win: $win:expr, mac: $mac:expr, linux: $linux:expr,
     bin: $bin:expr, env: $env:expr, all_versions: $all:expr, min_version: $min:expr) => {
        $crate::dcc_scan!($NAME, $name, $display, $tools,
            win: &[$win], mac: &[$mac], linux: &[$linux],
            bin: $bin, env: $env, all_versions: $all, min_version: Some($min), try_command_first: false, env_append: &[]);
    };
    // Multiple paths: linux_alt
    ($NAME:ident, $name:expr, $display:expr, $tools:expr,
     win: $win:expr, mac: $mac:expr, linux: $linux:expr, linux_alt: $linux_alt:expr,
     bin: $bin:expr, env: $env:expr) => {
        $crate::dcc_scan!($NAME, $name, $display, $tools,
            win: &[$win], mac: &[$mac], linux: &[$linux, $linux_alt],
            bin: $bin, env: $env, all_versions: false, min_version: None, try_command_first: false, env_append: &[]);
    };
    // Multiple paths: mac_alt
    ($NAME:ident, $name:expr, $display:expr, $tools:expr,
     win: $win:expr, mac: $mac:expr, mac_alt: $mac_alt:expr, linux: $linux:expr,
     bin: $bin:expr, env: $env:expr) => {
        $crate::dcc_scan!($NAME, $name, $display, $tools,
            win: &[$win], mac: &[$mac, $mac_alt], linux: &[$linux],
            bin: $bin, env: $env, all_versions: false, min_version: None, try_command_first: false, env_append: &[]);
    };
    // Multiple paths: linux_alt + mac_alt
    ($NAME:ident, $name:expr, $display:expr, $tools:expr,
     win: $win:expr, mac: $mac:expr, mac_alt: $mac_alt:expr, linux: $linux:expr, linux_alt: $linux_alt:expr,
     bin: $bin:expr, env: $env:expr) => {
        $crate::dcc_scan!($NAME, $name, $display, $tools,
            win: &[$win], mac: &[$mac, $mac_alt], linux: &[$linux, $linux_alt],
            bin: $bin, env: $env, all_versions: false, min_version: None, try_command_first: false, env_append: &[]);
    };
    // win_alt
    ($NAME:ident, $name:expr, $display:expr, $tools:expr,
     win: $win:expr, win_alt: $win_alt:expr, mac: $mac:expr, linux: $linux:expr,
     bin: $bin:expr, env: $env:expr) => {
        $crate::dcc_scan!($NAME, $name, $display, $tools,
            win: &[$win, $win_alt], mac: &[$mac], linux: &[$linux],
            bin: $bin, env: $env, all_versions: false, min_version: None, try_command_first: false, env_append: &[]);
    };
    // Maya modules: env_append for MAYA_MODULE_PATH
    ($NAME:ident, $name:expr, $display:expr, $tools:expr,
     win: $win:expr, mac: $mac:expr, linux: $linux:expr,
     bin: $bin:expr, env: $env:expr, env_append: $env_append:expr) => {
        $crate::dcc_scan!($NAME, $name, $display, $tools,
            win: &[$win], mac: &[$mac], linux: &[$linux],
            bin: $bin, env: $env, all_versions: false, min_version: None, try_command_first: false, env_append: $env_append);
    };
    // Full: paths as arrays, all options, bin as string -> Same(bin)
    ($NAME:ident, $name:expr, $display:expr, $tools:expr,
     win: $win:expr, mac: $mac:expr, linux: $linux:expr,
     bin: $bin:expr, env: $env:expr, all_versions: $all:expr, min_version: $min:expr, try_command_first: $cmd:expr, env_append: $env_append:expr) => {
        pub const $NAME: $crate::package::bind::detect::AppDef = $crate::package::bind::detect::AppDef {
            name: $name,
            display: $display,
            tools: $tools,
            search: $crate::package::bind::detect::SearchMethod::Scan {
                windows: $win,
                macos: $mac,
                linux: $linux,
                min_version: $min,
            },
            bin_sub: $crate::package::bind::detect::BinSub::Same($bin),
            env_vars: $env,
            version: $crate::package::bind::detect::VersionDetect::DirName,
            bind_all_versions: $all,
            try_command_first: $cmd,
            env_append: $env_append,
            path_prepend_dirs: &[],
            env_prepend: &[],
        };
    };
    // bin: per_platform(default, macos) — e.g. Blender, Nuke
    ($NAME:ident, $name:expr, $display:expr, $tools:expr,
     win: $win:expr, mac: $mac:expr, linux: $linux:expr,
     bin: per_platform($bin_default:expr, $bin_macos:expr),
     env: $env:expr) => {
        pub const $NAME: $crate::package::bind::detect::AppDef = $crate::package::bind::detect::AppDef {
            name: $name,
            display: $display,
            tools: $tools,
            search: $crate::package::bind::detect::SearchMethod::Scan {
                windows: $win,
                macos: $mac,
                linux: $linux,
                min_version: None,
            },
            bin_sub: $crate::package::bind::detect::BinSub::PerPlatform {
                default: $bin_default,
                macos: $bin_macos,
            },
            env_vars: $env,
            version: $crate::package::bind::detect::VersionDetect::DirName,
            bind_all_versions: false,
            try_command_first: false,
            env_append: &[],
            path_prepend_dirs: &[],
            env_prepend: &[],
        };
    };
    // bin: per_platform + version: command + try_command_first — e.g. Blender. Paths as arrays (mac can be &[]).
    ($NAME:ident, $name:expr, $display:expr, $tools:expr,
     win: $win:expr, mac: $mac:expr, linux: $linux:expr,
     bin: per_platform($bin_default:expr, $bin_macos:expr),
     env: $env:expr, version: command($vcmd:expr, $vargs:expr, $vprefix:expr), try_command_first: $cmd:expr) => {
        pub const $NAME: $crate::package::bind::detect::AppDef = $crate::package::bind::detect::AppDef {
            name: $name,
            display: $display,
            tools: $tools,
            search: $crate::package::bind::detect::SearchMethod::Scan {
                windows: $win,
                macos: $mac,
                linux: $linux,
                min_version: None,
            },
            bin_sub: $crate::package::bind::detect::BinSub::PerPlatform {
                default: $bin_default,
                macos: $bin_macos,
            },
            env_vars: $env,
            version: $crate::package::bind::detect::VersionDetect::Command {
                cmd: $vcmd,
                args: $vargs,
                prefix: $vprefix,
            },
            bind_all_versions: false,
            try_command_first: $cmd,
            env_append: &[],
            path_prepend_dirs: &[],
            env_prepend: &[],
        };
    };
}
