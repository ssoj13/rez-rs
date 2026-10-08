// SPDX-License-Identifier: Apache-2.0

//! Shared package install routing used by build and release commands.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::package::Package;

pub const PACKAGE_CATEGORIES: [&str; 5] = ["int", "ext", "dcc", "pip", "tool"];

/// Expand configured category repository paths into the complete category set.
pub fn category_paths(paths: impl IntoIterator<Item = PathBuf>, enabled: bool) -> Vec<PathBuf> {
    let paths: Vec<PathBuf> = paths.into_iter().collect();
    let mut expanded = Vec::new();
    let mut category_roots = HashSet::new();

    for (index, path) in paths.iter().enumerate() {
        let category_root = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| PACKAGE_CATEGORIES.contains(name))
            .and_then(|_| path.parent());

        if let Some(root) = category_root {
            if !enabled {
                if category_roots.insert(root.to_path_buf()) {
                    expanded.push(root.to_path_buf());
                }
                continue;
            }

            // Keep explicitly configured repositories in their original positions.
            if !expanded.contains(path) {
                expanded.push(path.clone());
            }
            let last = !paths[index + 1..].iter().any(|candidate| {
                candidate.parent() == Some(root)
                    && candidate
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| PACKAGE_CATEGORIES.contains(&name))
            });
            if last {
                for category in PACKAGE_CATEGORIES {
                    let sibling = root.join(category);
                    if !expanded.contains(&sibling) {
                        expanded.push(sibling);
                    }
                }
            }
        } else {
            expanded.push(path.clone());
        }
    }

    expanded
}

/// Route a category repository base to the package's category repository.
pub fn route_install_path(base: &Path, package: &Package, enabled: bool) -> PathBuf {
    let Some(base_category) = base.file_name().and_then(|name| name.to_str()) else {
        return base.to_path_buf();
    };
    if !PACKAGE_CATEGORIES.contains(&base_category) {
        return base.to_path_buf();
    }
    let Some(root) = base.parent() else {
        return base.to_path_buf();
    };

    if !enabled {
        return root.to_path_buf();
    }

    // Package extensions retain Python truthiness at the metadata boundary.
    let truthy = |value: &serde_json::Value| match value {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(value) => *value,
        serde_json::Value::Number(value) => value.as_f64() != Some(0.0),
        serde_json::Value::String(value) => !value.is_empty(),
        serde_json::Value::Array(value) => !value.is_empty(),
        serde_json::Value::Object(value) => !value.is_empty(),
    };
    let package_type = package
        .attributes
        .get("package_type")
        .filter(|value| truthy(value));
    let category = match package_type {
        Some(value) => value.as_str(),
        None if package.attributes.get("external").is_some_and(truthy) => Some("ext"),
        None => None,
    }
    .filter(|value| PACKAGE_CATEGORIES.contains(value));

    match category {
        Some(category) => root.join(category),
        None => base.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn expands_all_categories_once_from_any_category_root() {
        let paths = category_paths(
            [PathBuf::from("/repo/int"), PathBuf::from("/repo/ext")],
            true,
        );

        assert_eq!(
            paths,
            ["int", "ext", "dcc", "pip", "tool"]
                .map(|category| PathBuf::from("/repo").join(category))
        );
    }

    #[test]
    fn category_expansion_preserves_explicit_repository_priority() {
        let paths = ["/repo/int", "/repo/no_DFS", "/repo/ext"].map(PathBuf::from);
        assert_eq!(
            category_paths(paths, true),
            [
                "/repo/int",
                "/repo/no_DFS",
                "/repo/ext",
                "/repo/dcc",
                "/repo/pip",
                "/repo/tool"
            ]
            .map(PathBuf::from)
        );
    }

    #[test]
    fn disabled_categories_flatten_category_roots_once() {
        let paths = [PathBuf::from("/repo/int"), PathBuf::from("/repo/ext")];
        assert_eq!(category_paths(paths, false), [PathBuf::from("/repo")]);
    }

    #[test]
    fn routes_package_type_and_external_fallback() {
        let mut package = Package::default();
        package
            .attributes
            .insert("package_type".into(), json!("dcc"));
        assert_eq!(
            route_install_path(Path::new("/repo/int"), &package, true),
            PathBuf::from("/repo/dcc")
        );

        package.attributes.clear();
        package.attributes.insert("external".into(), json!(true));
        assert_eq!(
            route_install_path(Path::new("/repo/int"), &package, true),
            PathBuf::from("/repo/ext")
        );
    }

    #[test]
    fn routing_preserves_python_metadata_truthiness() {
        let mut package = Package::default();
        for external in [
            json!(true),
            json!(1),
            json!("yes"),
            json!([1]),
            json!({"enabled": true}),
        ] {
            package.attributes.insert("external".into(), external);
            for package_type in [
                json!(null),
                json!(false),
                json!(0),
                json!(""),
                json!([]),
                json!({}),
            ] {
                package
                    .attributes
                    .insert("package_type".into(), package_type);
                assert_eq!(
                    route_install_path(Path::new("/repo/int"), &package, true),
                    PathBuf::from("/repo/ext")
                );
            }
            for package_type in [
                json!(true),
                json!(1),
                json!("unknown"),
                json!([1]),
                json!({"custom": true}),
            ] {
                package
                    .attributes
                    .insert("package_type".into(), package_type);
                assert_eq!(
                    route_install_path(Path::new("/repo/int"), &package, true),
                    PathBuf::from("/repo/int")
                );
            }
        }
    }

    #[test]
    fn leaves_unknown_types_and_non_category_paths_unchanged() {
        let mut package = Package::default();
        package
            .attributes
            .insert("package_type".into(), json!("custom"));
        assert_eq!(
            route_install_path(Path::new("/repo/int"), &package, true),
            PathBuf::from("/repo/int")
        );
        package.attributes.insert("external".into(), json!(true));
        assert_eq!(
            route_install_path(Path::new("/repo/int"), &package, true),
            PathBuf::from("/repo/int")
        );
        package.attributes.insert("package_type".into(), json!(""));
        assert_eq!(
            route_install_path(Path::new("/repo/int"), &package, true),
            PathBuf::from("/repo/ext")
        );
        package
            .attributes
            .insert("package_type".into(), json!("dcc"));
        assert_eq!(
            route_install_path(Path::new("/custom/packages"), &package, true),
            PathBuf::from("/custom/packages")
        );
        assert_eq!(
            route_install_path(Path::new("/repo/int"), &package, false),
            PathBuf::from("/repo")
        );
    }
}
