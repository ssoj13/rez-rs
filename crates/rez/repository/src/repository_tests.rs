// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::collections::HashMap;

#[test]
fn publication_merge_retains_nested_destination_children_and_overwrites_leaves() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("merged/1");
    fs::create_dir_all(root.join("lib/nested")).unwrap();
    fs::write(root.join("lib/nested/keep"), "retained").unwrap();
    fs::write(root.join("lib/nested/change"), "old").unwrap();
    let package = publication_package(serde_json::json!({"name":"merged","version":"1"}));
    publish_package(&package, &root, &[0], None, None).unwrap();
    let stage = tempfile::tempdir().unwrap();
    fs::create_dir_all(stage.path().join("lib/nested")).unwrap();
    fs::write(stage.path().join("lib/nested/change"), "new").unwrap();
    fs::write(stage.path().join("lib/nested/add"), "added").unwrap();
    publish_package(
        &package,
        &root,
        &[0],
        Some(PublicationPayload::Merge {
            root: stage,
            paths: None,
            metadata: PublicationMetadata::Installed,
        }),
        None,
    )
    .unwrap();
    assert_eq!(fs::read(root.join("lib/nested/keep")).unwrap(), b"retained");
    assert_eq!(fs::read(root.join("lib/nested/change")).unwrap(), b"new");
    assert_eq!(fs::read(root.join("lib/nested/add")).unwrap(), b"added");
}

#[test]
fn publication_merge_later_failure_restores_bytes_links_directories_and_times() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("merge_rollback/1");
    fs::create_dir_all(root.join("a/nested")).unwrap();
    fs::write(root.join("a/nested/change"), "original").unwrap();
    fs::write(root.join("a/nested/keep"), "retained").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("keep", root.join("a/nested/link")).unwrap();
    let package = publication_package(serde_json::json!({
        "name":"merge_rollback","version":"1","variants":[["a"],["c"]],"hashed_variants":false
    }));
    publish_package(&package, &root, &[0], None, None).unwrap();
    let definition = fs::read(root.join("package.yaml")).unwrap();
    let time = filetime::FileTime::from_unix_time(123456, 0);
    filetime::set_file_times(root.join("a/nested"), time, time).unwrap();
    let stage = tempfile::tempdir().unwrap();
    fs::create_dir_all(stage.path().join("a/nested/newdir")).unwrap();
    fs::write(stage.path().join("a/nested/change"), "incoming").unwrap();
    fs::write(stage.path().join("a/nested/newdir/child"), "incoming").unwrap();
    #[cfg(unix)]
    fs::write(stage.path().join("a/nested/link"), "replacement").unwrap();
    assert!(publish_package(
        &package,
        &root,
        &[0, 1],
        Some(PublicationPayload::Merge {
            root: stage,
            paths: None,
            metadata: PublicationMetadata::Installed,
        }),
        None
    )
    .is_err());
    let restored = fs::metadata(root.join("a/nested")).unwrap();
    assert_eq!(filetime::FileTime::from_last_access_time(&restored), time);
    assert_eq!(
        filetime::FileTime::from_last_modification_time(&restored),
        time
    );
    assert_eq!(fs::read(root.join("a/nested/change")).unwrap(), b"original");
    assert_eq!(fs::read(root.join("a/nested/keep")).unwrap(), b"retained");
    assert_eq!(fs::read(root.join("package.yaml")).unwrap(), definition);
    assert!(!root.join("a/nested/newdir").exists());
    #[cfg(unix)]
    assert_eq!(
        fs::read_link(root.join("a/nested/link")).unwrap(),
        PathBuf::from("keep")
    );
}

#[test]
fn legacy_install_preserves_custom_payload_paths_and_declared_variants() {
    for variants in [
        serde_json::json!([]),
        serde_json::json!([["python-3"], ["python-4"]]),
    ] {
        for subpath in [None, Some("custom/nested")] {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("source");
            fs::create_dir_all(source.join("lib/nested")).unwrap();
            fs::write(source.join("lib/nested/change"), "incoming").unwrap();
            let definition = serde_json::json!({
                "name":"legacy","version":"1","variants":variants,
                "hashed_variants":false,"custom_property":"preserved","build_system":"cmake","build_command":"echo build"
            });
            fs::write(
                source.join("package.yaml"),
                serde_yaml::to_string(&definition).unwrap(),
            )
            .unwrap();
            let repo = temp.path().join("repo");
            let root = repo.join("legacy/1");
            let payload = root.join(subpath.unwrap_or(""));
            fs::create_dir_all(payload.join("lib/nested")).unwrap();
            fs::write(payload.join("lib/nested/keep"), "retained").unwrap();
            fs::write(payload.join("lib/nested/change"), "old").unwrap();
            assert_eq!(
                install_variant(&source, &repo, "legacy", "1", subpath).unwrap(),
                payload
            );
            assert_eq!(
                fs::read(payload.join("lib/nested/keep")).unwrap(),
                b"retained"
            );
            assert_eq!(
                fs::read(payload.join("lib/nested/change")).unwrap(),
                b"incoming"
            );
            let installed = load_package_data(&root.join("package.yaml")).unwrap();
            assert_eq!(installed["variants"], variants);
            assert_eq!(installed["custom_property"], "preserved");
            assert_eq!(installed["build_system"], "cmake");
            assert_eq!(installed["build_command"], "echo build");
            if subpath.is_some() {
                assert_eq!(
                    fs::read(payload.join("package.yaml")).unwrap(),
                    fs::read(source.join("package.yaml")).unwrap()
                );
            }
        }
    }
}

#[test]
fn legacy_install_rejects_invalid_inputs_before_repository_writes() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("payload"), "incoming").unwrap();
    let repo = temp.path().join("repo");
    assert!(install_variant(&source, &repo, "safe", "1", None).is_err());
    assert!(!repo.exists());
    fs::write(source.join("package.yaml"), "name: other\nversion: '1'\n").unwrap();
    assert!(install_variant(&source, &repo, "safe", "1", None).is_err());
    assert!(!repo.exists());
    fs::write(source.join("package.yaml"), "name: safe\nversion: '1'\n").unwrap();
    for path in ["../outside", "/absolute", "nested/../../outside"] {
        assert!(install_variant(&source, &repo, "safe", "1", Some(path)).is_err());
        assert!(!repo.exists());
    }
    assert!(install_variant(&source, &repo, "../unsafe", "1", None).is_err());
    assert!(install_variant(&source, &repo, "safe", "../unsafe", None).is_err());
    assert!(!repo.exists());
}

#[cfg(unix)]
#[test]
fn legacy_install_materializes_file_links_and_rejects_directory_links() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("package.yaml"), "name: linked\nversion: '1'\n").unwrap();
    fs::write(temp.path().join("foreign"), "external").unwrap();
    std::os::unix::fs::symlink("../foreign", source.join("file_link")).unwrap();
    let repo = temp.path().join("repo");
    let installed = install_variant(&source, &repo, "linked", "1", None).unwrap();
    assert_eq!(fs::read(installed.join("file_link")).unwrap(), b"external");
    assert!(!fs::symlink_metadata(installed.join("file_link"))
        .unwrap()
        .file_type()
        .is_symlink());
    let old_definition = fs::read(installed.join("package.yaml")).unwrap();
    std::os::unix::fs::symlink(".", source.join("cycle")).unwrap();
    assert!(install_variant(&source, &repo, "linked", "1", None).is_err());
    assert_eq!(
        fs::read(installed.join("package.yaml")).unwrap(),
        old_definition
    );
    assert!(!installed.join("cycle").exists());
}

#[test]
fn legacy_python_install_preserves_original_namespace_and_bytes() {
    for subpath in [None, Some("custom/nested")] {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        let original = "# original namespace and source formatting\nname = 'python_namespace'\nversion = '1'\n_prefix = 'owned'\ndef helper():\n    return _prefix\ndef commands():\n    env.X.set(helper())\n";
        fs::write(source.join("package.py"), original).unwrap();
        let repo = temp.path().join("repo");
        let installed = install_variant(&source, &repo, "python_namespace", "1", subpath).unwrap();
        let root = repo.join("python_namespace/1");
        assert_eq!(
            fs::read(root.join("package.py")).unwrap(),
            original.as_bytes()
        );
        assert_eq!(
            fs::read(installed.join("package.py")).unwrap(),
            original.as_bytes()
        );
        let package =
            Package::from_data(load_package_data(&root.join("package.py")).unwrap()).unwrap();
        assert_eq!(package.name, "python_namespace");
        // Execute the reloaded complete definition and its callable with a Rex-shaped
        // environment proxy. This checks the original helper/global namespace.
        let script = format!(
            "class _Proxy:\n    def set(self, value):\n        globals()['probe'] = value\nclass _Env:\n    X = _Proxy()\nenv = _Env()\n{}\ncommands()\n",
            fs::read_to_string(root.join("package.py")).unwrap()
        );
        let values =
            python_runtime::exec_py_globals(&script, "reloaded-python-definition", None, &["env"])
                .unwrap();
        assert_eq!(values["probe"], "owned");
    }
}

#[test]
fn legacy_python_definition_changed_during_loading_cannot_write_repository() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("package.py"),
        "name = 'changing_definition'\nversion = '1'\nwith open(_pkg_filename, 'a') as _stream:\n    _stream.write('\\n# changed while loading\\n')\n"
    ).unwrap();
    let repo = temp.path().join("repo");
    let error = install_variant(&source, &repo, "changing_definition", "1", None).unwrap_err();
    assert!(
        error.to_string().contains("changed while loading"),
        "{error}"
    );
    assert!(!repo.exists());
}

#[test]
fn legacy_verified_python_bytes_cannot_publish_different_typed_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let definition = temp.path().join("package.py");
    fs::write(&definition, "name = 'verified_python'\nversion = '1'\n").unwrap();
    let (mut package, format, definition) =
        VerifiedPythonDefinition::load(&definition, "verified_python", "1").unwrap();
    package.description = Some("changed after verification".into());
    let stage = tempfile::tempdir().unwrap();
    fs::write(stage.path().join("payload"), "incoming").unwrap();
    let repo = temp.path().join("repo");
    let error = publish_package(
        &package,
        &repo.join("verified_python/1"),
        &[],
        Some(PublicationPayload::Merge {
            root: stage,
            paths: Some(vec![(None, PathBuf::from("payload"))]),
            metadata: PublicationMetadata::Declared { definition },
        }),
        Some(format),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("does not match publication"),
        "{error}"
    );
    assert!(!repo.exists());
}

