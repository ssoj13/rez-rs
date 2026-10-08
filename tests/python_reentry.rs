//! The embedded interpreter remains executable through its real sys.executable.
use std::io::Write;
use std::process::{Command, Stdio};

fn rez() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rez"));
    command
        .env("REZ_DISABLE_HOME_CONFIG", "1")
        .env_remove("REZ_CONFIG_FILE")
        .env_remove("REZ_RS_PYTHON_REENTRY");
    command
}

#[test]
fn nested_python_runs_scripts_commands_and_modules() {
    let temp = tempfile::tempdir().unwrap();
    let scripts = temp.path().join("script directory");
    std::fs::create_dir(&scripts).unwrap();
    std::fs::write(scripts.join("helper.py"), "VALUE = 'local-import'\n").unwrap();
    std::fs::write(
        scripts.join("probe.py"),
        r#"
import atexit, helper, json, os, sys
assert __name__ == "__main__"
assert sys.modules["__main__"].__dict__ is globals()
assert os.path.samefile(__file__, sys.argv[0])
assert os.path.samefile(sys.path[0], os.path.dirname(__file__))
assert helper.VALUE == "local-import"
assert "REZ_RS_PYTHON_REENTRY" not in os.environ
atexit.register(lambda: print("FINALIZED"))
print(json.dumps(sys.argv[1:]))
"#,
    )
    .unwrap();
    std::fs::write(
        temp.path().join("module_probe.py"),
        r#"
import json, os, sys
assert __name__ == "__main__"
assert __spec__.name == "module_probe"
assert sys.argv[0].endswith("module_probe.py")
assert "REZ_RS_PYTHON_REENTRY" not in os.environ
print(json.dumps(sys.argv[1:]))
"#,
    )
    .unwrap();
    let probe = r#"
import json, os, subprocess, sys
assert "REZ_RS_PYTHON_REENTRY" not in os.environ
args = ["space argument", "-negative", "--version", "snowman-\u2603"]
script = subprocess.run([sys.executable, "script directory/probe.py", *args],
                        text=True, capture_output=True)
assert script.returncode == 0, (script.returncode, script.stdout, script.stderr)
assert json.loads(script.stdout.splitlines()[0]) == args
assert script.stdout.splitlines()[1:] == ["FINALIZED"]
module = subprocess.run([sys.executable, "-m", "module_probe", *args],
                        text=True, capture_output=True)
assert module.returncode == 0, (module.returncode, module.stdout, module.stderr)
assert json.loads(module.stdout) == args
code = subprocess.run([sys.executable, "-c",
                       "import sys; print(sys.argv); raise SystemExit(7)", "-3"],
                      text=True, capture_output=True)
assert code.returncode == 7, (code.returncode, code.stderr)
assert code.stdout.strip() == "['-c', '-3']"
zero = subprocess.run([sys.executable, "-c", "raise SystemExit(0)"])
assert zero.returncode == 0
stdin = subprocess.run([sys.executable, "-"],
                       input="import sys; print(sys.argv)\n",
                       text=True, capture_output=True)
assert stdin.returncode == 0, (stdin.returncode, stdin.stderr)
assert stdin.stdout.strip() == "['-']"
print("REENTRY_OK")
"#;
    for prefix in [vec!["python"], vec!["-v", "python"], vec![]] {
        let output = rez()
            .current_dir(temp.path())
            .args(prefix)
            .args(["-c", probe])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "REENTRY_OK");
    }
}

#[test]
fn explicit_python_preserves_native_options_stdin_and_exit_status() {
    for args in [
        vec![
            "python",
            "-I",
            "-S",
            "-c",
            "import sys; assert sys.flags.isolated; assert sys.flags.no_site; print(sys.argv)",
            "--version",
        ],
        vec!["python", "-c", "import sys; print(sys.argv)", "--version"],
    ] {
        let output = rez().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "['-c', '--version']"
        );
    }
    for code in [0, 7] {
        let output = rez()
            .args(["python", "-c", &format!("raise SystemExit({code})")])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(code),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stderr.is_empty(),
            "SystemExit became an exception diagnostic"
        );
    }
    let mut child = rez()
        .arg("python")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"print('STDIN_OK')\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "STDIN_OK");
}

