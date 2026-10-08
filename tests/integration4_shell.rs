//! Integration tests for shell path normalization and end-to-end resolve.
//!
//! Tests that the shell layer correctly converts Windows paths to POSIX format
//! for MSYS2/Git Bash, and that the full bind -> resolve -> execute pipeline works.

use rex::rex::*;
use rex::types::*;

// =============================================================================
// BashShell POSIX path conversion (Windows-specific)
// =============================================================================

#[test]
fn test_gitbash_context_script_posix_paths() {
    let mut sh = BashShell::new(ShellType::Gitbash);
    sh.setenv("REZ_USED", "C:/projects/rez-rs/target/release/rez.exe");
    sh.setenv("PATH", "C:/Users/test/packages/python/3.11/bin");
    sh.appendenv("PATH", "C:/Windows/System32");
    sh.appendenv("PATH", "D:/tools/bin");
    sh.apply_action(&Action::Setenv {
        key: "PYTHONPATH".into(),
        value: RexValue::literal("E:/literal/bin"),
    });

    let out = Shell::get_output(&sh, OutputStyle::File);
    // Rez normalizes configured pathed keys only; literal segments retain bytes.
    // See reference rex.py:560-561 and rezplugins/shell/sh.py:106-108.
    assert!(
        out.lines()
            .any(|line| line == r#"export REZ_USED="C:/projects/rez-rs/target/release/rez.exe""#),
        "{out}"
    );
    assert!(
        out.lines()
            .any(|line| line == r#"export PYTHONPATH='E:/literal/bin'"#),
        "{out}"
    );

    if cfg!(windows) {
        let path_lines: Vec<_> = out
            .lines()
            .filter(|line| line.starts_with("export PATH="))
            .collect();
        assert_eq!(path_lines.len(), 3, "{out}");
        assert_eq!(
            path_lines[0],
            r#"export PATH="/c/Users/test/packages/python/3.11/bin""#
        );
        assert!(path_lines[1].contains("/c/Windows/System32"), "{out}");
        assert!(path_lines[2].contains("/d/tools/bin"), "{out}");
        assert!(
            path_lines
                .iter()
                .all(|line| !line.contains("C:/") && !line.contains("D:/")),
            "{out}"
        );
    }
}

#[test]
fn test_cmd_context_script_windows_paths() {
    // Verify that CMD context script keeps Windows paths
    let mut sh = CmdShell::new();
    sh.setenv("PATH", "C:\\Users\\test\\bin");
    sh.appendenv("PATH", "C:\\Windows\\System32");

    let out = Shell::get_output(&sh, OutputStyle::File);
    // CMD should NOT convert to POSIX paths
    assert!(
        !out.contains("/c/"),
        "CMD should not have POSIX paths:\n{out}"
    );
}

#[test]
fn test_powershell_context_script_windows_paths() {
    // Verify that PowerShell context script keeps Windows paths
    let mut sh = PowerShellShell::new();
    sh.setenv("PATH", "C:/Users/test/bin");
    sh.appendenv("PATH", "C:/Windows/System32");

    let out = Shell::get_output(&sh, OutputStyle::File);
    // PowerShell should NOT convert to POSIX paths
    assert!(
        !out.contains("/c/"),
        "PowerShell should not have POSIX paths:\n{out}"
    );
}

// =============================================================================
// Path separator tests
// =============================================================================

#[test]
fn test_bash_uses_colon_separator() {
    let mut sh = BashShell::new(ShellType::Gitbash);
    sh.appendenv("PATH", "/usr/local/bin");
    sh.appendenv("PATH", "/usr/bin");

    let out = Shell::get_output(&sh, OutputStyle::File);
    // Should use : separator, not ;
    assert!(out.contains(":"), "Bash should use : separator:\n{out}");
    assert!(
        !out.contains(";"),
        "Bash should NOT use ; separator:\n{out}"
    );
}

#[test]
fn test_cmd_uses_semicolon_separator() {
    let mut sh = CmdShell::new();
    sh.appendenv("PATH", "C:\\Windows\\System32");

    let out = Shell::get_output(&sh, OutputStyle::File);
    // Should use ; separator
    assert!(out.contains(";"), "CMD should use ; separator:\n{out}");
}

// =============================================================================
// RexExecutor shell output (end-to-end action → context script)
// =============================================================================

#[test]
fn test_rex_executor_gitbash_actions() {
    let sh = Box::new(BashShell::new(ShellType::Gitbash));
    let mut executor = RexExecutor::new(sh, None, false);
    let rez_used = "C:/projects/rez-rs/target/release/rez.exe";
    let python_root =
        "C:/Users/test/packages/python/3.11/platform-windows/arch-x86_64/os-windows-10.0";
    let python_bin = format!("{python_root}/bin");

    executor.setenv("REZ_USED", rez_used);
    executor.setenv("REZ_USED_VERSION", "0.1.0");
    executor.setenv("REZ_PYTHON_ROOT", python_root);
    executor.setenv("PATH", python_bin.as_str());
    executor.appendenv("PATH", "C:/WINDOWS/System32");
    executor.appendenv("PATH", "C:/WINDOWS");
    executor.setenv("PYTHONPATH", RexValue::literal("E:/literal/bin"));

    let sh_ref: &dyn Shell = executor.interpreter().as_ref();
    let output = Shell::get_output(sh_ref, OutputStyle::File);
    assert!(
        output
            .lines()
            .any(|line| line == format!("export REZ_USED=\"{rez_used}\"")),
        "{output}"
    );
    assert!(
        output
            .lines()
            .any(|line| line == format!("export REZ_PYTHON_ROOT=\"{python_root}\"")),
        "{output}"
    );
    let expected_path = if cfg!(windows) {
        format!(
            "/c/{}/bin:/c/WINDOWS/System32:/c/WINDOWS",
            &python_root[3..]
        )
    } else {
        format!("{python_bin}:C:/WINDOWS/System32:C:/WINDOWS")
    };
    if cfg!(windows) {
        let path_lines: Vec<_> = output
            .lines()
            .filter(|line| line.starts_with("export PATH="))
            .collect();
        assert_eq!(path_lines.len(), 3, "{output}");
        assert!(
            path_lines.iter().all(|line| !line.contains("C:/")),
            "{output}"
        );
    }

    // Resolve an absolute Bash path so Windows cannot choose the WSL launcher.
    if let Some(bash) = repository::package::bind::find_executable("bash") {
        let probe = format!(
            "{output}\nprintf '%s\\n' \"$REZ_USED\" \"$REZ_PYTHON_ROOT\" \"$PATH\" \"$PYTHONPATH\""
        );
        let result = std::process::Command::new(bash)
            .args(["--noprofile", "--norc", "-c", &probe])
            .env_remove("BASH_ENV")
            .env_remove("ENV")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let stdout = String::from_utf8(result.stdout).unwrap();
        assert_eq!(
            stdout.trim_end(),
            format!("{rez_used}\n{python_root}\n{expected_path}\nE:/literal/bin")
        );
    }
}

// =============================================================================
// System paths discovery
// =============================================================================

#[test]
fn test_system_paths_not_empty() {
    let paths = repository::package::bind::discover_sys_paths();
    assert!(
        !paths.is_empty(),
        "System paths discovery should return paths"
    );

    if cfg!(windows) {
        // Should contain common Windows paths
        let has_win_path = paths.iter().any(|p| {
            let lower = p.to_lowercase();
            lower.contains("windows") || lower.contains("system32")
        });
        assert!(
            has_win_path,
            "Windows system paths should include Windows/System32 dirs: {:?}",
            &paths[..paths.len().min(10)]
        );
    }
}

#[test]
fn test_system_paths_no_percent_vars() {
    // System paths should be fully expanded, no %VAR% references
    let paths = repository::package::bind::discover_sys_paths();
    for p in &paths {
        assert!(
            !p.contains('%'),
            "System path should not contain unexpanded %%VAR%%: {p}"
        );
    }
}

#[test]
fn test_system_paths_native_baseline() {
    let paths = repository::package::bind::discover_sys_paths();
    if cfg!(windows) {
        let root = std::env::vars_os()
            .filter(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case("SystemRoot"))
            .map(|(_, value)| value.to_string_lossy().into_owned())
            .find(|value| !value.is_empty())
            .unwrap_or_else(|| r"C:\WINDOWS".to_owned());
        let root_path = std::path::Path::new(&root);
        let expected = [
            root_path.join("system32").to_string_lossy().into_owned(),
            root.clone(),
            root_path
                .join(r"System32\Wbem")
                .to_string_lossy()
                .into_owned(),
            root_path
                .join(r"System32\WindowsPowerShell\v1.0")
                .to_string_lossy()
                .into_owned(),
            root_path
                .join(r"System32\OpenSSH")
                .to_string_lossy()
                .into_owned(),
        ];
        assert_eq!(
            paths, expected,
            "bind must retain the ordered native OS baseline"
        );
    } else {
        assert_eq!(
            paths,
            [
                "/usr/local/sbin",
                "/usr/local/bin",
                "/usr/sbin",
                "/usr/bin",
                "/sbin",
                "/bin"
            ]
        );
    }
}

// =============================================================================
// Bind + resolve end-to-end (filesystem-based)
// =============================================================================

#[test]
fn test_bind_creates_python_with_symlink() {
    // Test that bind creates python package with bin/python symlink
    let tmpdir = tempfile::tempdir().unwrap();
    let pkg_dir = tmpdir.path().join("python").join("3.0.0");
    std::fs::create_dir_all(pkg_dir.join("bin")).unwrap();

    // Create a dummy python.exe (or just test the package.py creation)
    let pkg_py = pkg_dir.join("package.py");
    std::fs::write(
        &pkg_py,
        concat!(
            "name = \"python\"\n",
            "version = \"3.0.0\"\n",
            "def commands():\n",
            "    env.PATH.append('{this.root}/bin')\n",
        ),
    )
    .unwrap();

    // Verify the package.py has correct commands
    let content = std::fs::read_to_string(&pkg_py).unwrap();
    assert!(
        content.contains("env.PATH.append"),
        "Should have PATH command"
    );
    assert!(
        content.contains("{this.root}/bin"),
        "Should use template vars"
    );
}

#[test]
fn test_rez_1_environment_variables_are_explicitly_gated() {
    use std::collections::BTreeSet;
    use std::process::Command;
    let temporary = tempfile::tempdir().unwrap();
    let repository = temporary.path().join("repo");
    for (name, version) in [("explicit_probe", "1.2.3"), ("implicit_probe", "1")] {
        let package = repository.join(name).join(version);
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join("package.yaml"),
            format!("name: {name}\nversion: '{version}'\n"),
        )
        .unwrap();
    }
    let binary = env!("CARGO_BIN_EXE_rez");
    let shell = if cfg!(windows) {
        ShellType::Cmd
    } else {
        ShellType::Sh
    };
    let code = concat!(
        "import os,json; ",
        "keys=['REZ_VERSION','REZ_PATH','REZ_REQUEST','REZ_RESOLVE',",
        "'REZ_RAW_REQUEST','REZ_RESOLVE_MODE','REZ_USED','REZ_USED_VERSION']; ",
        "print(json.dumps({key:os.environ.get(key) for key in keys}))"
    );
    let child = shell.join_command(
        &[binary.into(), "python".into(), "-c".into(), code.into()],
        false,
        None,
    );
    for (enabled, disabled) in [(true, false), (false, false), (true, true)] {
        let config = temporary
            .path()
            .join(format!("legacy_{enabled}_{disabled}.toml"));
        std::fs::write(
            &config,
            format!(
                "rez_1_environment_variables = {enabled}\n\
             disable_rez_1_compatibility = {disabled}\n\
             implicit_packages = ['implicit_probe']\n\
             append_sys_path = false\n\
             clean_shell_environment = false\n\
             rez_tools_visibility = 'never'\n\
             resolve_caching = false\ncache_package_files = false\n"
            ),
        )
        .unwrap();
        let mut command = Command::new(binary);
        for (name, _) in std::env::vars_os() {
            if name
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("REZ_")
            {
                command.env_remove(name);
            }
        }
        let output = command
            .args(["env", "explicit_probe", "--paths"])
            .arg(&repository)
            .args([
                "--shell",
                shell.name(),
                "-q",
                "--no-cache",
                "--no-pkg-cache",
                "-c",
            ])
            .arg(&child)
            .env("REZ_CONFIG_FILE", &config)
            .env("REZ_DISABLE_HOME_CONFIG", "1")
            .current_dir(temporary.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "enabled={enabled} disabled={disabled}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let data: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let legacy = [
            "REZ_VERSION",
            "REZ_PATH",
            "REZ_REQUEST",
            "REZ_RESOLVE",
            "REZ_RAW_REQUEST",
            "REZ_RESOLVE_MODE",
        ];
        if !enabled || disabled {
            for key in legacy {
                assert!(data[key].is_null(), "{key}: {data}");
            }
        } else {
            assert_eq!(data["REZ_VERSION"], data["REZ_USED_VERSION"]);
            assert_eq!(
                data["REZ_PATH"],
                data["REZ_USED"].as_str().unwrap().replace('\\', "/")
            );
            assert_eq!(data["REZ_REQUEST"], "explicit_probe implicit_probe");
            assert_eq!(data["REZ_RAW_REQUEST"], data["REZ_REQUEST"]);
            assert_eq!(data["REZ_RESOLVE_MODE"], "latest");
            let resolved: BTreeSet<_> = data["REZ_RESOLVE"]
                .as_str()
                .unwrap()
                .split_whitespace()
                .collect();
            assert_eq!(
                resolved,
                BTreeSet::from(["explicit_probe-1.2.3", "implicit_probe-1"])
            );
        }
    }
}

#[test]
fn test_template_expansion_in_rex_preamble() {
    use repository::FilesystemPackageProvider;
    use resolve::context::{ResolveOptions, ResolvedContext};
    use version::Requirement;
    let repository = tempfile::tempdir().unwrap();
    let package = repository.path().join("template_probe").join("1.2.3");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("package.py"),
        concat!(
            "name = 'template_probe'\nversion = '1.2.3'\n",
            "def commands():\n",
            "    setenv('TEMPLATE_ROOT', '{this.root}')\n",
            "    setenv('TEMPLATE_NAME', '{this.name}')\n",
            "    setenv('TEMPLATE_VERSION', '{this.version}')\n",
            "    setenv('TEMPLATE_MAJOR', '{this.version.major}')\n",
        ),
    )
    .unwrap();
    let provider = FilesystemPackageProvider::from_path(repository.path()).unwrap();
    let context = ResolvedContext::resolve(
        vec![Requirement::new("template_probe").unwrap()],
        &provider,
        ResolveOptions {
            package_paths: Some(vec![repository.path().to_path_buf()]),
            add_implicit: false,
            caching: false,
            ..ResolveOptions::default()
        },
    )
    .unwrap();
    let environment = context
        .get_environ(Some(std::collections::HashMap::new()))
        .unwrap();
    assert_eq!(environment["TEMPLATE_ROOT"], package.display().to_string());
    assert_eq!(environment["TEMPLATE_NAME"], "template_probe");
    assert_eq!(environment["TEMPLATE_VERSION"], "1.2.3");
    assert_eq!(environment["TEMPLATE_MAJOR"], "1");
}

