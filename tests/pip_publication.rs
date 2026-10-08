// SPDX-License-Identifier: Apache-2.0

use model::package::{Package, Variant};
use model::serialise::load_package_data;
use repository::{publish_package, PublicationPayload};
use serde_json::json;
use std::fs;
use std::path::Path;

fn package(variants: serde_json::Value) -> Package {
    Package::from_data(
        serde_json::from_value(json!({
            "name": "staged_pip", "version": "1.0", "hashed_variants": true,
            "variants": variants, "description": "staged"
        }))
        .unwrap(),
    )
    .unwrap()
}

fn stage(root: &Path, package: &Package, index: usize, contents: &str) -> std::path::PathBuf {
    let path = Variant::compute_subpath(&package.variants[index], true).unwrap();
    let target = root.join(path);
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("payload.txt"), contents).unwrap();
    target
}

#[test]
fn staged_install_skip_replace_and_variant_merge() {
    let repo = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let root = repo.path().join("staged_pip/1.0");
    let first = package(json!([["python-3.12"]]));
    stage(source.path(), &first, 0, "first");
    let installed = publish_package(
        &first,
        &root,
        &[0],
        Some(PublicationPayload::Staged {
            root: source.path(),
            replace: false,
        }),
        None,
    )
    .unwrap();
    assert_eq!(installed.installed_indices, vec![0]);
    stage(source.path(), &first, 0, "second");
    let skipped = publish_package(
        &first,
        &root,
        &[0],
        Some(PublicationPayload::Staged {
            root: source.path(),
            replace: false,
        }),
        None,
    )
    .unwrap();
    assert_eq!(skipped.skipped_indices, vec![0]);
    let payload = root
        .join(Variant::compute_subpath(&first.variants[0], true).unwrap())
        .join("payload.txt");
    assert_eq!(fs::read_to_string(&payload).unwrap(), "first");
    publish_package(
        &first,
        &root,
        &[0],
        Some(PublicationPayload::Staged {
            root: source.path(),
            replace: true,
        }),
        None,
    )
    .unwrap();
    assert_eq!(fs::read_to_string(&payload).unwrap(), "second");
    let second = package(json!([["python-3.13"]]));
    stage(source.path(), &second, 0, "third");
    publish_package(
        &second,
        &root,
        &[0],
        Some(PublicationPayload::Staged {
            root: source.path(),
            replace: false,
        }),
        None,
    )
    .unwrap();
    let (data, _) = load_package_data(&root).unwrap();
    let metadata = Package::from_data(data).unwrap();
    assert_eq!(
        metadata.variants,
        vec![first.variants[0].clone(), second.variants[0].clone()]
    );
}

#[test]
fn failed_second_variant_rolls_back_first_payload_and_metadata() {
    let repo = tempfile::tempdir().unwrap();
    let original = tempfile::tempdir().unwrap();
    let replacement = tempfile::tempdir().unwrap();
    let root = repo.path().join("staged_pip/1.0");
    let first = package(json!([["python-3.12"]]));
    stage(original.path(), &first, 0, "original");
    publish_package(
        &first,
        &root,
        &[0],
        Some(PublicationPayload::Staged {
            root: original.path(),
            replace: false,
        }),
        None,
    )
    .unwrap();
    let metadata = fs::read(root.join("package.yaml")).unwrap();
    let both = package(json!([["python-3.12"], ["python-3.13"]]));
    stage(replacement.path(), &both, 0, "replacement");
    assert!(publish_package(
        &both,
        &root,
        &[0, 1],
        Some(PublicationPayload::Staged {
            root: replacement.path(),
            replace: true
        }),
        None
    )
    .is_err());
    assert_eq!(fs::read(root.join("package.yaml")).unwrap(), metadata);
    assert_eq!(
        fs::read_to_string(
            root.join(Variant::compute_subpath(&first.variants[0], true).unwrap())
                .join("payload.txt")
        )
        .unwrap(),
        "original"
    );
}

#[test]
fn nonvariant_replacement_preserves_definition_and_removes_old_payload() {
    let repo = tempfile::tempdir().unwrap();
    let original = tempfile::tempdir().unwrap();
    let replacement = tempfile::tempdir().unwrap();
    let root = repo.path().join("staged_pip/1.0");
    let package = package(json!([]));
    fs::write(original.path().join("old.txt"), "old").unwrap();
    publish_package(
        &package,
        &root,
        &[0],
        Some(PublicationPayload::Staged {
            root: original.path(),
            replace: false,
        }),
        None,
    )
    .unwrap();
    fs::write(replacement.path().join("new.txt"), "new").unwrap();
    publish_package(
        &package,
        &root,
        &[0],
        Some(PublicationPayload::Staged {
            root: replacement.path(),
            replace: true,
        }),
        None,
    )
    .unwrap();
    assert!(!root.join("old.txt").exists());
    assert_eq!(fs::read_to_string(root.join("new.txt")).unwrap(), "new");
    assert!(root.join("package.yaml").is_file());
}

