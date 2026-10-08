//! Python execution with the canonical Rex query adapter.

pub fn exec_py_globals_for_rex(
    code: &str,
    filename: &str,
    inject: Option<&std::collections::HashMap<String, serde_json::Value>>,
    exclude: &[&str],
) -> foundation::Result<std::collections::HashMap<String, serde_json::Value>> {
    python_runtime::exec_py_globals_for_rex_with_query(
        code,
        filename,
        inject,
        exclude,
        rex::rex_environ,
    )
}
