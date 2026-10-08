// SPDX-License-Identifier: Apache-2.0
//! Process-level configuration acceptance: policy errors must stop the CLI.

use std::process::Command;

fn command(config: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rez"));
    command.env_clear();
    for name in [
        "PATH",
        "SYSTEMROOT",
        "WINDIR",
        "TEMP",
        "TMP",
        "HOME",
        "USERPROFILE",
        "LOCALAPPDATA",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.env("REZ_CONFIG_FILE", config);
    command.env("REZ_DISABLE_HOME_CONFIG", "true");
    command
}

#[test]
fn recipe_settings_are_exposed_and_environment_overrides_file_defaults() {
    let temporary = tempfile::tempdir().unwrap();
    let config = temporary.path().join("rezconfig.toml");
    std::fs::write(
        &config,
        "sources_path = '/mirror'\nwheel_cache_path = '/wheels'\nuser_path = '/user'\nrepo_path = '/repository'\noffline = false\nlog_level = 'WARNING'\n",
    ).unwrap();
    let output = command(&config)
        .env("REZ_OFFLINE", "TrUe")
        .env("REZ_LOG_LEVEL", "ERROR")
        .args(["config", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let values: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(values["offline"], true);
    assert_eq!(values["log_level"], "ERROR");
    for key in ["sources_path", "wheel_cache_path", "user_path", "repo_path"] {
        assert!(values[key].is_string(), "{key}");
    }
}

#[test]
fn invalid_offline_environment_is_fatal_before_command_execution() {
    let temporary = tempfile::tempdir().unwrap();
    let config = temporary.path().join("rezconfig.toml");
    std::fs::write(&config, "offline = false\n").unwrap();
    for value in ["1", "0", "yes", "invalid", ""] {
        let output = command(&config)
            .env("REZ_OFFLINE", value)
            .args(["config", "offline"])
            .output()
            .unwrap();
        assert!(!output.status.success(), "accepted {value:?}");
        assert!(output.stdout.is_empty(), "executed command with {value:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("REZ_OFFLINE"));
    }
}

#[test]
fn invalid_offline_file_is_fatal_instead_of_falling_back_online() {
    let temporary = tempfile::tempdir().unwrap();
    let config = temporary.path().join("rezconfig.toml");
    std::fs::write(&config, "offline = 'invalid'\n").unwrap();
    let output = command(&config)
        .args(["config", "offline"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("offline"));
}
