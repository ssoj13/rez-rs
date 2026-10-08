//! GUI preferences — rez_gui.json in ~/.rez/.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn default_graph_depth() -> usize {
    4
}
fn default_solve_col1() -> f32 {
    0.15
}
fn default_solve_col2() -> f32 {
    0.15
}
fn default_window_width() -> f32 {
    1200.0
}
fn default_window_height() -> f32 {
    800.0
}
fn default_left_panel_width() -> f32 {
    250.0
}

/// Right panel mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RightPanel {
    #[default]
    Tree,
    Graph,
}

/// Current selection.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Selection {
    pub package: Option<String>,
    pub source_file: Option<String>,
    pub expanded: Vec<String>,
}

/// View mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ViewMode {
    #[default]
    Packages,
    Local,
}

/// Persistent GUI state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppState {
    pub view_mode: ViewMode,
    pub right_panel: RightPanel,
    pub selection: Selection,
    #[serde(default = "default_graph_depth")]
    pub graph_depth: usize,
    #[serde(default)]
    pub graph_persist: Option<nodes_editor::EditorPersist>,
    pub filter: String,
    #[serde(default)]
    pub local_only: bool,
    #[serde(default = "default_solve_col1")]
    pub solve_col1: f32,
    #[serde(default = "default_solve_col2")]
    pub solve_col2: f32,
    #[serde(default = "default_window_width")]
    pub window_width: f32,
    #[serde(default = "default_window_height")]
    pub window_height: f32,
    #[serde(default)]
    pub window_x: Option<f32>,
    #[serde(default)]
    pub window_y: Option<f32>,
    #[serde(default = "default_left_panel_width")]
    pub left_panel_width: f32,
    #[serde(default)]
    pub last_package_dir: Option<String>,
}

/// Path to rez_gui.json: ~/.rez/rez_gui.json
pub fn prefs_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".rez").join("rez_gui.json"))
}

impl AppState {
    pub fn load() -> Self {
        let Some(path) = prefs_path() else {
            return Self::default();
        };
        if !path.exists() {
            return Self::default();
        }
        match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let Some(path) = prefs_path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(&path, json);
        }
    }
}
