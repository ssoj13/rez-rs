//! Opt-in native toolchain conformance tests. Require the named tool on PATH.
use build_system::builders::{
    cargo_build::CargoBuildSystem, go::GoBuildSystem, pip::PipBuildSystem, BuildContext,
    BuildProcess, BuildSystem,
};
use std::{fs, process::Command};

#[test]
#[ignore = "requires local pip, setuptools and wheel; installs only into owned temporary staging"]
fn pip_build_prepares_local_wheel_and_entrypoint_without_mutating_source_or_live_payload() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir_all(&source).unwrap();
    let shared = temp.path().join("shared");
    fs::create_dir(&shared).unwrap();
    fs::write(shared.join("version.txt"), "1.0.0\n").unwrap();
    fs::write(
        source.join("pyproject.toml"),
        "[build-system]\nrequires = [\"setuptools\", \"wheel\"]\nbuild-backend = \"setuptools.build_meta\"\n",
    )
    .unwrap();
    fs::write(
        source.join("setup.py"),
        "from pathlib import Path\nfrom setuptools import setup\nversion = (Path(__file__).parent.parent / 'shared' / 'version.txt').read_text().strip()\nsetup(name='rez-native-pip-fixture', version=version, py_modules=['fixture_app'], entry_points={'console_scripts': ['fixture-pip=fixture_app:main']})\n",
    )
    .unwrap();
    let module = "def main():\n    print('pip-fixture')\n";
    fs::write(source.join("fixture_app.py"), module).unwrap();
    let live = temp.path().join("live");
    fs::create_dir_all(&live).unwrap();
    fs::write(live.join("original"), "retained").unwrap();
    let mut ctx = BuildContext::new(source.clone(), temp.path().join("build"), live.clone());
    let python = std::env::var("REZ_TEST_PIP_PYTHON").unwrap_or_else(|_| "python".into());
    ctx.package_config = Some(serde_json::json!({
        "pip": {"source_inputs": ["../shared/version.txt"], "python": python}
    }));
    ctx.build_args = vec![
        "--no-index".into(),
        "--no-deps".into(),
        "--no-build-isolation".into(),
    ];
    ctx.env_vars.insert("PIP_NO_INDEX".into(), "1".into());
    ctx.env_vars
        .insert("PIP_DISABLE_PIP_VERSION_CHECK".into(), "1".into());
    let mut builder = PipBuildSystem::new(source.clone());
    if let Ok(launcher) = std::env::var("REZ_TEST_PIP_LAUNCHER") {
        builder.pip_path = launcher;
    }
    let built = builder.build(&ctx).unwrap();
    assert!(built.success, "{:?}", built.error);
    assert!(built.prepared_payload.is_none());
    assert!(fs::read_dir(ctx.build_path.join("wheels"))
        .unwrap()
        .any(|entry| entry
            .unwrap()
            .path()
            .extension()
            .is_some_and(|ext| ext == "whl")));
    ctx.install = true;
    let prepared = builder.build(&ctx).unwrap();
    assert!(prepared.success, "{:?}", prepared.error);
    let payload = prepared.prepared_payload.as_ref().unwrap();
    let site_packages = payload.path().join("site-packages");
    assert_eq!(
        fs::read_to_string(site_packages.join("fixture_app.py")).unwrap(),
        module
    );
    let script = payload
        .path()
        .join("bin")
        .join(format!("fixture-pip{}", std::env::consts::EXE_SUFFIX));
    let output = Command::new(script)
        .env("PYTHONPATH", &site_packages)
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "pip-fixture"
    );
    assert_eq!(
        fs::read_to_string(live.join("original")).unwrap(),
        "retained"
    );
    assert!(!live.join("site-packages").exists());
    assert_eq!(
        fs::read_to_string(source.join("fixture_app.py")).unwrap(),
        module
    );
    let source_children = fs::read_dir(&source)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(source_children.len(), 3, "{source_children:?}");
    assert_eq!(
        fs::read_to_string(shared.join("version.txt")).unwrap(),
        "1.0.0\n"
    );

    ctx.build_args.extend(["--editable".into(), ".".into()]);
    let editable = builder.build(&ctx).unwrap();
    assert!(editable.success, "{:?}", editable.error);
    let payload = editable.prepared_payload.as_ref().unwrap();
    let python = std::env::var("REZ_TEST_PIP_PYTHON").unwrap_or_else(|_| "python".into());
    let output = Command::new(python)
        .args(["-c", "import ast,site,sys;from pathlib import Path;root=Path(sys.argv[1]);finder=next(root.glob('__editable__*_finder.py'));text=finder.read_text();assert 'rez-pip-build-' not in text;node=next(node for node in ast.parse(text).body if isinstance(node,ast.AnnAssign) and isinstance(node.target,ast.Name) and node.target.id=='MAPPING');mapping=ast.literal_eval(node.value);assert Path(mapping['fixture_app']).with_suffix('.py').resolve()==Path(sys.argv[2]).resolve();site.addsitedir(sys.argv[1]);import fixture_app;assert Path(fixture_app.__file__).resolve()==Path(sys.argv[2]).resolve();fixture_app.main()"])
        .arg(payload.path().join("site-packages"))
        .arg(source.join("fixture_app.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "pip-fixture"
    );
    assert_eq!(
        fs::read_to_string(live.join("original")).unwrap(),
        "retained"
    );
}

