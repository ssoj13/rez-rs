// SPDX-License-Identifier: Apache-2.0

//! VFX / DCC application bind definitions.
//!
//! Apps live in `apps/` subdir; `common` provides the `dcc_scan!` macro for minimal definitions.

mod apps;
mod common; // provides dcc_scan! at crate root

use super::detect::{AppDef, BindDef};
use super::BindInfo;

pub use apps::*;

/// All DCC application definitions.
pub const ALL_APPS: &[&AppDef] = &[
    &MAYA,
    &HOUDINI,
    &BLENDER,
    &NUKE,
    &MARI,
    &KATANA,
    &SUBSTANCE_DESIGNER,
    &SUBSTANCE_PAINTER,
    &EMBERGEN,
    &LIQUIGEN,
    &GEOGEN,
    &ILLUGEN,
    &DAVINCI_RESOLVE,
    &MAYA_USD,
    &MAYA_LOOKDEVX,
    &MAYA_BIFROST,
    &MAYA_ARNOLD,
    &DEADLINE,
    &UNREAL,
    &MESHROOM,
    &CINEMA4D,
    &CURSOR,
];

/// Detect a DCC app by its definition. Returns all versions when bind_all_versions; else single.
pub fn detect_dcc<D: BindDef>(def: &D) -> Option<Vec<BindInfo>> {
    let infos = def.detect_versions();
    if infos.is_empty() {
        None
    } else {
        Some(infos)
    }
}
