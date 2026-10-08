//! Rez GUI — browse packages, visualize dependency graphs, solve and export.
//!
//! Enable with `--features gui` and run with `rez gui` or `rez gui <file.rxt>`.

mod actions;
mod node_graph;
mod package_list;
mod prefs;
mod storage;
mod tree_editor;

pub use prefs::{AppState, RightPanel, Selection, ViewMode};

use crate::constants::ResolverStatus;
use actions::{ResolvedApp, SolveResult};
use resolve::context::ResolvedContext;
use std::path::Path;
use std::sync::mpsc;

/// Run the rez GUI application.
pub fn run(file: Option<std::path::PathBuf>) -> eframe::Result<()> {
    let state = AppState::load();

    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([state.window_width, state.window_height])
        .with_min_inner_size([800.0, 600.0]);

    if let (Some(x), Some(y)) = (state.window_x, state.window_y) {
        viewport = viewport.with_position([x, y]);
    }

    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(storage::Storage::scan(None));
    });

    eframe::run_native(
        "rez gui",
        options,
        Box::new(move |cc| Ok(Box::new(RezGuiApp::new(cc, rx, file)))),
    )
}

/// Main GUI application.
pub struct RezGuiApp {
    state: AppState,
    storage: Option<storage::Storage>,
    scan_receiver: Option<mpsc::Receiver<crate::errors::Result<storage::Storage>>>,
    refresh_receiver: Option<mpsc::Receiver<crate::errors::Result<storage::Storage>>>,
    scan_error: Option<String>,
    solve_result: SolveResult,
    node_graph_state: node_graph::NodeGraphState,
}

impl RezGuiApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        scan_receiver: mpsc::Receiver<crate::errors::Result<storage::Storage>>,
        file: Option<std::path::PathBuf>,
    ) -> Self {
        cc.egui_ctx.set_visuals(eframe::egui::Visuals::dark());

        let mut state = AppState::load();
        let graph_persist = state.graph_persist.take();
        let solve_result = file
            .as_deref()
            .map_or_else(SolveResult::default, load_context_result);

        Self {
            state,
            storage: None,
            scan_receiver: Some(scan_receiver),
            refresh_receiver: None,
            scan_error: None,
            solve_result,
            node_graph_state: node_graph::NodeGraphState::new(
                cc.wgpu_render_state.clone(),
                graph_persist,
            ),
        }
    }

    fn refresh_storage(&mut self) {
        if self.refresh_receiver.is_some() {
            return;
        }
        if let Some(ref s) = self.storage {
            let paths = s.location_paths();
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(storage::Storage::scan(Some(paths)));
            });
            self.refresh_receiver = Some(rx);
        }
    }

    fn poll_scan(&mut self) {
        let mut storage_changed = false;
        if let Some(rx) = self.scan_receiver.take() {
            match rx.try_recv() {
                Ok(Ok(s)) => {
                    self.storage = Some(s);
                    storage_changed = true;
                }
                Ok(Err(e)) => self.scan_error = Some(format!("{:?}", e)),
                Err(mpsc::TryRecvError::Empty) => self.scan_receiver = Some(rx),
                Err(mpsc::TryRecvError::Disconnected) => {}
            }
        }
        if let Some(rx) = self.refresh_receiver.take() {
            match rx.try_recv() {
                Ok(Ok(s)) => {
                    self.storage = Some(s);
                    storage_changed = true;
                }
                Ok(Err(_)) => {}
                Err(mpsc::TryRecvError::Empty) => self.refresh_receiver = Some(rx),
                Err(mpsc::TryRecvError::Disconnected) => {}
            }
        }
        if storage_changed {
            self.node_graph_state.invalidate();
        }
    }
}

fn load_context_result(path: &Path) -> SolveResult {
    let mut result = SolveResult {
        show: true,
        pkg_name: path.display().to_string(),
        ..SolveResult::default()
    };

    let context = match ResolvedContext::load(path, None) {
        Ok(context) => context,
        Err(error) => {
            result.error = Some(format!(
                "Failed to load context '{}': {error}",
                path.display()
            ));
            return result;
        }
    };

    if context.status != ResolverStatus::Solved {
        result.error = Some(context.failure_description.unwrap_or_else(|| {
            format!(
                "Context did not resolve successfully (status: {}).",
                context.status
            )
        }));
        return result;
    }

    if let Some(packages) = context.resolved_packages() {
        result
            .packages
            .extend(packages.iter().map(|package| package.qualified_name()));
    }

    let tools = match context.get_tools(false) {
        Ok(tools) => tools,
        Err(error) => {
            result.error = Some(error.to_string());
            return result;
        }
    };
    for (package_name, (_variant, tools)) in tools {
        result.apps.extend(tools.into_iter().map(|tool| {
            ResolvedApp {
                path: context
                    .which(&tool)
                    .map(|(_, path)| path.display().to_string()),
                name: tool,
                from_pkg: package_name.clone(),
            }
        }));
    }
    result
        .apps
        .sort_by(|left, right| (&left.name, &left.from_pkg).cmp(&(&right.name, &right.from_pkg)));

    match context.get_environ(None) {
        Ok(environment) => {
            result.env_lines = environment.into_iter().collect();
            result.env_lines.sort_by(|left, right| left.0.cmp(&right.0));
        }
        Err(error) => {
            result.error = Some(format!(
                "Failed to evaluate context environment '{}': {error}",
                path.display()
            ));
        }
    }

    result
}