fn publication_package(data: serde_json::Value) -> Package {
    Package::from_data(serde_json::from_value(data).unwrap()).unwrap()
}

#[test]
fn publication_maps_selected_indices_and_preserves_duplicate_destination_rows() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("mapped/1");
    fs::create_dir_all(root.join("b")).unwrap();
    let package = publication_package(
        serde_json::json!({"name":"mapped","version":"1","variants":[["a"],["b"]],"hashed_variants":false}),
    );
    let first = publish_package(&package, &root, &[1], None, None).unwrap();
    assert_eq!(first.variant_indices, vec![(1, Some(0))]);
    let mut existing = installed_package_data(&package).unwrap();
    existing.insert("variants".into(), serde_json::json!([["b"], ["b"]]));
    crate::serialise::dump_package_data(
        &existing,
        &root.join("package.yaml"),
        crate::serialise::FileFormat::Yaml,
        None,
    )
    .unwrap();
    let stage = tempfile::tempdir().unwrap();
    fs::create_dir_all(stage.path().join("b")).unwrap();
    let skipped = publish_package(
        &package,
        &root,
        &[1],
        Some(PublicationPayload::Staged {
            root: stage.path(),
            replace: false,
        }),
        None,
    )
    .unwrap();
    assert_eq!(skipped.skipped_indices, vec![1]);
    assert_eq!(skipped.variant_indices, vec![(1, Some(1))]);
    let installed =
        Package::from_data(load_package_data(&root.join("package.yaml")).unwrap()).unwrap();
    assert_eq!(installed.variants.len(), 2);
    let plain = publication_package(serde_json::json!({"name":"plain"}));
    let result = publish_package(&plain, &temp.path().join("plain"), &[0], None, None).unwrap();
    assert_eq!(result.variant_indices, vec![(0, None)]);
}

#[test]
fn publication_merges_nested_variant_roots_and_common_include_assets() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("nested/1");
    fs::create_dir_all(root.join("a/b")).unwrap();
    fs::write(root.join("a/b/keep"), "child").unwrap();
    let package = publication_package(
        serde_json::json!({"name":"nested","version":"1","variants":[["a"],["a","b"]],"hashed_variants":false}),
    );
    publish_package(&package, &root, &[1], None, None).unwrap();
    let stage = tempfile::tempdir().unwrap();
    fs::create_dir_all(stage.path().join("a")).unwrap();
    fs::write(stage.path().join("a/new"), "parent").unwrap();
    fs::create_dir_all(stage.path().join(".rez/include")).unwrap();
    fs::write(stage.path().join(".rez/include/helper.py"), "VALUE=42").unwrap();
    let result = publish_package(
        &package,
        &root,
        &[0],
        Some(PublicationPayload::Staged {
            root: stage.path(),
            replace: true,
        }),
        None,
    )
    .unwrap();
    assert_eq!(result.variant_indices, vec![(0, Some(1))]);
    assert_eq!(fs::read_to_string(root.join("a/b/keep")).unwrap(), "child");
    assert_eq!(fs::read_to_string(root.join("a/new")).unwrap(), "parent");
    assert!(root.join(".rez/include/helper.py").is_file());
}

#[test]
fn publication_nested_transfer_failure_rolls_back_payload_and_common_assets() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("rollback/1");
    fs::create_dir_all(root.join("a")).unwrap();
    fs::write(root.join("a/old"), "original").unwrap();
    let package = publication_package(
        serde_json::json!({"name":"rollback","version":"1","variants":[["a"],["c"]],"hashed_variants":false}),
    );
    publish_package(&package, &root, &[0], None, None).unwrap();
    let metadata = fs::read(root.join("package.yaml")).unwrap();
    let stage = tempfile::tempdir().unwrap();
    fs::create_dir_all(stage.path().join("a")).unwrap();
    fs::write(stage.path().join("a/new"), "incoming").unwrap();
    fs::create_dir_all(stage.path().join(".rez/include")).unwrap();
    fs::write(stage.path().join(".rez/include/helper.py"), "incoming").unwrap();
    assert!(publish_package(
        &package,
        &root,
        &[0, 1],
        Some(PublicationPayload::Staged {
            root: stage.path(),
            replace: true
        }),
        None
    )
    .is_err());
    assert_eq!(fs::read(root.join("package.yaml")).unwrap(), metadata);
    assert_eq!(fs::read_to_string(root.join("a/old")).unwrap(), "original");
    assert!(!root.join("a/new").exists());
    assert!(!root.join(".rez").exists());
}

#[test]
fn repository_publication_indexes_unversioned_package_for_discovery() {
    let temp = tempfile::tempdir().unwrap();
    let package_dir = temp.path().join("unversioned");
    fs::create_dir_all(&package_dir).unwrap();
    let package = publication_package(serde_json::json!({
        "name": "unversioned",
        "requires": []
    }));

    publish_package(&package, &package_dir, &[0], None, None).unwrap();

    let definition = package_dir.join("package.yaml");
    assert!(definition.is_file());
    let installed = Package::from_data(load_package_data(&definition).unwrap()).unwrap();
    assert_eq!(installed.version, Version::empty());
    assert!(installed.variants.is_empty());

    let manager = PackageRepositoryManager::from_paths(&[temp.path().to_path_buf()]).unwrap();
    let discovered = manager
        .get_package("unversioned", &Version::empty())
        .unwrap();
    assert!(discovered.is_some());
}

#[test]
fn repository_publication_preserves_hashed_empty_indexed_variant() {
    let temp = tempfile::tempdir().unwrap();
    let version_dir = temp.path().join("empty_variant").join("1.0");
    let package = publication_package(serde_json::json!({
        "name": "empty_variant",
        "version": "1.0",
        "variants": [[]],
        "hashed_variants": true
    }));
    let subpath =
        model::package::Variant::compute_subpath(&package.variants[0], package.hashed_variants)
            .unwrap();
    fs::create_dir_all(version_dir.join(subpath)).unwrap();

    publish_package(&package, &version_dir, &[0], None, None).unwrap();

    let installed =
        Package::from_data(load_package_data(&version_dir.join("package.yaml")).unwrap()).unwrap();
    assert_eq!(installed.variants.len(), 1);
    assert!(installed.variants[0].is_empty());
    assert!(is_variant_installed(&package, &version_dir, 0, None).unwrap());
}

#[test]
fn repository_publication_rewrites_python_definition_in_its_original_format() {
    let temp = tempfile::tempdir().unwrap();
    let version_dir = temp.path().join("python_definition").join("1.0");
    fs::create_dir_all(&version_dir).unwrap();
    let definition = version_dir.join("package.py");
    fs::write(
        &definition,
        "name = 'python_definition'\nversion = '1.0'\nvariants = [[]]\nhashed_variants = True\n",
    )
    .unwrap();
    let package = publication_package(serde_json::json!({
        "name": "python_definition",
        "version": "1.0",
        "variants": [[]],
        "hashed_variants": true
    }));
    let subpath =
        model::package::Variant::compute_subpath(&package.variants[0], package.hashed_variants)
            .unwrap();
    fs::create_dir_all(version_dir.join(subpath)).unwrap();

    publish_package(&package, &version_dir, &[0], None, None).unwrap();

    assert!(definition.is_file());
    assert!(!version_dir.join("package.yaml").exists());
    let installed = Package::from_data(load_package_data(&definition).unwrap()).unwrap();
    assert_eq!(installed.name, package.name);
    assert_eq!(installed.variants.len(), 1);
}

#[test]
fn repository_publication_rejects_malformed_existing_metadata_without_replacing_it() {
    let temp = tempfile::tempdir().unwrap();
    let version_dir = temp.path().join("malformed").join("1.0");
    fs::create_dir_all(&version_dir).unwrap();
    let definition = version_dir.join("package.yaml");
    let malformed = b"name: [\n";
    fs::write(&definition, malformed).unwrap();
    let package = publication_package(serde_json::json!({
        "name": "malformed",
        "version": "1.0"
    }));

    assert!(publish_package(&package, &version_dir, &[0], None, None).is_err());
    assert_eq!(fs::read(definition).unwrap(), malformed);
}

#[test]
fn repository_publication_rejects_hashed_mode_changes_without_replacing_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let version_dir = temp.path().join("hash_mode").join("1.0");
    fs::create_dir_all(&version_dir).unwrap();
    let definition = version_dir.join("package.yaml");
    let original = b"name: hash_mode\nversion: '1.0'\nvariants: [[]]\nhashed_variants: false\n";
    fs::write(&definition, original).unwrap();
    let package = publication_package(serde_json::json!({
        "name": "hash_mode",
        "version": "1.0",
        "variants": [[]],
        "hashed_variants": true
    }));
    let subpath =
        model::package::Variant::compute_subpath(&package.variants[0], package.hashed_variants)
            .unwrap();
    fs::create_dir_all(version_dir.join(subpath)).unwrap();

    assert!(publish_package(&package, &version_dir, &[0], None, None).is_err());
    assert_eq!(fs::read(definition).unwrap(), original);
}

#[test]
fn repository_publication_rejects_variant_to_nonvariant_transition() {
    let temp = tempfile::tempdir().unwrap();
    let version_dir = temp.path().join("mode_change").join("1.0");
    fs::create_dir_all(&version_dir).unwrap();
    let definition = version_dir.join("package.yaml");
    let original = b"name: mode_change\nversion: '1.0'\nvariants: [[]]\n";
    fs::write(&definition, original).unwrap();
    let package = publication_package(serde_json::json!({
        "name": "mode_change",
        "version": "1.0"
    }));

    assert!(publish_package(&package, &version_dir, &[0], None, None).is_err());
    assert_eq!(fs::read(definition).unwrap(), original);
}

