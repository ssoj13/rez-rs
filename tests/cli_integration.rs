// CLI integration tests: run rez binary and verify output.
//
// Tests use `std::process::Command` to invoke the compiled binary.
// Fixture packages at tests/fixtures/packages/ provide a known repo for
// search, view, and complete commands.

use std::path::PathBuf;
use std::process::Command;

/// Use the compiled CLI, or the explicitly selected installed acceptance binary.
fn rez_cmd() -> Command {
    Command::new(
        std::env::var_os("REZ_RS_TEST_BIN").unwrap_or_else(|| env!("CARGO_BIN_EXE_rez").into()),
    )
}

/// Absolute path to fixture packages directory.
fn fixtures_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/packages")
}

/// Fixture path as a string for --paths argument.
fn fixtures_str() -> String {
    fixtures_path().to_string_lossy().to_string()
}

#[test]
fn test_cli_env_preserves_child_exit_status() {
    let home = tempfile::tempdir().unwrap();
    let shells: &[&str] = if cfg!(windows) {
        &["cmd", "powershell"]
    } else {
        &["bash"]
    };
    let codes: &[i32] = if cfg!(windows) {
        &[0, 7, 1025]
    } else {
        &[0, 7]
    };
    for shell in shells {
        for code in codes {
            let mut command = rez_cmd();
            command.args(["env", "--no-cache", "--quiet", "--shell", shell, "--"]);
            #[cfg(windows)]
            {
                let comspec = std::env::var("COMSPEC").unwrap();
                assert!(std::path::Path::new(&comspec).is_absolute());
                assert!(std::path::Path::new(&comspec).is_file());
                command
                    .arg(comspec)
                    .args(["/D", "/C", "exit"])
                    .arg(code.to_string());
            }
            #[cfg(not(windows))]
            command
                .arg(env!("CARGO_BIN_EXE_rez"))
                .args(["python", "-c"])
                .arg(format!("import sys; sys.exit({code})"));
            let output = command
                .env("HOME", home.path())
                .env("USERPROFILE", home.path())
                .env("REZ_DISABLE_HOME_CONFIG", "1")
                .env_remove("REZ_CONFIG_FILE")
                .env("REZ_IMPLICIT_PACKAGES_JSON", "[]")
                .env("REZ_PACKAGES_PATH", "")
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(*code),
                "{shell}, {code}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(!String::from_utf8_lossy(&output.stderr).contains("rez: error:"));
        }
    }
}

#[test]
fn test_cli_forward_preserves_child_exit_status_and_streams() {
    let home = tempfile::tempdir().unwrap();
    let context_path = home.path().join("empty.rxt");
    let mut context = resolve::context::ResolvedContext::empty();
    context.status = foundation::constants::ResolverStatus::Solved;
    context.save(&context_path).unwrap();
    let forwarding_path = home.path().join("forward.yaml");
    std::fs::write(
        &forwarding_path,
        serde_yaml::to_string(&serde_json::json!({
            "module": "suite",
            "func_name": "_FWD__invoke_suite_tool_alias",
            "context_file": context_path,
            "tool_name": env!("CARGO_BIN_EXE_rez")
        }))
        .unwrap(),
    )
    .unwrap();
    let codes: &[i32] = if cfg!(windows) {
        &[0, 7, 1025]
    } else {
        &[0, 7]
    };
    for code in codes {
        let output = rez_cmd()
            .arg("forward").arg(&forwarding_path)
            .args(["python", "-c"])
            .arg(format!("import sys; sys.stdout.write('child stdout'); sys.stderr.write('child stderr'); sys.stdout.flush(); sys.stderr.flush(); sys.exit({code})"))
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("REZ_DISABLE_HOME_CONFIG", "1")
            .env_remove("REZ_CONFIG_FILE")
            .env("REZ_IMPLICIT_PACKAGES_JSON", "[]")
            .env("REZ_PACKAGES_PATH", "")
            .output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(*code),
            "{code}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "child stdout"
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stderr).trim(),
            "child stderr"
        );
    }
}

#[test]
fn test_cli_python_preserves_child_exit_status() {
    let home = tempfile::tempdir().unwrap();
    let codes: &[i32] = if cfg!(windows) {
        &[0, 7, 1025]
    } else {
        &[0, 7]
    };
    for code in codes {
        let output = rez_cmd()
            .args(["python", "-c"])
            .arg(format!("import sys; sys.exit({code})"))
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("REZ_DISABLE_HOME_CONFIG", "1")
            .env_remove("REZ_CONFIG_FILE")
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(*code),
            "{code}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("rez: error:"));
    }
}

#[test]
fn test_cli_passthrough_reports_rez_errors_as_failure() {
    let home = tempfile::tempdir().unwrap();
    let missing = home.path().join("missing.yaml");
    for args in [
        vec![
            "env".to_string(),
            "--no-cache".into(),
            "missing_exit_probe_package".into(),
        ],
        vec!["forward".into(), missing.to_string_lossy().into_owned()],
    ] {
        let output = rez_cmd()
            .args(args)
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("REZ_DISABLE_HOME_CONFIG", "1")
            .env_remove("REZ_CONFIG_FILE")
            .env("REZ_IMPLICIT_PACKAGES_JSON", "[]")
            .env("REZ_PACKAGES_PATH", "")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("rez: error:"));
    }
}

#[test]
fn test_cli_bind_platform_with_global_verbosity() {
    for flag in [None, Some("-v"), Some("-vv")] {
        for before in [true, false] {
            let repository = tempfile::tempdir().unwrap();
            let mut command = rez_cmd();
            if before {
                command.args(flag);
            }
            command
                .args(["bind", "platform", "--install-path"])
                .arg(repository.path())
                .arg("--no-deps");
            if !before {
                command.args(flag);
            }
            let output = command
                .env("REZ_DISABLE_HOME_CONFIG", "1")
                .env_remove("REZ_CONFIG_FILE")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "flag={flag:?}, before={before}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let platform = model::platform::SYSTEM.platform.name();
            assert!(
                repository
                    .path()
                    .join("platform")
                    .join(platform)
                    .join("package.py")
                    .is_file(),
                "platform package was not published: {}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
    }
}

#[test]
fn test_cli_env_preserves_command_argument_boundaries() {
    let shells: &[&str] = if cfg!(windows) {
        &["cmd", "powershell"]
    } else {
        &["bash"]
    };
    for shell in shells {
        let output = rez_cmd()
            .args(["env", "--shell", shell, "--"])
            .arg(env!("CARGO_BIN_EXE_rez"))
            .args([
                "python",
                "-c",
                "import sys,json; print(json.dumps(sys.argv[1:]))",
                "space value",
                "semi;colon",
                "",
            ])
            .env("REZ_DISABLE_HOME_CONFIG", "1")
            .env_remove("REZ_CONFIG_FILE")
            .env("REZ_IMPLICIT_PACKAGES_JSON", "[]")
            .env("REZ_PACKAGES_PATH", "")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{shell}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let argv: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(argv, ["space value", "semi;colon", ""], "{shell}");
    }
}

#[test]
fn test_cli_env_preserves_parent_by_default_and_honors_explicit_clear() {
    let shell = if cfg!(windows) { "cmd" } else { "bash" };
    for (options, expected) in [
        (vec![], "parent-value"),
        (vec!["--clear"], "absent"),
        (vec!["--inherited=false"], "absent"),
    ] {
        let output = rez_cmd()
            .args(["env", "--shell", shell])
            .args(options)
            .arg("--")
            .arg(env!("CARGO_BIN_EXE_rez"))
            .args([
                "python",
                "-c",
                "import os; print(os.environ.get('REZ_AUDIT_PARENT_SENTINEL', 'absent'))",
            ])
            .env("REZ_AUDIT_PARENT_SENTINEL", "parent-value")
            .env("REZ_DISABLE_HOME_CONFIG", "1")
            .env_remove("REZ_CONFIG_FILE")
            .env("REZ_CLEAN_SHELL_ENVIRONMENT", "false")
            .env("REZ_IMPLICIT_PACKAGES_JSON", "[]")
            .env("REZ_PACKAGES_PATH", "")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), expected);
    }
}

