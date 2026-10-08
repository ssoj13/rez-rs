use model::config::RezConfig;
use model::package::commands::normalize;
use model::serialise::{validate_package_data, validate_test_entry};
use serde_json::Value;
use std::collections::HashMap;

#[test]
fn canonical_package_boundaries_and_rex_consumer() {
    use model::serialise::{FileFormat, PackageDataCache};
    let directory = tempfile::tempdir().unwrap();
    let raw = HashMap::from([
        ("name".into(), serde_json::json!("legacy")),
        ("version".into(), serde_json::json!("1")),
        (
            "commands".into(),
            serde_json::json!(["export REZ_RS_LEGACY=B"]),
        ),
    ]);
    assert!(model::package::Package::from_data(raw.clone()).is_err());
    assert!(model::serialise::validate_package_data(&raw).is_err());
    for format in [FileFormat::Yaml, FileFormat::Toml, FileFormat::Py] {
        let path = directory.path().join(format!("raw.{}", format.extension()));
        model::serialise::dump_to_file(&raw, &path, format).unwrap();
        assert!(
            model::serialise::load_from_file(&path, format, PackageDataCache::Disabled).is_err()
        );
        let rejected = directory
            .path()
            .join(format!("rejected.{}", format.extension()));
        assert!(model::serialise::dump_package_data(&raw, &rejected, format, None).is_err());
        assert!(!rejected.exists());
    }
    let config = RezConfig {
        disable_rez_1_compatibility: false,
        warn_none: true,
        ..RezConfig::default()
    };
    let mut data = raw;
    normalize(&mut data, &config).unwrap();
    for format in [FileFormat::Yaml, FileFormat::Toml, FileFormat::Py] {
        let path = directory
            .path()
            .join(format!("normalized.{}", format.extension()));
        model::serialise::dump_package_data(&data, &path, format, None).unwrap();
        let loaded =
            model::serialise::load_from_file(&path, format, PackageDataCache::Disabled).unwrap();
        let package = model::package::Package::from_data(loaded).unwrap();
        assert!(package
            .commands
            .unwrap()
            .contains("setenv('REZ_RS_LEGACY', 'B')"));
    }
    let package_path = directory.path().join("legacy").join("1");
    std::fs::create_dir_all(&package_path).unwrap();
    model::serialise::dump_package_data(
        &data,
        &package_path.join("package.yaml"),
        FileFormat::Yaml,
        None,
    )
    .unwrap();
    let provider = repository::FilesystemPackageProvider::from_path(directory.path()).unwrap();
    let context = resolve::context::ResolvedContext::resolve(
        vec![version::Requirement::new("legacy").unwrap()],
        &provider,
        resolve::context::ResolveOptions {
            package_paths: Some(vec![directory.path().to_path_buf()]),
            add_implicit: false,
            caching: false,
            ..resolve::context::ResolveOptions::default()
        },
    )
    .unwrap();
    let environment = context.get_environ(Some(HashMap::new())).unwrap();
    assert_eq!(
        environment.get("REZ_RS_LEGACY").map(String::as_str),
        Some("B")
    );
}

#[test]
fn test_validate_rez_test_schema_shapes_and_extensions() {
    let empty_name_error = validate_test_entry("", &serde_json::json!("echo"))
        .unwrap_err()
        .to_string();
    assert!(empty_name_error.contains("test names must not be empty"));

    let tests = serde_json::json!({
        "shell": "python -m pytest",
        "argv": ["python", "-m", "pytest"],
        "object": {
            "command": ["python", "-m", "pytest"],
            "requires": ["python-3.11+", "!old_api"],
            "run_on": ["pre_release", "custom_ci"],
            "on_variants": {
                "type": "requires",
                "value": ["maya-2024+"]
            },
            "vendor_extension": {"metadata": [1, true, null]}
        },
        "ephemeral": {
            "command": "echo ephemeral",
            "requires": [".temporary_pkg-1.0"]
        }
    });
    let mut data = HashMap::from([
        ("name".to_owned(), Value::String("foo".to_owned())),
        ("tests".to_owned(), tests),
    ]);
    assert!(validate_package_data(&data).is_ok());

    let tests = data.remove("tests").unwrap();
    let specs = resolve::package::test::parse_tests_from_data(&tests).unwrap();
    assert_eq!(specs.len(), 4);
    assert!(matches!(
        specs[0].command,
        resolve::package::test::TestCommand::Args(_)
    ));
    assert_eq!(specs[2].run_on[1].as_str(), "custom_ci");
}