#[test]
fn repository_publication_payload_is_locked_and_failure_preserves_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("payload/1.0");
    fs::create_dir_all(&root).unwrap();
    let package = publication_package(serde_json::json!({
        "name": "payload", "version": "1.0", "description": "original"
    }));
    publish_package(&package, &root, &[0], None, None).unwrap();
    let definition = root.join("package.yaml");
    let original = fs::read(&definition).unwrap();
    let payload = || {
        let mut contender = LockFile::new(temp.path(), "payload", Some("1.0"));
        let error = contender.acquire(0).unwrap_err().to_string();
        assert!(error.contains("Lock acquisition timeout"), "{error}");
        Err(RezError::PackageRepository("payload failure".into()))
    };
    let error = publish_package(
        &package,
        &root,
        &[0],
        Some(PublicationPayload::Existing(&payload)),
        None,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("payload failure"), "{error}");
    assert_eq!(fs::read(&definition).unwrap(), original);
    // The publisher releases its lock even when its payload fails.
    let mut contender = LockFile::new(temp.path(), "payload", Some("1.0"));
    contender.acquire(0).unwrap();
}

#[test]
fn repository_publication_serializes_payload_and_metadata_for_same_version() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("concurrent_payload/1.0");
    fs::create_dir_all(&root).unwrap();
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        for marker in ["source-a", "source-b"] {
            let root = &root;
            let barrier = &barrier;
            scope.spawn(move || {
                let package = publication_package(serde_json::json!({
                    "name": "concurrent_payload", "version": "1.0", "description": marker
                }));
                let payload = || {
                    fs::write(root.join("payload-marker"), marker)?;
                    Ok(())
                };
                barrier.wait();
                for _ in 0..8 {
                    publish_package(
                        &package,
                        root,
                        &[0],
                        Some(PublicationPayload::Existing(&payload)),
                        None,
                    )
                    .unwrap();
                }
            });
        }
    });
    let installed =
        Package::from_data(load_package_data(&root.join("package.yaml")).unwrap()).unwrap();
    assert_eq!(
        installed.description.unwrap(),
        fs::read_to_string(root.join("payload-marker")).unwrap()
    );
}

#[test]
fn repository_lock_excludes_independent_handles_without_age_stealing() {
    let temp = tempfile::tempdir().unwrap();
    let mut owner = LockFile::new(temp.path(), "app", Some("1.0"));
    owner.acquire(0).unwrap();
    filetime::set_file_mtime(&owner.path, filetime::FileTime::from_unix_time(1, 0)).unwrap();

    let mut contender = LockFile::new(temp.path(), "app", Some("1.0"));
    assert!(contender.acquire(0).is_err());
    assert!(owner.path.is_file());
    owner.acquire(0).unwrap();
    assert!(contender.acquire(0).is_err());
    owner.release().unwrap();
    contender.acquire(0).unwrap();
}

#[test]
fn repository_lock_reuses_existing_unlocked_file_without_truncation() {
    let temp = tempfile::tempdir().unwrap();
    let mut lock = LockFile::new(temp.path(), "app", Some("1.0"));
    fs::write(&lock.path, b"persistent lock inode").unwrap();

    lock.acquire(0).unwrap();
    // Windows file locks also exclude reads through independently opened handles.
    lock.release().unwrap();
    assert_eq!(fs::read(&lock.path).unwrap(), b"persistent lock inode");
    lock.acquire(0).unwrap();
    lock.release().unwrap();
    assert_eq!(fs::read(&lock.path).unwrap(), b"persistent lock inode");
}

#[test]
fn repository_lock_waiter_acquires_after_release_or_drop() {
    for explicit_release in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let mut owner = LockFile::new(&root, "app", Some("1.0"));
        owner.acquire(0).unwrap();
        let lock_path = owner.path.clone();
        let (blocked_tx, blocked_rx) = std::sync::mpsc::channel();

        let waiter = std::thread::spawn(move || {
            let mut contender = LockFile::new(&root, "app", Some("1.0"));
            assert!(contender.acquire(0).is_err());
            blocked_tx.send(()).unwrap();
            contender.acquire(5).unwrap();
            contender.release().unwrap();
        });
        blocked_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        if explicit_release {
            owner.release().unwrap();
        }
        drop(owner);
        waiter.join().unwrap();
        assert!(lock_path.is_file());
    }
}

#[test]
fn repository_lock_serializes_concurrent_read_modify_write() {
    let temp = tempfile::tempdir().unwrap();
    let counter_path = temp.path().join("counter");
    fs::write(&counter_path, "0").unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
    let mut workers = Vec::new();
    for _ in 0..4 {
        let root = temp.path().to_path_buf();
        let counter_path = counter_path.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            for _ in 0..8 {
                let mut lock = LockFile::new(&root, "app", Some("1.0"));
                lock.acquire(5).unwrap();
                let value: usize = fs::read_to_string(&counter_path).unwrap().parse().unwrap();
                std::thread::yield_now();
                fs::write(&counter_path, (value + 1).to_string()).unwrap();
                lock.release().unwrap();
            }
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(fs::read_to_string(counter_path).unwrap(), "32");
}

#[test]
fn repository_constructors_skip_missing_paths_without_creating_them() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing");
    assert!(FsRepo::open(&missing, false).unwrap().is_none());
    assert!(FsRepoCached::new(&missing, false).unwrap().is_none());
    assert!(
        FsRepoMemcached::new(&missing, MemcacheClient::new(&[]), false, false, false)
            .unwrap()
            .is_none()
    );
    assert!(!missing.exists());

    let created = temp.path().join("created");
    assert!(FsRepo::open(&created, true).unwrap().is_some());
    assert!(created.is_dir());
    assert!(FsRepo::open(&created, false).unwrap().is_some());
    assert!(FsRepoCached::new(&created, false).unwrap().is_some());
    assert!(
        FsRepoMemcached::new(&created, MemcacheClient::new(&[]), false, false, false)
            .unwrap()
            .is_some()
    );

    let non_directory = temp.path().join("file");
    fs::write(&non_directory, "not a directory").unwrap();
    assert!(FsRepo::open(&non_directory, false).is_err());
    assert!(FsRepoCached::new(&non_directory, false).is_err());
    assert!(FsRepoMemcached::new(
        &non_directory,
        MemcacheClient::new(&[]),
        false,
        false,
        false
    )
    .is_err());
}

#[test]
fn memcache_endpoint_normalization_matches_rez_forms() {
    let bare = memcache_url("127.0.0.1:11212").unwrap();
    assert_eq!(bare.client_url, "memcache://127.0.0.1:11212");
    assert_eq!(
        bare.transport,
        MemcacheTransport::Tcp {
            host: "127.0.0.1".into(),
            port: 11212
        }
    );

    let default_port = memcache_url("cache.example").unwrap();
    assert_eq!(default_port.client_url, "memcache://cache.example:11211");

    let inet = memcache_url("inet:cache.example:11213").unwrap();
    assert_eq!(inet.client_url, "memcache://cache.example:11213");

    let inet6 = memcache_url("inet6:[::1]:11214").unwrap();
    assert_eq!(inet6.client_url, "memcache://[::1]:11214");
    assert_eq!(
        inet6.transport,
        MemcacheTransport::Tcp {
            host: "::1".into(),
            port: 11214
        }
    );

    let configured_url = memcache_url("memcache://cache.example?timeout=2").unwrap();
    assert_eq!(
        configured_url.client_url,
        "memcache://cache.example:11211?timeout=2"
    );

    #[cfg(unix)]
    {
        let unix = memcache_url("unix:/tmp/memcached.sock").unwrap();
        assert_eq!(unix.client_url, "memcache+unix:/tmp/memcached.sock");
        assert_eq!(
            unix.transport,
            MemcacheTransport::Unix {
                path: "/tmp/memcached.sock".into()
            }
        );
    }

    #[cfg(not(unix))]
    assert!(memcache_url("unix:/tmp/memcached.sock").is_err());
}

#[test]
fn memcache_endpoint_normalization_rejects_malformed_and_non_stream_uris() {
    for uri in [
        "",
        "inet:",
        "inet6:::1",
        "inet6:[::1]:bad",
        "cache.example:port",
        "cache.example:11211:extra",
        "memcache://cache.example?udp=true",
    ] {
        assert!(memcache_url(uri).is_err(), "accepted malformed URI {uri:?}");
    }
}

#[cfg(unix)]
#[test]
fn memcache_stats_skips_unreachable_endpoints_and_parses_rows() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;

    let missing_path = std::env::temp_dir().join(format!(
        "rez-rs-missing-memcache-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let reachable_uri = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream);
        let mut command = String::new();
        reader.read_line(&mut command).unwrap();
        assert_eq!(command, "stats\r\n");
        reader
            .get_mut()
            .write_all(b"STAT cmd_get 12\r\nSTAT curr_connections 3\r\nEND\r\n")
            .unwrap();
    });

    let client = MemcacheClient::new(&[
        format!("unix:{}", missing_path.display()),
        reachable_uri.clone(),
    ]);
    let rows = client.stats().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, reachable_uri);
    assert_eq!(rows[0].1["cmd_get"], "12");
    assert_eq!(rows[0].1["curr_connections"], "3");
    server.join().unwrap();
}

#[cfg(unix)]
#[test]
fn memcache_probe_reports_unavailable_endpoint_without_aborting() {
    let missing_path = std::env::temp_dir().join(format!(
        "rez-rs-missing-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let uri = format!("unix:{}", missing_path.display());
    let outcomes = MemcacheClient::new(std::slice::from_ref(&uri))
        .probe_servers(Some(std::slice::from_ref(&uri)))
        .unwrap();
    assert!(matches!(
        &outcomes[..],
        [MemcacheProbeOutcome::Unavailable { server, .. }] if server == &uri
    ));
}

#[test]
fn memcache_reset_stats_sends_reset_to_every_server() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;

    let listeners = [
        TcpListener::bind("127.0.0.1:0").unwrap(),
        TcpListener::bind("127.0.0.1:0").unwrap(),
    ];
    let server_uris: Vec<_> = listeners
        .iter()
        .map(|listener| format!("127.0.0.1:{}", listener.local_addr().unwrap().port()))
        .collect();
    let threads: Vec<_> = listeners
        .into_iter()
        .map(|listener| {
            thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut command = String::new();
                reader.read_line(&mut command).unwrap();
                assert_eq!(command, "stats reset\r\n");
                reader.get_mut().write_all(b"RESET\r\n").unwrap();
            })
        })
        .collect();

    MemcacheClient::new(&server_uris).reset_stats().unwrap();
    for thread in threads {
        thread.join().unwrap();
    }
}