#[test]
fn python_dispatch_does_not_capture_rez_commands() {
    let temp = tempfile::tempdir().unwrap();
    // Even a directory sharing a command name must not override Rez's command.
    std::fs::create_dir(temp.path().join("env")).unwrap();
    let output = rez()
        .current_dir(temp.path())
        .args(["env", "--help"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("PKG"));
    let output = rez().args(["-v", "bind", "--list"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("platform"));
}

#[test]
fn python_identity_owns_help_version_verbose_and_recursive_children() {
    let temp = tempfile::tempdir().unwrap();
    let probe = r#"
import os, subprocess, sys
assert os.path.basename(sys.executable) in ("rez-python", "rez-python.exe"), sys.executable
assert sys._base_executable == sys.executable
for option in ("--version", "-V"):
    child = subprocess.run([sys.executable, option], text=True, capture_output=True)
    assert child.returncode == 0, child.stderr
    assert "Python" in child.stdout and not child.stdout.startswith("rez "), child.stdout
for option in ("--help", "-h"):
    child = subprocess.run([sys.executable, option], text=True, capture_output=True)
    assert child.returncode == 0, child.stderr
    assert "Studio Setup Walkthrough" not in child.stdout, child.stdout
    assert "-c" in child.stdout and "-m" in child.stdout, child.stdout
verbose = subprocess.run([sys.executable, "-v", "-c", "print('PYTHON_VERBOSE')"],
                         text=True, capture_output=True)
assert verbose.returncode == 0, verbose.stderr
assert verbose.stdout.strip() == "PYTHON_VERBOSE", verbose.stdout
stdin = subprocess.run([sys.executable], input="print('NESTED_STDIN')\n",
                       text=True, capture_output=True)
assert stdin.returncode == 0, stdin.stderr
assert stdin.stdout.strip() == "NESTED_STDIN", stdin.stdout
recursive = subprocess.run([sys.executable, "-c",
    "import subprocess, sys; raise SystemExit(subprocess.call([sys.executable, '-c', 'raise SystemExit(9)']))"])
assert recursive.returncode == 9, recursive.returncode
# Children explicitly addressing the primary executable continue to run Rez.
rez = subprocess.run([sys.argv[1], "--version"], text=True, capture_output=True)
assert rez.returncode == 0 and rez.stdout.startswith("rez "), (rez.stdout, rez.stderr)
print("IDENTITY_OK")
"#;
    let output = rez()
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .args(["python", "-c", probe, env!("CARGO_BIN_EXE_rez")])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "IDENTITY_OK"
    );
}

#[test]
fn package_source_has_the_same_native_python_identity() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repository");
    let package = repository.join("identity_probe").join("1");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("package.py"),
        r#"
import subprocess, sys
name = "identity_probe"
version = "1"
@early()
def build_command():
    child = subprocess.run([sys.executable, "--version"], text=True, capture_output=True)
    assert child.returncode == 0 and "Python" in child.stdout, (child.stdout, child.stderr)
    return [sys.executable, "--version"]
"#,
    )
    .unwrap();
    let output = rez()
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .args([
            "view",
            "identity_probe",
            "--brief",
            "--format",
            "json",
            "--paths",
        ])
        .arg(&repository)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let data: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let identity = std::path::Path::new(data["build_command"][0].as_str().unwrap());
    assert_eq!(
        identity.file_name().unwrap(),
        if cfg!(windows) {
            "rez-python.exe"
        } else {
            "rez-python"
        }
    );
    assert!(identity.starts_with(temp.path().join(".rez").join("python")));
}

#[test]
fn library_embedding_preserves_the_host_executable_identity() {
    assert!(python_runtime::executable().unwrap().is_none());
    let globals = python_runtime::exec_py_globals(
        "import sys\nidentity = sys.executable\n",
        "<embedding>",
        None,
        &[],
    )
    .unwrap();
    let identity = globals["identity"].as_str().unwrap();
    assert!(
        std::path::Path::new(identity).file_name().unwrap()
            != if cfg!(windows) {
                "rez-python.exe"
            } else {
                "rez-python"
            }
    );
    assert_eq!(
        std::fs::canonicalize(identity).unwrap(),
        std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap()
    );
}

#[test]
fn python_identity_normalizes_home_and_survives_changed_working_directory() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("profile space snowman-☃");
    let other = temp.path().join("other directory");
    std::fs::create_dir(&profile).unwrap();
    std::fs::create_dir(&other).unwrap();
    let probe = r#"
import os, subprocess, sys
assert os.path.isabs(sys.executable), sys.executable
assert os.path.samefile(os.path.dirname(os.path.dirname(sys.executable)), sys.argv[1])
os.chdir(sys.argv[2])
child = subprocess.run([sys.executable, "--version"], text=True, capture_output=True)
assert child.returncode == 0 and "Python" in child.stdout, (child.stdout, child.stderr)
print("HOME_IDENTITY_OK")
"#;
    // A blank HOME must fall through to USERPROFILE. A nonempty relative HOME
    // remains supported but must never become a relative sys.executable.
    for home in ["", "profile space snowman-☃"] {
        let output = rez()
            .current_dir(temp.path())
            .env("HOME", home)
            .env("USERPROFILE", &profile)
            .args(["python", "-c", probe])
            .arg(profile.join(".rez").join("python"))
            .arg(&other)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "home={home:?} stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "HOME_IDENTITY_OK"
        );
    }
    assert!(!temp.path().join(".rez").exists());
}

