//! Action buttons: Solve, Launch, Export.

use eframe::egui::{self, Color32, RichText, Ui};

use crate::config::CONFIG;
use repository::provider::FilesystemPackageProvider;
use resolve::context::{ResolveOptions, ResolvedContext};
use version::Requirement;

use super::prefs::AppState;
use super::storage::Storage;

/// Resolved app info.
#[derive(Debug, Clone, Default)]
pub struct ResolvedApp {
    pub name: String,
    pub path: Option<String>,
    /// Package providing this tool (e.g. "maya-2024"); shown in UI tooltip.
    pub from_pkg: String,
}

/// Request to augment a package in storage with resolved data (paths, env).
/// Set when solve succeeds; main loop applies it so tree view shows correct paths.
#[derive(Debug, Clone, Default)]
pub struct AugmentRequest {
    pub pkg_name: String,
    pub resolved_pkgs: Vec<resolve::resolver::ResolvedPackageInfo>,
    pub env: std::collections::HashMap<String, String>,
}

/// Solve result for display.
#[derive(Debug, Clone, Default)]
pub struct SolveResult {
    pub show: bool,
    pub pkg_name: String,
    pub packages: Vec<String>,
    pub apps: Vec<ResolvedApp>,
    pub env_lines: Vec<(String, String)>,
    pub error: Option<String>,
    /// Pending augment: applied by main loop so storage package gets paths/env
    pub augment_request: Option<AugmentRequest>,
}

/// Render action buttons.
pub fn render(
    ui: &mut Ui,
    state: &mut AppState,
    storage: &Storage,
    solve_result: &mut SolveResult,
) {
    let has_selection = state.selection.package.is_some();

    ui.horizontal(|ui| {
        ui.add_enabled_ui(has_selection, |ui| {
            if ui.button("Solve").clicked() {
                if let Some(pkg_name) = &state.selection.package {
                    run_solve(pkg_name, storage, solve_result);
                }
            }
        });

        ui.separator();

        let has_env = solve_result.show && !solve_result.env_lines.is_empty();
        ui.add_enabled_ui(has_env, |ui| {
            if ui.button("Export .cmd").clicked() {
                export_env(solve_result, ExportFormat::Cmd);
            }
            if ui.button("Export .ps1").clicked() {
                export_env(solve_result, ExportFormat::Ps1);
            }
            if ui.button("Export .sh").clicked() {
                export_env(solve_result, ExportFormat::Sh);
            }
        });

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(pkg_name) = &state.selection.package {
                ui.label(RichText::new(pkg_name).color(Color32::LIGHT_BLUE));
            } else {
                ui.label(RichText::new("No selection").color(Color32::GRAY));
            }
        });
    });
}

