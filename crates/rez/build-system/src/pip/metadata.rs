// SPDX-License-Identifier: Apache-2.0

//! Read distribution metadata and map only verified, contained payload files.

use crate::config::RezConfig;
use crate::errors::{Result, RezError};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Default, Serialize)]
pub(crate) struct Distribution {
    pub name: String,
    pub version: String,
    pub summary: String,
    pub author: String,
    pub author_email: String,
    pub home_page: String,
    pub download_url: String,
    pub requires_dist: Vec<String>,
    pub requires_python: String,
    pub wheel_tags: Vec<String>,
    #[serde(skip)]
    pub directory: PathBuf,
    #[serde(skip)]
    pub pure: bool,
}

impl Distribution {
    /// Rewrite installed metadata after all payload and launcher transformations.
    pub(crate) fn finalize(
        &self,
        mapping: &BTreeMap<PathBuf, PathBuf>,
        source: &Path,
        destination: &Path,
        generated: &[PathBuf],
    ) -> Result<()> {
        use base64::Engine;
        use std::collections::BTreeSet;
        use std::io::Write;
        let info_source = self
            .directory
            .strip_prefix(source)
            .map_err(|_| RezError::Build("Distribution metadata lies outside its source".into()))?;
        let metadata = mapping.get(&info_source.join("METADATA")).ok_or_else(|| {
            RezError::Build(format!("Missing installed METADATA for {}", self.name))
        })?;
        safe_relative(metadata)?;
        let info = metadata
            .parent()
            .ok_or_else(|| RezError::Build("Missing dist-info directory".into()))?;
        let library = info.parent().unwrap_or_else(|| Path::new(""));
        let record = info.join("RECORD");
        if let Some(mapped) = mapping.get(&info_source.join("RECORD")) {
            if mapped != &record {
                return Err(RezError::Build(
                    "RECORD and METADATA map to different directories".into(),
                ));
            }
        }
        let mut files = mapping
            .values()
            .cloned()
            .chain(generated.iter().cloned())
            .collect::<BTreeSet<_>>();
        files.insert(record.clone());
        let quote = |value: &str| {
            if value.contains([',', '"', '\r', '\n']) {
                format!("\"{}\"", value.replace('"', "\"\""))
            } else {
                value.to_owned()
            }
        };
        let mut text = String::new();
        for relative in files {
            safe_relative(&relative)?;
            let path = pathdiff::diff_paths(&relative, library)
                .ok_or_else(|| RezError::Build("Cannot relativize installed RECORD path".into()))?;
            let components = path
                .components()
                .map(|component| {
                    component.as_os_str().to_str().ok_or_else(|| {
                        RezError::Build("Installed RECORD paths must be UTF-8".into())
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let name = components.join("/");
            if relative == record {
                text.push_str(&format!("{},,\n", quote(&name)));
                continue;
            }
            let parent = crate::util::directory(
                destination,
                relative.parent().unwrap_or_else(|| Path::new("")),
                false,
            )?;
            let path = parent.join(
                relative
                    .file_name()
                    .ok_or_else(|| RezError::Build("Missing installed file name".into()))?,
            );
            let mut file = crate::util::open_file(&path, fs::OpenOptions::new().read(true))?;
            let size = file.metadata()?.len();
            let digest = crate::util::hash_reader::<sha2::Sha256>(&mut file)?;
            let hash = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
            text.push_str(&format!("{},sha256={},{}\n", quote(&name), hash, size));
        }
        let directory = crate::util::directory(destination, info, false)?;
        let mut staged = tempfile::NamedTempFile::new_in(&directory)?;
        staged.write_all(text.as_bytes())?;
        staged.flush()?;
        let permissions = fs::symlink_metadata(directory.join("RECORD"))
            .or_else(|_| fs::symlink_metadata(destination.join(metadata)))?
            .permissions();
        staged.as_file().set_permissions(permissions)?;
        staged.persist(directory.join("RECORD")).map_err(|error| {
            RezError::Build(format!("Cannot publish installed RECORD: {error}"))
        })?;
        Ok(())
    }
}

pub(super) fn headers(text: &str) -> Result<Vec<(String, String)>> {
    let mut values: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            let (_, value) = values.last_mut().ok_or_else(|| {
                RezError::Build("Metadata starts with a continuation line".into())
            })?;
            value.push(' ');
            value.push_str(line.trim());
        } else {
            let (key, value) = line.split_once(':').ok_or_else(|| {
                RezError::Build(format!("Malformed distribution metadata header {line:?}"))
            })?;
            values.push((key.to_ascii_lowercase(), value.trim_start().into()));
        }
    }
    Ok(values)
}

pub(crate) fn distributions(root: &Path) -> Result<Vec<Distribution>> {
    let mut directories = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().ends_with(".dist-info") {
            if !entry.file_type()?.is_dir() {
                return Err(RezError::Build("Invalid dist-info directory".into()));
            }
            directories.push(entry.path());
        }
    }
    directories.sort();
    let mut result = Vec::new();
    for directory in directories {
        let mut item = Distribution {
            directory: directory.clone(),
            ..Distribution::default()
        };
        for (key, value) in headers(&fs::read_to_string(directory.join("METADATA"))?)? {
            match key.as_str() {
                "name" => item.name = value,
                "version" => item.version = value,
                "summary" => item.summary = value,
                "author" => item.author = value,
                "author-email" => item.author_email = value,
                "home-page" => item.home_page = value,
                "download-url" => item.download_url = value,
                "requires-dist" => item.requires_dist.push(value),
                "requires-python" => item.requires_python = value,
                _ => {}
            }
        }
        if item.name.is_empty() || item.version.is_empty() {
            return Err(RezError::Build(format!(
                "Missing Name/Version in {}",
                directory.display()
            )));
        }
        let wheel = headers(&fs::read_to_string(directory.join("WHEEL"))?)?;
        item.pure = wheel
            .iter()
            .any(|(key, value)| key == "root-is-purelib" && value.eq_ignore_ascii_case("true"));
        item.wheel_tags = wheel
            .into_iter()
            .filter_map(|(key, value)| (key == "tag").then_some(value))
            .collect();
        result.push(item);
    }
    Ok(result)
}

pub(super) fn safe_relative(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() {
        return Err(RezError::Build("Empty payload path".into()));
    }
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                let value = value
                    .to_str()
                    .ok_or_else(|| RezError::Build("Non-Unicode payload path".into()))?;
                if !foundation::path::is_safe_rez_path_component(value, false) {
                    return Err(RezError::Build(format!(
                        "Unsafe payload component {value:?}"
                    )));
                }
            }
            _ => {
                return Err(RezError::Build(format!(
                    "Unsafe payload path {}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

fn csv(text: &str) -> Result<Vec<Vec<String>>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut ended_quote = false;
    let mut chars = text.chars().peekable();
    while let Some(value) = chars.next() {
        match value {
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                field.push('"');
            }
            '"' if quoted => {
                quoted = false;
                ended_quote = true;
            }
            '"' if field.is_empty() && !ended_quote => quoted = true,
            ',' if !quoted => {
                row.push(std::mem::take(&mut field));
                ended_quote = false;
            }
            '\n' if !quoted => {
                row.push(std::mem::take(&mut field));
                if row.iter().any(|value| !value.is_empty()) {
                    rows.push(std::mem::take(&mut row));
                } else {
                    row.clear();
                }
                ended_quote = false;
            }
            '\r' if !quoted && chars.peek() == Some(&'\n') => {}
            _ if !quoted && ended_quote => {
                return Err(RezError::Build("Malformed quoted RECORD field".into()));
            }
            _ => field.push(value),
        }
    }
    if quoted {
        return Err(RezError::Build("Unterminated quoted RECORD field".into()));
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    Ok(rows)
}

fn substitution(value: &str) -> String {
    let regex = regex::Regex::new(r"\\([0-9]+)").expect("static substitution regex");
    regex
        .replace_all(value, |captures: &regex::Captures<'_>| {
            format!("${{{}}}", &captures[1])
        })
        .replace("{s}", "/")
        .replace("{p}", "..")
}

pub(crate) fn mapping(
    item: &Distribution,
    root: &Path,
    config: &RezConfig,
    library: Option<&Path>,
) -> Result<BTreeMap<PathBuf, PathBuf>> {
    let root = fs::canonicalize(root)?;
    let mut mapping = BTreeMap::new();
    let mut destinations = HashMap::new();
    for row in csv(&fs::read_to_string(item.directory.join("RECORD"))?)? {
        if row.len() != 3 {
            return Err(RezError::Build(
                "RECORD row must contain path, hash and size".into(),
            ));
        }
        let original = row[0].replace('\\', "/");
        let mut source = original.clone();
        let mut destination = if original.starts_with("bin/") || original.starts_with("Scripts/") {
            format!("bin/{}", original.split_once('/').unwrap().1)
        } else {
            library
                .unwrap_or_else(|| Path::new("python"))
                .join(&original)
                .to_string_lossy()
                .into_owned()
        };
        if original.starts_with("../") {
            let mut matched = false;
            for remap in &config.pip_install_remaps {
                let get = |key| {
                    remap
                        .get(key)
                        .ok_or_else(|| RezError::Config(format!("Missing pip remap {key}")))
                };
                let pattern = get("record_path")?
                    .replace("{p}", r"\.\.")
                    .replace("{s}", "/");
                let regex = regex::Regex::new(&pattern)?;
                if regex.is_match(&original) {
                    source = regex
                        .replace(&original, substitution(get("pip_install")?).as_str())
                        .into_owned();
                    destination = regex
                        .replace(&original, substitution(get("rez_install")?).as_str())
                        .into_owned();
                    matched = true;
                    break;
                }
            }
            if !matched {
                return Err(RezError::Build(format!(
                    "No pip_install_remaps rule for RECORD path {original:?}"
                )));
            }
        }
        let source = PathBuf::from(source);
        let destination = PathBuf::from(destination);
        safe_relative(&source)?;
        safe_relative(&destination)?;
        let absolute = root.join(&source);
        if !absolute.exists() {
            eprintln!("Skipping missing RECORD file {}", absolute.display());
            continue;
        }
        let actual = fs::canonicalize(&absolute)?;
        if !actual.starts_with(&root) || !actual.is_file() {
            return Err(RezError::Build(format!(
                "RECORD file escapes pip target: {}",
                absolute.display()
            )));
        }
        if let Some(previous) = destinations.insert(destination.clone(), source.clone()) {
            if previous != source {
                return Err(RezError::Build(format!(
                    "Duplicate pip payload destination {}",
                    destination.display()
                )));
            }
        }
        mapping.insert(source, destination);
    }
    Ok(mapping)
}

pub(crate) fn copy(
    mapping: &BTreeMap<PathBuf, PathBuf>,
    source: &Path,
    destination: &Path,
) -> Result<()> {
    for (from, to) in mapping {
        safe_relative(from)?;
        safe_relative(to)?;
        let parent = to
            .parent()
            .ok_or_else(|| RezError::Build("Payload has no parent".into()))?;
        let directory = crate::util::directory(destination, parent, true)?;
        let target = directory.join(
            to.file_name()
                .ok_or_else(|| RezError::Build("Payload has no filename".into()))?,
        );
        foundation::filesystem::copy_file(&source.join(from), &target, true)?;
    }
    Ok(())
}

pub(crate) fn parse_entry_points(dist_info_dir: &Path) -> Result<Vec<(String, String)>> {
    let ep_path = dist_info_dir.join("entry_points.txt");
    if !ep_path.exists() {
        return Ok(vec![]);
    }
    let content = fs::read_to_string(&ep_path)
        .map_err(|e| RezError::Build(format!("Failed to read {}: {}", ep_path.display(), e)))?;

    let mut result = Vec::new();
    let mut in_console = false;
    let mut in_gui = false;

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_console = line.eq_ignore_ascii_case("[console_scripts]");
            in_gui = line.eq_ignore_ascii_case("[gui_scripts]");
            continue;
        }
        if (in_console || in_gui) && !line.is_empty() && !line.starts_with('#') {
            if let Some((name, spec)) = line.split_once('=') {
                let name = name.trim().to_string();
                safe_relative(Path::new(&name))?;
                if !name
                    .chars()
                    .all(|value| value.is_ascii_alphanumeric() || "_-.".contains(value))
                {
                    return Err(RezError::Build(format!("Invalid entry point name {name}")));
                }
                let spec = spec.trim().to_string();
                if !name.is_empty() && !spec.is_empty() && spec.contains(':') {
                    result.push((name, spec));
                }
            }
        }
    }
    Ok(result)
}

/// Generate launcher scripts for entry points and write to bin_dir.
/// Returns the actual generated files for installed RECORD ownership.
pub(super) fn generate_entry_point_launchers(
    entry_points: &[(String, String)],
    bin_dir: &Path,
    portable: bool,
) -> Result<Vec<PathBuf>> {
    if entry_points.is_empty() {
        return Ok(vec![]);
    }
    fs::create_dir_all(bin_dir)?;

    let identifier = regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*(\.[A-Za-z_][A-Za-z0-9_]*)*$")?;
    let mut tools = Vec::new();
    for (name, spec) in entry_points {
        let (module, attr) = match spec.split_once(':') {
            Some(p) => (p.0.trim(), p.1.trim()),
            None => continue,
        };

        let attr = attr.split('[').next().unwrap_or(attr).trim();
        if !identifier.is_match(module) || !identifier.is_match(attr) {
            return Err(RezError::Build(format!(
                "Invalid entry point {name}={spec}"
            )));
        }
        let py_content = format!(
            "# -*- coding: utf-8 -*-\nimport sys\nif __name__ == \"__main__\":\n    if sys.path and not sys.flags.isolated and not getattr(sys.flags, \"safe_path\", False):\n        sys.path.pop(0)\n    import importlib\n    entry = importlib.import_module({})\n    for part in {}.split('.'):\n        entry = getattr(entry, part)\n    sys.exit(entry())\n",
            serde_json::to_string(module)?,
            serde_json::to_string(attr)?,
        );

        if portable || cfg!(target_os = "windows") {
            let py_path = bin_dir.join(format!("{}.py", name));
            let cmd_content = format!("@\"python\" \"%~dp0{}.py\" %*\r\n", name,);
            fs::write(&py_path, &py_content).map_err(|e| {
                RezError::Build(format!("Failed to write {}: {}", py_path.display(), e))
            })?;
            let cmd_path = bin_dir.join(format!("{}.cmd", name));
            fs::write(&cmd_path, cmd_content).map_err(|e| {
                RezError::Build(format!("Failed to write {}: {}", cmd_path.display(), e))
            })?;
            tools.extend([py_path, cmd_path]);
        }
        if portable || !cfg!(target_os = "windows") {
            let script_content = format!("#!/usr/bin/env python3\n{}", py_content);
            let script_path = bin_dir.join(name);
            fs::write(&script_path, script_content).map_err(|e| {
                RezError::Build(format!("Failed to write {}: {}", script_path.display(), e))
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = fs::metadata(&script_path)?.permissions();
                perms.set_mode(0o755);
                fs::set_permissions(&script_path, perms)?;
            }
            tools.push(script_path);
        }
    }
    Ok(tools)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn folded_metadata_and_quoted_record() {
        assert_eq!(
            headers("Requires-Dist: foo>=1;\n python_version >= '3'\n\n").unwrap()[0].1,
            "foo>=1; python_version >= '3'"
        );
        assert_eq!(
            csv("\"pkg/a,b.py\",sha256=x,12\n").unwrap()[0][0],
            "pkg/a,b.py"
        );
        assert!(csv("\"broken,x,1").is_err());
    }
    #[test]
    fn installed_record_tracks_only_owned_final_bytes_and_generated_launchers() {
        use base64::Engine;
        use sha2::Digest;
        let source = tempfile::tempdir().unwrap();
        let payload = tempfile::tempdir().unwrap();
        let info = PathBuf::from("fixture-1.dist-info");
        let library = PathBuf::from("site-packages");
        let mut mapping = BTreeMap::new();
        for (name, contents) in [
            ("METADATA", "Name: fixture\nVersion: 1\n"),
            ("WHEEL", "Root-Is-Purelib: true\n"),
            ("RECORD", "stale.py,sha256=wrong,0\n"),
            ("direct_url.json", "{\"url\":\"file:///original-checkout\"}"),
        ] {
            let from = info.join(name);
            let to = library.join(&from);
            let parent = payload.path().join(to.parent().unwrap());
            fs::create_dir_all(parent).unwrap();
            fs::write(payload.path().join(&to), contents).unwrap();
            mapping.insert(from, to);
        }
        let module = PathBuf::from("pkg/a,b.py");
        fs::create_dir_all(payload.path().join("site-packages/pkg")).unwrap();
        fs::write(payload.path().join(library.join(&module)), b"abc").unwrap();
        mapping.insert(module.clone(), library.join(&module));
        fs::create_dir(payload.path().join("bin")).unwrap();
        fs::write(payload.path().join("bin/original"), "#!/logical/python\n").unwrap();
        mapping.insert("Scripts/original".into(), "bin/original".into());
        fs::write(
            payload.path().join("site-packages/unrelated.py"),
            "not owned",
        )
        .unwrap();
        let generated = generate_entry_point_launchers(
            &[("fixture".into(), "fixture:main".into())],
            &payload.path().join("bin"),
            true,
        )
        .unwrap()
        .iter()
        .map(|path| path.strip_prefix(payload.path()).unwrap().to_path_buf())
        .collect::<Vec<_>>();
        let distribution = Distribution {
            directory: source.path().join(&info),
            ..Default::default()
        };
        distribution
            .finalize(&mapping, source.path(), payload.path(), &generated)
            .unwrap();
        let record = payload.path().join(library.join(&info).join("RECORD"));
        let rows = csv(&fs::read_to_string(&record).unwrap()).unwrap();
        assert_eq!(rows.len(), mapping.len() + generated.len());
        assert!(rows.iter().all(|row| row.len() == 3
            && !row[0].contains("stale.py")
            && !row[0].contains("unrelated.py")));
        assert!(rows.iter().any(|row| row
            == &vec![
                "fixture-1.dist-info/RECORD".to_string(),
                String::new(),
                String::new()
            ]));
        assert!(rows.iter().any(|row| row[0] == "../bin/original"));
        for row in rows.iter().filter(|row| !row[1].is_empty()) {
            let path = payload.path().join(&library).join(&row[0]);
            let bytes = fs::read(path).unwrap();
            let digest = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(sha2::Sha256::digest(&bytes));
            assert_eq!(row[1], format!("sha256={digest}"));
            assert_eq!(row[2], bytes.len().to_string());
        }
        let changed = payload.path().join(library.join(&module));
        fs::write(&changed, "transformed final bytes").unwrap();
        distribution
            .finalize(&mapping, source.path(), payload.path(), &generated)
            .unwrap();
        let rows = csv(&fs::read_to_string(record).unwrap()).unwrap();
        let row = rows.iter().find(|row| row[0] == "pkg/a,b.py").unwrap();
        assert_eq!(row[2], "23");
    }

    #[cfg(unix)]
    #[test]
    fn installed_record_quotes_multiline_native_paths_and_rejects_redirects() {
        let source = tempfile::tempdir().unwrap();
        let payload = tempfile::tempdir().unwrap();
        let info = PathBuf::from("fixture.dist-info");
        fs::create_dir(payload.path().join(&info)).unwrap();
        fs::write(
            payload.path().join(info.join("METADATA")),
            "Name: fixture\n",
        )
        .unwrap();
        let name = PathBuf::from("line\n\"quoted\",.py");
        fs::write(payload.path().join(&name), "abc").unwrap();
        let mapping = BTreeMap::from([
            (info.join("METADATA"), info.join("METADATA")),
            (name.clone(), name.clone()),
        ]);
        let distribution = Distribution {
            directory: source.path().join(&info),
            ..Default::default()
        };
        distribution
            .finalize(&mapping, source.path(), payload.path(), &[])
            .unwrap();
        let rows =
            csv(&fs::read_to_string(payload.path().join(info.join("RECORD"))).unwrap()).unwrap();
        assert!(rows.iter().any(|row| row[0] == name.to_str().unwrap()));
        fs::remove_file(payload.path().join(&name)).unwrap();
        std::os::unix::fs::symlink(source.path().join("foreign"), payload.path().join(&name))
            .unwrap();
        fs::write(source.path().join("foreign"), "valuable").unwrap();
        assert!(distribution
            .finalize(&mapping, source.path(), payload.path(), &[])
            .is_err());
        assert_eq!(
            fs::read_to_string(source.path().join("foreign")).unwrap(),
            "valuable"
        );
    }

    #[test]
    fn portable_launchers_and_distribution_headers_preserve_payload_contract() {
        let temp = tempfile::tempdir().unwrap();
        let info = temp.path().join("fixture-1.dist-info");
        fs::create_dir(&info).unwrap();
        fs::write(
            info.join("METADATA"),
            "Name: fixture\nVersion: 1\nRequires-Python: >=3.8\n",
        )
        .unwrap();
        fs::write(
            info.join("WHEEL"),
            "Root-Is-Purelib: true\nTag: py2-none-any\nTag: py3-none-any\n",
        )
        .unwrap();
        let distributions = distributions(temp.path()).unwrap();
        assert_eq!(distributions[0].requires_python, ">=3.8");
        assert_eq!(
            distributions[0].wheel_tags,
            ["py2-none-any", "py3-none-any"]
        );
        let bin = temp.path().join("bin");
        assert_eq!(
            generate_entry_point_launchers(
                &[("fixture".into(), "fixture:main".into())],
                &bin,
                true
            )
            .unwrap(),
            [
                bin.join("fixture.py"),
                bin.join("fixture.cmd"),
                bin.join("fixture")
            ]
        );
        assert!(fs::read_to_string(bin.join("fixture"))
            .unwrap()
            .starts_with("#!/usr/bin/env python3"));
        assert!(fs::read_to_string(bin.join("fixture.cmd"))
            .unwrap()
            .contains("%~dp0fixture.py"));
        assert!(bin.join("fixture.py").is_file());
        let generated = fs::read_to_string(bin.join("fixture.py")).unwrap();
        let probe = temp.path().join("probe.py");
        let prefix = r#"import sys,importlib,types
expected=list(sys.path)
protected=sys.flags.isolated or getattr(sys.flags,"safe_path",False)
def verify(name):
    assert name == "fixture", name
    assert sys.path == (expected if protected else expected[1:]), (sys.path,expected,protected)
    return types.SimpleNamespace(main=lambda: 0)
importlib.import_module=verify
"#;
        fs::write(&probe, format!("{prefix}{generated}")).unwrap();
        let executable = std::env::var("REZ_TEST_PIP_PYTHON").unwrap_or_else(|_| "python".into());
        let has_safe_path = std::process::Command::new(&executable)
            .args(["-c", "import sys;print(hasattr(sys.flags,'safe_path'))"])
            .output()
            .unwrap();
        assert!(has_safe_path.status.success());
        let mut modes = vec![vec![], vec!["-I"]];
        if String::from_utf8_lossy(&has_safe_path.stdout).trim() == "True" {
            modes.push(vec!["-P"]);
        }
        for mode in modes {
            let output = std::process::Command::new(&executable)
                .args(mode)
                .arg(&probe)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let output = std::process::Command::new(&executable)
            .args(["-c", "import sys;from pathlib import Path;expected=list(sys.path);exec(compile(Path(sys.argv[1]).read_text(),sys.argv[1],'exec'),{'__name__':'imported'});assert sys.path==expected"])
            .arg(bin.join("fixture.py")).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn payload_paths_must_be_contained() {
        assert!(safe_relative(Path::new("../escape")).is_err());
        assert!(safe_relative(Path::new("/absolute")).is_err());
        assert!(safe_relative(Path::new("python/pkg/data.txt")).is_ok());
    }
}
