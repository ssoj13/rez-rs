// SPDX-License-Identifier: Apache-2.0

//! Per-app DCC modules. Use dcc_scan! where possible; raw AppDef for special cases.

mod blender;
mod cinema4d;
mod cursor;
mod davinci_resolve;
mod deadline;
mod embergen;
mod geogen;
mod houdini;
mod illugen;
mod katana;
mod liquigen;
mod mari;
mod maya;
mod maya_arnold;
mod maya_bifrost;
mod maya_lookdevx;
mod maya_usd;
mod meshroom;
mod nuke;
mod substance_designer;
mod substance_painter;
mod unreal;

pub use blender::BLENDER;
pub use cinema4d::CINEMA4D;
pub use cursor::CURSOR;
pub use davinci_resolve::DAVINCI_RESOLVE;
pub use deadline::DEADLINE;
pub use embergen::EMBERGEN;
pub use geogen::GEOGEN;
pub use houdini::HOUDINI;
pub use illugen::ILLUGEN;
pub use katana::KATANA;
pub use liquigen::LIQUIGEN;
pub use mari::MARI;
pub use maya::MAYA;
pub use maya_arnold::MAYA_ARNOLD;
pub use maya_bifrost::MAYA_BIFROST;
pub use maya_lookdevx::MAYA_LOOKDEVX;
pub use maya_usd::MAYA_USD;
pub use meshroom::MESHROOM;
pub use nuke::NUKE;
pub use substance_designer::SUBSTANCE_DESIGNER;
pub use substance_painter::SUBSTANCE_PAINTER;
pub use unreal::UNREAL;
