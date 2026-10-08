//! Rez package dependency graph hosted by the reusable nodes-rs editor widget.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use eframe::egui::{self, Ui};
use nodes_core::{
    auto_layout_subnet, fn_node, NodeDef, NodeId, NodeLibrary, NodeType, NodeTypeRegistry, PortDef,
    Subnet, ValueTypes,
};
use nodes_editor::{CookPolicy, EditorPersist, EditorWidget};

use super::prefs::AppState;
use super::storage::{GuiPackage, Storage};

#[derive(Clone)]
struct PackageRequirement {
    parsed: Option<version::Requirement>,
    port_name: String,
}

#[derive(Clone)]
struct GraphPackage {
    package: GuiPackage,
    type_name: String,
    requirements: Vec<PackageRequirement>,
}

#[derive(Clone)]
struct PackageNodeDef {
    display_name: String,
    type_name: String,
    input_ports: Vec<String>,
}

struct RezPackageNodes {
    packages: Vec<PackageNodeDef>,
}

impl NodeLibrary for RezPackageNodes {
    fn name(&self) -> &str {
        "rez"
    }

    fn types(&self) -> Vec<Arc<dyn NodeType>> {
        self.packages
            .iter()
            .map(|package| {
                let inputs = package
                    .input_ports
                    .iter()
                    .map(|port_name| PortDef::any(port_name.as_str()))
                    .collect();
                let def = NodeDef::new(package.type_name.as_str(), "Rez Packages")
                    .with_display(package.display_name.as_str())
                    .with_inputs(inputs)
                    .with_outputs(vec![PortDef::any("package")]);
                fn_node(def, |_| Ok(()))
            })
            .collect()
    }
}

/// Editor runtime state. The graph and widget are rebuilt when the selected package or
/// depth changes; only the editor's camera and view settings are persisted.
pub struct NodeGraphState {
    editor: Option<EditorWidget>,
    render_state: Option<eframe::egui_wgpu::RenderState>,
    persist: Option<EditorPersist>,
    current_pkg: Option<String>,
    node_count: usize,
    needs_rebuild: bool,
}

impl NodeGraphState {
    pub fn new(
        render_state: Option<eframe::egui_wgpu::RenderState>,
        persist: Option<EditorPersist>,
    ) -> Self {
        Self {
            editor: None,
            render_state,
            persist,
            current_pkg: None,
            node_count: 0,
            needs_rebuild: true,
        }
    }

    pub fn invalidate(&mut self) {
        self.needs_rebuild = true;
    }

    pub fn persist_state(&self) -> Option<EditorPersist> {
        self.editor
            .as_ref()
            .map(EditorWidget::persist_state)
            .or_else(|| self.persist.clone())
    }

    fn set_package(&mut self, package: &str) {
        if self.current_pkg.as_deref() != Some(package) {
            self.current_pkg = Some(package.to_string());
            self.needs_rebuild = true;
        }
    }