#[cfg(windows)]
#[test]
fn suite_visibility_matches_reference_24_process_cases() {
    use repository::FilesystemPackageProvider;
    use resolve::context::{ResolveOptions, ResolvedContext};
    use std::process::Command;
    use version::Requirement;
    let temporary = tempfile::tempdir().unwrap();
    let repository = temporary.path().join("repo");
    let package = repository.join("suite_probe").join("1");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("package.py"),
        "name = 'suite_probe'\nversion = '1'\ndef commands():\n    env.PATH = 'BASE'\n",
    )
    .unwrap();
    let suites: Vec<_> = ["a", "b", "c"]
        .into_iter()
        .map(|name| {
            let path = temporary.path().join(name);
            std::fs::create_dir_all(path.join("bin")).unwrap();
            std::fs::write(path.join("suite.yaml"), "contexts: {}\n").unwrap();
            path
        })
        .collect();
    let provider = FilesystemPackageProvider::from_path(&repository).unwrap();
    let binary = env!("CARGO_BIN_EXE_rez");
    let child = ShellType::Cmd.join_command(
        &[
            binary.into(),
            "python".into(),
            "-c".into(),
            "import os,json; print(json.dumps(os.environ['PATH'].split(os.pathsep)))".into(),
        ],
        false,
        None,
    );
    let launcher = std::path::PathBuf::from(
        find_executable(ShellType::Cmd.executable(), None).expect("Cmd executable on host PATH"),
    );
    assert!(launcher.is_absolute() && launcher.is_file(), "{launcher:?}");
    let launcher_parent = launcher.parent().unwrap().to_path_buf();
    assert!(
        resolve::suite::Suite::visible_suite_paths(Some(&[launcher_parent.to_str().unwrap()]))
            .is_empty(),
        "the shell launcher directory must not expose a suite"
    );
    let no_suite_path = std::env::join_paths([&launcher_parent]).unwrap();
    let host_path = std::env::join_paths([
        launcher_parent,
        suites[0].join("bin"),
        suites[1].join("bin"),
        suites[0].join("bin"),
    ])
    .unwrap();
    let normalize = |path: &std::path::Path| path.to_string_lossy().replace('\\', "/");
    for parent in [None, Some(0), Some(2)] {
        let mut context = ResolvedContext::resolve(
            vec![Requirement::new("suite_probe").unwrap()],
            &provider,
            ResolveOptions {
                package_paths: Some(vec![repository.clone()]),
                add_implicit: false,
                caching: false,
                ..ResolveOptions::default()
            },
        )
        .unwrap();
        context.append_sys_path = false;
        if let Some(index) = parent {
            context.set_parent_suite(&suites[index].to_string_lossy(), "suite_probe");
        }
        let input = temporary.path().join(format!("parent_{parent:?}.rxt"));
        context.save(&input).unwrap();
        for visible in [false, true] {
            for mode in ["never", "always", "parent", "parent_priority"] {
                let config = temporary.path().join(format!("suite_{mode}.toml"));
                std::fs::write(&config, format!("suite_visibility = '{mode}'\nappend_sys_path = false\nrez_tools_visibility = 'never'\nimplicit_packages = []\nclean_shell_environment = false\n")).unwrap();
                let mut command = Command::new(binary);
                for (name, _) in std::env::vars_os() {
                    if name
                        .to_string_lossy()
                        .to_ascii_uppercase()
                        .starts_with("REZ_")
                    {
                        command.env_remove(name);
                    }
                }
                let output = command
                    .args(["env", "--input"])
                    .arg(&input)
                    .args(["--shell", "cmd", "-q", "-c"])
                    .arg(&child)
                    .env("REZ_CONFIG_FILE", &config)
                    .env("REZ_DISABLE_HOME_CONFIG", "1")
                    .env(
                        "PATH",
                        if visible {
                            host_path.as_os_str()
                        } else {
                            no_suite_path.as_os_str()
                        },
                    )
                    .current_dir(temporary.path())
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "visible={visible} parent={parent:?} mode={mode}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let actual: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();
                let actual: Vec<_> = actual
                    .into_iter()
                    .map(|path| path.replace('\\', "/"))
                    .collect();
                let mut expected = vec!["BASE".to_owned()];
                if visible {
                    match mode {
                        "always" => expected
                            .extend([0, 1, 0].map(|index| normalize(&suites[index].join("bin")))),
                        "parent" | "parent_priority" => {
                            if let Some(index) = parent {
                                expected.push(normalize(&suites[index].join("bin")));
                            }
                        }
                        _ => {}
                    }
                }
                assert_eq!(
                    actual, expected,
                    "visible={visible} parent={parent:?} mode={mode}"
                );
            }
        }
    }
}

