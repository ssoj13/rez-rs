//! CLI utility functions shared across multiple command modules.

use std::env;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use foundation::errors::{Result, RezError};
use model::config::CONFIG;

/// Expand ~ prefix in a path string to the user's home directory.
/// Returns the input unchanged if ~ is not present or home is unavailable.
pub fn expand_tilde(s: &str) -> String {
    if let Some(rest) = s.strip_prefix('~') {
        if let Ok(home) = env::var("HOME").or_else(|_| env::var("USERPROFILE")) {
            return format!("{home}{rest}");
        }
    }
    s.to_string()
}

/// Expand ~ prefix in a Path to the user's home directory.
pub fn expand_tilde_path(path: &std::path::Path) -> PathBuf {
    let s = path.to_string_lossy();
    PathBuf::from(expand_tilde(&s))
}

/// Parse OS PATH-style string (colon/semicolon-separated) into Vec<PathBuf>.
/// Returns None if the input is None, Some(empty) if the string is empty or contains only empty paths.
pub fn parse_path_list(paths_str: Option<&str>) -> Option<Vec<PathBuf>> {
    paths_str.map(|s| {
        env::split_paths(s)
            .filter(|p| !p.as_os_str().is_empty())
            .collect()
    })
}

/// Render a DOT graph with Graphviz and open it using the configured or platform viewer.
pub fn view_graph(dot: &str) -> Result<PathBuf> {
    let format = CONFIG.dot_image_format.trim();
    if format.is_empty() || !format.chars().all(|ch| ch.is_ascii_alphanumeric()) {
        return Err(RezError::Config(format!(
            "invalid Graphviz image format: {format:?}"
        )));
    }

    let image = tempfile::Builder::new()
        .prefix("rez-resolve-")
        .suffix(&format!(".{format}"))
        .tempfile()?;
    let image_path = image.path().to_path_buf();
    let mut renderer = Command::new("dot")
        .arg(format!("-T{format}"))
        .arg("-o")
        .arg(&image_path)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| RezError::System(format!("failed to start Graphviz `dot`: {error}")))?;
    renderer
        .stdin
        .take()
        .ok_or_else(|| RezError::System("Graphviz stdin was not available".into()))?
        .write_all(dot.as_bytes())?;
    let output = renderer.wait_with_output()?;
    if !output.status.success() {
        return Err(RezError::System(format!(
            "Graphviz failed to render the resolve graph: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    let viewer = CONFIG
        .image_viewer
        .as_deref()
        .filter(|viewer| !viewer.trim().is_empty());
    let detected_viewer = model::platform::detect_image_viewer();
    let viewers = viewer
        .into_iter()
        .chain(detected_viewer.as_deref())
        .collect::<Vec<_>>();
    let mut last_error = None;
    for viewer in viewers {
        match Command::new(viewer).arg(&image_path).status() {
            Ok(status) if status.success() => {
                let (_, path) = image.keep().map_err(|error| RezError::Io(error.error))?;
                return Ok(path);
            }
            Ok(status) => {
                last_error = Some(format!("image viewer `{viewer}` exited with {status}"));
            }
            Err(error) => {
                last_error = Some(format!("failed to start image viewer `{viewer}`: {error}"));
            }
        }
    }

    let (_, image_path) = image.keep().map_err(|error| RezError::Io(error.error))?;
    Err(RezError::System(format!(
        "could not open resolve graph image {}; {}",
        image_path.display(),
        last_error.unwrap_or_else(|| "no image viewer was detected".into())
    )))
}
