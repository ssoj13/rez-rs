//! Package-aware Python evaluation over the shared interpreter runtime.

use std::collections::HashMap;
use std::path::Path;

use crate::errors::{Result, RezError};
use serde_json::Value;

use python_runtime::LateBindingContext;

/// Evaluate a package late-binding function with its canonical include preparation.
pub fn eval_late_binding(
    source: &str,
    field_name: &str,
    pkg_data: &HashMap<String, Value>,
    in_context: bool,
) -> Result<Value> {
    eval_late_binding_with_context(source, field_name, pkg_data, in_context, None)
}

/// Evaluate a package late-binding function with resolved context bindings.
pub fn eval_late_binding_with_context(
    source: &str,
    field_name: &str,
    pkg_data: &HashMap<String, Value>,
    in_context: bool,
    ctx_bindings: Option<&LateBindingContext>,
) -> std::result::Result<Value, RezError> {
    let base = pkg_data.get("base").and_then(Value::as_str).map(Path::new);
    let prepared =
        crate::serialise::source_with_includes(source, base, pkg_data.get("config"), true)?;
    python_runtime::eval_prepared_late_binding_with_context(
        &prepared,
        field_name,
        pkg_data,
        in_context,
        ctx_bindings,
    )
}