#[test]
#[ignore = "requires an installed Rust toolchain; performs real native builds"]
fn cargo_build_prepares_binary_library_and_resources_without_touching_live_payload() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir_all(source.join("src")).unwrap();
    fs::create_dir_all(source.join("payload")).unwrap();
    fs::write(
        source.join("Cargo.toml"),
        "[package]\nname = \"rez_native_fixture\"\nversion = \"1.0.0\"\nedition = \"2021\"\n[lib]\ncrate-type = [\"cdylib\"]\n",
    )
    .unwrap();
    fs::write(
        source.join("src/lib.rs"),
        "#[no_mangle]\npub extern \"C\" fn fixture_value() -> i32 { 42 }\n",
    )
    .unwrap();
    fs::write(
        source.join("src/main.rs"),
        "fn main() { println!(\"native-fixture\"); }\n",
    )
    .unwrap();
    fs::write(source.join("payload/settings.toml"), "enabled = true\n").unwrap();
    let destination = temp.path().join("installed");
    fs::create_dir_all(destination.join("bin")).unwrap();
    fs::write(destination.join("bin/previous"), "original").unwrap();
    let mut ctx = BuildContext::new(
        source.clone(),
        temp.path().join("build"),
        destination.clone(),
    );
    ctx.install = true;
    ctx.build_args = vec!["--offline".into()];
    ctx.env_vars.insert("CARGO_BUILD_JOBS".into(), "1".into());
    ctx.env_vars
        .insert("CARGO_NET_OFFLINE".into(), "true".into());
    ctx.package_config = Some(serde_json::json!({"cargo": {
        "locked": false,
        "artifacts": [{
            "from": "source",
            "source": "payload/settings.toml",
            "destination": "share/settings.toml"
        }]
    }}));
    let builder = CargoBuildSystem::new(source.clone());
    let result = builder.build(&ctx).unwrap();
    assert!(result.success, "{:?}", result.error);
    assert_eq!(result.install_path.as_ref(), Some(&destination));
    let payload = result.prepared_payload.as_ref().unwrap();
    let binary = payload.path().join("bin").join(format!(
        "rez_native_fixture{}",
        std::env::consts::EXE_SUFFIX
    ));
    let output = Command::new(binary).output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "native-fixture"
    );
    let library = format!(
        "{}rez_native_fixture{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    assert!(payload.path().join("lib").join(library).is_file());
    assert_eq!(
        fs::read_to_string(payload.path().join("share/settings.toml")).unwrap(),
        "enabled = true\n"
    );
    assert_eq!(
        fs::read_to_string(destination.join("bin/previous")).unwrap(),
        "original"
    );
    assert!(!destination.join("share").exists());
    fs::write(source.join("src/main.rs"), "this is not Rust").unwrap();
    let failed = builder.build(&ctx).unwrap();
    assert!(!failed.success);
    assert!(failed.prepared_payload.is_none());
    assert_eq!(
        fs::read_to_string(destination.join("bin/previous")).unwrap(),
        "original"
    );
    assert!(!destination.join("share").exists());

    // The process owns the transaction boundary and publishes validated metadata.
    fs::write(
        source.join("src/main.rs"),
        "fn main() { println!(\"native-fixture\"); }\n",
    )
    .unwrap();
    let package = model::package::Package::from_data(std::collections::HashMap::from([
        ("name".into(), serde_json::json!("native_cargo")),
        ("version".into(), serde_json::json!("1.0.0")),
        ("config".into(), ctx.package_config.clone().unwrap()),
    ]))
    .unwrap();
    let config = model::config::RezConfig::default();
    let mut process = BuildProcess::new(&source, None, Some("cargo"), Some(&config)).unwrap();
    process.set_package(package, None).unwrap();
    process.build_args = vec!["--offline".into(), "-j".into(), "1".into()];
    let repository = temp.path().join("repository");
    let results = process
        .build(&repository, true, true, None, true, false)
        .unwrap();
    assert!(results.iter().all(|result| result.success));
    assert!(results
        .iter()
        .all(|result| result.prepared_payload.is_none()));
    let published = repository.join("native_cargo/1.0.0");
    let executable = published.join("bin").join(format!(
        "rez_native_fixture{}",
        std::env::consts::EXE_SUFFIX
    ));
    let output = Command::new(&executable).output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "native-fixture"
    );
    let metadata =
        model::serialise::find_package_definition_file(&published, &["py", "yaml", "toml", "yml"])
            .unwrap()
            .unwrap();
    let old_metadata = fs::read(&metadata).unwrap();
    let old_binary = fs::read(&executable).unwrap();
    fs::write(source.join("src/main.rs"), "this is not Rust").unwrap();
    let failed = process
        .build(&repository, false, true, None, true, false)
        .unwrap();
    assert!(failed.iter().any(|result| !result.success));
    assert_eq!(fs::read(metadata).unwrap(), old_metadata);
    assert_eq!(fs::read(executable).unwrap(), old_binary);
}