#[test]
fn memcache_hard_flush_flushes_then_resets_each_server() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;

    let listeners = [
        TcpListener::bind("127.0.0.1:0").unwrap(),
        TcpListener::bind("127.0.0.1:0").unwrap(),
    ];
    let server_uris: Vec<_> = listeners
        .iter()
        .map(|listener| format!("127.0.0.1:{}", listener.local_addr().unwrap().port()))
        .collect();
    let threads: Vec<_> = listeners
        .into_iter()
        .map(|listener| {
            thread::spawn(move || {
                for (expected_command, response) in [
                    ("flush_all\r\n", b"OK\r\n".as_slice()),
                    ("stats reset\r\n", b"END\r\n".as_slice()),
                ] {
                    let (stream, _) = listener.accept().unwrap();
                    let mut reader = BufReader::new(stream);
                    let mut command = String::new();
                    reader.read_line(&mut command).unwrap();
                    assert_eq!(command, expected_command);
                    reader.get_mut().write_all(response).unwrap();
                }
            })
        })
        .collect();

    MemcacheClient::new(&server_uris).flush().unwrap();
    for thread in threads {
        thread.join().unwrap();
    }
}

#[test]
fn memcache_probe_measures_and_cleans_up_each_server_key() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server_uri = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let thread = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream);
        let mut command = String::new();
        reader.read_line(&mut command).unwrap();
        let fields: Vec<_> = command.split_whitespace().collect();
        assert_eq!(fields[0], "set");
        assert!(fields[1].starts_with("rez_rs_probe_"));
        let key = fields[1].to_string();
        let size: usize = fields[4].parse().unwrap();
        let mut value_frame = vec![0; size + 2];
        reader.read_exact(&mut value_frame).unwrap();
        let value = String::from_utf8(value_frame[..size].to_vec()).unwrap();
        reader.get_mut().write_all(b"STORED\r\n").unwrap();

        command.clear();
        reader.read_line(&mut command).unwrap();
        assert_eq!(command, format!("get {key}\r\n"));
        reader
            .get_mut()
            .write_all(format!("VALUE {key} 0 {}\r\n{}\r\nEND\r\n", value.len(), value).as_bytes())
            .unwrap();

        drop(reader);
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream);
        command.clear();
        reader.read_line(&mut command).unwrap();
        assert_eq!(command, format!("delete {key}\r\n"));
        reader.get_mut().write_all(b"DELETED\r\n").unwrap();
    });

    let outcomes = MemcacheClient::new(&[server_uri])
        .probe_servers(None)
        .unwrap();
    assert_eq!(outcomes.len(), 1);
    let MemcacheProbeOutcome::Available(probe) = &outcomes[0] else {
        panic!("expected a successful probe");
    };
    assert!(probe.set_time >= std::time::Duration::ZERO);
    assert!(probe.get_time >= std::time::Duration::ZERO);
    thread.join().unwrap();
}

#[test]
fn memcache_probe_does_not_cleanup_before_stored_confirmation() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server_uri = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let thread = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream);
        let mut command = String::new();
        reader.read_line(&mut command).unwrap();
        let fields: Vec<_> = command.split_whitespace().collect();
        assert_eq!(fields[0], "set");
        let key = fields[1].to_string();
        assert!(key.starts_with("rez_rs_probe_"));

        // The client sends the complete set frame before it waits for the
        // server response. Consume its value and CRLF before returning an
        // error so closing the socket cannot replace the response with RST.
        let value_size: usize = fields[4].parse().unwrap();
        let mut value_frame = vec![0; value_size + 2];
        reader.read_exact(&mut value_frame).unwrap();
        assert_eq!(&value_frame[value_size..], b"\r\n");
        reader.get_mut().write_all(b"NOT_STORED\r\n").unwrap();
    });

    let error = MemcacheClient::new(&[server_uri])
        .probe_servers(None)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("probe set returned"),
        "unexpected probe error: {error}"
    );
    thread.join().unwrap();
}

#[test]
fn package_info_conversion_uses_canonical_package_data_parser() {
    let data =
        serde_json::from_value::<serde_json::Map<String, serde_json::Value>>(serde_json::json!({
            "name": "demo",
            "version": "1.2.3",
            "authors": ["Rez Team"],
            "requires": ["python-3.11"],
            "tools": ["demo"],
            "tags": ["stable"],
            "build_system": "custom",
            "pre_test_commands": "echo pre-test"
        }))
        .unwrap()
        .into_iter()
        .collect();

    let info = PackageInfo::from_data(data).unwrap();
    let package = info.to_package().unwrap();

    assert_eq!(package.name, "demo");
    assert_eq!(package.version.to_string(), "1.2.3");
    assert_eq!(package.authors, vec!["Rez Team"]);
    assert_eq!(package.requires.len(), 1);
    assert_eq!(package.tools, vec!["demo"]);
    assert_eq!(package.tags, vec!["stable"]);
    assert_eq!(package.build_system.as_deref(), Some("custom"));
    assert_eq!(package.pre_test_commands.as_deref(), Some("echo pre-test"));
}

#[test]
fn package_info_conversion_propagates_invalid_package_metadata() {
    let mut data = HashMap::new();
    data.insert("name".to_string(), serde_json::json!("demo"));
    data.insert("version".to_string(), serde_json::json!("1.2.3"));
    data.insert("requires".to_string(), serde_json::json!(17));

    let info = PackageInfo::from_data(data).unwrap();

    assert!(info.to_package().is_err());
}

#[test]
fn repository_loader_uses_authoritative_base_before_deferred_includes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("included/1.0");
    fs::create_dir_all(root.join(".rez/include")).unwrap();
    fs::write(
        root.join(".rez/include/repository_helper.py"),
        "requirements = ['dependency-2']\n",
    )
    .unwrap();
    let definition = root.join("package.py");
    fs::write(
        &definition,
        "name = 'included'\nversion = '1.0'\nbase = 'untrusted-definition-base'\n@include('repository_helper')\n@late()\ndef requires():\n    return repository_helper.requirements\n",
    )
    .unwrap();

    let data = load_package_data(&definition).unwrap();
    assert_eq!(data["base"], serde_json::json!(root.to_string_lossy()));
    let package = PackageInfo::from_data(data).unwrap().to_package().unwrap();
    assert_eq!(package.base.as_deref(), Some(root.as_path()));
    assert_eq!(package.requires, vec!["dependency-2".parse().unwrap()]);
    assert!(!package.to_data().unwrap().contains_key("base"));

    let repo = FsRepo::open(temp.path(), false).unwrap().unwrap();
    let loaded = repo
        .get_package("included", &Version::new("1.0").unwrap())
        .unwrap()
        .unwrap()
        .to_package()
        .unwrap();
    assert_eq!(loaded.requires, package.requires);
    assert_eq!(loaded.base, package.base);

    // Existing installed metadata follows the same include-aware loading path.
    assert!(is_variant_installed(&package, &root, 0, None).unwrap());
    publish_package(&package, &root, &[0], None, None).unwrap();
    let republished = PackageInfo::from_data(load_package_data(&definition).unwrap())
        .unwrap()
        .to_package()
        .unwrap();
    assert_eq!(republished.requires, package.requires);
    assert!(!republished.to_data().unwrap().contains_key("base"));
}

#[test]
fn repository_deferred_include_errors_propagate_from_installed_modules() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("broken_include/1.0");
    fs::create_dir_all(root.join(".rez/include")).unwrap();
    fs::write(
        root.join(".rez/include/repository_broken_helper.py"),
        "raise ValueError('installed include failure')\n",
    )
    .unwrap();
    fs::write(
        root.join("package.py"),
        "name = 'broken_include'\nversion = '1.0'\n@include('repository_broken_helper')\n@late()\ndef requires():\n    return []\n",
    )
    .unwrap();
    let repo = FsRepo::open(temp.path(), false).unwrap().unwrap();
    let info = repo
        .get_package("broken_include", &Version::new("1.0").unwrap())
        .unwrap()
        .unwrap();
    let error = info.to_package().unwrap_err().to_string();
    assert!(error.contains("ValueError"), "{error}");
    assert!(error.contains("installed include failure"), "{error}");
}

#[test]
fn combined_family_materialization_discards_base_in_source_and_overrides() {
    let raw = serde_json::from_value(serde_json::json!({
        "name": "combined", "versions": ["1.0"],
        "base": "definition-authored-base",
        "version_overrides": {"1": {"base": "override-authored-base"}}
    }))
    .unwrap();
    let family = CombinedFamilyData::from_data(raw, Path::new("combined.yaml")).unwrap();
    let materialized = family
        .materialize(&Version::new("1.0").unwrap(), true)
        .unwrap()
        .unwrap();
    assert!(!materialized.contains_key("base"));
    assert!(PackageInfo::from_data(materialized)
        .unwrap()
        .to_package()
        .unwrap()
        .base
        .is_none());

    let unversioned = CombinedFamilyData::from_data(
        serde_json::from_value(serde_json::json!({"name": "combined", "base": "source-base"}))
            .unwrap(),
        Path::new("combined.yaml"),
    )
    .unwrap()
    .materialize(&Version::empty(), true)
    .unwrap()
    .unwrap();
    assert!(!unversioned.contains_key("base"));
}

#[test]
fn repository_package_cache_keys_exclude_old_provenance_payloads() {
    let key = cache_key_for_package(Path::new("repo"), "package", "1.0");
    assert!(key.starts_with("rez:pkg:v2:"), "{key}");
}

