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
        let environment = std::env::vars().collect::<HashMap<_, _>>();
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
        let mut command = Command::new(&self.executable);
        command
            .args(args)
            .env_clear()
            .envs(&self.process_environment);
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