/// Render solve result inline.
pub fn render_solve_inline(ui: &mut Ui, state: &mut AppState, result: &mut SolveResult) {
    ui.horizontal(|ui| {
        ui.heading(&result.pkg_name);
        if result.error.is_none() {
            ui.label(
                RichText::new(format!("→ {} packages", result.packages.len()))
                    .color(Color32::GREEN),
            );
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("×").on_hover_text("Close").clicked() {
                result.show = false;
            }
        });
    });

    if let Some(err) = &result.error {
        ui.colored_label(Color32::RED, "Resolution failed:");
        egui::ScrollArea::vertical()
            .max_height(150.0)
            .show(ui, |ui| {
                ui.label(err);
            });
    } else {
        let total_width = ui.available_width();
        let available_height = ui.available_height();

        state.solve_col1 = state.solve_col1.clamp(0.05, 0.4);
        state.solve_col2 = state.solve_col2.clamp(0.05, 0.4);

        let pkg_width = total_width * state.solve_col1;
        let apps_width = total_width * state.solve_col2;
        let env_width = total_width * (1.0 - state.solve_col1 - state.solve_col2 - 0.02);

        ui.horizontal_top(|ui| {
            ui.set_min_height(available_height);

            ui.vertical(|ui| {
                ui.set_width(pkg_width);
                ui.set_min_height(available_height);
                ui.strong("Packages");
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (i, pkg) in result.packages.iter().enumerate() {
                            let color = if i == 0 {
                                Color32::YELLOW
                            } else {
                                Color32::LIGHT_GRAY
                            };
                            ui.label(RichText::new(format!("{}. {}", i + 1, pkg)).color(color));
                        }
                    });
            });

            let sep1 = ui.separator();
            let sep1_rect = sep1.rect.expand2(egui::vec2(4.0, 0.0));
            let sep1_id = ui.id().with("sep1");
            let sep1_response = ui.interact(sep1_rect, sep1_id, egui::Sense::drag());
            if sep1_response.dragged() {
                let delta = sep1_response.drag_delta().x / total_width;
                state.solve_col1 = (state.solve_col1 + delta).clamp(0.05, 0.4);
            }
            if sep1_response.hovered() || sep1_response.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }

            ui.vertical(|ui| {
                ui.set_width(apps_width);
                ui.set_min_height(available_height);
                ui.strong("Apps");
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if result.apps.is_empty() {
                            ui.label(RichText::new("(no apps)").color(Color32::GRAY));
                        } else {
                            for app in &result.apps {
                                ui.horizontal(|ui| {
                                    let path_str = app.path.as_deref().unwrap_or("(no path)");
                                    let hover = if app.from_pkg.is_empty() {
                                        path_str.to_string()
                                    } else {
                                        format!("{} (from {})", path_str, app.from_pkg)
                                    };
                                    if ui
                                        .small_button("▶")
                                        .on_hover_text(format!("Launch: {}", hover))
                                        .clicked()
                                    {
                                        launch_with_env(
                                            &app.name,
                                            app.path.as_deref(),
                                            &result.env_lines,
                                        );
                                    }
                                    ui.label(RichText::new(&app.name).color(Color32::GREEN))
                                        .on_hover_text(hover);
                                });
                            }
                        }
                    });
            });

            let sep2 = ui.separator();
            let sep2_rect = sep2.rect.expand2(egui::vec2(4.0, 0.0));
            let sep2_id = ui.id().with("sep2");
            let sep2_response = ui.interact(sep2_rect, sep2_id, egui::Sense::drag());
            if sep2_response.dragged() {
                let delta = sep2_response.drag_delta().x / total_width;
                state.solve_col2 = (state.solve_col2 + delta).clamp(0.05, 0.4);
            }
            if sep2_response.hovered() || sep2_response.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }

            ui.vertical(|ui| {
                ui.set_width(env_width);
                ui.set_min_height(available_height);
                ui.strong("Environment");
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if result.env_lines.is_empty() {
                            ui.label(RichText::new("(no env vars)").color(Color32::GRAY));
                        } else {
                            for (name, value) in &result.env_lines {
                                egui::CollapsingHeader::new(
                                    RichText::new(name).color(Color32::LIGHT_BLUE),
                                )
                                .default_open(false)
                                .show(ui, |ui| {
                                    ui.label(RichText::new(value).color(Color32::GRAY));
                                });
                            }
                        }
                    });
            });
        });
    }
}