#[test]
fn alternate_definition_cannot_shadow_nonvariant_metadata() {
    let repo = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let root = repo.path().join("staged_pip/1.0");
    fs::write(source.path().join("package.py"), "name='shadow'").unwrap();
    assert!(publish_package(
        &package(json!([])),
        &root,
        &[0],
        Some(PublicationPayload::Staged {
            root: source.path(),
            replace: false
        }),
        None
    )
    .is_err());
    assert!(!root.join("package.py").exists());
    assert!(!root.join("package.yaml").exists());
}

#[test]
fn prefix_variant_overlap_is_rejected_before_swapping() {
    let repo = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let root = repo.path().join("staged_pip/1.0");
    let package = Package::from_data(
        serde_json::from_value(json!({
            "name":"staged_pip", "version":"1.0", "hashed_variants":false,
            "variants":[["python-3"],["python-3","tool"]]
        }))
        .unwrap(),
    )
    .unwrap();
    let first = Variant::compute_subpath(&package.variants[0], false).unwrap();
    let second = Variant::compute_subpath(&package.variants[1], false).unwrap();
    assert_eq!(first, "python-3");
    assert_eq!(second, "python-3/tool");
    fs::create_dir_all(source.path().join(&second)).unwrap();
    fs::write(
        source.path().join(&first).join("parent.txt"),
        "original parent",
    )
    .unwrap();
    fs::write(
        source.path().join(&second).join("child.txt"),
        "original child",
    )
    .unwrap();
    // Nested prefixes are supported. A selected parent overwrite must protect
    // the already advertised child variant, matching Rez's overlap exclusion.
    let installed = publish_package(
        &package,
        &root,
        &[0, 1],
        Some(PublicationPayload::Staged {
            root: source.path(),
            replace: true,
        }),
        None,
    )
    .unwrap();
    assert_eq!(installed.variant_indices, vec![(0, Some(0)), (1, Some(1))]);
    let metadata = fs::read(root.join("package.yaml")).unwrap();
    fs::write(
        source.path().join(&first).join("parent.txt"),
        "updated parent",
    )
    .unwrap();
    fs::write(
        source.path().join(&second).join("child.txt"),
        "unselected replacement",
    )
    .unwrap();
    let replaced = publish_package(
        &package,
        &root,
        &[0],
        Some(PublicationPayload::Staged {
            root: source.path(),
            replace: true,
        }),
        None,
    )
    .unwrap();
    assert_eq!(replaced.variant_indices, vec![(0, Some(0))]);
    assert_eq!(fs::read(root.join("package.yaml")).unwrap(), metadata);
    assert_eq!(
        fs::read_to_string(root.join(&first).join("parent.txt")).unwrap(),
        "updated parent"
    );
    assert_eq!(
        fs::read_to_string(root.join(&second).join("child.txt")).unwrap(),
        "original child"
    );
    let (data, _) = load_package_data(&root).unwrap();
    assert_eq!(Package::from_data(data).unwrap().variants, package.variants);
}

#[test]
fn malformed_existing_metadata_aborts_before_nonvariant_payload_swap() {
    let repo = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let root = repo.path().join("staged_pip/1.0");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("package.yaml"), "requires: [").unwrap();
    fs::write(root.join("old.txt"), "original").unwrap();
    fs::write(source.path().join("new.txt"), "replacement").unwrap();
    assert!(publish_package(
        &package(json!([])),
        &root,
        &[0],
        Some(PublicationPayload::Staged {
            root: source.path(),
            replace: true
        }),
        None
    )
    .is_err());
    assert_eq!(
        fs::read_to_string(root.join("old.txt")).unwrap(),
        "original"
    );
    assert!(!root.join("new.txt").exists());
}

#[test]
fn concurrent_staged_writers_preserve_both_variants() {
    let repo = tempfile::tempdir().unwrap();
    let source_a = tempfile::tempdir().unwrap();
    let source_b = tempfile::tempdir().unwrap();
    let root = repo.path().join("staged_pip/1.0");
    let a = package(json!([["python-3.12"]]));
    let b = package(json!([["python-3.13"]]));
    stage(source_a.path(), &a, 0, "a");
    stage(source_b.path(), &b, 0, "b");
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        for (variants, source) in [
            (json!([["python-3.12"]]), source_a.path()),
            (json!([["python-3.13"]]), source_b.path()),
        ] {
            let root = &root;
            let barrier = &barrier;
            scope.spawn(move || {
                let package = package(variants);
                barrier.wait();
                publish_package(
                    &package,
                    root,
                    &[0],
                    Some(PublicationPayload::Staged {
                        root: source,
                        replace: false,
                    }),
                    None,
                )
                .unwrap();
            });
        }
    });
    let (data, _) = load_package_data(&root).unwrap();
    let installed = Package::from_data(data).unwrap();
    assert_eq!(installed.variants.len(), 2);
    assert!(installed.variants.contains(&a.variants[0]));
    assert!(installed.variants.contains(&b.variants[0]));
}

