//! Verify buffered output and CRC integrity through the actual embedded CLI.
use std::process::Command;

#[test]
fn embedded_compression_drains_pending_output_and_validates_zip_crc() {
    let output = Command::new(env!("CARGO_BIN_EXE_rez"))
        .env("REZ_DISABLE_HOME_CONFIG", "1")
        .args([
            "python",
            "-c",
            include_str!("fixtures/python_compression.py"),
        ])
        .output()
        .expect("run embedded compression regression");
    assert!(
        output.status.success(),
        "compression failed: status={}; stdout={}; stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "COMPRESSION_OK",
    );
}