#[test]
#[ignore = "requires an installed Go toolchain; performs real native builds"]
fn go_build_installs_multiple_targets_payload_and_removes_stale_staging() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let module = source.join("src");
    for name in ["alpha", "beta"] {
        let directory = module.join("cmd").join(name);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("main.go"),
            format!("package main\nimport \"fmt\"\nfunc main() {{ fmt.Println(\"{name}\") }}\n"),
        )
        .unwrap();
    }
    fs::create_dir_all(source.join("payload")).unwrap();
    fs::write(source.join("payload/AMI.py"), "payload").unwrap();
    fs::write(
        module.join("go.mod"),
        "module example.com/rez-rs-fixture\n\ngo 1.20\n",
    )
    .unwrap();
    let scratch = temp.path().join("scratch");
    let destination = temp.path().join("installed");
    let mut ctx = BuildContext::new(source.clone(), scratch.clone(), destination.clone());
    ctx.install = true;
    ctx.build_args = vec!["-p".into(), "1".into()];
    ctx.env_vars.insert("GOPROXY".into(), "off".into());
    ctx.env_vars.insert("GOTOOLCHAIN".into(), "local".into());
    ctx.package_config = Some(serde_json::json!({"go": {
        "manifest_path": "src/go.mod",
        "artifacts": [{"from": "source", "source": "payload/AMI.py", "destination": "bin/AMI"}]
    }}));
    let builder = GoBuildSystem::new(source);
    let result = builder.build(&ctx).unwrap();
    assert!(result.success, "{:?}", result.error);
    assert_eq!(result.install_path.as_ref(), Some(&destination));
    assert!(
        !destination.exists(),
        "adapter must not publish live payload"
    );
    let payload = result.prepared_payload.as_ref().unwrap();
    assert_eq!(
        fs::read_to_string(payload.path().join("bin/AMI")).unwrap(),
        "payload"
    );
    for name in ["alpha", "beta"] {
        let binary = payload
            .path()
            .join("bin")
            .join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        let output = Command::new(binary).output().unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), name);
    }
    let stale = scratch.join("go-bin/stale");
    fs::write(&stale, "obsolete").unwrap();
    let cache_marker = scratch.join("go-cache/retained");
    fs::write(&cache_marker, "retained").unwrap();
    ctx.install = false;
    let result = builder.build(&ctx).unwrap();
    assert!(result.success, "{:?}", result.error);
    assert!(!stale.exists());
    assert!(result.prepared_payload.is_none());
    assert!(!destination.exists());
    assert_eq!(fs::read_to_string(cache_marker).unwrap(), "retained");
}