fn run_solve(pkg_name: &str, _storage: &Storage, result: &mut SolveResult) {
    result.pkg_name = pkg_name.to_string();
    result.packages.clear();
    result.apps.clear();
    result.env_lines.clear();
    result.error = None;
    result.augment_request = None;

    let req = match Requirement::new(pkg_name) {
        Ok(r) => r,
        Err(e) => {
            result.error = Some(format!("Invalid package request '{}': {}", pkg_name, e));
            result.show = true;
            return;
        }
    };

    let paths = CONFIG.expanded_packages_path_os();
    let provider = match FilesystemPackageProvider::from_paths(&paths) {
        Ok(provider) => provider,
        Err(error) => {
            result.error = Some(error.to_string());
            result.show = true;
            return;
        }
    };

    let opts = ResolveOptions {
        package_paths: Some(paths),
        ..Default::default()
    };

    match ResolvedContext::resolve(vec![req], &provider, opts) {
        Ok(ctx) => {
            if ctx.status != crate::constants::ResolverStatus::Solved {
                result.error = Some(
                    ctx.failure_description
                        .unwrap_or_else(|| "Resolution failed".to_string()),
                );
                result.show = true;
                return;
            }

            for pkg in ctx.resolved_packages().into_iter().flatten() {
                result.packages.push(pkg.qualified_name());
            }

            let tools = match ctx.get_tools(false) {
                Ok(tools) => tools,
                Err(error) => {
                    result.error = Some(error.to_string());
                    result.show = true;
                    return;
                }
            };
            for (pkg_name, (_variant, tools_list)) in tools {
                for tool in tools_list {
                    let path = ctx.which(&tool).map(|(_, p)| p.display().to_string());
                    result.apps.push(ResolvedApp {
                        name: tool,
                        path,
                        from_pkg: pkg_name.clone(),
                    });
                }
            }

            match ctx.get_environ(None) {
                Ok(env) => {
                    let mut env_vec: Vec<_> =
                        env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                    env_vec.sort_by(|a, b| a.0.cmp(&b.0));
                    result.env_lines = env_vec;

                    // Request augment so tree view shows paths/env for this package
                    let resolved: Vec<_> = ctx
                        .resolved_packages()
                        .into_iter()
                        .flatten()
                        .cloned()
                        .collect();
                    if !resolved.is_empty() {
                        result.augment_request = Some(AugmentRequest {
                            pkg_name: pkg_name.to_string(),
                            resolved_pkgs: resolved,
                            env,
                        });
                    }
                }
                Err(e) => {
                    result.error = Some(format!("get_environ: {:?}", e));
                }
            }

            result.show = true;
        }
        Err(e) => {
            result.error = Some(format!("{:?}", e));
            result.show = true;
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum ExportFormat {
    Cmd,
    Ps1,
    Sh,
}

/// Launch app from tree editor (uses resolve + which).
pub fn launch_app(pkg_name: &str, app_name: &str, _storage: &Storage) {
    let req = match Requirement::new(pkg_name) {
        Ok(r) => r,
        Err(_) => return,
    };

    let paths = CONFIG.expanded_packages_path_os();
    let provider = match FilesystemPackageProvider::from_paths(&paths) {
        Ok(provider) => provider,
        Err(error) => {
            eprintln!("Failed to open application package repositories: {error}");
            return;
        }
    };
    let opts = ResolveOptions {
        package_paths: Some(paths),
        ..Default::default()
    };

    let Ok(ctx) = ResolvedContext::resolve(vec![req], &provider, opts) else {
        return;
    };
    if ctx.status != crate::constants::ResolverStatus::Solved {
        return;
    }

    let Some((_, exe_path)) = ctx.which(app_name) else {
        return;
    };

    let Ok(env) = ctx.get_environ(None) else {
        return;
    };

    let mut cmd = std::process::Command::new(&exe_path);
    cmd.envs(&env);
    let _ = cmd.spawn();
}

fn launch_with_env(_app_name: &str, path: Option<&str>, env_lines: &[(String, String)]) {
    let Some(exe_path) = path else {
        return;
    };

    let mut cmd = std::process::Command::new(exe_path);
    for (k, v) in env_lines {
        cmd.env(k, v);
    }
    let _ = cmd.spawn();
}

fn export_env(result: &SolveResult, format: ExportFormat) {
    if result.env_lines.is_empty() {
        return;
    }

    let script = match render_export(&result.env_lines, format) {
        Ok(script) => script,
        Err(error) => {
            rfd::MessageDialog::new()
                .set_title("Environment export failed")
                .set_description(error)
                .set_level(rfd::MessageLevel::Error)
                .show();
            return;
        }
    };

    let (ext, filter_name) = match format {
        ExportFormat::Cmd => ("cmd", "Windows Batch"),
        ExportFormat::Ps1 => ("ps1", "PowerShell"),
        ExportFormat::Sh => ("sh", "Shell Script"),
    };

    let default_name = format!("{}.{}", result.pkg_name, ext);

    let file = rfd::FileDialog::new()
        .set_title("Export Environment")
        .set_file_name(&default_name)
        .add_filter(filter_name, &[ext])
        .add_filter("All files", &["*"])
        .save_file();

    if let Some(path) = file {
        let _ = std::fs::write(&path, &script);
    }
}

fn render_export(env_lines: &[(String, String)], format: ExportFormat) -> Result<String, String> {
    for (key, value) in env_lines {
        if !valid_env_name(key) {
            return Err(format!("Invalid environment variable name: {key:?}"));
        }
        if value.contains('\0') {
            return Err(format!("Environment variable {key} contains a NUL byte."));
        }
        if matches!(format, ExportFormat::Cmd) && (value.contains('\r') || value.contains('\n')) {
            return Err(format!(
                "Windows batch cannot represent line breaks in {key}."
            ));
        }
        if matches!(format, ExportFormat::Cmd) && value.contains('"') {
            return Err(format!(
                "Windows batch cannot safely represent double quotes in {key}."
            ));
        }
        if matches!(format, ExportFormat::Cmd) && value.contains('!') {
            return Err(format!(
                "Windows batch cannot safely represent exclamation marks in {key}."
            ));
        }
    }

    let mut script = match format {
        ExportFormat::Cmd => String::from("@echo off\r\n"),
        ExportFormat::Ps1 => String::new(),
        ExportFormat::Sh => String::from("#!/bin/bash\n"),
    };
    for (key, value) in env_lines {
        match format {
            ExportFormat::Cmd => {
                script.push_str(&format!("set \"{key}={}\"\r\n", escape_cmd_value(value)));
            }
            ExportFormat::Ps1 => {
                script.push_str(&format!("$env:{key} = '{}'\n", value.replace('\'', "''")));
            }
            ExportFormat::Sh => {
                let literal = format!("'{}'", value.replace('\'', "'\\''"));
                script.push_str(&format!("export {key}={literal}\n"));
            }
        }
    }
    Ok(script)
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('_' | 'A'..='Z' | 'a'..='z'))
        && chars.all(|ch| matches!(ch, '_' | 'A'..='Z' | 'a'..='z' | '0'..='9'))
}

fn escape_cmd_value(value: &str) -> String {
    value.replace('%', "%%")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_line(key: &str, value: &str) -> Vec<(String, String)> {
        vec![(key.to_string(), value.to_string())]
    }

    #[test]
    fn export_scripts_quote_shell_metacharacters_as_literals() {
        let value = "a'b \"$HOME $(touch /tmp/rez-export-marker) `cmd` %PATH% ! & |";
        let lines = env_line("TEST_VALUE", value);

        let sh = render_export(&lines, ExportFormat::Sh).unwrap();
        assert!(sh.contains(
            "export TEST_VALUE='a'\\''b \"$HOME $(touch /tmp/rez-export-marker) `cmd` %PATH% ! & |'"
        ));

        let ps = render_export(&lines, ExportFormat::Ps1).unwrap();
        assert!(ps.contains(
            "$env:TEST_VALUE = 'a''b \"$HOME $(touch /tmp/rez-export-marker) `cmd` %PATH% ! & |'"
        ));

        let cmd_value = "a'b %PATH% & |";
        let cmd = render_export(&env_line("TEST_VALUE", cmd_value), ExportFormat::Cmd).unwrap();
        assert!(cmd.contains("set \"TEST_VALUE=a'b %%PATH%% & |\""));
    }

    #[test]
    fn export_rejects_invalid_environment_names_and_unrepresentable_values() {
        for name in ["", "1START", "HAS-DASH", "HAS SPACE", "HAS=EQUALS"] {
            assert!(
                render_export(&env_line(name, "value"), ExportFormat::Sh).is_err(),
                "{name:?}"
            );
        }
        assert!(render_export(&env_line("VALID", "nul\0byte"), ExportFormat::Sh).is_err());
        assert!(render_export(&env_line("VALID", "line\nbreak"), ExportFormat::Cmd).is_err());
        assert!(render_export(&env_line("VALID", "double\"quote"), ExportFormat::Cmd).is_err());
        assert!(render_export(&env_line("VALID", "bang!value"), ExportFormat::Cmd).is_err());
        assert!(render_export(&env_line("VALID", "bang!value"), ExportFormat::Sh).is_ok());
        assert!(render_export(&env_line("VALID", "line\nbreak"), ExportFormat::Sh).is_ok());
    }
}