#[test]
fn test_cli_batch_build_metadata_is_json_and_checks_installed_readiness() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("target source");
    let dependency = temp.path().join("dependency source");
    for (directory, data) in [
        (
            &target,
            "name: batch_target\nversion: '1'\nbuild_requires: [batch_dep-1]\n",
        ),
        (&dependency, "name: batch_dep\nversion: '1'\n"),
    ] {
        std::fs::create_dir(directory).unwrap();
        std::fs::write(directory.join("package.yaml"), data).unwrap();
    }
    for installed_only in [false, true] {
        let mut command = rez_cmd();
        command
            .args(["view", "--building", "--format", "json", "--source-path"])
            .arg(&target)
            .arg("--source-path")
            .arg(&dependency);
        if installed_only {
            command.arg("--installed-only");
        }
        let output = command
            .env("REZ_DISABLE_HOME_CONFIG", "1")
            .env_remove("REZ_CONFIG_FILE")
            .env("REZ_IMPLICIT_PACKAGES_JSON", "[]")
            .env("REZ_PACKAGES_PATH", "")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(metadata["producer"], "rez-rs");
        assert_eq!(
            metadata["mode"],
            if installed_only {
                "installed"
            } else {
                "prospective"
            }
        );
        let resolve = &metadata["packages"][0]["variants"][0]["resolve"];
        assert_eq!(resolve["success"], !installed_only);
        if !installed_only {
            assert_eq!(resolve["selected"][0]["origin"], "source");
            assert_eq!(resolve["prerequisites"].as_array().unwrap().len(), 1);
        }
        for directory in [&target, &dependency] {
            assert!(!directory.join("build.rxt").exists());
            assert!(!directory.join("build").exists());
        }
    }
}

#[test]
fn test_cli_batch_build_metadata_rejects_malformed_implicit_requirements() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("package.yaml"),
        "name: implicit_probe\nversion: '1'\n",
    )
    .unwrap();
    for installed_only in [false, true] {
        let mut command = rez_cmd();
        command
            .args(["view", "--building", "--format", "json", "--source-path"])
            .arg(temp.path());
        if installed_only {
            command.arg("--installed-only");
        }
        let output = command
            .env("REZ_DISABLE_HOME_CONFIG", "1")
            .env_remove("REZ_CONFIG_FILE")
            .env("REZ_IMPLICIT_PACKAGES_JSON", "[\"bad ???\"]")
            .env("REZ_PACKAGES_PATH", "")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let resolve = &metadata["packages"][0]["variants"][0]["resolve"];
        assert_eq!(resolve["success"], false);
        assert!(!resolve["error"].as_str().unwrap().is_empty());
    }
}

#[test]
fn test_cli_build_prefix_installs_from_source_instead_of_loading_destination() {
    for flag in ["-p", "--prefix", "--install-path"] {
        for explicit_source in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("source with spaces");
            let destination = temp.path().join("repository with spaces");
            std::fs::create_dir(&source).unwrap();
            std::fs::write(
                source.join("package.yaml"),
                "name: prefix_probe\nversion: '1.0'\nbuild_system: noop\n",
            )
            .unwrap();
            let mut command = rez_cmd();
            command.args(["build", "-i", flag]).arg(&destination);
            if explicit_source {
                command.arg("--source-path").arg(&source);
                command.current_dir(temp.path());
            } else {
                command.current_dir(&source);
            }
            let output = command
                .env("REZ_DISABLE_HOME_CONFIG", "1")
                .env_remove("REZ_CONFIG_FILE")
                .env("REZ_IMPLICIT_PACKAGES_JSON", "[]")
                .env("REZ_PACKAGES_PATH", "")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "flag={flag}, explicit_source={explicit_source}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let published = destination.join("prefix_probe/1.0/package.yaml");
            assert!(published.is_file());
            let (data, _) =
                model::serialise::load_package_data(published.parent().unwrap()).unwrap();
            let package = model::package::Package::from_data(data).unwrap();
            assert_eq!(package.name, "prefix_probe");
            assert_eq!(package.version.to_string(), "1.0");
            assert!(source.join("package.yaml").is_file());
        }
    }
}

