// SPDX-License-Identifier: Apache-2.0

//! Select one Python/pip executable and retain its identity and execution environment.

use crate::config::RezConfig;
use crate::errors::{Result, RezError};
use repository::provider::FilesystemPackageProvider;
use resolve::{ResolveOptions, ResolvedContext};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use version::{Requirement, Version};

const PROBE: &str = r#"import json,sys,pip
from pip._vendor.packaging.markers import default_environment
print(json.dumps(dict(executable=sys.executable,version='.'.join(map(str,sys.version_info[:3])),pip_version=pip.__version__,environment=default_environment())))
"#;

#[derive(Debug, Deserialize)]
pub(super) struct Python {
    pub executable: PathBuf,
    pub version: String,
    pub pip_version: String,
    pub environment: HashMap<String, String>,
    #[serde(skip)]
    pub process_environment: HashMap<String, String>,
}

/// Isolated builders spawn a fresh installer outside this process's pip guard.
fn managed_arguments(args: &[String], offline: bool) -> Vec<String> {
    let mut effective = args.to_vec();
    if offline {
        let flag = if args.starts_with(&["-m".into(), "pip".into()])
            && args
                .get(2)
                .is_some_and(|command| matches!(command.as_str(), "install" | "wheel"))
        {
            Some("--no-build-isolation")
        } else if args.starts_with(&["-m".into(), "build".into()]) {
            Some("--no-isolation")
        } else {
            None
        };
        if let Some(flag) = flag {
            let boundary = effective
                .iter()
                .position(|argument| argument == "--")
                .unwrap_or(effective.len());
            effective.insert(boundary, flag.into());
        }
    }
    effective
}

impl Python {
    pub fn select(
        config: &RezConfig,
        python_version: Option<&str>,
        pip_version: Option<&str>,
    ) -> Result<Self> {
        let requested = python_version
            .map(Version::new)
            .transpose()?
            .map(|v| v.trim(2));
        let python = Requirement::new(
            &requested
                .as_ref()
                .map(|v| format!("python-{v}"))
                .unwrap_or_else(|| "python".into()),
        )?;
        let paths = config.expanded_packages_path_os();
        let provider = FilesystemPackageProvider::from_paths(&paths)?;
        let implicit = config
            .resolved_implicit_packages()
            .iter()
            .map(|s| Requirement::new(s))
            .collect::<Result<Vec<_>>>()?;
        let mut attempts = Vec::new();
        if pip_version.is_none() {
            attempts.push(vec![python.clone()]);
        }
        let pip = match pip_version.filter(|v| *v != "latest") {
            Some(value) => Requirement::new(&format!("pip-{value}"))?,
            None => Requirement::new("pip")?,
        };
        attempts.push(vec![python, pip]);
        for requests in attempts {
            let opts = ResolveOptions {
                package_paths: Some(paths.clone()),
                add_implicit: true,
                implicit_packages: Some(implicit.clone()),
                ..ResolveOptions::default()
            };
            let mut context = match ResolvedContext::resolve(requests, &provider, opts) {
                Ok(value) => value,
                Err(RezError::PackageFamilyNotFound(_) | RezError::PackageNotFound(_)) => continue,
                Err(error) => return Err(error),
            };
            if !context.success() {
                return Err(RezError::Resolve(
                    context
                        .failure_description
                        .unwrap_or_else(|| "Python/pip context resolution failed".into()),
                ));
            }
            context.append_sys_path = false;
            let version = context
                .get_resolved_package("python")
                .ok_or_else(|| RezError::Build("Pip context did not resolve Python".into()))?
                .version
                .clone();
            let names = [
                format!("python{}", version.trim(2)),
                format!("python{}", version.trim(1)),
                "python".into(),
            ];
            // Evaluate environment errors before which(), whose public API returns Option.
            let mut environment = std::env::vars().collect::<HashMap<_, _>>();
            environment.extend(context.get_environ(Some(environment.clone()))?);
            let environment = config.pip_environment(&environment);
            for name in names {
                if let Some((family, executable)) = context.which(&name) {
                    if family != "python" {
                        continue;
                    }
                    if let Some(value) = Self::probe(
                        &executable,
                        environment.clone(),
                        requested.as_ref(),
                        pip_version,
                    )? {
                        return Ok(value);
                    }
                }
            }
        }
        let environment = config.pip_environment(&std::env::vars().collect::<HashMap<_, _>>());
        for name in ["python3", "python"] {
            if let Some(value) = Self::probe(
                Path::new(name),
                environment.clone(),
                requested.as_ref(),
                pip_version,
            )? {
                eprintln!(
                    "No compatible Rez Python/pip found; using {}",
                    value.executable.display()
                );
                return Ok(value);
            }
        }
        Err(RezError::Build(format!(
            "No Python with pip >=19 satisfies Python {:?}, pip {:?}",
            python_version, pip_version
        )))
    }