#[test]
fn test_memory_repo_create() {
    let repo = MemoryPackageRepository::new();
    assert_eq!(repo.name(), "memory");
    assert!(repo.location().starts_with("memory@"));
}

#[test]
fn test_memory_repo_add_package() {
    let mut repo = MemoryPackageRepository::new();

    let mut data = HashMap::new();
    data.insert("name".to_string(), serde_json::json!("foo"));
    data.insert("version".to_string(), serde_json::json!("1.0.0"));
    data.insert("description".to_string(), serde_json::json!("Test package"));

    let info = PackageInfo::from_data(data).unwrap();
    repo.add_package(info).unwrap();

    let families = repo.iter_family_names().unwrap();
    assert_eq!(families, vec!["foo"]);

    let versions = repo.iter_versions("foo").unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].to_string(), "1.0.0");
}

#[test]
fn test_memory_repo_get_package() {
    let mut repo = MemoryPackageRepository::new();

    let mut data = HashMap::new();
    data.insert("name".to_string(), serde_json::json!("bar"));
    data.insert("version".to_string(), serde_json::json!("2.1.0"));
    data.insert("requires".to_string(), serde_json::json!(["foo-1.0+"]));

    let info = PackageInfo::from_data(data).unwrap();
    repo.add_package(info).unwrap();

    let version: Version = "2.1.0".parse().unwrap();
    let pkg = repo.get_package("bar", &version).unwrap();

    assert!(pkg.is_some());
    let pkg = pkg.unwrap();
    assert_eq!(pkg.name, "bar");
    assert_eq!(pkg.version.to_string(), "2.1.0");
    assert!(pkg.get("requires").is_some());
}

#[test]
fn test_memory_repo_multiple_versions() {
    let mut repo = MemoryPackageRepository::new();

    // Add version 1.0.0
    let mut data1 = HashMap::new();
    data1.insert("name".to_string(), serde_json::json!("pkg"));
    data1.insert("version".to_string(), serde_json::json!("1.0.0"));
    repo.add_package(PackageInfo::from_data(data1).unwrap())
        .unwrap();

    // Add version 2.0.0
    let mut data2 = HashMap::new();
    data2.insert("name".to_string(), serde_json::json!("pkg"));
    data2.insert("version".to_string(), serde_json::json!("2.0.0"));
    repo.add_package(PackageInfo::from_data(data2).unwrap())
        .unwrap();

    // Add version 1.5.0
    let mut data3 = HashMap::new();
    data3.insert("name".to_string(), serde_json::json!("pkg"));
    data3.insert("version".to_string(), serde_json::json!("1.5.0"));
    repo.add_package(PackageInfo::from_data(data3).unwrap())
        .unwrap();

    let versions = repo.iter_versions("pkg").unwrap();
    assert_eq!(versions.len(), 3);

    // Should be sorted
    assert_eq!(versions[0].to_string(), "1.0.0");
    assert_eq!(versions[1].to_string(), "1.5.0");
    assert_eq!(versions[2].to_string(), "2.0.0");
}

#[test]
fn memory_repository_rejects_malformed_version_keys() {
    let data = HashMap::from([(
        "pkg".to_string(),
        HashMap::from([
            ("1.0".to_string(), HashMap::new()),
            ("1.".to_string(), HashMap::new()),
        ]),
    )]);
    let repo = MemoryPackageRepository::with_data(data);

    assert!(repo.iter_versions("pkg").is_err());
}

#[test]
fn test_package_repo_manager() {
    let mut manager = PackageRepositoryManager::new();

    let mut repo1 = MemoryPackageRepository::new();
    let mut data = HashMap::new();
    data.insert("name".to_string(), serde_json::json!("test"));
    data.insert("version".to_string(), serde_json::json!("1.0.0"));
    repo1
        .add_package(PackageInfo::from_data(data).unwrap())
        .unwrap();

    manager.add_repo(Box::new(repo1));

    let packages = manager.iter_packages("test").unwrap();
    assert_eq!(packages.len(), 1);
    assert_eq!(packages[0].name, "test");

    let latest = manager.get_latest("test").unwrap();
    assert!(latest.is_some());
    assert_eq!(latest.unwrap().version.to_string(), "1.0.0");
}

#[test]
fn test_manager_priority() {
    // First repo wins for same package version
    let mut manager = PackageRepositoryManager::new();

    // Repo 1: has pkg 1.0.0
    let mut repo1 = MemoryPackageRepository::new();
    let mut data1 = HashMap::new();
    data1.insert("name".to_string(), serde_json::json!("pkg"));
    data1.insert("version".to_string(), serde_json::json!("1.0.0"));
    data1.insert("repo".to_string(), serde_json::json!("repo1"));
    repo1
        .add_package(PackageInfo::from_data(data1).unwrap())
        .unwrap();

    // Repo 2: has pkg 1.0.0 and 2.0.0
    let mut repo2 = MemoryPackageRepository::new();
    let mut data2 = HashMap::new();
    data2.insert("name".to_string(), serde_json::json!("pkg"));
    data2.insert("version".to_string(), serde_json::json!("1.0.0"));
    data2.insert("repo".to_string(), serde_json::json!("repo2"));
    repo2
        .add_package(PackageInfo::from_data(data2).unwrap())
        .unwrap();

    let mut data3 = HashMap::new();
    data3.insert("name".to_string(), serde_json::json!("pkg"));
    data3.insert("version".to_string(), serde_json::json!("2.0.0"));
    data3.insert("repo".to_string(), serde_json::json!("repo2"));
    repo2
        .add_package(PackageInfo::from_data(data3).unwrap())
        .unwrap();

    manager.add_repo(Box::new(repo1));
    manager.add_repo(Box::new(repo2));

    let packages = manager.iter_packages("pkg").unwrap();
    assert_eq!(packages.len(), 2); // 1.0.0 from repo1, 2.0.0 from repo2

    // Latest should be 2.0.0
    let latest = manager.get_latest("pkg").unwrap().unwrap();
    assert_eq!(latest.version.to_string(), "2.0.0");
    assert_eq!(latest.get("repo").unwrap().as_str().unwrap(), "repo2");

    // 1.0.0 should be from repo1 (first match wins)
    let v1: Version = "1.0.0".parse().unwrap();
    let pkg_v1 = manager.get_package("pkg", &v1).unwrap().unwrap();
    assert_eq!(pkg_v1.get("repo").unwrap().as_str().unwrap(), "repo1");
}

#[test]
fn test_unversioned_package() {
    let mut repo = MemoryPackageRepository::new();

    let mut data = HashMap::new();
    data.insert("name".to_string(), serde_json::json!("unver"));
    // No version field

    let info = PackageInfo::from_data(data).unwrap();
    assert!(info.version.is_empty());

    repo.add_package(info).unwrap();

    let families = repo.iter_family_names().unwrap();
    assert_eq!(families, vec!["unver"]);

    let versions = repo.iter_versions("unver").unwrap();
    assert_eq!(versions.len(), 0); // Unversioned packages don't show in iter_versions

    // Get unversioned package
    let empty_version = Version::empty();
    let pkg = repo.get_package("unver", &empty_version).unwrap();
    assert!(pkg.is_some());
    assert_eq!(pkg.unwrap().name, "unver");
}