#[test]
fn test_cli_context_interpret_exports_executable_shell_code() {
    let temp = tempfile::tempdir().unwrap();
    let provider = repository::provider::MemoryPackageProvider::new();
    let mut context = resolve::context::ResolvedContext::resolve(
        vec![],
        &provider,
        resolve::context::ResolveOptions {
            caching: false,
            add_implicit: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(context.success());
    context.append_sys_path = false;
    let rxt = temp.path().join("export.rxt");
    context.save(&rxt).unwrap();
    let shell = if cfg!(windows) { "cmd" } else { "sh" };
    let output = rez_cmd()
        .args([
            "context",
            rxt.to_str().unwrap(),
            "--interpret",
            "--format",
            shell,
            "--no-env",
        ])
        .env("REZ_DISABLE_HOME_CONFIG", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = String::from_utf8(output.stdout).unwrap();
    assert!(
        code.contains("REZ_USED_VERSION"),
        "empty or incomplete shell export: {code:?}"
    );
    let script = temp.path().join(if cfg!(windows) {
        "export.bat"
    } else {
        "export.sh"
    });
    let probe = if cfg!(windows) {
        "\r\necho EXPORTED=%REZ_USED_VERSION%\r\n"
    } else {
        "\nprintf 'EXPORTED=%s\\n' \"$REZ_USED_VERSION\"\n"
    };
    std::fs::write(&script, format!("{code}{probe}")).unwrap();
    let mut child = if cfg!(windows) {
        let mut child = Command::new("cmd.exe");
        child.args(["/D", "/C"]).arg(&script);
        child
    } else {
        let mut child = Command::new("sh");
        child.arg(&script);
        child
    };
    let result = child.env_remove("REZ_USED_VERSION").output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains(&format!("EXPORTED={}", context.rez_version)),
        "{stdout}"
    );
}

#[test]
fn test_cli_interpret_exports_executable_shell_code() {
    let temp = tempfile::tempdir().unwrap();
    let rex = temp.path().join("commands.py");
    std::fs::write(&rex, "setenv(\"REZ_RS_INTERPRET_PROBE\", \"rendered\")\n").unwrap();
    let shell = if cfg!(windows) { "cmd" } else { "sh" };
    let output = rez_cmd()
        .args([
            "interpret",
            rex.to_str().unwrap(),
            "--format",
            shell,
            "--no-env",
        ])
        .env("REZ_DISABLE_HOME_CONFIG", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = String::from_utf8(output.stdout).unwrap();
    assert!(
        code.contains("REZ_RS_INTERPRET_PROBE"),
        "empty shell export: {code:?}"
    );
    let script = temp.path().join(if cfg!(windows) {
        "interpret.bat"
    } else {
        "interpret.sh"
    });
    let probe = if cfg!(windows) {
        "\r\necho EXPORTED=%REZ_RS_INTERPRET_PROBE%\r\n"
    } else {
        "\nprintf 'EXPORTED=%s\\n' \"$REZ_RS_INTERPRET_PROBE\"\n"
    };
    std::fs::write(&script, format!("{code}{probe}")).unwrap();
    let mut child = if cfg!(windows) {
        let mut child = Command::new("cmd.exe");
        child.args(["/D", "/C"]).arg(&script);
        child
    } else {
        let mut child = Command::new("sh");
        child.arg(&script);
        child
    };
    let result = child.env_remove("REZ_RS_INTERPRET_PROBE").output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("EXPORTED=rendered"));
}

#[test]
fn test_cli_clean_shell_uses_baseline_and_explicit_allowlist() {
    let temp = tempfile::tempdir().unwrap();
    let config_file = temp.path().join("rezconfig.toml");
    let config = model::config::RezConfig {
        implicit_packages: vec![],
        parent_variables: vec!["REZ_RS_ALLOWED_PROBE".into()],
        standard_system_paths: vec!["rez-rs-extra-system-path".into()],
        rez_tools_visibility: foundation::constants::RezToolsVisibility::Never,
        ..model::config::RezConfig::default()
    };
    std::fs::write(&config_file, toml::to_string(&config).unwrap()).unwrap();
    for (enabled, inherited, append) in [
        (false, None, false),
        (false, Some(true), false),
        (false, Some(false), false),
        (true, None, false),
        (true, Some(true), false),
        (true, Some(false), false),
        (true, None, true),
        (true, Some(true), true),
        (true, Some(false), true),
    ] {
        let mut command = rez_cmd();
        command.arg("env");
        match inherited {
            Some(true) => {
                command.arg("--inherited");
            }
            Some(false) => {
                command.arg("--inherited=false");
            }
            None => {}
        }
        let explicit_empty = inherited == Some(false);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("REZ_") {
                command.env_remove(key);
            }
        }
        let (shell, script) = if cfg!(windows) {
            (
                "cmd",
                "set REZ_RS_ALLOWED_PROBE & set REZ_RS_POLLUTION_PROBE & set PATH & set PROCESSOR_ARCHITECTURE & set SystemRoot & exit /b 0",
            )
        } else {
            (
                "sh",
                "printf 'ALLOWED=%s\\nPOLLUTION=%s\\nPATH=%s\\n' \"$REZ_RS_ALLOWED_PROBE\" \"$REZ_RS_POLLUTION_PROBE\" \"$PATH\"",
            )
        };
        command
            .args([
                "--ni",
                "--no-cache",
                "--quiet",
                "--shell",
                shell,
                "-c",
                script,
            ])
            .env("REZ_CONFIG_FILE", &config_file)
            .env("REZ_DISABLE_HOME_CONFIG", "1")
            .env(
                "REZ_CLEAN_SHELL_ENVIRONMENT",
                if enabled { "1" } else { "0" },
            )
            .env("REZ_APPEND_SYS_PATH", if append { "1" } else { "0" })
            .env("REZ_RS_ALLOWED_PROBE", "allowed-value")
            .env("REZ_RS_POLLUTION_PROBE", "polluted-value");
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "enabled={enabled}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            stdout.contains("allowed-value"),
            !explicit_empty,
            "{stdout}"
        );
        assert_eq!(
            stdout.contains("polluted-value"),
            !enabled && !explicit_empty,
            "{stdout}"
        );
        assert_eq!(
            stdout.contains("rez-rs-extra-system-path"),
            append,
            "{stdout}"
        );
        if enabled && !append && !explicit_empty {
            let expected = model::environment::system_paths(
                model::platform::Platform::current(),
                &std::env::vars().collect(),
            );
            for path in expected {
                assert!(
                    stdout.contains(&path),
                    "missing system path {path:?}: {stdout}"
                );
            }
            if cfg!(windows) {
                assert!(stdout.contains("PROCESSOR_ARCHITECTURE="), "{stdout}");
            }
        }
    }
}