    fn probe(
        executable: &Path,
        environment: HashMap<String, String>,
        requested: Option<&Version>,
        pip_version: Option<&str>,
    ) -> Result<Option<Self>> {
        let output = match Command::new(executable)
            .args(["-E", "-s", "-c", PROBE])
            .env_clear()
            .envs(&environment)
            .output()
        {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if !output.status.success() {
            return Ok(None);
        }
        let mut value: Self = serde_json::from_slice(&output.stdout).map_err(|error| {
            RezError::Build(format!(
                "Invalid Python/pip probe from {}: {error}",
                executable.display()
            ))
        })?;
        let version = Version::new(&value.version)?;
        if requested.is_some_and(|r| version.trim(2) != *r) {
            return Ok(None);
        }
        let pip = Version::new(&value.pip_version)?;
        if pip < Version::new("19")? {
            return Ok(None);
        }
        if let Some(request) = pip_version.filter(|v| *v != "latest") {
            if !Requirement::new(&format!("pip-{request}"))?
                .range()
                .is_some_and(|range| range.contains_version(&pip))
            {
                return Ok(None);
            }
        }
        value.process_environment = environment;
        Ok(Some(value))
    }

    pub fn run(&self, args: &[String], cwd: Option<&Path>) -> Result<Output> {
        let offline = self
            .process_environment
            .get("REZ_OFFLINE")
            .is_some_and(|value| value == "true");
        let effective = managed_arguments(args, offline);
        let mut command = Command::new(&self.executable);
        if args.starts_with(&["-m".into(), "pip".into()]) && offline {
            let entrypoint = format!(
                "{}\n_rez_apply_pip_offline()\nimport sys\nfrom pip._internal.cli.main import main\nsys.exit(main(sys.argv[1:]))",
                crate::builders::pip_utils::PIP_OFFLINE_GUARD,
            );
            command
                .args(["-s", "-c", &entrypoint])
                .args(&effective[2..]);
        } else {
            command.args(&effective);
        }
        command.env_clear().envs(&self.process_environment);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let output = command.output()?;
        if !output.status.success() {
            return Err(RezError::Build(format!(
                "Python command {:?} failed ({})\n{}\n{}",
                args,
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn offline_managed_arguments_disable_isolation_before_operands() {
        for (module, command, flag) in [
            ("pip", "install", "--no-build-isolation"),
            ("pip", "wheel", "--no-build-isolation"),
            ("build", "--wheel", "--no-isolation"),
        ] {
            let args = vec![
                "-m".into(),
                module.into(),
                command.into(),
                "--".into(),
                "/project".into(),
            ];
            assert_eq!(managed_arguments(&args, false), args);
            let effective = managed_arguments(&args, true);
            let flag_index = effective
                .iter()
                .position(|argument| argument == flag)
                .unwrap();
            let boundary = effective
                .iter()
                .position(|argument| argument == "--")
                .unwrap();
            assert!(flag_index < boundary);
        }
        let args = vec!["-m".into(), "pip".into(), "list".into()];
        assert_eq!(managed_arguments(&args, true), args);
    }

    #[test]
    fn offline_pyproject_requires_cannot_start_an_isolated_download() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("pyproject.toml"), format!(
            "[build-system]\nrequires = ['remote @ http://{address}/remote-1-py3-none-any.whl']\nbuild-backend = 'unavailable_offline_backend'\n"
        )).unwrap();
        let config = RezConfig {
            offline: true,
            ..RezConfig::default()
        };
        let environment = config.pip_environment(&std::env::vars().collect());
        let executable = std::env::var("REZ_TEST_PIP_PYTHON").unwrap_or_else(|_| "python".into());
        let python = Python::probe(Path::new(&executable), environment, None, None)
            .unwrap()
            .expect("test Python with pip");
        for command in ["wheel", "install"] {
            let output_flag = if command == "wheel" {
                "--wheel-dir"
            } else {
                "--target"
            };
            let error = python
                .run(
                    &[
                        "-m".into(),
                        "pip".into(),
                        command.into(),
                        "--no-index".into(),
                        output_flag.into(),
                        root.path().join(command).to_string_lossy().into_owned(),
                        root.path().to_string_lossy().into_owned(),
                    ],
                    None,
                )
                .unwrap_err();
            assert!(
                error.to_string().contains("unavailable_offline_backend"),
                "{error}"
            );
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
    }

    fn dependency_wheel(root: &Path, dependency: &str) -> PathBuf {
        let path = root.join("offline_fixture-1-py3-none-any.whl");
        let mut wheel = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        for (name, content) in [
            ("offline_fixture-1.dist-info/METADATA", format!("Metadata-Version: 2.1\nName: offline_fixture\nVersion: 1\nRequires-Dist: {dependency}\n")),
            ("offline_fixture-1.dist-info/WHEEL", "Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n".into()),
            ("offline_fixture-1.dist-info/RECORD", "".into()),
        ] {
            wheel.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            wheel.write_all(content.as_bytes()).unwrap();
        }
        wheel.finish().unwrap();
        path
    }

    #[test]
    fn managed_offline_pip_blocks_remote_wheel_dependencies_and_find_links() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let config = RezConfig {
            offline: true,
            ..RezConfig::default()
        };
        let environment = config.pip_environment(&std::env::vars().collect());
        let executable = std::env::var("REZ_TEST_PIP_PYTHON").unwrap_or_else(|_| "python".into());
        let python = Python::probe(Path::new(&executable), environment, None, None)
            .unwrap()
            .expect("test Python with pip");
        for dependency in [
            format!("remote @ http://{address}/remote-1-py3-none-any.whl"),
            format!("remote @ git+http://{address}/repo.git"),
        ] {
            let root = tempfile::tempdir().unwrap();
            let wheel = dependency_wheel(root.path(), &dependency);
            let error = python
                .run(
                    &[
                        "-m".into(),
                        "pip".into(),
                        "install".into(),
                        "--no-index".into(),
                        "--target".into(),
                        root.path().join("target").to_string_lossy().into_owned(),
                        wheel.to_string_lossy().into_owned(),
                    ],
                    None,
                )
                .unwrap_err();
            assert!(error.to_string().contains("REZ_OFFLINE=true:"), "{error}");
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
        let root = tempfile::tempdir().unwrap();
        let error = python
            .run(
                &[
                    "-m".into(),
                    "pip".into(),
                    "install".into(),
                    "--no-index".into(),
                    "--find-links".into(),
                    format!("http://{address}/wheels"),
                    "--target".into(),
                    root.path().join("target").to_string_lossy().into_owned(),
                    "unavailable-offline-fixture".into(),
                ],
                None,
            )
            .unwrap_err();
        assert!(error.to_string().contains("REZ_OFFLINE=true:"), "{error}");
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}
