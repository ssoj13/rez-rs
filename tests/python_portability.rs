//! Verify that a relocated CLI carries its Python standard library with it.

use std::process::Command;
use std::sync::Mutex;

const PYTHON_PROBE: &str = r#"
import sys
import os
import subprocess
assert os.path.basename(sys.executable) in ("rez-python", "rez-python.exe")
assert sys._base_executable == sys.executable
assert os.path.samefile(os.path.dirname(os.path.dirname(sys.executable)), sys.argv[1])
child = subprocess.run([sys.executable, "--version"], text=True, capture_output=True)
assert child.returncode == 0 and "Python" in child.stdout, (child.stdout, child.stderr)
child = subprocess.run([sys.executable, "-c", "import json; print(json.dumps({'child': True}))"],
                       text=True, capture_output=True)
assert child.returncode == 0 and child.stdout.strip() == '{"child": true}', (child.stdout, child.stderr)
# No filesystem importer can supply a Python module after this point.
# Frozen metadata may still contain an informational build-time stdlib path.
sys.path[:] = []

import encodings
import json
import os
import pathlib
import hashlib
import ast
import inspect
import linecache
import shutil
import tomllib

for module in (encodings, json, pathlib, ast, inspect, linecache, shutil, tomllib):
    assert module.__spec__.origin == "frozen", (
        module.__name__, module.__spec__.origin, module.__spec__.loader
    )
assert sys.path == [], sys.path
value = {"unicode": "\u2603", "items": [1, True, None]}
assert json.loads(json.dumps(value)) == value
assert pathlib.PurePosixPath("a/b").name == "b"
assert hashlib.sha256(b"rez-rs").hexdigest() == (
    "be670ac65338018bec725d5d19d4fe2c85e71721b5ddab823b630d3352af8768"
)
assert isinstance(ast.parse("value = 1"), ast.Module)
assert tomllib.loads('name = "rez-rs"')["name"] == "rez-rs"
print("FROZEN_STDLIB_OK")
"#;

fn run_portable_python(poison_homes: bool, home_environment: bool) {
    // Serialize this file's tests: on Linux a child forked by one test can inherit
    // another test's still-open write handle to its freshly copied binary, and exec
    // of that binary then fails with ETXTBSY ("Text file busy").
    static SERIAL: Mutex<()> = Mutex::new(());
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().expect("create isolated executable directory");
    let executable = temp
        .path()
        .join(if cfg!(windows) { "rez.exe" } else { "rez" });
    std::fs::copy(env!("CARGO_BIN_EXE_rez"), &executable).expect("relocate CLI executable");
    let empty_path = temp.path().join("empty-path");
    std::fs::create_dir(&empty_path).expect("create PATH without Python");
    let mut command = Command::new(&executable);
    command
        .current_dir(temp.path())
        .env_clear()
        .env("PATH", &empty_path)
        .env("TMP", temp.path())
        .env("TEMP", temp.path())
        .env("REZ_DISABLE_HOME_CONFIG", "1")
        .args(["python", "-c", PYTHON_PROBE]);
    let home = if home_environment {
        command
            .env("HOME", temp.path())
            .env("USERPROFILE", temp.path());
        temp.path().to_path_buf()
    } else {
        dirs::home_dir().expect("native account home without child environment overrides")
    };
    command.arg(home.join(".rez").join("python"));
    // Windows system DLL loading still needs the operating system directory.
    #[cfg(windows)]
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", system_root);
    }
    if poison_homes {
        let absent_home = temp.path().join("python-does-not-exist");
        assert!(!absent_home.exists());
        command
            .env("PYTHONHOME", &absent_home)
            .env("RUSTPYTHONHOME", &absent_home)
            .env("PYTHONPATH", &absent_home)
            .env("RUSTPYTHONPATH", &absent_home);
    }
    let output = command.output().expect("execute relocated CLI");
    assert!(
        output.status.success(),
        "relocated Python failed (poison_homes={poison_homes}): status={}; stdout={}; stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout)
            .expect("UTF-8 Python probe output")
            .trim(),
        "FROZEN_STDLIB_OK"
    );
}

#[test]
fn relocated_python_runs_without_host_python_or_stdlib_paths() {
    run_portable_python(false, true);
}

#[test]
fn relocated_python_ignores_unrelated_host_python_homes() {
    run_portable_python(true, true);
}

#[test]
fn relocated_python_discovers_native_account_without_home_environment() {
    run_portable_python(true, false);
}
