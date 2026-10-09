//! Native legacy entry points use the same executable and dispatcher.
use std::collections::BTreeMap;
use std::process::Command;
use std::sync::{Mutex, MutexGuard};

/// Serializes this file's tests: on Linux a child forked by one test can inherit
/// another test's still-open write handle to a freshly copied binary, and exec of
/// that binary then fails with ETXTBSY ("Text file busy").
fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn isolated(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    command
        .env("REZ_DISABLE_HOME_CONFIG", "1")
        .env_remove("REZ_CONFIG_FILE");
    command
}

fn metadata() -> BTreeMap<String, String> {
    let output = isolated(env!("CARGO_BIN_EXE_rez"))
        .arg("--list-cli-aliases")
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    serde_json::from_slice(&output.stdout).unwrap()
}

fn alias(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    let name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    let path = root.join(name);
    std::fs::copy(env!("CARGO_BIN_EXE_rez"), &path).unwrap();
    path
}

#[test]
fn every_metadata_entry_point_dispatches_its_own_help() {
    let _serial = serial();
    let temp = tempfile::tempdir().unwrap();
    for (name, target) in metadata() {
        if target == "python" {
            continue;
        }
        let path = alias(temp.path(), &name);
        let output = isolated(&path).arg("--help").output().unwrap();
        assert!(output.status.success(), "{name}: {:?}", output);
        let canonical = if target.is_empty() {
            isolated(env!("CARGO_BIN_EXE_rez"))
                .arg("--help")
                .output()
                .unwrap()
        } else {
            isolated(env!("CARGO_BIN_EXE_rez"))
                .args([target.as_str(), "--help"])
                .output()
                .unwrap()
        };
        assert!(canonical.status.success(), "{target}: {:?}", canonical);
        assert_eq!(output.stdout, canonical.stdout, "{name}");
    }
}

#[test]
fn reference_entry_point_names_are_present() {
    let _serial = serial();
    let names = metadata();
    for name in [
        "rezolve",
        "_rez-complete",
        "_rez_fwd",
        "rez-bind",
        "rez-build",
        "rez-config",
        "rez-context",
        "rez-cp",
        "rez-depends",
        "rez-diff",
        "rez-env",
        "rez-help",
        "rez-interpret",
        "rez-memcache",
        "rez-pip",
        "rez-pkg-cache",
        "rez-plugins",
        "rez-python",
        "rez-release",
        "rez-search",
        "rez-selftest",
        "rez-status",
        "rez-suite",
        "rez-test",
        "rez-view",
        "rez-yaml2py",
        "rez-bundle",
        "rez-benchmark",
        "rez-pkg-ignore",
        "rez-mv",
        "rez-rm",
    ] {
        assert!(
            names.contains_key(name),
            "missing reference entry point {name}"
        );
    }
    #[cfg(feature = "gui")]
    assert!(names.contains_key("rez-gui"));
}

#[test]
fn python_alias_preserves_native_options_arguments_and_reentry() {
    let _serial = serial();
    let temp = tempfile::tempdir().unwrap();
    let path = alias(temp.path(), "rez-python");
    let output = isolated(&path)
        .current_dir(temp.path())
        .args([
            "-I", "-S", "-c",
            "import subprocess,sys; assert sys.flags.isolated; assert sys.flags.no_site; assert sys.argv == ['-c','--version','space argument','-3']; child=subprocess.run([sys.executable,'-c','raise SystemExit(7)']); assert child.returncode == 7; print('ALIAS_PYTHON_OK')",
            "--version", "space argument", "-3",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "ALIAS_PYTHON_OK"
    );
}
