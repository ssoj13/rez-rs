//! A standalone Rez binding must not require an external Python package.

use std::process::Command;

#[test]
fn quickstart_bound_rez_runs_without_host_python() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let executable = root.join(if cfg!(windows) { "rez.exe" } else { "rez" });
    std::fs::copy(env!("CARGO_BIN_EXE_rez"), &executable).unwrap();
    let packages = root.join("packages");
    let command = || {
        let mut cmd = Command::new(&executable);
        cmd.current_dir(root)
            .env_clear()
            .env("HOME", root)
            .env("USERPROFILE", root)
            .env("TMP", root)
            .env("TEMP", root)
            .env("PATH", root)
            .env("REZ_DISABLE_HOME_CONFIG", "1");
        #[cfg(windows)]
        for name in ["SystemRoot", "COMSPEC"] {
            if let Some(value) = std::env::var_os(name) {
                cmd.env(name, value);
            }
        }
        cmd
    };
    let output = command()
        .args(["bind", "--quickstart", "--install-path"])
        .arg(&packages)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!packages.join("python").exists());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Unknown bind module"));

    // An OS shell is needed for rez env, but no host Python directory is present.
    let shell_path = if cfg!(windows) {
        std::path::PathBuf::from(std::env::var_os("COMSPEC").unwrap())
            .parent()
            .unwrap()
            .to_path_buf()
    } else {
        std::path::PathBuf::from("/bin")
    };
    let output = command()
        .env("PATH", shell_path)
        .args(["env", "--paths"])
        .arg(&packages)
        .args([
            "--no-cache",
            "--shell",
            if cfg!(windows) { "cmd" } else { "sh" },
            "rez",
            "--",
            "rez",
            "--version",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout)
        .contains(&format!("rez {}", env!("CARGO_PKG_VERSION"))));
}