    fn rebuild(&mut self, storage: &Storage, max_depth: usize) {
        if let Some(editor) = self.editor.as_ref() {
            self.persist = Some(editor.persist_state());
        }
        self.needs_rebuild = false;
        self.node_count = 0;

        let Some(render_state) = self.render_state.as_ref() else {
            self.editor = None;
            return;
        };
        let Some(root_name) = self.current_pkg.as_deref() else {
            self.editor = None;
            return;
        };
        let Some(root_package) = storage.get(root_name) else {
            self.editor = None;
            return;
        };

        let mut packages = BTreeMap::<String, GraphPackage>::new();
        let mut visited = HashSet::new();
        let mut queue = VecDeque::from([(root_package.name.clone(), 0usize)]);

        while let Some((package_name, depth)) = queue.pop_front() {
            if !visited.insert(package_name.clone()) {
                continue;
            }
            let Some(package) = storage.get(&package_name) else {
                continue;
            };

            let requirements = package
                .reqs
                .iter()
                .enumerate()
                .map(|(index, source)| {
                    let parsed = version::Requirement::new(source).ok();
                    let port_name = requirement_port_name(index, parsed.as_ref(), source);
                    PackageRequirement { parsed, port_name }
                })
                .collect::<Vec<_>>();

            let graph_package = GraphPackage {
                package: package.clone(),
                type_name: String::new(),
                requirements,
            };
            if depth < max_depth {
                for requirement in &graph_package.requirements {
                    let Some(parsed) = requirement.parsed.as_ref() else {
                        continue;
                    };
                    if parsed.conflict() {
                        continue;
                    }
                    if let Some(dependency) = storage.latest(parsed.name(), Some(parsed)) {
                        if !visited.contains(&dependency.name) {
                            queue.push_back((dependency.name.clone(), depth + 1));
                        }
                    }
                }
            }
            packages.insert(package_name, graph_package);
        }

        for (index, package) in packages.values_mut().enumerate() {
            package.type_name = format!("rez_package_{index}");
        }

        let package_nodes = RezPackageNodes {
            packages: packages
                .values()
                .map(|package| PackageNodeDef {
                    display_name: package.package.base.clone(),
                    type_name: package.type_name.clone(),
                    input_ports: package
                        .requirements
                        .iter()
                        .map(|requirement| requirement.port_name.clone())
                        .collect(),
                })
                .collect(),
        };
        let mut registry = NodeTypeRegistry::new();
        let mut value_types = ValueTypes::new();
        registry.add_library(&package_nodes, &mut value_types);

        let mut subnet = Subnet::new();
        let mut node_ids = HashMap::<String, NodeId>::new();
        for package in packages.values() {
            if let Ok(id) =
                subnet.create_node(&package.type_name, [0.0, 0.0], &registry, &value_types)
            {
                if let Some(node) = subnet.nodes.get_mut(&id) {
                    let display_name = if package.package.name == root_name {
                        format!("[ROOT] {}", package.package.name)
                    } else {
                        package.package.name.clone()
                    };
                    node.set_display_name(&display_name);
                }
                node_ids.insert(package.package.name.clone(), id);
            }
        }

        for package in packages.values() {
            let Some(&consumer) = node_ids.get(&package.package.name) else {
                continue;
            };
            for requirement in &package.requirements {
                let Some(parsed) = requirement.parsed.as_ref() else {
                    continue;
                };
                if parsed.conflict() {
                    continue;
                }
                let Some(dependency) = storage.latest(parsed.name(), Some(parsed)) else {
                    continue;
                };
                let Some(&provider) = node_ids.get(&dependency.name) else {
                    continue;
                };
                let _ = subnet.connect(
                    provider,
                    "package",
                    consumer,
                    &requirement.port_name,
                    &registry,
                    &value_types,
                );
            }
        }

        auto_layout_subnet(&mut subnet);
        self.node_count = subnet.nodes.len();

        let mut editor = EditorWidget::with_subnet_bare(render_state, &[&package_nodes], subnet);
        editor.set_file_actions_enabled(false);
        editor.set_cook_policy(CookPolicy::Manual);
        if let Some(persist) = self.persist.as_ref() {
            editor.restore_state(persist);
        }
        self.persist = None;
        self.editor = Some(editor);
    }

    fn layout(&mut self) {
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        auto_layout_subnet(&mut editor.subnet_mut());
        editor.refresh_layout();
    }
}

fn requirement_port_name(
    index: usize,
    requirement: Option<&version::Requirement>,
    source: &str,
) -> String {
    let name = requirement.map_or(source, version::Requirement::name);
    let suffix: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    format!("dep_{index}_{suffix}")
}

pub fn render(
    ui: &mut Ui,
    state: &mut AppState,
    storage: &Storage,
    graph_state: &mut NodeGraphState,
) {
    let Some(package_name) = state.selection.package.as_deref() else {
        ui.centered_and_justified(|ui| {
            ui.label("Select a package to view its dependency graph");
        });
        return;
    };

    if storage.get(package_name).is_none() {
        ui.label(format!("Package not found: {package_name}"));
        return;
    }

    graph_state.set_package(package_name);
    if graph_state.needs_rebuild {
        graph_state.rebuild(storage, state.graph_depth);
    }

    ui.horizontal(|ui| {
        ui.label("Depth:");
        if ui
            .add(egui::Slider::new(&mut state.graph_depth, 0..=10))
            .changed()
        {
            graph_state.needs_rebuild = true;
        }

        ui.separator();
        ui.label(format!("{} nodes", graph_state.node_count));

        ui.separator();
        if ui
            .button("Layout")
            .on_hover_text("Arrange package nodes")
            .clicked()
        {
            graph_state.layout();
        }

        ui.separator();
        ui.weak("Drag to move · F: frame · TAB: node menu");
    });
    ui.separator();

    if graph_state.needs_rebuild {
        graph_state.rebuild(storage, state.graph_depth);
    }

    let response = graph_state.editor.as_mut().map(|editor| editor.ui(ui));
    if let Some(response) = response {
        if response.graph_changed {
            graph_state.rebuild(storage, state.graph_depth);
        }
        if response.wants_repaint || response.graph_changed {
            ui.ctx().request_repaint();
        }
    } else {
        ui.centered_and_justified(|ui| {
            ui.label("The node graph requires eframe's wgpu renderer.");
        });
    }
}