#[test]
fn test_cli_build_and_release_use_configured_layout_and_scratch() {
    for categories in [true, false] {
        for shared in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("source");
            std::fs::create_dir_all(&source).unwrap();
            std::fs::write(
                source.join("package.yaml"),
                "name: layout_probe\nversion: '1.0'\npackage_type: dcc\nbuild_system: noop\n",
            )
            .unwrap();
            let repo = temp.path().join("repo");
            let local = temp.path().join("local");
            let scratch = temp.path().join("scratch");
            let config = model::config::RezConfig {
                packages_path: vec![
                    local.join("int").to_string_lossy().into_owned(),
                    repo.join("int").to_string_lossy().into_owned(),
                ],
                local_packages_path: local.join("int").to_string_lossy().into_owned(),
                release_packages_path: repo.join("int").to_string_lossy().into_owned(),
                release_bind_path: None,
                release_pip_path: None,
                release_build_path: None,
                build_directory: scratch.to_string_lossy().into_owned(),
                ..model::config::RezConfig::default()
            };
            let config_file = temp.path().join("rezconfig.toml");
            std::fs::write(&config_file, toml::to_string(&config).unwrap()).unwrap();
            for release in [false, true] {
                let mut command = rez_cmd();
                for (key, _) in std::env::vars_os() {
                    if key.to_string_lossy().starts_with("REZ_") {
                        command.env_remove(key);
                    }
                }
                command
                    .current_dir(&source)
                    .env("REZ_CONFIG_FILE", &config_file)
                    .env("REZ_DISABLE_HOME_CONFIG", "1")
                    .env("REZ_INSTALL_CATEGORIES", if categories { "1" } else { "0" })
                    .env("REZ_INSTALL_LOCATION", if shared { "1" } else { "0" });
                if release {
                    command.args([
                        "release",
                        "--buildsys",
                        "noop",
                        "--clean",
                        "--skip-repo-errors",
                        "--no-latest",
                        "--no-message",
                    ]);
                } else {
                    command.args(["build", "-i", "--clean", "--build-system", "noop"]);
                }
                let output = command.output().unwrap();
                assert!(
                    output.status.success(),
                    "release={release}, categories={categories}, shared={shared}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let base = if release || shared { &repo } else { &local };
                let base = if categories {
                    base.join("dcc")
                } else {
                    base.clone()
                };
                assert!(base.join("layout_probe/1.0").is_dir());
                let manager =
                    repository::PackageRepositoryManager::from_paths(std::slice::from_ref(&base))
                        .unwrap();
                let published = manager
                    .get_package("layout_probe", &version::Version::new("1.0").unwrap())
                    .unwrap()
                    .expect("a successful installation must be discoverable");
                let package = published.to_package().unwrap();
                assert_eq!(package.attributes["package_type"], serde_json::json!("dcc"));
                assert!(scratch.join("layout_probe/1.0").is_dir());
                assert!(source.join("package.yaml").is_file());
                assert!(!source.join("build").exists());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level binary flags
// ---------------------------------------------------------------------------

#[test]
fn test_cli_version_flag() {
    let out = rez_cmd()
        .arg("--version")
        .output()
        .expect("failed to run rez --version");
    assert!(out.status.success(), "--version should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("rez"),
        "--version output should contain 'rez', got: {stdout}"
    );
}

#[test]
fn test_cli_help_flag() {
    let out = rez_cmd()
        .arg("--help")
        .output()
        .expect("failed to run rez --help");
    assert!(out.status.success(), "--help should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Help must list some known subcommands
    assert!(stdout.contains("env"), "help should list 'env' command");
    assert!(
        stdout.contains("search"),
        "help should list 'search' command"
    );
    assert!(
        stdout.contains("python"),
        "help should list 'python' command"
    );
}

#[test]
fn test_cli_help_subcommand() {
    // Rez's `help` command owns package documentation. Use --help to inspect
    // its parser without opening the manual in the user's browser.
    let out = rez_cmd()
        .args(["help", "--help"])
        .output()
        .expect("failed to run rez help --help");
    assert!(out.status.success(), "package help parser should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("[PACKAGE]") && stdout.contains("--manual"),
        "help should describe package documentation, got: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// Subcommand --help flags (each subcommand should respond to --help)
// ---------------------------------------------------------------------------

#[test]
fn test_cli_subcommand_help_flags() {
    let subcommands = [
        "env",
        "build",
        "test",
        "config",
        "search",
        "view",
        "status",
        "bind",
        "suite",
        "bundle",
        "cp",
        "mv",
        "rm",
        "pkg-cache",
        "pkg-ignore",
        "context",
        "interpret",
        "complete",
        "python",
        "release",
        "depends",
        "diff",
        "plugins",
        "help-pkg",
        "pip",
        "yaml2py",
        "selftest",
        "benchmark",
        "memcache",
        "forward",
    ];
    for sub in subcommands {
        let out = rez_cmd()
            .args([sub, "--help"])
            .output()
            .unwrap_or_else(|e| panic!("failed to run rez {sub} --help: {e}"));
        assert!(
            out.status.success(),
            "rez {sub} --help should exit 0, stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("Usage") || stdout.contains("usage"),
            "rez {sub} --help should print usage info, got: {stdout}"
        );
    }
}

// ---------------------------------------------------------------------------
// rez python
// ---------------------------------------------------------------------------

#[test]
fn test_cli_python_version() {
    let out = rez_cmd()
        .args(["python", "--version"])
        .output()
        .expect("failed to run rez python --version");
    assert!(out.status.success(), "python --version should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("RustPython"),
        "python --version must mention RustPython, got: {stdout}"
    );
}

#[test]
fn test_cli_python_exec_arithmetic() {
    let out = rez_cmd()
        .args(["python", "-c", "print(2 + 3)"])
        .output()
        .expect("failed to run rez python -c");
    assert!(out.status.success(), "python -c should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim(), "5", "print(2+3) should produce '5'");
}

#[test]
fn test_cli_python_exec_string() {
    let out = rez_cmd()
        .args(["python", "-c", "print('hello rez')"])
        .output()
        .expect("failed to run rez python -c string");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim(), "hello rez");
}

#[test]
fn test_cli_python_exec_multiline() {
    // Verify multi-statement code separated by semicolons
    let out = rez_cmd()
        .args(["python", "-c", "x = 10; print(x * 2)"])
        .output()
        .expect("failed to run rez python -c multiline");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim(), "20");
}

#[test]
fn test_cli_python_import_sys() {
    let out = rez_cmd()
        .args(["python", "-c", "import sys; print(sys.platform)"])
        .output()
        .expect("failed to run rez python -c import");
    assert!(
        out.status.success(),
        "python import sys should exit 0, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.trim().is_empty(),
        "sys.platform should produce output"
    );
}

#[test]
fn test_cli_python_import_json() {
    let out = rez_cmd()
        .args(["python", "-c", "import json; print(json.dumps({'a': 1}))"])
        .output()
        .expect("failed to run rez python -c json");
    assert!(
        out.status.success(),
        "python import json should exit 0, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"a\"") && stdout.contains("1"),
        "json.dumps should produce valid JSON, got: {stdout}"
    );
}

#[test]
fn test_cli_python_syntax_error() {
    // Invalid Python should produce non-zero exit
    let out = rez_cmd()
        .args(["python", "-c", "def"])
        .output()
        .expect("failed to run rez python -c bad syntax");
    assert!(
        !out.status.success(),
        "python with syntax error should fail"
    );
}

// ---------------------------------------------------------------------------
// rez config
// ---------------------------------------------------------------------------

#[test]
fn test_cli_config_output_json() {
    let out = rez_cmd()
        .arg("config")
        .output()
        .expect("failed to run rez config");
    assert!(
        out.status.success(),
        "config should exit 0, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Config dumps as JSON
    assert!(
        stdout.contains("packages_path"),
        "config should contain 'packages_path'"
    );
    assert!(
        stdout.contains("local_packages_path"),
        "config should contain 'local_packages_path'"
    );
    assert!(
        stdout.contains("implicit_packages"),
        "config should contain 'implicit_packages'"
    );
    // Verify it parses as JSON
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(stdout.trim());
    assert!(parsed.is_ok(), "config output should be valid JSON");
}

// ---------------------------------------------------------------------------
// rez status
// ---------------------------------------------------------------------------

#[test]
fn test_cli_status_output() {
    let out = rez_cmd()
        .arg("status")
        .output()
        .expect("failed to run rez status");
    assert!(
        out.status.success(),
        "status should exit 0, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Match Rez's environment summary: it reports context and suites here.
    assert!(
        stdout.contains("Using rez-rs"),
        "status should identify the Rez implementation"
    );
    assert!(
        stdout.contains("No active context."),
        "status should report the missing active context"
    );
    assert!(
        stdout.contains("No visible suites."),
        "status should report the missing visible suites"
    );
}

// ---------------------------------------------------------------------------
// rez search (with fixture repo)
// ---------------------------------------------------------------------------

#[test]
fn test_cli_search_latest_short_flag() {
    let out = rez_cmd()
        .args(["search", "-l", "--paths", &fixtures_str(), "foo"])
        .output()
        .expect("failed to run rez search -l");
    assert!(
        out.status.success(),
        "search -l should select latest, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("foo-2.0.0"));
    assert!(!stdout.contains("foo-1.1.0"));
    assert!(!stdout.contains("foo-1.0.0"));
}

#[test]
fn test_cli_search_fixture_foo() {
    let out = rez_cmd()
        .args(["search", "--paths", &fixtures_str(), "foo"])
        .output()
        .expect("failed to run rez search foo");
    assert!(
        out.status.success(),
        "search foo should exit 0, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // All three foo versions should appear
    assert!(stdout.contains("foo-2.0.0"), "search should list foo-2.0.0");
    assert!(stdout.contains("foo-1.1.0"), "search should list foo-1.1.0");
    assert!(stdout.contains("foo-1.0.0"), "search should list foo-1.0.0");
}

#[test]
fn test_cli_search_fixture_bar() {
    let out = rez_cmd()
        .args(["search", "--paths", &fixtures_str(), "bar"])
        .output()
        .expect("failed to run rez search bar");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("bar-2.0.0"), "search should list bar-2.0.0");
    assert!(stdout.contains("bar-1.0.0"), "search should list bar-1.0.0");
}

#[test]
fn test_cli_search_fixture_not_found() {
    let out = rez_cmd()
        .args(["search", "--paths", &fixtures_str(), "nonexistent_xyz"])
        .output()
        .expect("failed to run rez search nonexistent");
    // Searching for a package that doesn't exist should fail
    assert!(!out.status.success(), "search for nonexistent should fail");
}

// ---------------------------------------------------------------------------
// rez view (with fixture repo)
// ---------------------------------------------------------------------------

#[test]
fn test_cli_view_fixture_foo_latest() {
    let out = rez_cmd()
        .args(["view", "--paths", &fixtures_str(), "foo"])
        .output()
        .expect("failed to run rez view foo");
    assert!(
        out.status.success(),
        "view foo should exit 0, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Latest foo is 2.0.0 with requires
    assert!(stdout.contains("foo"), "view should show package name");
    assert!(
        stdout.contains("2.0.0"),
        "view should show latest version 2.0.0"
    );
    assert!(stdout.contains("bar-2+"), "foo-2.0.0 requires bar-2+");
    assert!(stdout.contains("baz-1"), "foo-2.0.0 requires baz-1");
}

#[test]
fn test_cli_view_fixture_foo_all() {
    let out = rez_cmd()
        .args(["view", "--paths", &fixtures_str(), "--all", "foo"])
        .output()
        .expect("failed to run rez view --all foo");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // --all should show all three versions
    assert!(stdout.contains("2.0.0"), "view --all should include 2.0.0");
    assert!(stdout.contains("1.1.0"), "view --all should include 1.1.0");
    assert!(stdout.contains("1.0.0"), "view --all should include 1.0.0");
}

#[test]
fn test_cli_view_fixture_json_format() {
    let out = rez_cmd()
        .args(["view", "--paths", &fixtures_str(), "-f", "json", "foo"])
        .output()
        .expect("failed to run rez view -f json foo");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // JSON output should contain braces and key fields
    assert!(
        stdout.contains("{"),
        "JSON output should contain '{{' brace"
    );
    assert!(
        stdout.contains("\"name\""),
        "JSON output should have 'name' field"
    );
    assert!(
        stdout.contains("\"version\""),
        "JSON output should have 'version' field"
    );
}

#[test]
fn test_cli_view_fixture_bar() {
    let out = rez_cmd()
        .args(["view", "--paths", &fixtures_str(), "bar"])
        .output()
        .expect("failed to run rez view bar");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("bar"), "view should show bar");
    assert!(
        stdout.contains("2.0.0"),
        "view should show latest bar version"
    );
}

#[test]
fn test_cli_view_fixture_baz() {
    let out = rez_cmd()
        .args(["view", "--paths", &fixtures_str(), "baz"])
        .output()
        .expect("failed to run rez view baz");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("baz"), "view should show baz");
    assert!(stdout.contains("1.0.0"), "baz only has 1.0.0");
}

#[test]
fn test_cli_view_fixture_not_found() {
    let out = rez_cmd()
        .args(["view", "--paths", &fixtures_str(), "nonexistent_xyz"])
        .output()
        .expect("failed to run rez view nonexistent");
    assert!(
        !out.status.success(),
        "view nonexistent package should fail"
    );
}

// ---------------------------------------------------------------------------
// rez complete (with fixture repo)
// ---------------------------------------------------------------------------

#[test]
fn test_cli_complete_families() {
    let out = rez_cmd()
        .args(["complete", "--paths", &fixtures_str(), "--families"])
        .output()
        .expect("failed to run rez complete --families");
    assert!(
        out.status.success(),
        "complete --families should exit 0, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("foo"),
        "complete families should include foo"
    );
    assert!(
        stdout.contains("bar"),
        "complete families should include bar"
    );
    assert!(
        stdout.contains("baz"),
        "complete families should include baz"
    );
}

#[test]
fn test_cli_complete_prefix_f() {
    let out = rez_cmd()
        .args(["complete", "--paths", &fixtures_str(), "f"])
        .output()
        .expect("failed to run rez complete f");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Prefix "f" should match foo and its versions
    assert!(stdout.contains("foo"), "complete 'f' should match foo");
    // Should NOT contain bar or baz
    assert!(!stdout.contains("bar"), "complete 'f' should not match bar");
    assert!(!stdout.contains("baz"), "complete 'f' should not match baz");
}

#[test]
fn test_cli_complete_prefix_b() {
    let out = rez_cmd()
        .args(["complete", "--paths", &fixtures_str(), "b"])
        .output()
        .expect("failed to run rez complete b");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Prefix "b" should match bar and baz
    assert!(stdout.contains("bar"), "complete 'b' should match bar");
    assert!(stdout.contains("baz"), "complete 'b' should match baz");
    assert!(!stdout.contains("foo"), "complete 'b' should not match foo");
}

// ---------------------------------------------------------------------------
// Error paths
// ---------------------------------------------------------------------------

#[test]
fn test_cli_unknown_command_error() {
    let out = rez_cmd()
        .arg("nonexistent_cmd")
        .output()
        .expect("failed to run rez nonexistent_cmd");
    assert!(
        !out.status.success(),
        "unknown command should exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("error") || stderr.contains("unrecognized") || stderr.contains("invalid"),
        "stderr should mention error for unknown command, got: {stderr}"
    );
}

#[test]
fn test_cli_env_nonexistent_package() {
    let out = rez_cmd()
        .args(["env", "nonexistent_pkg_xyz"])
        .output()
        .expect("failed to run rez env nonexistent");
    assert!(
        !out.status.success(),
        "env with nonexistent package should fail"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.is_empty(), "env error should produce stderr");
}

#[test]
fn test_cli_view_missing_argument() {
    // view requires a PACKAGE argument
    let out = rez_cmd()
        .arg("view")
        .output()
        .expect("failed to run rez view (no arg)");
    assert!(
        !out.status.success(),
        "view without package arg should fail"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("error") || stderr.contains("required"),
        "stderr should indicate missing argument, got: {stderr}"
    );
}

#[test]
fn test_cli_interpret_missing_argument() {
    // interpret requires a FILE argument
    let out = rez_cmd()
        .arg("interpret")
        .output()
        .expect("failed to run rez interpret (no arg)");
    assert!(
        !out.status.success(),
        "interpret without file arg should fail"
    );
}

fn private_cli(
    root: &std::path::Path,
    config: &std::path::Path,
    executable: Option<&std::path::Path>,
) -> Command {
    let mut command = executable.map(Command::new).unwrap_or_else(rez_cmd);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("REZ_") {
            command.env_remove(name);
        }
    }
    command
        .env("HOME", root)
        .env("USERPROFILE", root)
        .env("REZ_DISABLE_HOME_CONFIG", "1")
        .env("REZ_CONFIG_FILE", config);
    command
}

#[test]
fn test_cli_bundle_custom_definition_survives_relocation() {
    let owned = tempfile::tempdir().unwrap();
    let root = owned.path();
    let repository = root.join("repository");
    let package = repository.join("cli_bundle/1");
    std::fs::create_dir_all(package.join(".rez/include")).unwrap();
    std::fs::write(
        package.join("manifest.yaml"),
        "name: cli_bundle\nversion: '1'\nrelocatable: true\ncommands: |\n  env.BUNDLE_ROOT = '{this.root}'\n",
    ).unwrap();
    std::fs::write(package.join("payload.txt"), "owned bundle payload").unwrap();
    std::fs::write(
        package.join(".rez/include/helper.py"),
        "VALUE = 'included'\n",
    )
    .unwrap();
    let config = root.join("config.py");
    std::fs::write(
        &config,
        "implicit_packages = []\nplugins = {'package_repository': {'filesystem': {'package_filenames': ['manifest']}}}\n",
    ).unwrap();
    let source = root.join("source.rxt");
    let output = private_cli(root, &config, None)
        .args([
            "env",
            "cli_bundle-1",
            "--ni",
            "--no-cache",
            "--no-pkg-cache",
            "--paths",
        ])
        .arg(&repository)
        .arg("--output")
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let source_data: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&source).unwrap()).unwrap();
    let yaml_source = root.join("source-yaml.rxt");
    std::fs::write(&yaml_source, serde_yaml::to_string(&source_data).unwrap()).unwrap();
    for (index, source) in [&source, &yaml_source].into_iter().enumerate() {
        let destination = root.join(format!("bundle-{index}"));
        let output = private_cli(root, &config, None)
            .arg("bundle")
            .arg(source)
            .arg(&destination)
            .arg("--no-lib-patch")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let moved = root.join(format!("relocated-{index}"));
        std::fs::rename(&destination, &moved).unwrap();
        let copied = moved.join("packages/cli_bundle/1");
        assert_eq!(
            std::fs::read_to_string(copied.join("payload.txt")).unwrap(),
            "owned bundle payload"
        );
        assert!(copied.join("manifest.yaml").is_file());
        assert_eq!(
            std::fs::read_to_string(copied.join(".rez/include/helper.py")).unwrap(),
            "VALUE = 'included'\n"
        );
        // Hide only our owned source repository: the relocated context must
        // execute using its bundled handle, not historical source paths.
        let unavailable = root.join("unavailable_repository");
        std::fs::rename(&repository, &unavailable).unwrap();
        let context = moved.join("context.rxt");
        // Shell built-ins only, so the result does not depend on host tools
        // such as `cat` on the context PATH.
        let command = if cfg!(windows) {
            "type \"%BUNDLE_ROOT%\\payload.txt\""
        } else {
            "printf '%s\\n' \"$(< \"$BUNDLE_ROOT/payload.txt\")\""
        };
        let output = private_cli(root, &config, None)
            .args(["env", "--input"])
            .arg(&context)
            .args([
                "--quiet",
                "--shell",
                if cfg!(windows) { "cmd" } else { "bash" },
                "--command",
                command,
            ])
            .output()
            .unwrap();
        std::fs::rename(&unavailable, &repository).unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("owned bundle payload"));
    }
}

#[test]
fn test_cli_cache_uses_repository_policy_and_exact_removal() {
    let owned = tempfile::tempdir().unwrap();
    let root = owned.path();
    let repository = root.join("repository");
    let package = repository.join("cli_cache/1");
    let cache = root.join("cache");
    let scratch = root.join("scratch");
    for path in [&package, &cache, &scratch] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::write(
        package.join("package.yaml"),
        "name: cli_cache\nversion: '1'\ncachable: true\n",
    )
    .unwrap();
    std::fs::write(package.join("payload.txt"), "cached payload").unwrap();
    let config = root.join("config.py");
    std::fs::write(&config, format!(
        "implicit_packages = []\nlocal_packages_path = {}\npackage_cache_local = False\npackage_cache_same_device = True\ntmpdir = {}\n",
        serde_json::to_string(&repository.to_string_lossy()).unwrap(),
        serde_json::to_string(&scratch.to_string_lossy()).unwrap(),
    )).unwrap();
    let output = private_cli(root, &config, None)
        .args(["pkg-cache", "--dir"])
        .arg(&cache)
        .arg("add")
        .arg(&package)
        .args(["--name", "cli_cache", "--version", "1", "--repository"])
        .arg(&repository)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Package is local"));
    assert!(!cache.join("cli_cache").exists());
    let output = private_cli(root, &config, None)
        .args(["pkg-cache", "--dir"])
        .arg(&cache)
        .arg("add")
        .arg(&package)
        .args(["--name", "cli_cache", "--version", "1", "--repository"])
        .arg(&repository)
        .arg("--force")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = private_cli(root, &config, None)
        .args(["pkg-cache", "--dir"])
        .arg(&cache)
        .arg("list")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("cli_cache-1"));
    let output = private_cli(root, &config, None)
        .args(["pkg-cache", "--dir"])
        .arg(&cache)
        .args(["remove", "cli_cache", "1", "--repository"])
        .arg(&repository)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Variant removed."));
    assert_eq!(
        std::fs::read_to_string(package.join("payload.txt")).unwrap(),
        "cached payload"
    );
    let output = private_cli(root, &config, None)
        .args(["pkg-cache", "--dir"])
        .arg(&cache)
        .arg("list")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("cli_cache-1"));
}

#[test]
fn test_cli_copy_renames_selected_variant_and_move_forwards_force() {
    let owned = tempfile::tempdir().unwrap();
    let root = owned.path();
    let repository = root.join("repository");
    let package = repository.join("cli_copy/1");
    let destination = root.join("destination");
    std::fs::create_dir_all(package.join("dep-a/dep-b")).unwrap();
    std::fs::create_dir_all(package.join(".rez/include")).unwrap();
    std::fs::create_dir_all(&destination).unwrap();
    std::fs::write(
        package.join("package.yaml"),
        "name: cli_copy\nversion: '1'\nrelocatable: true\nvariants: [[], ['dep-a'], ['dep-a', 'dep-b']]\ncustom: retained\n",
    ).unwrap();
    std::fs::write(package.join("dep-a/dep-b/payload.txt"), "selected payload").unwrap();
    std::fs::write(package.join("dep-a/unselected.txt"), "preserved source").unwrap();
    std::fs::write(package.join(".rez/include/helper.py"), "VALUE = 1\n").unwrap();
    let config = root.join("config.py");
    std::fs::write(&config, "implicit_packages = []\n").unwrap();
    for _ in 0..2 {
        let output = private_cli(root, &config, None)
            .args(["cp", "cli_copy-1", "--paths"])
            .arg(&repository)
            .arg("--dest-path")
            .arg(&destination)
            .args([
                "--allow-empty",
                "--rename",
                "renamed",
                "--reversion",
                "2",
                "--variants",
                "2",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let copied = destination.join("renamed/2");
    assert_eq!(
        std::fs::read_to_string(copied.join("dep-a/dep-b/payload.txt")).unwrap(),
        "selected payload"
    );
    assert!(!copied.join("dep-a/unselected.txt").exists());
    assert!(copied.join(".rez/include/helper.py").is_file());
    let metadata: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(copied.join("package.yaml")).unwrap())
            .unwrap();
    assert_eq!(metadata["name"].as_str(), Some("renamed"));
    assert_eq!(metadata["version"].as_str(), Some("2"));
    assert_eq!(metadata["variants"].as_sequence().unwrap().len(), 1);
    assert_eq!(metadata["custom"].as_str(), Some("retained"));
    let blocked = repository.join("blocked/1");
    std::fs::create_dir_all(&blocked).unwrap();
    std::fs::write(
        blocked.join("package.yaml"),
        "name: blocked\nversion: '1'\nrelocatable: false\n",
    )
    .unwrap();
    std::fs::write(blocked.join("payload.txt"), "kept original").unwrap();
    let output = private_cli(root, &config, None)
        .args(["mv", "blocked-1"])
        .arg(&repository)
        .arg("--dest-path")
        .arg(&destination)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!destination.join("blocked").exists());
    let output = private_cli(root, &config, None)
        .args(["mv", "blocked-1"])
        .arg(&repository)
        .arg("--dest-path")
        .arg(&destination)
        .arg("--force")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(blocked.join("payload.txt")).unwrap(),
        "kept original"
    );
    assert_eq!(
        std::fs::read_to_string(destination.join("blocked/1/payload.txt")).unwrap(),
        "kept original"
    );
    let output = private_cli(root, &config, None)
        .args(["search", "--paths"])
        .arg(&repository)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("blocked-1"));
}

