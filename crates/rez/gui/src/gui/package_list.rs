//! Package list panel (left side).
//!
//! Modes: Packages (all), Local (local_packages_path only).

use std::cell::RefCell;

use eframe::egui::{self, Color32, RichText, Ui};

use super::prefs::AppState;
use super::storage::{GuiPackage, Storage};
use crate::config::CONFIG;

/// Action returned from package list.
#[derive(Debug, Clone)]
pub enum ListAction {
    Refresh,
}

/// Render packages grouped by base name.
fn render_packages(ui: &mut Ui, state: &mut AppState, packages: &[&GuiPackage]) {
    let mut bases: Vec<&str> = packages.iter().map(|p| p.base.as_str()).collect();
    bases.sort();
    bases.dedup();

    for base in bases {
        let mut versions: Vec<_> = packages
            .iter()
            .filter(|p| p.base == base)
            .copied()
            .collect();
        versions.sort_by(|a, b| super::storage::compare_versions(&a.version, &b.version));

        if versions.len() == 1 {
            let pkg = versions[0];
            let selected = state.selection.package.as_ref() == Some(&pkg.name);
            if ui.selectable_label(selected, &pkg.name).clicked() {
                state.selection.package = Some(pkg.name.clone());
            }
        } else {
            egui::CollapsingHeader::new(base)
                .default_open(false)
                .show(ui, |ui| {
                    for pkg in versions {
                        let selected = state.selection.package.as_ref() == Some(&pkg.name);
                        if ui.selectable_label(selected, &pkg.version).clicked() {
                            state.selection.package = Some(pkg.name.clone());
                        }
                    }
                });
        }
    }
}

/// Render package list panel.
pub fn render(ui: &mut Ui, state: &mut AppState, storage: &Storage) -> Option<ListAction> {
    let action: RefCell<Option<ListAction>> = RefCell::new(None);
    let local_path = CONFIG.expanded_local_packages_path().to_os();

    let (title, packages): (_, Vec<_>) = match state.view_mode {
        super::prefs::ViewMode::Packages => {
            let pkgs: Vec<_> = storage.packages_iter().collect();
            ("Packages", pkgs)
        }
        super::prefs::ViewMode::Local => {
            let pkgs: Vec<_> = storage
                .packages_iter()
                .filter(|p| {
                    p.package_source
                        .as_ref()
                        .map(|s| std::path::Path::new(s).starts_with(&local_path))
                        .unwrap_or(false)
                })
                .collect();
            ("Local", pkgs)
        }
    };

    let count = packages.len();

    ui.horizontal(|ui| {
        ui.heading(title);
        ui.label(RichText::new(format!("({})", count)).color(Color32::GRAY));
    });

    ui.horizontal(|ui| {
        ui.label("Filter:");
        ui.add(
            egui::TextEdit::singleline(&mut state.filter)
                .hint_text("package name...")
                .desired_width(120.0),
        );
        if ui.small_button("↻").on_hover_text("Refresh").clicked() {
            *action.borrow_mut() = Some(ListAction::Refresh);
        }
    });

    ui.separator();

    let filter_lower = state.filter.to_lowercase();
    let packages: Vec<_> = packages
        .into_iter()
        .filter(|pkg| filter_lower.is_empty() || pkg.name.to_lowercase().contains(&filter_lower))
        .collect();

    egui::ScrollArea::vertical().show(ui, |ui| {
        if packages.is_empty() {
            ui.label(RichText::new("(no matches)").color(Color32::GRAY));
            return;
        }

        let refs: Vec<&GuiPackage> = packages.to_vec();
        render_packages(ui, state, &refs);
    });

    action.into_inner()
}
