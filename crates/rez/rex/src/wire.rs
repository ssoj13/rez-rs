use crate::{ActionInterpreter, RexExecutor};
use foundation::errors::{Result, RezError};
use std::collections::HashMap;

/// Evaluate Python Rex environment queries through the canonical Rust manager.
#[doc(hidden)]
pub fn rex_environ(
    actions: &serde_json::Value,
    state: &serde_json::Value,
    query: Option<serde_json::Value>,
) -> Result<serde_json::Value> {
    use crate::shell::rex::{PythonInterpreter, RexValue};
    let parse_map = |key: &str| -> Result<HashMap<String, String>> {
        serde_json::from_value(
            state
                .get(key)
                .cloned()
                .ok_or_else(|| RezError::Rex(format!("Rex environment state lacks {key:?}")))?,
        )
        .map_err(|error| RezError::Rex(format!("Rex environment {key:?}: {error}")))
    };
    let parent = parse_map("parent")?;
    let current = parse_map("current")?;
    let separators = parse_map("separators")?;
    let default_separator = state
        .get("separator")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RezError::Rex("Rex environment state lacks separator".into()))?;
    let mut executor = RexExecutor::new(PythonInterpreter::new(), Some(parent.clone()), false);
    executor.manager_mut().restore(current);
    executor.manager_mut().set_env_sep("", default_separator);
    for (key, separator) in separators {
        executor.manager_mut().set_env_sep(&key, &separator);
    }
    apply_rex_actions(&mut executor, actions)?;
    if let Some(query) = query {
        if query.is_array() {
            let value: RexValue = serde_json::from_value(query)
                .map_err(|error| RezError::Rex(format!("Rex expansion query: {error}")))?;
            return Ok(serde_json::Value::String(
                executor.manager().expandvars(value),
            ));
        }
        let key = query
            .get("key")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| RezError::Rex("Rex query lacks a string key".into()))?;
        return match query.get("operation").and_then(serde_json::Value::as_str) {
            Some("defined") => Ok(serde_json::Value::Bool(executor.defined(key))),
            Some("getenv") => Ok(serde_json::Value::String(executor.getenv(key)?.to_owned())),
            operation => Err(RezError::Rex(format!(
                "Unknown Rex query operation: {operation:?}"
            ))),
        };
    }
    let mut result = parent;
    result.extend(executor.env().clone());
    serde_json::to_value(result)
        .map_err(|error| RezError::Rex(format!("Rex environment serialization: {error}")))
}

/// Decode the shared Python action wire format without discarding value provenance.
#[doc(hidden)]
pub fn apply_rex_actions<I: ActionInterpreter>(
    executor: &mut RexExecutor<I>,
    actions: &serde_json::Value,
) -> Result<()> {
    use crate::shell::rex::RexValue;
    let actions = actions
        .as_array()
        .ok_or_else(|| RezError::Rex("Rex actions must be an array".into()))?;
    for (index, record) in actions.iter().enumerate() {
        let text = |key: &str| -> Result<&str> {
            record.get(key).and_then(|v| v.as_str()).ok_or_else(|| {
                RezError::Rex(format!("Rex action {index} field {key:?} must be a string"))
            })
        };
        let value = |key: &str| -> Result<RexValue> {
            let value = record
                .get(key)
                .ok_or_else(|| RezError::Rex(format!("Rex action {index} lacks {key:?}")))?;
            if let Some(text) = value.as_str() {
                return Ok(text.into());
            }
            serde_json::from_value(value.clone()).map_err(|error| {
                RezError::Rex(format!("Rex action {index} field {key:?}: {error}"))
            })
        };
        match text("t")? {
            "pkg_begin" => executor.comment(&format!(
                "{} from {}-{}",
                text("phase")?,
                text("name")?,
                text("version")?
            )),
            "set" => executor.setenv(text("k")?, value("v")?),
            "unset" => executor.unsetenv(text("k")?),
            "prepend" => executor.prependenv(text("k")?, value("v")?),
            "append" => executor.appendenv(text("k")?, value("v")?),
            "reset" => {
                let friends = record
                    .get("f")
                    .filter(|value| !value.is_null())
                    .map(|v| serde_json::from_value::<Vec<String>>(v.clone()))
                    .transpose()
                    .map_err(|error| RezError::Rex(format!("Rex reset friends: {error}")))?;
                executor.resetenv(text("k")?, value("v")?, friends);
            }
            "alias" => executor.alias(text("name")?, text("cmd")?),
            "source" => executor.source(value("path")?),
            "info" => executor.info(value("msg")?),
            "error" => executor.error(value("msg")?),
            "command" => executor.command(text("cmd")?),
            "command_args" => {
                let args = serde_json::from_value::<Vec<RexValue>>(
                    record.get("args").cloned().ok_or_else(|| {
                        RezError::Rex(format!("Rex action {index} lacks command args"))
                    })?,
                )
                .map_err(|error| RezError::Rex(format!("Rex command args: {error}")))?;
                executor.command_args(&args);
            }
            "comment" => executor.comment(text("msg")?),
            "shebang" => executor.shebang(),
            "stop" => return Err(RezError::RexStop(value("msg")?.to_string())),
            other => {
                return Err(RezError::Rex(format!(
                    "Unknown Rex action {other:?} at {index}"
                )));
            }
        }
    }
    Ok(())
}