#[cfg(windows)]
fn directory_link(target: &Path, link: &Path) {
    let output = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        // cmd builtins treat forward slashes as switches, including when a
        // PathBuf contains a mixture of separators from repository joins.
        .arg(link.components().collect::<std::path::PathBuf>())
        .arg(target.components().collect::<std::path::PathBuf>())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
#[cfg(unix)]
fn directory_link(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(any(windows, unix))]
#[test]
fn escaping_family_link_is_rejected_before_version_creation() {
    let repo = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let root = repo.path().join("staged_pip/1.0");
    directory_link(outside.path(), &repo.path().join("staged_pip"));
    fs::write(source.path().join("payload.txt"), "payload").unwrap();
    let error = publish_package(
        &package(json!([])),
        &root,
        &[0],
        Some(PublicationPayload::Staged {
            root: source.path(),
            replace: true,
        }),
        None,
    )
    .unwrap_err();
    assert!(error.to_string().contains("escapes repository"));
    assert!(!outside.path().join("1.0").exists());
}

#[cfg(any(windows, unix))]
#[test]
fn escaping_variant_link_is_rejected_without_modifying_target() {
    let repo = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let package = package(json!([["python-3.13"]]));
    let root = repo.path().join("staged_pip/1.0");
    fs::create_dir_all(&root).unwrap();
    fs::write(outside.path().join("original.txt"), "original").unwrap();
    let path = Variant::compute_subpath(&package.variants[0], true).unwrap();
    directory_link(outside.path(), &root.join(path));
    stage(source.path(), &package, 0, "replacement");
    let error = publish_package(
        &package,
        &root,
        &[0],
        Some(PublicationPayload::Staged {
            root: source.path(),
            replace: true,
        }),
        None,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Variant ancestor is not a regular directory"),
        "{error}"
    );
    assert!(!root.join("package.yaml").exists());
    assert!(!root.join("package.py").exists());
    assert_eq!(
        fs::read_to_string(outside.path().join("original.txt")).unwrap(),
        "original"
    );
    assert!(!outside.path().join("payload.txt").exists());
}

#[test]
fn post_swap_toml_serialization_failure_restores_nonvariant_payload_and_metadata() {
    let repo = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let root = repo.path().join("staged_pip/1.0");
    fs::create_dir_all(&root).unwrap();
    let original = b"name = 'staged_pip'\nversion = '1.0'\nhashed_variants = true\n";
    fs::write(root.join("package.toml"), original).unwrap();
    fs::write(root.join("old.txt"), "original").unwrap();
    fs::write(source.path().join("new.txt"), "replacement").unwrap();
    let replacement = Package::from_data(
        serde_json::from_value(json!({
            "name":"staged_pip", "version":"1.0", "hashed_variants":true,
            "custom":{"nested":null}
        }))
        .unwrap(),
    )
    .unwrap();
    let error = publish_package(
        &replacement,
        &root,
        &[0],
        Some(PublicationPayload::Staged {
            root: source.path(),
            replace: true,
        }),
        None,
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("TOML has no null value"),
        "{error}"
    );
    assert_eq!(fs::read(root.join("package.toml")).unwrap(), original);
    assert_eq!(
        fs::read_to_string(root.join("old.txt")).unwrap(),
        "original"
    );
    assert!(!root.join("new.txt").exists());
    assert!(!root.join("package.yaml").exists());
}

#[test]
fn publication_configured_stem_and_explicit_format() {
    if let Some(root) = std::env::var_os("REZ_PUBLICATION_TEST_ROOT") {
        let root = std::path::PathBuf::from(root).join("staged_pip/1.0");
        let package = package(json!([]));
        publish_package(
            &package,
            &root,
            &[0],
            None,
            Some(model::serialise::FileFormat::Py),
        )
        .unwrap();
        assert!(root.join("recipe.py").is_file());
        assert!(!root.join("package.py").exists());
        let previous = fs::read(root.join("recipe.py")).unwrap();
        let called = std::cell::Cell::new(false);
        let callback = || {
            called.set(true);
            Ok(())
        };
        assert!(publish_package(
            &package,
            &root,
            &[0],
            Some(PublicationPayload::Existing(&callback)),
            Some(model::serialise::FileFormat::Yaml),
        )
        .is_err());
        assert!(!called.get());
        assert_eq!(fs::read(root.join("recipe.py")).unwrap(), previous);
        assert!(!root.join("recipe.yaml").exists());
        return;
    }
    // A fresh process isolates LazyLock CONFIG; never mutate shared test configuration.
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.py");
    fs::write(&config, "plugins = {'package_repository': {'filesystem': {'package_filenames': ['recipe', 'package']}}}\n").unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "publication_configured_stem_and_explicit_format",
            "--nocapture",
        ])
        .env("REZ_CONFIG_FILE", &config)
        .env_remove("REZ_PLUGINS_JSON")
        .env("REZ_PUBLICATION_TEST_ROOT", root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
