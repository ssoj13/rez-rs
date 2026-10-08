// SPDX-License-Identifier: Apache-2.0

//! Actual offline pip conversion into the shared Rez publisher; no network or host installation.

use build_system::pip::{self, Options, VariantPolicy};
use model::config::RezConfig;
use model::package::Package;
use model::serialise::load_package_data;
use serde_json::json;
use std::fs;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn wheel(root: &Path, name: &str, requires: &str, tool: bool) -> PathBuf {
    let path = root.join(format!("{name}-1.0-py3-none-any.whl"));
    let source = r#"import json,sys,zipfile
data=json.loads(sys.argv[1])
name=data["name"]
info=name+"-1.0.dist-info/"
files={
 name+".py": "def main():\n    import json,sys\n    print(json.dumps(dict(module=__name__,args=sys.argv[1:]),ensure_ascii=False))\n    return 0\n",
 name+"/templates/portable-asset.exe": "MZ-preserved-template",
 info+"METADATA": "Metadata-Version: 2.1\nName: "+name+"\nVersion: 1.0\nRequires-Python: >=3.8\n"+data["requires"]+"\n",
 info+"WHEEL": "Wheel-Version: 1.0\nGenerator: rez-test\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
}
if data["tool"]:
 files[info+"entry_points.txt"]="[console_scripts]\n"+name+" = "+name+":main\n"
files[info+"RECORD"]="".join(key+",,\n" for key in [*files,info+"RECORD"])
with zipfile.ZipFile(data["path"],"w") as archive:
 for key,value in files.items(): archive.writestr(key,value)
"#;
    let output =
        Command::new(std::env::var("REZ_TEST_PIP_PYTHON").unwrap_or_else(|_| "python".into()))
            .args(["-I", "-c", source])
            .arg(json!({"path":path,"name":name,"requires":requires,"tool":tool}).to_string())
            .output()
            .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    path
}

fn config() -> RezConfig {
    RezConfig {
        packages_path: vec![],
        implicit_packages: vec![],
        ..RezConfig::default()
    }
}

fn options(repo: &Path, wheels: &[PathBuf], policy: VariantPolicy) -> Options {
    Options {
        packages: wheels
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
        prefix: Some(repo.to_path_buf()),
        no_deps: true,
        variant_policy: policy,
        extra: vec![
            "--no-index".into(),
            "--no-compile".into(),
            "--disable-pip-version-check".into(),
        ],
        ..Options::default()
    }
}

fn package(root: &Path) -> Package {
    let (data, _) = load_package_data(root).unwrap();
    Package::from_data(data).unwrap()
}

#[test]
fn portable_payload_keeps_templates_inactive_extras_and_tools() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let wheel = wheel(
        temp.path(),
        "portable_fixture",
        "Requires-Dist: never-installed; (python_version < '3.10' and sys_platform == 'linux') and extra == 'test'\n",
        true,
    );
    let mut opts = options(&repo, &[wheel], VariantPolicy::None);
    opts.python_requires = Some("3.10+".into());
    let result = pip::install(&opts, &config()).unwrap();
    let root = repo.join("portable_fixture/1.0");
    assert_eq!(result.installed, vec![root.clone()]);
    let package = package(&root);
    assert!(package.variants.is_empty());
    assert!(!package.hashed_variants);
    assert_eq!(
        package
            .requires
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["python-3.10+"]
    );
    assert_eq!(package.tools, ["portable_fixture"]);
    assert_eq!(
        fs::read_to_string(root.join("python/portable_fixture/templates/portable-asset.exe"))
            .unwrap(),
        "MZ-preserved-template"
    );
    for name in [
        "portable_fixture",
        "portable_fixture.py",
        "portable_fixture.cmd",
    ] {
        assert!(root.join("bin").join(name).is_file());
    }
    assert!(!root.join("bin/portable_fixture.exe").exists());
    let arguments = ["argument with spaces", "Unicode Ω"];
    #[cfg(windows)]
    let mut command = {
        let argv = vec![
            root.join("bin/portable_fixture.cmd")
                .to_string_lossy()
                .into_owned(),
            arguments[0].to_string(),
            arguments[1].to_string(),
        ];
        let source = rex::types::ShellType::Cmd.join_command(&argv, false, None);
        let mut command =
            Command::new(std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into()));
        command
            .args(["/D", "/S", "/C"])
            .raw_arg(format!(r#""{source}""#));
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut command = Command::new(root.join("bin/portable_fixture"));
        command.args(arguments);
        command
    };
    let output = command
        .env("PYTHONPATH", root.join("python"))
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let evidence: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        evidence,
        json!({"module":"portable_fixture", "args":arguments})
    );
}

#[test]
fn invalid_later_plan_never_publishes_earlier_distribution() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let first = wheel(temp.path(), "a_portable_fixture", "", false);
    let later = wheel(
        temp.path(),
        "z_nonportable_fixture",
        "Requires-Dist: unavailable; python_version < '3.10'\n",
        false,
    );
    let error = pip::install(
        &options(&repo, &[first, later], VariantPolicy::None),
        &config(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("not portable"), "{error}");
    assert!(
        !repo.exists(),
        "No final package may be published before all plans are validated"
    );
}

#[test]
fn floor_intersection_and_policy_misuse_fail_explicitly() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let wheel = wheel(temp.path(), "floor_fixture", "", false);
    let mut opts = options(&repo, &[wheel], VariantPolicy::None);
    opts.python_requires = Some("<3".into());
    let error = pip::install(&opts, &config()).unwrap_err();
    assert!(error.to_string().contains("empty intersection"), "{error}");
    assert!(!repo.exists());
    opts.variant_policy = VariantPolicy::Current;
    let error = pip::install(&opts, &config()).unwrap_err();
    assert!(
        error.to_string().contains("requires --variant-policy none"),
        "{error}"
    );
    opts.variant_policy = VariantPolicy::None;
    opts.python_requires = Some("not a range".into());
    assert!(pip::install(&opts, &config()).is_err());
}

#[test]
fn current_remains_interpreter_specific_by_default() {
    assert_eq!(Options::default().variant_policy, VariantPolicy::Current);
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let wheel = wheel(temp.path(), "current_fixture", "", false);
    pip::install(&options(&repo, &[wheel], VariantPolicy::Current), &config()).unwrap();
    let package = package(&repo.join("current_fixture/1.0"));
    assert!(package.hashed_variants);
    assert_eq!(package.variants.len(), 1);
    assert!(package.variants[0]
        .iter()
        .any(|requirement| requirement.name() == "python"));
}
