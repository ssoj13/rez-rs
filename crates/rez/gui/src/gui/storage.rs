//! Package storage for GUI — wraps rez discover/search.

use crate::config::CONFIG;
use crate::package::discover::iter_all_packages;
use crate::repository::PackageInfo;
use std::path::PathBuf;
use std::str::FromStr;
use version::{Requirement, Version};

/// Package view for GUI display.
#[derive(Debug, Clone)]
pub struct GuiPackage {
    pub name: String,
    pub base: String,
    pub version: String,
    pub reqs: Vec<String>,
    pub tags: Vec<String>,
    pub apps: Vec<GuiApp>,
    pub envs: Vec<GuiEnv>,
    pub package_source: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GuiApp {
    pub name: String,
    pub path: Option<String>,
    pub from_pkg: String,
}

#[derive(Debug, Clone)]
pub struct GuiEnv {
    pub name: String,
    pub evars: Vec<GuiEvar>,
}

#[derive(Debug, Clone)]
pub struct GuiEvar {
    pub name: String,
    pub value: String,
}

impl GuiPackage {
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }
}

pub(crate) fn compare_versions(left: &str, right: &str) -> std::cmp::Ordering {
    match (Version::from_str(left), Version::from_str(right)) {
        (Ok(left), Ok(right)) => left.cmp(&right),
        _ => left.cmp(right),
    }
}

/// Package storage — scans rez repositories.
/// Uses HashMap indexes for O(1) get/latest lookups (node graph rebuild does many).
pub struct Storage {
    packages: Vec<GuiPackage>,
    by_name: std::collections::HashMap<String, usize>,
    by_base: std::collections::HashMap<String, Vec<usize>>,
    paths: Vec<PathBuf>,
}

impl Storage {
    pub fn scan(paths: Option<Vec<PathBuf>>) -> crate::errors::Result<Self> {
        let paths = paths.unwrap_or_else(|| CONFIG.expanded_packages_path_os());
        let infos = iter_all_packages(Some(&paths))?;
        let packages: Vec<GuiPackage> = infos
            .iter()
            .map(|info| info_to_gui_package(info, &paths))
            .collect();

        let mut by_name = std::collections::HashMap::new();
        let mut by_base: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        for (i, pkg) in packages.iter().enumerate() {
            by_name.insert(pkg.name.clone(), i);
            by_base.entry(pkg.base.clone()).or_default().push(i);
        }
        for indices in by_base.values_mut() {
            indices.sort_by(|&a, &b| compare_versions(&packages[b].version, &packages[a].version));
        }

        Ok(Self {
            packages,
            by_name,
            by_base,
            paths,
        })
    }

    pub fn refresh(&mut self) -> crate::errors::Result<()> {
        *self = Self::scan(Some(self.paths.clone()))?;
        Ok(())
    }

    pub fn location_paths(&self) -> Vec<PathBuf> {
        self.paths.clone()
    }

    pub fn get(&self, qualified_name: &str) -> Option<&GuiPackage> {
        self.by_name.get(qualified_name).map(|&i| &self.packages[i])
    }

    pub fn latest(&self, base: &str, requirement: Option<&Requirement>) -> Option<&GuiPackage> {
        self.by_base.get(base)?.iter().find_map(|&index| {
            let package = &self.packages[index];
            let matches = requirement
                .and_then(Requirement::range)
                .is_none_or(|range| {
                    Version::from_str(&package.version)
                        .is_ok_and(|version| range.contains_version(&version))
                });
            matches.then_some(package)
        })
    }

    pub fn packages_iter(&self) -> impl Iterator<Item = &GuiPackage> {
        self.packages.iter()
    }

    pub fn packages(&self) -> Vec<GuiPackage> {
        self.packages.clone()
    }

    /// Augment a package by qualified name with resolved paths and env.
    /// Used after solve so tree view shows tool paths.
    pub fn augment_package(
        &mut self,
        qualified_name: &str,
        resolved_pkgs: &[resolve::resolver::ResolvedPackageInfo],
        env: &std::collections::HashMap<String, String>,
    ) {
        if let Some(&idx) = self.by_name.get(qualified_name) {
            augment_with_resolved(&mut self.packages[idx], resolved_pkgs, env);
        }
    }
}

fn info_to_gui_package(info: &PackageInfo, _paths: &[PathBuf]) -> GuiPackage {
    let package_source = info
        .get("base")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let qualified = if info.version.is_truthy() {
        format!("{}-{}", info.name, info.version)
    } else {
        info.name.clone()
    };
    let version = info.version.to_string();

    let reqs: Vec<String> = info
        .get("requires")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();

    let tags: Vec<String> = info
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|_| vec!["package".to_string()])
        .unwrap_or_default();

    let tools: Vec<String> = info
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();

    let apps = tools
        .into_iter()
        .map(|name| GuiApp {
            name: name.clone(),
            path: None,
            from_pkg: info.name.clone(),
        })
        .collect();

    GuiPackage {
        name: qualified,
        base: info.name.clone(),
        version,
        reqs,
        tags,
        apps,
        envs: vec![],
        package_source,
    }
}

/// Augment GuiPackage with resolved data (apps with paths, envs).
/// Called after solve so tree view shows correct tool paths and env.
fn augment_with_resolved(
    pkg: &mut GuiPackage,
    resolved_pkgs: &[resolve::resolver::ResolvedPackageInfo],
    env: &std::collections::HashMap<String, String>,
) {
    let tools_map = resolved_pkgs
        .iter()
        .flat_map(|p| {
            let root = p.root.as_ref().or(p.repo_path.as_ref())?;
            let bin_dir = root.join("bin");
            if !bin_dir.is_dir() {
                return None;
            }
            let entries = std::fs::read_dir(&bin_dir).ok()?;
            let items: Vec<_> = entries
                .filter_map(|e| {
                    let e = e.ok()?;
                    let name = e.file_name().to_str()?.to_string();
                    let tool_name = if cfg!(windows) {
                        name.strip_suffix(".exe")
                            .or_else(|| name.strip_suffix(".bat"))
                            .or_else(|| name.strip_suffix(".cmd"))
                            .unwrap_or(&name)
                    } else {
                        &name
                    };
                    Some((
                        tool_name.to_string(),
                        bin_dir.join(&name).display().to_string(),
                        p.name.clone(),
                    ))
                })
                .collect();
            Some(items.into_iter())
        })
        .flatten();

    let mut tool_paths: std::collections::HashMap<String, (String, String)> =
        std::collections::HashMap::new();
    for (tool, path, from) in tools_map {
        tool_paths.entry(tool).or_insert((path, from));
    }

    for app in &mut pkg.apps {
        if let Some((path, from)) = tool_paths.get(&app.name) {
            app.path = Some(path.clone());
            app.from_pkg = from.clone();
        }
    }

    if !env.is_empty() {
        let evars: Vec<GuiEvar> = env
            .iter()
            .map(|(k, v)| GuiEvar {
                name: k.clone(),
                value: v.clone(),
            })
            .collect();
        pkg.envs = vec![GuiEnv {
            name: "default".to_string(),
            evars,
        }];
    }
}