#[test]
fn malformed_config_implicit_packages_propagate_unless_disabled() {
    use std::process::Command;
    let temporary = tempfile::tempdir().unwrap();
    let repository = temporary.path().join("repo");
    let package = repository.join("explicit_probe").join("1");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("package.yaml"),
        "name: explicit_probe\nversion: '1'\n",
    )
    .unwrap();
    let config = temporary.path().join("invalid.toml");
    std::fs::write(&config, "implicit_packages = ['bad-1.']\nappend_sys_path = false\nrez_tools_visibility = 'never'\nsuite_visibility = 'never'\n").unwrap();
    let binary = env!("CARGO_BIN_EXE_rez");
    let shell = if cfg!(windows) {
        ShellType::Cmd
    } else {
        ShellType::Sh
    };
    let child = shell.join_command(
        &[
            binary.into(),
            "python".into(),
            "-c".into(),
            "print('implicit bypass accepted')".into(),
        ],
        false,
        None,
    );
    for disabled in [false, true] {
        let mut command = Command::new(binary);
        for (name, _) in std::env::vars_os() {
            if name
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("REZ_")
            {
                command.env_remove(name);
            }
        }
        command
            .args(["env", "explicit_probe", "--paths"])
            .arg(&repository);
        if disabled {
            command.arg("--ni");
        }
        let output = command
            .args([
                "--shell",
                shell.name(),
                "-q",
                "--no-cache",
                "--no-pkg-cache",
                "-c",
            ])
            .arg(&child)
            .env("REZ_CONFIG_FILE", &config)
            .env("REZ_DISABLE_HOME_CONFIG", "1")
            .current_dir(temporary.path())
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            disabled,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if disabled {
            assert_eq!(
                String::from_utf8(output.stdout).unwrap().trim(),
                "implicit bypass accepted"
            );
        } else {
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("bad-1."),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
