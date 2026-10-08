#[test]
fn test_discover_sys_paths() {
    // Test that discover_sys_paths returns non-empty paths
    let paths = repository::package::bind::discover_sys_paths();
    assert!(!paths.is_empty(), "System paths should not be empty");
    // On Windows, should include System32
    if cfg!(windows) {
        let has_system32 = paths.iter().any(|p| {
            let lower = p.to_lowercase();
            lower.contains("system32") || lower.contains("windows")
        });
        assert!(
            has_system32,
            "Windows system paths should include System32: {:?}",
            paths
        );
    }
    // On Unix, should include /usr/bin or /bin
    if !cfg!(windows) {
        let has_usr_bin = paths.iter().any(|p| p == "/usr/bin" || p == "/bin");
        assert!(
            has_usr_bin,
            "Unix system paths should include /usr/bin: {:?}",
            paths
        );
    }
}
