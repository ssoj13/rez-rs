//! `rez forward` - execute YAML forwarding scripts (hidden command).
//!
//! Reads YAML forwarding script and executes specified module function.

use clap::Args;
use foundation::errors::{Result, RezError};
use resolve::context::ResolvedContext;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

/// Execute a forwarding script (internal/hidden).
#[derive(Args, Debug)]
pub struct ForwardArgs {
    /// YAML forwarding script path
    pub yaml: PathBuf,

    /// Additional arguments passed through to the forwarded function
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

pub fn run(args: &ForwardArgs) -> Result<std::process::ExitCode> {
    let yaml_path = args
        .yaml
        .canonicalize()
        .unwrap_or_else(|_| args.yaml.clone());

    // Read forwarding script
    let content = fs::read_to_string(&yaml_path)
        .map_err(|e| RezError::Config(format!("Cannot read {}: {}", yaml_path.display(), e)))?;

    // Skip script header: Windows .cmd has 4-line batch header; Unix has shebang line
    let content = if cfg!(windows)
        && yaml_path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd"))
    {
        content.lines().skip(4).collect::<Vec<_>>().join("\n")
    } else if content.starts_with("#!") {
        content.lines().skip(1).collect::<Vec<_>>().join("\n")
    } else {
        content
    };

    let doc: serde_yaml::Value = serde_yaml::from_str(&content)
        .map_err(|e| RezError::Config(format!("Invalid YAML in {}: {}", yaml_path.display(), e)))?;

    // Extract required fields
    let func_name = doc
        .get("func_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RezError::Config("Missing 'func_name' in forwarding script".into()))?;

    // Suite tool: context_file direct, or derive from kwargs (context_name, tool_name) + script location
    let (context_file, tool_name) = if let Some(cf) =
        doc.get("context_file").and_then(|v| v.as_str())
    {
        let tn = doc
            .get("tool_name")
            .and_then(|v| v.as_str())
            .unwrap_or(func_name);
        (PathBuf::from(cf), tn.to_string())
    } else if doc.get("module").and_then(|v| v.as_str()) == Some("suite")
        && doc.get("func_name").and_then(|v| v.as_str()) == Some("_FWD__invoke_suite_tool_alias")
    {
        // Python rez format: kwargs has context_name, tool_name, prefix_char
        // Derive context_file: script is at suite/bin/alias(.cmd), so contexts/context_name.rxt
        let kwargs = doc
            .get("kwargs")
            .and_then(|v| v.as_mapping())
            .ok_or_else(|| {
                RezError::Config("Suite wrapper missing kwargs (context_name, tool_name)".into())
            })?;
        let context_name = kwargs
            .get("context_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RezError::Config("Suite wrapper kwargs missing context_name".into()))?;
        let tool_name = kwargs
            .get("tool_name")
            .and_then(|v| v.as_str())
            .unwrap_or(context_name)
            .to_string();
        // yaml_path is .../suite/bin/alias or .../suite/bin/alias.cmd
        let bin_dir = yaml_path
            .parent()
            .ok_or_else(|| RezError::Config("Cannot determine suite path from script".into()))?;
        let suite_path = bin_dir
            .parent()
            .ok_or_else(|| RezError::Config("Cannot determine suite path from script".into()))?;
        let ctx_path = suite_path
            .join("contexts")
            .join(format!("{context_name}.rxt"));
        (ctx_path, tool_name)
    } else {
        (PathBuf::new(), String::new())
    };

    if !context_file.as_os_str().is_empty() {
        if !context_file.is_file() {
            return Err(RezError::Config(format!(
                "Suite context file not found: {}",
                context_file.display()
            )));
        }
        let ctx = ResolvedContext::load(&context_file, None).map_err(|e| {
            RezError::Config(format!(
                "Cannot load context {}: {}",
                context_file.display(),
                e
            ))
        })?;

        // Suite commands retain their existing string argument convention.
        let mut all_args: Vec<String> = doc
            .get("nargs")
            .and_then(|v| v.as_sequence())
            .map(|seq| {
                seq.iter()
                    .filter_map(|v| {
                        v.as_str()
                            .map(String::from)
                            .or_else(|| Some(format!("{:?}", v)))
                    })
                    .collect()
            })
            .unwrap_or_default();
        all_args.extend(args.args.iter().cloned());

        let mut cmd_args = vec![tool_name.as_str()];
        cmd_args.extend(all_args.iter().map(|s| s.as_str()));

        let output = ctx.execute_command(&cmd_args[..], None)?;
        std::io::stdout()
            .write_all(&output.stdout)
            .and_then(|()| std::io::stdout().flush())
            .map_err(|e| RezError::Config(e.to_string()))?;
        std::io::stderr()
            .write_all(&output.stderr)
            .and_then(|()| std::io::stderr().flush())
            .map_err(|e| RezError::Config(e.to_string()))?;
        return Ok(rustpython_host_env::os::exit_code(
            output.status.code().unwrap_or(1) as u32,
        ));
    }

    let module = python_literal(&doc["module"])?;
    let func_name = python_literal(&serde_yaml::Value::String(func_name.into()))?;
    let nargs = python_literal(
        doc.get("nargs")
            .unwrap_or(&serde_yaml::Value::Sequence(Vec::new())),
    )?;
    let kwargs = python_literal(
        doc.get("kwargs")
            .unwrap_or(&serde_yaml::Value::Mapping(serde_yaml::Mapping::new())),
    )?;
    let script = format!(
        r#"import importlib
import inspect
import os
import sys

if "REZ_QUIET" not in os.environ:
    from rez.config import config
    config.override("quiet", True)

module_spec = {module}
if isinstance(module_spec, str):
    module = importlib.import_module("rez." + module_spec)
else:
    from rez.plugin_managers import plugin_manager
    plugin_type, plugin_name = module_spec
    module = plugin_manager.get_plugin_module(plugin_type, plugin_name)

target_func = getattr(module, {func_name})
spec = inspect.getfullargspec(target_func)
func_args = spec.args + spec.kwonlyargs
kwargs = {kwargs}
if "_script" in func_args:
    kwargs["_script"] = os.path.abspath(sys.argv[1])
if "_cli_args" in func_args:
    kwargs["_cli_args"] = sys.argv[2:]
target_func(*{nargs}, **kwargs)
"#
    );
    // A file avoids Windows command-line limits for nested forwarding data.
    let mut script_file = tempfile::NamedTempFile::new()
        .map_err(|error| RezError::Python(format!("Create forwarding script: {error}")))?;
    script_file
        .write_all(script.as_bytes())
        .map_err(|error| RezError::Python(format!("Write forwarding script: {error}")))?;
    let script_path = script_file.into_temp_path();
    let mut arguments = vec![args.yaml.as_os_str().to_owned()];
    arguments.extend(args.args.iter().map(OsString::from));
    super::python::run(&super::python::PythonArgs { arguments }, Some(script_path))
}

fn python_literal(value: &serde_yaml::Value) -> Result<String> {
    use serde_yaml::Value;
    match value {
        Value::Null => Ok("None".into()),
        Value::Bool(value) => Ok(if *value { "True" } else { "False" }.into()),
        Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                Ok(value.to_string())
            } else if let Some(value) = value.as_u64() {
                Ok(value.to_string())
            } else if let Some(value) = value.as_f64() {
                if value.is_nan() {
                    Ok("float('nan')".into())
                } else if value.is_infinite() {
                    Ok(if value.is_sign_negative() {
                        "float('-inf')"
                    } else {
                        "float('inf')"
                    }
                    .into())
                } else {
                    let mut literal = value.to_string();
                    if !literal.contains(['.', 'e', 'E']) {
                        literal.push_str(".0");
                    }
                    Ok(literal)
                }
            } else {
                Err(RezError::Config("Unsupported forwarding number".into()))
            }
        }
        Value::String(value) => serde_json::to_string(value)
            .map_err(|error| RezError::Config(format!("Encode forwarding string: {error}"))),
        Value::Sequence(values) => {
            let values = values
                .iter()
                .map(python_literal)
                .collect::<Result<Vec<_>>>()?;
            Ok(format!("[{}]", values.join(", ")))
        }
        Value::Mapping(values) => {
            let values = values
                .iter()
                .map(|(key, value)| {
                    Ok(format!(
                        "{}: {}",
                        python_literal(key)?,
                        python_literal(value)?
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(format!("{{{}}}", values.join(", ")))
        }
        Value::Tagged(value) => Err(RezError::Config(format!(
            "Unsupported forwarding YAML tag: {}",
            value.tag
        ))),
    }
}