#[test]
fn test_cli_cache_worker_uses_frozen_custom_definition_config() {
    use sha2::{Digest, Sha256};
    let owned = tempfile::tempdir().unwrap();
    let root = owned.path();
    let repository = root.join("repository");
    let package = repository.join("cli_worker/1");
    let cache_path = root.join("cache");
    let scratch = root.join("scratch");
    for path in [&package, &cache_path, &scratch] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::write(
        package.join("manifest.yaml"),
        "name: cli_worker\nversion: '1'\ncachable: true\n",
    )
    .unwrap();
    std::fs::write(package.join("payload.txt"), "snapshot payload").unwrap();
    let config_file = root.join("config.py");
    std::fs::write(
        &config_file,
        "implicit_packages = []\nplugins = {'package_repository': {'filesystem': {'package_filenames': ['manifest']}}}\n",
    ).unwrap();
    let context_file = root.join("source.rxt");
    let output = private_cli(root, &config_file, None)
        .args([
            "env",
            "cli_worker-1",
            "--ni",
            "--no-cache",
            "--no-pkg-cache",
            "--paths",
        ])
        .arg(&repository)
        .arg("--output")
        .arg(&context_file)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&context_file).unwrap()).unwrap();
    let handle = repository::ResourceHandle::from_json(
        &document["resolved_packages"][0],
        Some("worker fixture"),
    )
    .unwrap();
    let mut snapshot = serde_json::to_value(model::config::RezConfig::default()).unwrap();
    snapshot["implicit_packages"] = serde_json::json!([]);
    snapshot["cache_packages_path"] = serde_json::json!(cache_path);
    snapshot["package_cache_local"] = serde_json::json!(true);
    snapshot["package_cache_same_device"] = serde_json::json!(true);
    snapshot["package_cache_clean_limit"] = serde_json::json!(0.0);
    snapshot["tmpdir"] = serde_json::json!(scratch);
    snapshot["platform_map"] = serde_json::json!({
        "arch": {"^.*$": "snapshot_arch"},
        "os": {"^.*$": "snapshot_os"},
    });
    snapshot["plugins"]["package_repository"]["filesystem"]["package_filenames"] =
        serde_json::json!(["manifest"]);
    let snapshot: model::config::RezConfig = serde_json::from_value(snapshot).unwrap();
    let cache = repository::package::cache::PackageCache::new(&cache_path).unwrap();
    let digest = foundation::util::hex_encode(Sha256::digest(serde_json::to_vec(&handle).unwrap()));
    let request = cache_path
        .join(".sys/pending")
        .join(format!("request-{digest}.json"));
    let request_data =
        serde_json::to_vec(&serde_json::json!({"handle": handle, "config": snapshot})).unwrap();
    std::fs::write(&request, &request_data).unwrap();
    // Dispatch under a different current config. Each child must initialize its
    // typed snapshot before Python and repository configuration are first used.
    std::fs::write(&config_file, "implicit_packages = []\n").unwrap();
    let bin = root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let primary = bin.join(if cfg!(windows) { "rez.exe" } else { "rez" });
    std::fs::copy(rez_cmd().get_program(), &primary).unwrap();
    std::fs::write(bin.join(".rez_production_install"), "").unwrap();
    let output = private_cli(root, &config_file, Some(&primary))
        .args(["pkg-cache", "--dir"])
        .arg(&cache_path)
        .args(["worker", "--wait"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !request.exists(),
        "request not consumed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let entries = cache.get_variants().unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert!(matches!(
        entries[0].status,
        repository::package::cache::CacheStatus::Found(_)
    ));
    assert_eq!(
        std::fs::read_to_string(entries[0].cache_path.join("payload.txt")).unwrap(),
        "snapshot payload"
    );
    assert!(entries[0].cache_path.join("manifest.yaml").is_file());
    let receipt = cache_path
        .join(".sys/log")
        .join(format!("request-{digest}.log"));
    let receipt_text = std::fs::read_to_string(&receipt).unwrap();
    assert!(receipt_text.contains("snapshot_arch"), "{receipt_text}");
    assert!(receipt_text.contains("snapshot_os"), "{receipt_text}");
    // A normal absolute CLI path must select the same request as canonical
    // paths emitted by the dispatcher (notably Windows verbatim prefixes).
    std::fs::write(&request, &request_data).unwrap();
    let output = private_cli(root, &config_file, Some(&primary))
        .args(["pkg-cache", "--dir"])
        .arg(&cache_path)
        .args(["worker", "--wait", "--request"])
        .arg(&request)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!request.exists(), "absolute request was not selected");
    assert_eq!(
        std::fs::read_to_string(package.join("payload.txt")).unwrap(),
        "snapshot payload"
    );
}

