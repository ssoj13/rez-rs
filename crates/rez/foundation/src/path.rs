// SPDX-License-Identifier: Apache-2.0

//! Repository identifier and filesystem path validation.

pub fn is_valid_rez_package_name(name: &str) -> bool {
    if name.is_empty() || name == "__pycache__" {
        return false;
    }
    let mut chars = name.chars();
    if !chars
        .next()
        .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return false;
    }
    let mut needs_segment_char = false;
    for ch in chars {
        if ch == '.' {
            if needs_segment_char {
                return false;
            }
            needs_segment_char = true;
        } else if ch.is_ascii_alphanumeric() || ch == '_' {
            needs_segment_char = false;
        } else {
            return false;
        }
    }
    !needs_segment_char
}

/// Return whether a string can be used as one filesystem path component on the current OS.
pub fn is_safe_rez_path_component(value: &str, allow_empty: bool) -> bool {
    if value.is_empty() {
        return allow_empty;
    }
    value != "."
        && value != ".."
        && !value.contains(['/', '\\'])
        && !(value.as_bytes().get(1) == Some(&b':') && value.as_bytes()[0].is_ascii_alphabetic())
        && (!cfg!(windows) || is_safe_windows_path_component(value))
}

/// Check a relative repository path with the shared OS-aware component rules.
pub fn is_safe_rez_path(value: &str, allow_empty: bool) -> bool {
    if value.is_empty() {
        return allow_empty;
    }
    // Reject backslashes on Unix, where they are literal component characters.
    if !cfg!(windows) && value.contains('\\') {
        return false;
    }
    value
        .split(['/', '\\'])
        .all(|part| is_safe_rez_path_component(part, false))
}

#[cfg(not(windows))]
fn is_safe_windows_path_component(_value: &str) -> bool {
    true
}

#[cfg(windows)]
fn is_safe_windows_path_component(value: &str) -> bool {
    if value
        .chars()
        .any(|ch| ch <= '\u{1f}' || matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        || value.ends_with([' ', '.'])
    {
        return false;
    }

    let device_name = value
        .split('.')
        .next()
        .unwrap_or(value)
        .trim_end_matches([' ', '.'])
        .to_ascii_uppercase();
    if matches!(device_name.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return false;
    }

    let device_suffix = device_name
        .strip_prefix("COM")
        .or_else(|| device_name.strip_prefix("LPT"));
    !device_suffix.is_some_and(|suffix| {
        matches!(
            suffix,
            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
        )
    })
}