#[test]
fn combined_yaml_and_python_apply_overrides_in_source_order() {
    use std::fs;
    use tempfile::tempdir;

    let temp = tempdir().unwrap();
    let yaml_path = temp.path().join("combined_yaml.yaml");
    fs::write(
        &yaml_path,
        "name: combined_yaml\nversions: ['2.0', '1.0']\ndescription: base\nsettings:\n  base: true\n  source: base\nversion_overrides:\n  '1.0+':\n    description: first\n    settings: {source: first}\n  '>=1.0':\n    description: second\n    settings: {source: second}\n",
    )
    .unwrap();
    let repo = FsRepo::new(temp.path()).unwrap();
    let raw = repo.load_package_file(&yaml_path).unwrap();
    let override_keys: Vec<_> = raw["version_overrides"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(override_keys, ["1.0+", ">=1.0"]);

    let versions: Vec<_> = repo
        .iter_versions("combined_yaml")
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(versions, ["2.0", "1.0"]);

    let package = repo
        .get_package("combined_yaml", &Version::new("2.0").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(package.data["description"], "second");
    assert_eq!(
        package.data["settings"],
        serde_json::json!({"source": "second"})
    );
    assert!(!package.data.contains_key("base"));

    let py_path = temp.path().join("combined_python.py");
    fs::write(
        &py_path,
        "name = 'combined_python'\nversions = ['2.0', '1.0']\ndescription = 'base'\nsettings = {'base': True, 'source': 'base'}\nversion_overrides = {'1.0+': {'description': 'first', 'settings': {'source': 'first'}}, '>=1.0': {'description': 'second', 'settings': {'source': 'second'}}}\n",
    )
    .unwrap();
    let py_raw = repo.load_package_file(&py_path).unwrap();
    let py_override_keys: Vec<_> = py_raw["version_overrides"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(py_override_keys, ["1.0+", ">=1.0"]);

    let package = repo
        .get_package("combined_python", &Version::new("2.0").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(package.data["description"], "second");
    assert_eq!(
        package.data["settings"],
        serde_json::json!({"source": "second"})
    );
    assert!(!package.data.contains_key("base"));
}

#[test]
fn combined_family_supports_unversioned_setting_and_malformed_metadata_errors() {
    use std::fs;
    use tempfile::tempdir;

    let temp = tempdir().unwrap();
    let repo = FsRepo::new(temp.path()).unwrap();
    let no_versions = temp.path().join("plain.yaml");
    fs::write(&no_versions, "name: plain\ndescription: unversioned\n").unwrap();
    let combined = repo
        .load_combined_family(
            &repo
                .family_sources("plain")
                .unwrap()
                .into_iter()
                .next()
                .unwrap(),
        )
        .unwrap();
    let empty = Version::empty();
    assert!(combined.materialize(&empty, true).unwrap().is_some());
    assert!(combined.materialize(&empty, false).unwrap().is_none());
    assert!(combined
        .materialize(&Version::new("1.0").unwrap(), true)
        .unwrap()
        .is_none());
    if CONFIG.allow_unversioned_packages {
        assert_eq!(repo.iter_versions("plain").unwrap(), vec![Version::empty()]);
        let package = repo.get_package("plain", &empty).unwrap().unwrap();
        assert!(package.version.is_empty());
        assert!(!package.data.contains_key("base"));
    } else {
        assert!(repo.iter_versions("plain").unwrap().is_empty());
        assert!(repo.get_package("plain", &empty).unwrap().is_none());
    }

    fs::write(
        temp.path().join("bad_versions.yaml"),
        "name: bad_versions\nversions: 1.0\n",
    )
    .unwrap();
    assert!(repo.iter_versions("bad_versions").is_err());

    fs::write(
        temp.path().join("bad_overrides.yaml"),
        "name: bad_overrides\nversions: ['1.0']\nversion_overrides:\n  'not a range':\n    description: invalid\n",
    )
    .unwrap();
    assert!(repo.iter_versions("bad_overrides").is_err());
}

#[test]
fn combined_family_direct_lookup_prefers_directory_then_python_then_yaml() {
    use std::fs;
    use tempfile::tempdir;

    let temp = tempdir().unwrap();
    fs::write(
        temp.path().join("priority.py"),
        "name = 'priority'\nversions = ['2.0', '3.0']\ndescription = 'python'\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("priority.yaml"),
        "name: priority\nversions: ['2.0', '3.0', '4.0']\ndescription: yaml\n",
    )
    .unwrap();
    let repo = FsRepo::new(temp.path()).unwrap();
    assert_eq!(repo.iter_family_names().unwrap(), ["priority"]);
    assert_eq!(
        repo.iter_versions("priority").unwrap(),
        vec![
            Version::new("2.0").unwrap(),
            Version::new("3.0").unwrap(),
            Version::new("4.0").unwrap(),
        ]
    );
    let package = repo
        .get_package("priority", &Version::new("2.0").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(package.data["description"], "python");
    let (package, origin) = repo
        .get_package_with_source("priority", &Version::new("2.0").unwrap(), None)
        .unwrap()
        .unwrap();
    assert_eq!(package.data["description"], "python");
    let origin = origin.unwrap();
    assert_eq!(origin.kind, PackageSourceKind::Python);
    assert_eq!(origin.path, temp.path().join("priority.py"));
    assert_eq!(
        repo.get_package("priority", &Version::new("3.0").unwrap())
            .unwrap()
            .unwrap()
            .data["description"],
        "python"
    );
    let (package, origin) = repo
        .get_package_with_source("priority", &Version::new("4.0").unwrap(), None)
        .unwrap()
        .unwrap();
    assert_eq!(package.data["description"], "yaml");
    let origin = origin.unwrap();
    assert_eq!(origin.kind, PackageSourceKind::Yaml);
    assert_eq!(origin.path, temp.path().join("priority.yaml"));

    for (version, description) in [("1.0", "directory"), ("2.0", "directory duplicate")] {
        let package_dir = temp.path().join("priority").join(version);
        fs::create_dir_all(&package_dir).unwrap();
        fs::write(
            package_dir.join("package.yaml"),
            format!("name: priority\nversion: '{version}'\ndescription: '{description}'\n"),
        )
        .unwrap();
    }
    assert_eq!(
        repo.iter_versions("priority").unwrap(),
        vec![
            Version::new("1.0").unwrap(),
            Version::new("2.0").unwrap(),
            Version::new("3.0").unwrap(),
            Version::new("4.0").unwrap(),
        ]
    );
    let package = repo
        .get_package("priority", &Version::new("1.0").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(package.data["description"], "directory");
    let (package, origin) = repo
        .get_package_with_source("priority", &Version::new("1.0").unwrap(), None)
        .unwrap()
        .unwrap();
    assert_eq!(package.data["description"], "directory");
    let origin = origin.unwrap();
    assert_eq!(origin.kind, PackageSourceKind::Directory);
    assert_eq!(origin.path, temp.path().join("priority").join("1.0"));
    let package = repo
        .get_package("priority", &Version::new("2.0").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(package.data["description"], "directory duplicate");
    let scanned = repo.scan_all().unwrap();
    assert_eq!(
        scanned["priority"]
            .iter()
            .map(|package| package.version.to_string())
            .collect::<Vec<_>>(),
        ["1.0", "2.0", "3.0", "4.0"]
    );
    assert_eq!(scanned["priority"][2].data["description"], "python");
    assert_eq!(scanned["priority"][3].data["description"], "yaml");
}

#[test]
fn removing_family_counts_distinct_versions_across_all_sources() {
    use std::fs;
    use tempfile::tempdir;

    let temp = tempdir().unwrap();
    let directory_package = temp.path().join("removable").join("1.0");
    fs::create_dir_all(&directory_package).unwrap();
    fs::write(
        directory_package.join("package.yaml"),
        "name: removable\nversion: '1.0'\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("removable.py"),
        "name = 'removable'\nversions = ['1.0', '2.0']\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("removable.yaml"),
        "name: removable\nversions: ['1.0', '3.0']\n",
    )
    .unwrap();
    fs::write(temp.path().join("removable").join(".ignore1.0"), "").unwrap();

    assert!(remove_package_family(temp.path(), "removable", false).is_err());
    assert!(directory_package.exists());
    assert!(temp.path().join("removable.py").exists());
    assert!(temp.path().join("removable.yaml").exists());

    assert_eq!(
        remove_package_family(temp.path(), "removable", true).unwrap(),
        Some(3)
    );
    assert!(!temp.path().join("removable").exists());
    assert!(!temp.path().join("removable.py").exists());
    assert!(!temp.path().join("removable.yaml").exists());
}

#[test]
fn combined_family_flows_through_scan_discovery_and_resolver_provider() {
    use crate::provider::{FilesystemPackageProvider, PackageProvider};
    use std::fs;
    use tempfile::tempdir;

    let temp = tempdir().unwrap();
    fs::write(
        temp.path().join("resolve_family.yaml"),
        "name: resolve_family\nversions: ['2.0', '1.0']\ndescription: yaml\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("resolve_family.py"),
        "name = 'resolve_family'\nversions = ['2.0', '3.0']\ndescription = 'python'\n",
    )
    .unwrap();
    for version in ["0.5", "2.0"] {
        let package_dir = temp.path().join("resolve_family").join(version);
        fs::create_dir_all(&package_dir).unwrap();
        fs::write(
            package_dir.join("package.yaml"),
            format!("name: resolve_family\nversion: '{version}'\ndescription: directory\n"),
        )
        .unwrap();
    }
    let repo = FsRepo::new(temp.path()).unwrap();

    let expected = ["0.5", "2.0", "3.0", "1.0"];
    let scanned = repo.scan_all().unwrap();
    assert_eq!(
        scanned["resolve_family"]
            .iter()
            .map(|package| package.version.to_string())
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(
        scanned["resolve_family"][1].data["description"],
        "directory"
    );
    assert_eq!(scanned["resolve_family"][2].data["description"], "python");
    assert_eq!(scanned["resolve_family"][3].data["description"], "yaml");
    let manager = PackageRepositoryManager::from_paths(&[temp.path().to_path_buf()]).unwrap();
    assert_eq!(manager.iter_family_names().unwrap(), ["resolve_family"]);
    assert_eq!(manager.iter_packages("resolve_family").unwrap().len(), 4);

    let provider = FilesystemPackageProvider::from_path(temp.path()).unwrap();
    let packages = provider
        .get_packages("resolve_family", &VersionRange::new("").unwrap())
        .unwrap();
    assert_eq!(packages.len(), expected.len());
    assert_eq!(
        packages
            .iter()
            .map(|package| package.version.to_string())
            .collect::<std::collections::BTreeSet<_>>(),
        expected.into_iter().map(str::to_owned).collect()
    );
}

#[test]
fn combined_source_fingerprints_invalidate_index_and_cached_repository_entries() {
    use std::fs;
    use tempfile::tempdir;

    let temp = tempdir().unwrap();
    let path = temp.path().join("changing.yaml");
    fs::write(
        &path,
        "name: changing\nversions: ['1.0']\ndescription: old\n",
    )
    .unwrap();
    let repo = FsRepo::new(temp.path()).unwrap();
    let cached = FsRepoCached::new(temp.path(), true).unwrap().unwrap();
    assert_eq!(
        repo.scan_all().unwrap()["changing"][0].version.to_string(),
        "1.0"
    );
    assert_eq!(
        cached.iter_versions("changing").unwrap()[0].to_string(),
        "1.0"
    );
    assert_eq!(
        cached
            .get_package("changing", &Version::new("1.0").unwrap())
            .unwrap()
            .unwrap()
            .data["description"],
        "old"
    );
    let (cached_info, cached_origin) = cached
        .get_package_with_source("changing", &Version::new("1.0").unwrap(), None)
        .unwrap()
        .unwrap();
    assert_eq!(cached_info.data["description"], "old");
    let cached_origin = cached_origin.unwrap();
    assert_eq!(cached_origin.kind, PackageSourceKind::Yaml);
    assert_eq!(cached_origin.path, path);
    let payload = MemcachedPackage {
        data: cached_info.data,
        origin: cached_origin.clone(),
    };
    let encoded = serde_json::to_string(&payload).unwrap();
    let decoded: MemcachedPackage = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded.origin, cached_origin);
    let old_fingerprint = repo.family_fingerprint("changing").unwrap().unwrap();
    let old_key = cache_key_for_source("rez:test", &old_fingerprint).unwrap();

    let py_path = temp.path().join("changing.py");
    fs::write(
        &py_path,
        "name = 'changing'\nversions = ['2.0']\ndescription = 'python'\n",
    )
    .unwrap();
    assert_eq!(
        cached.iter_versions("changing").unwrap(),
        vec![Version::new("2.0").unwrap(), Version::new("1.0").unwrap()]
    );
    let (package, origin) = cached
        .get_package_with_source("changing", &Version::new("2.0").unwrap(), None)
        .unwrap()
        .unwrap();
    assert_eq!(package.data["description"], "python");
    let origin = origin.unwrap();
    assert_eq!(origin.kind, PackageSourceKind::Python);
    assert_eq!(origin.path, py_path);

    fs::write(
        &py_path,
        "name = 'changing'\nversions = ['3.0']\ndescription = 'python'\n",
    )
    .unwrap();
    let new_fingerprint = repo.family_fingerprint("changing").unwrap().unwrap();
    let new_key = cache_key_for_source("rez:test", &new_fingerprint).unwrap();
    assert_ne!(old_key, new_key);
    assert_eq!(
        repo.scan_all().unwrap()["changing"]
            .iter()
            .map(|package| package.version.to_string())
            .collect::<Vec<_>>(),
        ["3.0", "1.0"]
    );
    assert_eq!(
        cached.iter_versions("changing").unwrap(),
        vec![Version::new("3.0").unwrap(), Version::new("1.0").unwrap()]
    );
    let (package, origin) = cached
        .get_package_with_source("changing", &Version::new("3.0").unwrap(), None)
        .unwrap()
        .unwrap();
    assert_eq!(package.data["description"], "python");
    let origin = origin.unwrap();
    assert_eq!(origin.kind, PackageSourceKind::Python);
    assert_eq!(origin.path, py_path);
    assert!(cached
        .get_package("changing", &Version::new("2.0").unwrap())
        .unwrap()
        .is_none());

    fs::remove_file(&py_path).unwrap();
    assert_eq!(
        cached.iter_versions("changing").unwrap(),
        vec![Version::new("1.0").unwrap()]
    );
    assert!(cached
        .get_package("changing", &Version::new("3.0").unwrap())
        .unwrap()
        .is_none());
}

#[test]
fn test_filesystem_scan_all() {
    use std::fs;
    use tempfile::tempdir;

    let temp_dir = tempdir().unwrap();
    let repo_path = temp_dir.path();

    // Create package structure: foo/1.0.0, foo/2.0.0, bar/1.0.0
    let foo_dir = repo_path.join("foo");
    fs::create_dir_all(foo_dir.join("1.0.0")).unwrap();
    fs::create_dir_all(foo_dir.join("2.0.0")).unwrap();

    let bar_dir = repo_path.join("bar");
    fs::create_dir_all(bar_dir.join("1.0.0")).unwrap();

    // Write package.yaml files
    fs::write(
        foo_dir.join("1.0.0/package.yaml"),
        "name: foo\nversion: '1.0.0'\n",
    )
    .unwrap();
    fs::write(
        foo_dir.join("2.0.0/package.yaml"),
        "name: foo\nversion: '2.0.0'\n",
    )
    .unwrap();
    fs::write(
        bar_dir.join("1.0.0/package.yaml"),
        "name: bar\nversion: '1.0.0'\n",
    )
    .unwrap();

    let repo = FsRepo::new(repo_path).unwrap();
    let all_packages = repo.scan_all().unwrap();

    // Should have 2 families
    assert_eq!(all_packages.len(), 2);
    assert!(all_packages.contains_key("foo"));
    assert!(all_packages.contains_key("bar"));

    // foo should have 2 versions
    let foo_pkgs = &all_packages["foo"];
    assert_eq!(foo_pkgs.len(), 2);

    // bar should have 1 version
    let bar_pkgs = &all_packages["bar"];
    assert_eq!(bar_pkgs.len(), 1);
}

#[test]
fn filesystem_package_lookup_rejects_parent_traversal_names() {
    use std::fs;
    use tempfile::tempdir;

    let temp_dir = tempdir().unwrap();
    let repo_path = temp_dir.path().join("packages");
    let escaped_package = temp_dir.path().join("1.0");
    fs::create_dir_all(&escaped_package).unwrap();
    fs::write(
        escaped_package.join("package.yaml"),
        "name: escaped\nversion: '1.0'\n",
    )
    .unwrap();

    let repo = FsRepo::new(&repo_path).unwrap();
    let version = Version::new("1.0").unwrap();

    assert!(matches!(
        repo.get_package("..", &version),
        Err(RezError::PackageRequest(_))
    ));
    let path_separated_version = Version::new("1/0").unwrap();
    assert!(matches!(
        repo.get_package("escaped", &path_separated_version),
        Err(RezError::PackageRequest(_))
    ));
}

#[test]
fn filesystem_family_discovery_uses_rez_package_name_rules() {
    use std::fs;
    use tempfile::tempdir;

    let temp_dir = tempdir().unwrap();
    for name in ["foo.bar", "_private", "bad name", "__pycache__", ".hidden"] {
        fs::create_dir(temp_dir.path().join(name)).unwrap();
    }

    let repo = FsRepo::new(temp_dir.path()).unwrap();

    assert_eq!(
        repo.iter_family_names().unwrap(),
        vec!["_private".to_owned(), "foo.bar".to_owned()]
    );
}

#[test]
fn filesystem_version_discovery_reports_invalid_version_names() {
    use std::fs;
    use tempfile::tempdir;

    let temp_dir = tempdir().unwrap();
    let repo_path = temp_dir.path();
    let invalid_version_dir = repo_path.join("pkg").join("1..0");
    fs::create_dir_all(&invalid_version_dir).unwrap();
    fs::write(
        invalid_version_dir.join("package.yaml"),
        "name: pkg\nversion: '1.0.0'\n",
    )
    .unwrap();

    let repo = FsRepo::new(repo_path).unwrap();

    assert!(matches!(repo.scan_all(), Err(RezError::Version(_))));
}

#[test]
fn directory_fingerprint_preserves_subsecond_timestamp_changes() {
    let cached = DirectoryFingerprint {
        modified: Some(DirectoryTimestamp {
            seconds: 42,
            nanoseconds: 100,
        }),
        entries: Vec::new(),
    };
    let changed_within_the_same_second = DirectoryFingerprint {
        modified: Some(DirectoryTimestamp {
            seconds: 42,
            nanoseconds: 101,
        }),
        entries: Vec::new(),
    };

    assert_ne!(cached, changed_within_the_same_second);
    assert_eq!(cached, cached.clone());
}

#[test]
fn filesystem_scan_observes_versions_created_and_removed_immediately() {
    use std::fs;
    use tempfile::tempdir;

    let temp_dir = tempdir().unwrap();
    let repo_path = temp_dir.path();
    let family_path = repo_path.join("pkg");
    let v1_path = family_path.join("1.0.0");
    fs::create_dir_all(&v1_path).unwrap();
    fs::write(
        v1_path.join("package.yaml"),
        "name: pkg\nversion: '1.0.0'\n",
    )
    .unwrap();

    let repo = FsRepo::new(repo_path).unwrap();
    assert_eq!(repo.scan_all().unwrap()["pkg"].len(), 1);

    let v2_path = family_path.join("2.0.0");
    fs::create_dir_all(&v2_path).unwrap();
    fs::write(
        v2_path.join("package.yaml"),
        "name: pkg\nversion: '2.0.0'\n",
    )
    .unwrap();
    assert_eq!(repo.scan_all().unwrap()["pkg"].len(), 2);

    fs::remove_dir_all(v2_path).unwrap();
    assert_eq!(repo.scan_all().unwrap()["pkg"].len(), 1);
}

#[cfg(unix)]
#[test]
fn broken_repository_symlinks_do_not_break_directory_fingerprints() {
    use std::fs;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    let temp = tempdir().unwrap();
    let root = temp.path();
    let version = root.join("pkg").join("1.0.0");
    fs::create_dir_all(&version).unwrap();
    fs::write(
        version.join("package.yaml"),
        "name: pkg\nversion: '1.0.0'\n",
    )
    .unwrap();

    let repo = FsRepo::new(root).unwrap();
    let cached = FsRepoCached::new(root, true).unwrap().unwrap();
    assert_eq!(cached.iter_family_names().unwrap(), ["pkg"]);
    repo.scan_all().unwrap();

    symlink(root.join("missing-target"), root.join("dangling")).unwrap();

    assert_eq!(cached.iter_family_names().unwrap(), ["pkg"]);
    assert_eq!(repo.scan_all().unwrap()["pkg"].len(), 1);
}

#[test]
fn repository_directory_caches_detect_entry_changes_when_timestamps_are_restored() {
    use filetime::{set_file_mtime, FileTime};
    use std::fs;
    use std::time::SystemTime;
    use tempfile::tempdir;

    let temp = tempdir().unwrap();
    let root = temp.path();
    let family = root.join("pkg");
    let version = family.join("1.0.0");
    fs::create_dir_all(&version).unwrap();
    fs::write(
        version.join("package.yaml"),
        "name: pkg\nversion: '1.0.0'\n",
    )
    .unwrap();

    let repo = FsRepo::new(root).unwrap();
    let cached = FsRepoCached::new(root, true).unwrap().unwrap();
    assert_eq!(cached.iter_family_names().unwrap(), ["pkg"]);
    assert_eq!(
        cached.iter_versions("pkg").unwrap(),
        [Version::new("1.0.0").unwrap()]
    );
    repo.scan_all().unwrap();

    let restore_mtime = |path: &Path, modified: SystemTime| {
        set_file_mtime(path, FileTime::from_system_time(modified)).unwrap();
    };

    let root_mtime = fs::metadata(root).unwrap().modified().unwrap();
    let added_family = root.join("added");
    let added_version = added_family.join("2.0.0");
    fs::create_dir_all(&added_version).unwrap();
    fs::write(
        added_version.join("package.yaml"),
        "name: added\nversion: '2.0.0'\n",
    )
    .unwrap();
    restore_mtime(root, root_mtime);

    assert_eq!(cached.iter_family_names().unwrap(), ["added", "pkg"]);
    assert!(repo.scan_all().unwrap().contains_key("added"));

    fs::remove_dir_all(&added_family).unwrap();
    restore_mtime(root, root_mtime);
    assert_eq!(cached.iter_family_names().unwrap(), ["pkg"]);
    assert!(!repo.scan_all().unwrap().contains_key("added"));

    let family_mtime = fs::metadata(&family).unwrap().modified().unwrap();
    let root_mtime = fs::metadata(root).unwrap().modified().unwrap();
    let added_version = family.join("2.0.0");
    fs::create_dir(&added_version).unwrap();
    let version_mtime = fs::metadata(&added_version).unwrap().modified().unwrap();
    fs::write(
        added_version.join("package.yaml"),
        "name: pkg\nversion: '2.0.0'\n",
    )
    .unwrap();
    restore_mtime(&added_version, version_mtime);
    restore_mtime(&family, family_mtime);
    restore_mtime(root, root_mtime);

    assert_eq!(
        cached.iter_versions("pkg").unwrap(),
        [
            Version::new("1.0.0").unwrap(),
            Version::new("2.0.0").unwrap()
        ]
    );
    assert_eq!(repo.scan_all().unwrap()["pkg"].len(), 2);

    fs::remove_dir_all(&added_version).unwrap();
    restore_mtime(&family, family_mtime);
    restore_mtime(root, root_mtime);
    assert_eq!(
        cached.iter_versions("pkg").unwrap(),
        [Version::new("1.0.0").unwrap()]
    );
    assert_eq!(repo.scan_all().unwrap()["pkg"].len(), 1);
}

#[test]
fn filesystem_scan_rejects_legacy_second_precision_indexes() {
    let legacy = serde_json::json!({
        "repo_path": "legacy-repository",
        "root_mtime_secs": 0,
        "families": {
            "pkg": {
                "mtime_secs": 0,
                "versions": ["1.0.0"]
            }
        }
    });

    let index: RepoIndex = serde_json::from_value(legacy).unwrap();
    assert!(index.root_fingerprint.is_none());
    assert!(index.families["pkg"].source.is_none());
}

#[test]
fn test_filesystem_ignore_support() {
    use std::fs;
    use tempfile::tempdir;

    let temp_dir = tempdir().unwrap();
    let repo_path = temp_dir.path();

    // Create package structure
    let pkg_dir = repo_path.join("testpkg");
    fs::create_dir_all(pkg_dir.join("1.0.0")).unwrap();
    fs::create_dir_all(pkg_dir.join("2.0.0")).unwrap();

    // Write package files
    fs::write(
        pkg_dir.join("1.0.0/package.yaml"),
        "name: testpkg\nversion: '1.0.0'\n",
    )
    .unwrap();
    fs::write(
        pkg_dir.join("2.0.0/package.yaml"),
        "name: testpkg\nversion: '2.0.0'\n",
    )
    .unwrap();

    // Create .ignore marker for version 1.0.0
    fs::write(pkg_dir.join(".ignore1.0.0"), "").unwrap();

    let repo = FsRepo::new(repo_path).unwrap();
    let versions = repo.iter_versions("testpkg").unwrap();

    // Should only see version 2.0.0, 1.0.0 is ignored
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].to_string(), "2.0.0");
}

#[test]
fn test_cached_repo_basic() {
    use std::fs;
    use tempfile::tempdir;

    let temp_dir = tempdir().unwrap();
    let repo_path = temp_dir.path();

    // Create simple package
    let pkg_dir = repo_path.join("cached");
    fs::create_dir_all(pkg_dir.join("1.0.0")).unwrap();
    fs::write(
        pkg_dir.join("1.0.0/package.yaml"),
        "name: cached\nversion: '1.0.0'\n",
    )
    .unwrap();

    let cached_repo = FsRepoCached::new(repo_path, true).unwrap().unwrap();

    // First call - cache miss
    let families = cached_repo.iter_family_names().unwrap();
    assert_eq!(families.len(), 1);
    assert_eq!(families[0], "cached");

    // Second call - should use cache
    let families2 = cached_repo.iter_family_names().unwrap();
    assert_eq!(families2, families);

    // Test versions caching
    let versions = cached_repo.iter_versions("cached").unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].to_string(), "1.0.0");

    // Test package caching
    let version: Version = "1.0.0".parse().unwrap();
    let pkg = cached_repo.get_package("cached", &version).unwrap();
    assert!(pkg.is_some());
    assert_eq!(pkg.unwrap().name, "cached");
}

#[test]
fn test_cached_repo_invalidate() {
    use std::fs;
    use tempfile::tempdir;

    let temp_dir = tempdir().unwrap();
    let repo_path = temp_dir.path();

    // Create initial package
    let pkg_dir = repo_path.join("invalidtest");
    fs::create_dir_all(pkg_dir.join("1.0.0")).unwrap();
    fs::write(
        pkg_dir.join("1.0.0/package.yaml"),
        "name: invalidtest\nversion: '1.0.0'\n",
    )
    .unwrap();

    let cached_repo = FsRepoCached::new(repo_path, true).unwrap().unwrap();

    // Load into cache
    let families = cached_repo.iter_family_names().unwrap();
    assert_eq!(families.len(), 1);

    // Invalidate cache
    cached_repo.invalidate();

    // Should still work after invalidation
    let families2 = cached_repo.iter_family_names().unwrap();
    assert_eq!(families2.len(), 1);
}

#[test]
fn repository_publication_prepares_all_selected_variants_once_under_lock() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("prepared/1.0");
    let package = publication_package(serde_json::json!({
        "name": "prepared", "version": "1.0", "variants": [["python-3"], ["python-4"]],
        "hashed_variants": true
    }));
    let calls = std::cell::Cell::new(0);
    let payload = || {
        calls.set(calls.get() + 1);
        assert!(!root.join("package.py").exists());
        let mut contender = LockFile::new(temp.path(), "prepared", Some("1.0"));
        assert!(contender.acquire(0).is_err());
        for requirements in &package.variants {
            fs::create_dir_all(
                root.join(model::package::Variant::compute_subpath(requirements, true).unwrap()),
            )?;
        }
        Ok(())
    };
    publish_package(
        &package,
        &root,
        &[0, 1],
        Some(PublicationPayload::Existing(&payload)),
        Some(crate::serialise::FileFormat::Py),
    )
    .unwrap();
    assert_eq!(calls.get(), 1);
    let installed =
        Package::from_data(load_package_data(&root.join("package.py")).unwrap()).unwrap();
    assert_eq!(installed.variants, package.variants);
}

#[test]
fn repository_publication_format_conflict_precedes_payload_and_keeps_definition() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("format_conflict/1.0");
    let package = publication_package(serde_json::json!({
        "name": "format_conflict", "version": "1.0"
    }));
    publish_package(
        &package,
        &root,
        &[0],
        None,
        Some(crate::serialise::FileFormat::Py),
    )
    .unwrap();
    let definition = root.join("package.py");
    let previous = fs::read(&definition).unwrap();
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
        Some(crate::serialise::FileFormat::Yaml),
    )
    .is_err());
    assert!(!called.get());
    assert_eq!(fs::read(definition).unwrap(), previous);
    assert!(!root.join("package.yaml").exists());
}