#[test]
#[ignore = "requires local CMake and Ninja; installs only into owned staging"]
fn cmake_build_preserves_runtime_prefix_and_stages_installation() {
    use build_system::builders::cmake::CMakeBuildSystem;
    let owned = tempfile::tempdir().unwrap();
    let source = owned.path().join("source");
    let live = owned.path().join("live");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&live).unwrap();
    fs::write(live.join("original"), "retained").unwrap();
    let recipe = "cmake_minimum_required(VERSION 3.15)\nproject(rez_stage NONE)\nfile(WRITE \"${CMAKE_BINARY_DIR}/prefix.txt\" \"${CMAKE_INSTALL_PREFIX}\")\ninstall(FILES \"${CMAKE_BINARY_DIR}/prefix.txt\" DESTINATION share)\n";
    fs::write(source.join("CMakeLists.txt"), recipe).unwrap();
    let mut ctx = BuildContext::new(source.clone(), owned.path().join("build"), live.clone());
    ctx.build_args = vec!["-G".into(), "Ninja".into()];
    ctx.build_threads = 1;
    ctx.install = true;
    let builder = CMakeBuildSystem::new(source.clone());
    let result = builder.build(&ctx).unwrap();
    assert!(result.success, "{:?}", result.error);
    let payload = result.prepared_payload.as_ref().unwrap();
    let expected = if cfg!(windows) {
        live.to_string_lossy().replace('\\', "/")
    } else {
        live.to_string_lossy().into_owned()
    };
    assert_eq!(
        fs::read_to_string(payload.path().join("share/prefix.txt")).unwrap(),
        expected
    );
    assert_eq!(
        fs::read_to_string(live.join("original")).unwrap(),
        "retained"
    );
    assert!(!live.join("share").exists());
    assert_eq!(
        fs::read_to_string(source.join("CMakeLists.txt")).unwrap(),
        recipe
    );

    // An absolute install rule must fail before writing the foreign destination.
    let foreign = owned.path().join("foreign");
    let foreign_cmake = if cfg!(windows) {
        foreign.to_string_lossy().replace('\\', "/")
    } else {
        foreign.to_string_lossy().into_owned()
    };
    fs::write(
        source.join("CMakeLists.txt"),
        format!("cmake_minimum_required(VERSION 3.15)\nproject(rez_stage NONE)\nfile(WRITE \"${{CMAKE_BINARY_DIR}}/prefix.txt\" \"payload\")\ninstall(FILES \"${{CMAKE_BINARY_DIR}}/prefix.txt\" DESTINATION \"{foreign_cmake}\")\n"),
    ).unwrap();
    let failed = builder.build(&ctx).unwrap();
    assert!(!failed.success, "{failed:?}");
    assert!(failed.prepared_payload.is_none());
    assert!(!foreign.exists());
    assert_eq!(
        fs::read_to_string(live.join("original")).unwrap(),
        "retained"
    );
    // A subsequent failed build must not invalidate the prior prepared payload.
    assert_eq!(
        fs::read_to_string(payload.path().join("share/prefix.txt")).unwrap(),
        expected
    );
}
