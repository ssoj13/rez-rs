//! Tree editor for package details.
//!
//! Displays: envs, apps (with Launch), reqs, tags.

use std::cell::RefCell;

use eframe::egui::{self, Color32, RichText, Ui};

use super::prefs::AppState;
use super::storage::Storage;

/// Action returned from tree editor.
#[derive(Debug, Clone)]
pub enum TreeAction {
    Refresh,
}

/// Render tree editor panel.
pub fn render(ui: &mut Ui, state: &mut AppState, storage: &Storage) -> Option<TreeAction> {
    let action: RefCell<Option<TreeAction>> = RefCell::new(None);
    let Some(pkg_name) = &state.selection.package else {
        ui.allocate_ui_with_layout(
            ui.available_size(),
            eframe::egui::Layout::top_down(eframe::egui::Align::Center)
                .with_main_align(eframe::egui::Align::Center),
            |ui| {
                ui.label(RichText::new("Select a package from the list").color(Color32::GRAY));
            },
        );
        return None;
    };

    let Some(pkg) = storage.get(pkg_name) else {
        ui.label(RichText::new(format!("Package not found: {}", pkg_name)).color(Color32::RED));
        return None;
    };

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.heading(&pkg.name);
            if ui
                .small_button("↻")
                .on_hover_text("Refresh storage")
                .clicked()
            {
                *action.borrow_mut() = Some(TreeAction::Refresh);
            }
        });

        ui.label(RichText::new(format!("v{}", pkg.version)).color(Color32::GRAY));
        ui.add_space(8.0);

        // Envs
        let env_header = format!("envs ({})", pkg.envs.len());
        egui::CollapsingHeader::new(RichText::new(env_header).strong())
            .default_open(true)
            .show(ui, |ui| {
                if pkg.envs.is_empty() {
                    ui.label(RichText::new("(no environments)").color(Color32::GRAY));
                } else {
                    for env in &pkg.envs {
                        egui::CollapsingHeader::new(&env.name)
                            .default_open(true)
                            .show(ui, |ui| {
                                if env.evars.is_empty() {
                                    ui.label(RichText::new("(no variables)").color(Color32::GRAY));
                                } else {
                                    egui::Grid::new(format!("env_grid_{}", env.name))
                                        .striped(true)
                                        .show(ui, |ui| {
                                            for evar in &env.evars {
                                                ui.label(
                                                    RichText::new(&evar.name)
                                                        .color(Color32::LIGHT_BLUE),
                                                );
                                                ui.label("=");
                                                let val = if evar.value.len() > 50 {
                                                    format!("{}...", &evar.value[..47])
                                                } else {
                                                    evar.value.clone()
                                                };
                                                ui.label(&val);
                                                ui.end_row();
                                            }
                                        });
                                }
                            });
                    }
                }
            });

        // Apps
        let apps_header = format!("apps ({})", pkg.apps.len());
        egui::CollapsingHeader::new(RichText::new(apps_header).strong())
            .default_open(true)
            .show(ui, |ui| {
                if pkg.apps.is_empty() {
                    ui.label(RichText::new("(no applications)").color(Color32::GRAY));
                } else {
                    for app in &pkg.apps {
                        ui.horizontal(|ui| {
                            egui::CollapsingHeader::new(
                                RichText::new(&app.name).color(Color32::GREEN),
                            )
                            .default_open(false)
                            .show(ui, |ui| {
                                if let Some(path) = &app.path {
                                    ui.horizontal(|ui| {
                                        ui.label("path:");
                                        ui.label(RichText::new(path).color(Color32::GRAY));
                                    });
                                }
                            });

                            if ui.small_button("▶ Launch").clicked() {
                                super::actions::launch_app(pkg_name, &app.name, storage);
                            }
                        });
                    }
                }
            });

        // Reqs (read-only for now)
        let reqs_header = format!("reqs ({})", pkg.reqs.len());
        egui::CollapsingHeader::new(RichText::new(reqs_header).strong())
            .default_open(true)
            .show(ui, |ui| {
                if pkg.reqs.is_empty() {
                    ui.label(RichText::new("(no requirements)").color(Color32::GRAY));
                } else {
                    for req in &pkg.reqs {
                        ui.horizontal(|ui| {
                            ui.label("•");
                            ui.label(RichText::new(req).color(Color32::LIGHT_BLUE));
                        });
                    }
                }
            });

        // Tags
        let tags_header = format!("tags ({})", pkg.tags.len());
        egui::CollapsingHeader::new(RichText::new(tags_header).strong())
            .default_open(true)
            .show(ui, |ui| {
                if pkg.tags.is_empty() {
                    ui.label(RichText::new("(no tags)").color(Color32::GRAY));
                } else {
                    ui.horizontal_wrapped(|ui| {
                        for tag in &pkg.tags {
                            let color = tag_color(tag);
                            ui.label(RichText::new(format!("[{}]", tag)).color(color));
                        }
                    });
                }
            });
    });

    action.into_inner()
}

fn tag_color(tag: &str) -> Color32 {
    match tag {
        "dcc" => Color32::from_rgb(50, 205, 50),
        "render" | "renderer" => Color32::from_rgb(255, 140, 0),
        "plugin" | "ext" => Color32::from_rgb(186, 85, 211),
        _ => Color32::GRAY,
    }
}