#[test]
fn repository_publication_failed_preparation_does_not_advertise_new_package() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("failed_prepare/1.0");
    let package = publication_package(serde_json::json!({
        "name": "failed_prepare", "version": "1.0", "variants": [["python-3"]]
    }));
    let callback = || Err(RezError::Bind("preparation failed".into()));
    let error = publish_package(
        &package,
        &root,
        &[0],
        Some(PublicationPayload::Existing(&callback)),
        None,
    )
    .unwrap_err();
    assert!(error.to_string().contains("preparation failed"));
    assert!(!root.join("package.yaml").exists());
}

#[cfg(unix)]
#[test]
fn repository_publication_rejects_redirected_variant_ancestor_before_callback() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = temp.path().join("redirected_prepare/1.0");
    fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("python-3")).unwrap();
    let package = publication_package(serde_json::json!({
        "name": "redirected_prepare", "version": "1.0",
        "variants": [["python-3", "tool"]], "hashed_variants": false
    }));
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
        None,
    )
    .is_err());
    assert!(!called.get());
    assert!(!outside.path().join("tool").exists());
}

#[test]
fn borrowed_publication_preserves_payload_filesystem_stats() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().join("stats/1");
    let stage = tempfile::tempdir().unwrap();
    fs::create_dir_all(stage.path().join("payload")).unwrap();
    fs::write(stage.path().join("payload/file"), b"data").unwrap();
    let original = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(3600);
    fs::OpenOptions::new()
        .write(true)
        .open(stage.path().join("payload/file"))
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(original))
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            stage.path().join("payload"),
            fs::Permissions::from_mode(0o750),
        )
        .unwrap();
    }
    let package = publication_package(
        serde_json::json!({"name":"stats","version":"1","hashed_variants":false}),
    );
    publish_package(
        &package,
        &root,
        &[0],
        Some(PublicationPayload::Staged {
            root: stage.path(),
            replace: true,
        }),
        None,
    )
    .unwrap();
    assert_eq!(
        fs::metadata(root.join("payload/file"))
            .unwrap()
            .modified()
            .unwrap(),
        original
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(root.join("payload"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o750
        );
    }
    assert!(stage.path().join("payload/file").exists());
}

// APFS/HFS+ require UTF-8 file names (EILSEQ), so macOS cannot hold these payloads.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn publication_preserves_distinct_non_utf8_payload_names() {
    use std::os::unix::ffi::OsStringExt;
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().join("native_names/1");
    let stage = tempfile::tempdir().unwrap();
    let names = [0xfe, 0xff].map(|byte| std::ffi::OsString::from_vec(vec![b'f', byte]));
    for (index, name) in names.iter().enumerate() {
        fs::write(stage.path().join(name), [index as u8]).unwrap();
    }
    let package = publication_package(
        serde_json::json!({"name":"native_names","version":"1","hashed_variants":false}),
    );
    publish_package(
        &package,
        &root,
        &[0],
        Some(PublicationPayload::Staged {
            root: stage.path(),
            replace: true,
        }),
        None,
    )
    .unwrap();
    for (index, name) in names.iter().enumerate() {
        assert_eq!(fs::read(root.join(name)).unwrap(), [index as u8]);
    }
}