#[test]
fn invalid_runtime_home_fails_before_creating_a_cache() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("file-home"), b"not a directory").unwrap();
    for home in ["missing-home", "file-home"] {
        let output = rez()
            .current_dir(temp.path())
            .env("HOME", home)
            .env("USERPROFILE", "")
            .args([
                "python",
                "-c",
                "raise AssertionError('VM should not start')",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("home"), "{stderr}");
        assert!(!stderr.contains("panicked"), "{stderr}");
        assert!(!temp.path().join(".rez").exists());
        assert!(!temp.path().join("missing-home").exists());
        // Registering the CLI does not require a writable cache for plain Rez flags.
        let version = rez()
            .current_dir(temp.path())
            .env("HOME", home)
            .env("USERPROFILE", "")
            .arg("--version")
            .output()
            .unwrap();
        assert!(version.status.success());
        assert!(String::from_utf8_lossy(&version.stdout).starts_with("rez "));
    }
}

#[test]
fn piped_python_stdin_runs_one_program_with_native_metadata_and_encoding() {
    let temp = tempfile::tempdir().unwrap();
    let source = "import atexit, sys\nassert __name__ == '__main__'\nassert sys.modules['__main__'].__dict__ is globals()\nassert __file__ == '<stdin>'\nassert __cached__ is None\nassert sys.path[0] == ''\nassert not sys.flags.interactive\nassert sys.argv == EXPECTED_ARGV\ndef answer():\n    return 4\nprint(answer())\n2 + 2\natexit.register(lambda: print('FINALIZED'))\n";
    for selector in [None, Some("-")] {
        let expected = if selector.is_some() { "['-']" } else { "['']" };
        let mut command = rez();
        command
            .env("HOME", temp.path())
            .env("USERPROFILE", temp.path())
            .arg("python");
        if let Some(selector) = selector {
            command.arg(selector);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(source.replace("EXPECTED_ARGV", expected).as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .collect::<Vec<_>>(),
            ["4", "FINALIZED"]
        );
    }
    let mut child = rez()
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env("PYTHONIOENCODING", "latin-1")
        .arg("python")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"print('caf\xe9')\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = if cfg!(windows) {
        b"caf\xe9\r\n".as_slice()
    } else {
        b"caf\xe9\n".as_slice()
    };
    assert_eq!(output.stdout.as_slice(), expected);
}

#[test]
fn piped_stdin_exceptions_stop_the_program_and_preserve_exit_status() {
    for source in [
        "raise RuntimeError('stop')\nprint('SHOULD_NOT_RUN')\n",
        "if True\n    print('SHOULD_NOT_RUN')\n",
    ] {
        let mut child = rez()
            .arg("python")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(source.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(
            output.stdout.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("<stdin>"));
    }
}

#[test]
fn explicit_inspection_keeps_interactive_semantics_without_history_side_effects() {
    let temp = tempfile::tempdir().unwrap();
    let blocked = temp.path().join("blocked-config");
    std::fs::write(&blocked, b"config path is a file").unwrap();
    for history_root in [temp.path().join("missing-config"), blocked.clone()] {
        for arguments in [
            vec!["python", "-i"],
            vec!["python", "-i", "-c", "value = 3"],
            vec![
                "python",
                "-i",
                "-c",
                "raise RuntimeError('before inspection')",
            ],
        ] {
            let expected_argv = if arguments.contains(&"-c") {
                "['-c']"
            } else {
                "['']"
            };
            let source = format!(
                "import sys; assert sys.flags.interactive; assert sys.argv == {expected_argv}\n2 + 2\nraise SystemExit(7)\n"
            );
            let has_exception = arguments
                .last()
                .is_some_and(|argument| argument.contains("RuntimeError"));
            let mut child = rez()
                .env_remove("TERM")
                .env("HOME", temp.path())
                .env("USERPROFILE", temp.path())
                .env("XDG_CONFIG_HOME", &history_root)
                .env("APPDATA", &history_root)
                .args(arguments)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(source.as_bytes())
                .unwrap();
            let output = child.wait_with_output().unwrap();
            assert_eq!(
                output.status.code(),
                Some(7),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .collect::<Vec<_>>(),
                ["4"]
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(!stderr.contains("panicked"));
            if has_exception {
                assert!(
                    stderr.contains("RuntimeError: before inspection"),
                    "{stderr}"
                );
            }
        }
    }
    assert!(!temp.path().join("missing-config").exists());
    assert_eq!(std::fs::read(blocked).unwrap(), b"config path is a file");
}
