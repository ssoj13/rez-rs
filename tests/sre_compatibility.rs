//! CPython-stamped SRE behavior, including Bootstrap117's platform-map patterns.

use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn embedded_sre_matches_cpython_prefix_capture_corpus() {
    let directory = tempfile::tempdir().expect("create isolated regex probe directory");
    let probe = directory.path().join("probe.py");
    std::fs::write(&probe, include_str!("fixtures/sre/probe.py")).expect("write regex probe");
    let mut command = Command::new(env!("CARGO_BIN_EXE_rez"));
    command
        .args(["python"])
        .arg(&probe)
        .current_dir(directory.path())
        .env("REZ_DISABLE_HOME_CONFIG", "1")
        .env("PYTHONHOME", directory.path().join("absent-python-home"))
        .env(
            "RUSTPYTHONHOME",
            directory.path().join("absent-rustpython-home"),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("start embedded regex probe");
    child
        .stdin
        .take()
        .expect("probe input")
        .write_all(include_bytes!("fixtures/sre/cpython.json"))
        .expect("write CPython regex corpus");
    let output = child
        .wait_with_output()
        .expect("collect regex probe result");
    assert!(
        output.status.success(),
        "SRE differential probe failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // Python owns decoding the oracle: its strings deliberately include lone
    // surrogates, which cannot be represented by serde_json's Rust strings.
    // The probe emits this summary only after every case agrees with CPython.
    let stdout = std::str::from_utf8(&output.stdout).expect("UTF-8 regex probe summary");
    let summary = stdout
        .trim()
        .strip_prefix("SRE_DIFFERENTIAL_OK ")
        .unwrap_or_else(|| panic!("unexpected regex probe output: {stdout}"));
    let fields: Vec<_> = summary.split_whitespace().collect();
    assert_eq!(fields.len(), 2, "unexpected regex probe summary: {summary}");
    let count: usize = fields[0].parse().expect("verified regex case count");
    assert!(count > 0, "regex corpus must not be empty");
    let version: Vec<_> = fields[1].split('.').collect();
    assert_eq!(version.len(), 3, "unexpected CPython oracle version");
    assert!(
        version.iter().all(|part| part.parse::<u32>().is_ok()),
        "unexpected CPython oracle version: {}",
        fields[1]
    );
}