impl eframe::App for RezGuiApp {
    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        self.state.graph_persist = self.node_graph_state.persist_state();
        self.state.save();
    }

    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        ctx.set_visuals(eframe::egui::Visuals::dark());

        self.poll_scan();

        if ctx.input(|i| i.key_pressed(eframe::egui::Key::Escape)) {
            ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
        }

        ctx.input(|i| {
            if let Some(rect) = i.viewport().inner_rect {
                self.state.window_width = rect.width();
                self.state.window_height = rect.height();
            }
            if let Some(pos) = i.viewport().outer_rect {
                self.state.window_x = Some(pos.min.x);
                self.state.window_y = Some(pos.min.y);
            }
        });

        if let Some(err) = self.scan_error.clone() {
            eframe::egui::CentralPanel::default().show(ui, |ui| {
                ui.allocate_ui_with_layout(
                    ui.available_size(),
                    eframe::egui::Layout::top_down(eframe::egui::Align::Center)
                        .with_main_align(eframe::egui::Align::Center),
                    |ui| {
                        ui.colored_label(eframe::egui::Color32::RED, "Failed to scan packages:");
                        ui.label(err);
                        if self.solve_result.show {
                            ui.separator();
                            actions::render_solve_inline(
                                ui,
                                &mut self.state,
                                &mut self.solve_result,
                            );
                        }
                    },
                );
            });
            return;
        }

        if self.storage.is_none() {
            eframe::egui::CentralPanel::default().show(ui, |ui| {
                ui.allocate_ui_with_layout(
                    ui.available_size(),
                    eframe::egui::Layout::top_down(eframe::egui::Align::Center)
                        .with_main_align(eframe::egui::Align::Center),
                    |ui| {
                        ui.spinner();
                        ui.label("Scanning packages...");
                    },
                );
            });
            ctx.request_repaint();
            return;
        }

        let is_refreshing = self.refresh_receiver.is_some();
        let list_action = {
            let storage = self.storage.as_ref().unwrap();
            let state = &mut self.state;
            eframe::egui::Panel::left("package_list")
                .default_size(state.left_panel_width.max(220.0))
                .min_size(180.0)
                .resizable(true)
                .show(ui, |ui| {
                    state.left_panel_width = ui.available_width();
                    if is_refreshing {
                        ui.spinner();
                        ui.label("Refreshing...");
                    }
                    package_list::render(ui, state, storage)
                })
                .inner
        };
        if let Some(action) = list_action {
            self.handle_list_action(action);
        }

        let storage = self.storage.as_ref().unwrap();
        let tree_action = {
            let state = &mut self.state;
            let solve_result = &mut self.solve_result;
            let node_graph_state = &mut self.node_graph_state;
            eframe::egui::Panel::top("top_panel").show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut state.view_mode, ViewMode::Packages, "Packages");
                    ui.selectable_value(&mut state.view_mode, ViewMode::Local, "Local");
                    ui.separator();
                    ui.selectable_value(&mut state.right_panel, prefs::RightPanel::Tree, "Tree");
                    ui.selectable_value(&mut state.right_panel, prefs::RightPanel::Graph, "Graph");
                });
            });
            eframe::egui::CentralPanel::default()
                .show(ui, |ui| {
                    let available = ui.available_height();
                    let has_solve = solve_result.show;

                    let top_height = if has_solve {
                        (available * 0.55).max(200.0)
                    } else {
                        available - 40.0
                    };

                    let tree_action = eframe::egui::Frame::default()
                        .show(ui, |ui| {
                            ui.set_max_height(top_height);
                            match state.right_panel {
                                prefs::RightPanel::Tree => tree_editor::render(ui, state, storage),
                                prefs::RightPanel::Graph => {
                                    node_graph::render(ui, state, storage, node_graph_state);
                                    None
                                }
                            }
                        })
                        .inner;

                    ui.separator();
                    actions::render(ui, state, storage, solve_result);
                    if has_solve {
                        ui.separator();
                        actions::render_solve_inline(ui, state, solve_result);
                    }
                    tree_action
                })
                .inner
        };

        if tree_action.is_some() {
            self.refresh_storage();
        }

        // Apply augment request from solve (so tree view shows tool paths)
        if let Some(ref mut storage) = self.storage {
            if let Some(req) = self.solve_result.augment_request.take() {
                storage.augment_package(&req.pkg_name, &req.resolved_pkgs, &req.env);
            }
        }
    }
}

impl RezGuiApp {
    fn handle_list_action(&mut self, action: package_list::ListAction) {
        use package_list::ListAction;

        match action {
            ListAction::Refresh => {
                self.refresh_storage();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_context_result_shows_load_error_with_path() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("broken.rxt");
        std::fs::write(&path, "{").expect("write invalid context");

        let result = load_context_result(&path);

        assert!(result.show);
        let error = result.error.expect("visible load error");
        assert!(error.contains("Failed to load context"));
        assert!(error.contains(&path.display().to_string()));
    }

    #[test]
    fn load_context_result_preserves_failed_context_description() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("failed.rxt");
        let mut context = ResolvedContext::empty();
        context.status = ResolverStatus::Failed;
        context.failure_description = Some("required package was not found".to_string());
        context.save(&path).expect("save failed context");

        let result = load_context_result(&path);

        assert!(result.show);
        assert_eq!(
            result.error.as_deref(),
            Some("required package was not found")
        );
    }
}