#[test]
fn test_cli_payload_cache_roots_and_flags_apply_before_loading() {
    let owned = tempfile::tempdir().unwrap();
    let root = owned.path();
    let repository = root.join("repository");
    let package = repository.join("cli_roots/1");
    let cache = root.join("cache");
    let scratch = root.join("scratch");
    for path in [&package, &cache, &scratch] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::write(package.join("package.yaml"),
        "name: cli_roots\nversion: '1'\ncachable: true\ncommands: |\n  env.TEST_THIS_ROOT = this.root\n  env.TEST_RESOLVE_ROOT = resolve.cli_roots.root\n").unwrap();
    std::fs::write(package.join("payload.txt"), "root payload").unwrap();
    let config = root.join("config.py");
    std::fs::write(&config, format!(
        "implicit_packages = []\ncache_packages_path = {}\npackage_cache_same_device = True\npackage_cache_local = True\npackage_cache_during_build = False\npackage_cache_clean_limit = 0.0\ntmpdir = {}\n",
        serde_json::to_string(&cache.to_string_lossy()).unwrap(),
        serde_json::to_string(&scratch.to_string_lossy()).unwrap(),
    )).unwrap();
    let bin = root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let primary = bin.join(if cfg!(windows) { "rez.exe" } else { "rez" });
    std::fs::copy(rez_cmd().get_program(), &primary).unwrap();
    std::fs::write(bin.join(".rez_production_install"), "").unwrap();
    let context = root.join("context.rxt");
    let output = private_cli(root, &config, Some(&primary))
        .args([
            "env",
            "cli_roots-1",
            "--ni",
            "--no-cache",
            "--no-pkg-cache",
            "--paths",
        ])
        .arg(&repository)
        .arg("--output")
        .arg(&context)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!cache.join("cli_roots").exists());
    let mut document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&context).unwrap()).unwrap();
    // Input has caching enabled; --no-pkg-cache must override before any work.
    document["package_caching"] = serde_json::json!(true);
    std::fs::write(&context, serde_json::to_vec(&document).unwrap()).unwrap();
    for args in [vec!["--no-pkg-cache"], vec!["--pkg-cache-mode", "invalid"]] {
        let output = private_cli(root, &config, Some(&primary))
            .args(["env", "--input"])
            .arg(&context)
            .args(args.clone())
            .arg("--output")
            .arg(root.join("disabled.rxt"))
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            args[0] == "--no-pkg-cache",
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!cache.join("cli_roots").exists());
    }
    let output = private_cli(root, &config, Some(&primary))
        .args(["env", "--input"])
        .arg(&context)
        .args(["--pkg-cache-mode", "sync", "--output"])
        .arg(root.join("cached.rxt"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let entries = repository::package::cache::PackageCache::new(&cache)
        .unwrap()
        .get_variants()
        .unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    let cached = entries[0].cache_path.to_string_lossy().replace('\\', "/");
    let original = package.to_string_lossy().replace('\\', "/");
    let command = if cfg!(windows) {
        "echo ROOT=%REZ_CLI_ROOTS_ROOT%&echo ORIG=%REZ_CLI_ROOTS_ORIG_ROOT%&echo THIS=%TEST_THIS_ROOT%&echo RESOLVE=%TEST_RESOLVE_ROOT%"
    } else {
        "printf 'ROOT=%s\\nORIG=%s\\nTHIS=%s\\nRESOLVE=%s\\n' \"$REZ_CLI_ROOTS_ROOT\" \"$REZ_CLI_ROOTS_ORIG_ROOT\" \"$TEST_THIS_ROOT\" \"$TEST_RESOLVE_ROOT\""
    };
    let output = private_cli(root, &config, Some(&primary))
        .args(["env", "--input"])
        .arg(&context)
        .args([
            "--pkg-cache-mode",
            "sync",
            "--quiet",
            "--shell",
            if cfg!(windows) { "cmd" } else { "bash" },
            "--command",
            command,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).replace('\\', "/");
    for expected in [
        format!("ROOT={cached}"),
        format!("ORIG={original}"),
        format!("THIS={cached}"),
        format!("RESOLVE={cached}"),
    ] {
        assert!(
            stdout.contains(&expected),
            "{expected:?} missing from {stdout:?}"
        );
    }
    // Build environments respect their independent cache policy.
    let build_cache = root.join("build-cache");
    std::fs::create_dir(&build_cache).unwrap();
    let output = private_cli(root, &config, Some(&primary))
        .env("REZ_CACHE_PACKAGES_PATH", &build_cache)
        .args([
            "env",
            "cli_roots-1",
            "--build",
            "--ni",
            "--no-cache",
            "--paths",
        ])
        .arg(&repository)
        .arg("--output")
        .arg(root.join("build.rxt"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!build_cache.join("cli_roots").exists());
}

#[test]
fn test_cli_search_no_arg_lists_all() {
    // search without arguments lists all packages (exits 0)
    let out = rez_cmd()
        .args(["search", "--paths", &fixtures_str()])
        .output()
        .expect("failed to run rez search (no arg)");
    assert!(
        out.status.success(),
        "search without package arg should list all"
    );
}
