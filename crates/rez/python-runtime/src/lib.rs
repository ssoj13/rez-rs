//! Embedded RustPython VM for package.py and rezconfig.py execution.
//!
//! Also provides `rez python` REPL and script runner functionality.

use foundation::errors::RezError;
use rustpython::{InterpreterBuilder, InterpreterBuilderExt};
use rustpython_vm as vm;
use rustpython_vm::builtins::{PyStr, PyStrRef};
use rustpython_vm::AsObject;
use serde_json::{Map, Value};
use sha1::Digest;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{mpsc, OnceLock};

#[path = "python_vm/runtime.rs"]
mod runtime;

/// Canonical include identity used by runtime caching and installed sidecars.
pub fn include_module_hash(content: &[u8]) -> String {
    let whitespace = b" \t\n\r\x0b\x0c";
    let start = content
        .iter()
        .position(|byte| !whitespace.contains(byte))
        .unwrap_or(content.len());
    let end = content
        .iter()
        .rposition(|byte| !whitespace.contains(byte))
        .map_or(start, |index| index + 1);
    foundation::util::hex_encode(sha1::Sha1::digest(&content[start..end]))
}

/// Shared Python adapters over the canonical Rust requirement/version parser.
pub const VERSION_BINDINGS: &str = include_str!("python_vm/version_bindings.py");

// py_freeze reads directories inside a procedural macro. Explicit source includes
// also register these files with Cargo's dependency tracking for subsequent edits.
const _: &str = include_str!("python_vm/bridge/rez/__init__.py");
const _: &str = include_str!("python_vm/bridge/rez/exceptions.py");

pub use runtime::{executable, is_python_executable, register_cli};

/// Apply executable identity only to a registered Rez CLI, preserving embedding hosts.
fn interpreter_builder(executable: Option<std::path::PathBuf>) -> InterpreterBuilder {
    let builder = InterpreterBuilder::new()
        .init_stdlib()
        // py_freeze's directory root is a container, not a package prefix.
        // Its rez/__init__.py is registered as the actual frozen rez package.
        .add_frozen_modules(vm::py_freeze!(dir = "python_vm/bridge"));
    if let Some(path) = executable {
        let path = path.to_string_lossy().into_owned();
        builder.init_hook(move |vm| {
            // Init hooks precede VM initialization, with exclusively owned state.
            // This is the same invariant used by RustPython's frozen-stdlib hook.
            let state = rustpython_vm::common::rc::PyRc::get_mut(&mut vm.state)
                .expect("exclusive interpreter state during initialization");
            state.config.paths.executable = path.clone();
            state.config.paths.base_executable = path;
        })
    } else {
        builder
    }
}

/// Stack size for Python VM threads (8MB — stdlib imports need deep recursion)
const PY_STACK_SIZE: usize = 8 * 1024 * 1024;

// --- Python Worker Thread (reuses interpreter + 8MB stack thread) ---

/// Type-erased job: closure receiving &Interpreter, returning boxed Any result.
type PyJob = Box<dyn FnOnce(&vm::Interpreter) -> Box<dyn std::any::Any + Send> + Send>;

struct PyWorker {
    tx: mpsc::Sender<(PyJob, mpsc::Sender<Box<dyn std::any::Any + Send>>)>,
}

static PY_WORKER: OnceLock<PyWorker> = OnceLock::new();

/// Get or init the singleton Python worker thread.
fn get_py_worker() -> Result<&'static PyWorker, RezError> {
    let executable = runtime::executable()?;
    Ok(PY_WORKER.get_or_init(move || {
        let (tx, rx) = mpsc::channel::<(PyJob, mpsc::Sender<Box<dyn std::any::Any + Send>>)>();
        std::thread::Builder::new()
            .stack_size(PY_STACK_SIZE)
            .name("rez-python-worker".into())
            .spawn(move || {
                let interp = interpreter_builder(executable).interpreter();
                interp.enter(|vm| {
                    vm.builtins
                        .set_attr(
                            "_rez_include_module",
                            vm.new_function("_rez_include_module", rez_include_module_native),
                            vm,
                        )
                        .expect("failed to register include module loader");
                });
                while let Ok((job, reply_tx)) = rx.recv() {
                    let result = job(&interp);
                    let _ = reply_tx.send(result);
                }
            })
            .expect("failed to spawn Python worker thread");
        PyWorker { tx }
    }))
}

/// Run a closure on the shared Python worker thread with interpreter reuse.
/// Each call gets a fresh scope (via vm.new_scope_with_builtins) for isolation.
fn run_on_py_worker<F, T>(f: F) -> Result<T, RezError>
where
    F: FnOnce(&vm::Interpreter) -> Result<T, RezError> + Send + 'static,
    T: Send + 'static,
{
    let worker = get_py_worker()?;
    let (reply_tx, reply_rx) = mpsc::channel();
    let job: PyJob = Box::new(move |interp| Box::new(f(interp)) as Box<dyn std::any::Any + Send>);
    worker
        .tx
        .send((job, reply_tx))
        .map_err(|_| RezError::Python("Python worker channel closed".into()))?;
    let result = reply_rx
        .recv()
        .map_err(|_| RezError::Python("Python worker reply channel closed".into()))?;
    *result
        .downcast::<Result<T, RezError>>()
        .map_err(|_| RezError::Python("Python worker result type mismatch".into()))?
}

/// Call Python str(obj) and return the Rust string.
fn pyobj_to_str(vm: &vm::VirtualMachine, obj: &vm::PyObjectRef) -> vm::PyResult<String> {
    let str_type = vm
        .get_attribute_opt(vm.builtins.as_object(), "str")?
        .ok_or_else(|| vm.new_value_error("builtins.str not found".to_string()))?;
    let result = str_type.call((obj.clone(),), vm)?;
    let pystr = result
        .downcast::<PyStr>()
        .map_err(|_| vm.new_type_error("str() did not return str".to_string()))?;
    pystr.to_str().map(str::to_owned).ok_or_else(|| {
        vm.new_value_error("Python string contains surrogate code points".to_owned())
    })
}

/// Cache include modules by source identity for this interpreter's lifetime.
/// Rez's IncludeModuleManager shares identical stripped source, even under
/// different names/paths. Failed execution must never publish a partial module.
fn rez_include_module_native(
    name: PyStrRef,
    path: PyStrRef,
    content: PyStrRef,
    vm: &vm::VirtualMachine,
) -> vm::PyResult<vm::PyObjectRef> {
    let name = name
        .to_str()
        .ok_or_else(|| vm.new_value_error("Invalid include name"))?;
    let path = path
        .to_str()
        .ok_or_else(|| vm.new_value_error("Invalid include path"))?;
    let content = content
        .to_str()
        .ok_or_else(|| vm.new_value_error("Invalid include source"))?;
    // Compute from actual bytes rather than trusting a stale installed .sha1.
    let identity = include_module_hash(content.as_bytes());
    let sys = vm.sys_module.dict();
    let cache = match sys.get_item_opt("_rez_include_modules", vm)? {
        Some(cache) => cache
            .downcast::<vm::builtins::PyDict>()
            .map_err(|_| vm.new_type_error("Invalid include module cache"))?,
        None => {
            let cache = vm.ctx.new_dict();
            sys.set_item("_rez_include_modules", cache.clone().into(), vm)?;
            cache
        }
    };
    if let Some(module) = cache.get_item_opt(identity.as_str(), vm)? {
        return Ok(module);
    }

    // Match Rez util.load_module_from_file: importlib metadata without adding
    // this include module to sys.modules. Execute the current source bytes,
    // avoiding timestamp-based .pyc reuse when content has changed.
    vm.import("importlib.util", 0)?;
    let util = vm
        .sys_module
        .get_attr("modules", vm)?
        .get_item("importlib.util", vm)?;
    let spec = util
        .get_attr("spec_from_file_location", vm)?
        .call((vm.ctx.new_str(name), vm.ctx.new_str(path)), vm)?;
    let module = util.get_attr("module_from_spec", vm)?.call((spec,), vm)?;
    let globals = module
        .get_attr("__dict__", vm)?
        .downcast::<vm::builtins::PyDict>()
        .map_err(|_| vm.new_type_error("Include module has no namespace"))?;
    let code = vm
        .compile(content, vm::compiler::Mode::Exec, path.to_owned())
        .map_err(|error| error.into_pyexception(vm, Some(content)))?;
    vm.run_code_obj(code, vm::scope::Scope::with_builtins(None, globals, vm))?;
    cache.set_item(identity.as_str(), module.clone(), vm)?;
    Ok(module)
}

/// Requirement and range operations share the resolver's Rust implementation.
fn rez_version_native(
    operation: PyStrRef,
    left: PyStrRef,
    right: PyStrRef,
    vm: &vm::VirtualMachine,
) -> vm::PyResult<vm::PyObjectRef> {
    let parse = || -> Result<Value, RezError> {
        let operation = operation
            .to_str()
            .ok_or_else(|| RezError::Python("Invalid version operation string".into()))?;
        let left = left
            .to_str()
            .ok_or_else(|| RezError::Python("Invalid version operand string".into()))?;
        let right = right
            .to_str()
            .ok_or_else(|| RezError::Python("Invalid version operand string".into()))?;
        if operation == "requirement" {
            let requirement = version::Requirement::new(left)?;
            return Ok(serde_json::json!({
                "name": requirement.name(), "range": requirement.range().map(ToString::to_string),
                "conflict": requirement.conflict(), "weak": requirement.weak(),
                "string": requirement.to_string(), "safe": requirement.safe_str()
            }));
        }
        if operation == "conflicts" || operation == "merged" {
            let left = version::Requirement::new(left)?;
            let right = version::Requirement::new(right)?;
            return Ok(if operation == "conflicts" {
                Value::Bool(left.conflicts_with_req(&right))
            } else {
                left.merged(&right)
                    .map_or(Value::Null, |value| Value::String(value.to_string()))
            });
        }
        let left = version::VersionRange::new(left)?;
        Ok(match operation {
            "range" => Value::String(left.to_string()),
            "contains" => Value::Bool(left.contains_version(&version::Version::new(right)?)),
            "any" => Value::Bool(left.is_any()),
            "intersects" => Value::Bool(left.intersects(&version::VersionRange::new(right)?)),
            "issuperset" => Value::Bool(left.issuperset(&version::VersionRange::new(right)?)),
            "issubset" => Value::Bool(left.issubset(&version::VersionRange::new(right)?)),
            "intersection" => left
                .intersection(&version::VersionRange::new(right)?)
                .map_or(Value::Null, |value| Value::String(value.to_string())),
            "union" => Value::String(left.union(&version::VersionRange::new(right)?).to_string()),
            _ => {
                return Err(RezError::Python(format!(
                    "Unknown version operation: {operation}"
                )));
            }
        })
    };
    parse()
        .map(|value| value_to_pyobj(vm, &value))
        .map_err(|error| vm.new_value_error(error.to_string()))
}

/// Native Rex intersection queries use the same typed ranges as the resolver.
fn rez_intersects_native(
    obj: vm::PyObjectRef,
    range_str: PyStrRef,
    vm: &vm::VirtualMachine,
) -> vm::PyResult<bool> {
    let range_s = range_str.to_str().ok_or_else(|| {
        vm.new_value_error("Version range contains surrogate code points".to_owned())
    })?;
    let range = version::VersionRange::new(range_s)
        .map_err(|error| vm.new_value_error(error.to_string()))?;
    let Some(other) = extract_version_or_range_from_obj(&obj, vm)? else {
        return Ok(false);
    };
    let other = version::VersionRange::new(&other)
        .map_err(|error| vm.new_value_error(error.to_string()))?;
    Ok(range.intersects(&other))
}

/// Generic native queries accept requirement strings; Rex captures genuine binding types in Python.
fn extract_version_or_range_from_obj(
    obj: &vm::PyObjectRef,
    vm: &vm::VirtualMachine,
) -> vm::PyResult<Option<String>> {
    if let Some(text) = obj.downcast_ref::<PyStr>() {
        let text = text.to_str().ok_or_else(|| {
            vm.new_value_error("Requirement contains surrogate code points".to_owned())
        })?;
        let requirement = version::Requirement::new(text)
            .map_err(|error| vm.new_value_error(error.to_string()))?;
        return Ok(if requirement.conflict() {
            None
        } else {
            Some(
                requirement
                    .range()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            )
        });
    }
    Err(vm.new_runtime_error(format!(
        "Invalid type {} passed as first arg to 'intersects'",
        obj.class().name()
    )))
}

/// Rex queries replay the canonical Rust action manager, preserving value provenance.
fn rez_environ_native(
    actions: vm::PyObjectRef,
    state: vm::PyObjectRef,
    query: vm::function::OptionalArg<vm::PyObjectRef>,
    vm: &vm::VirtualMachine,
    environment_query: RexEnvironmentQuery,
) -> vm::PyResult<vm::PyObjectRef> {
    let actions = pyobj_to_value(vm, &actions, "Rex actions")
        .map_err(|error| vm.new_value_error(error.to_string()))?;
    let state = pyobj_to_value(vm, &state, "Rex environment state")
        .map_err(|error| vm.new_value_error(error.to_string()))?;
    let query = match query {
        vm::function::OptionalArg::Present(value) if !vm.is_none(&value) => Some(
            pyobj_to_value(vm, &value, "Rex environment query")
                .map_err(|error| vm.new_value_error(error.to_string()))?,
        ),
        _ => None,
    };
    let value = match environment_query(&actions, &state, query) {
        Ok(value) => value,
        Err(RezError::RexUndefinedVariable(message)) => {
            // Invoke the shared Python constructor: RezError stores value for __str__.
            // VM's raw exception allocator bypasses that initializer.
            let package = vm.import("rez.exceptions", 0)?;
            let exceptions = package.get_attr("exceptions", vm)?;
            let class = exceptions.get_attr("RexUndefinedVariableError", vm)?;
            let exception = class.call((message,), vm)?;
            return Err(exception
                .downcast::<vm::builtins::PyBaseException>()
                .map_err(|_| {
                    vm.new_type_error(
                        "Rex exception constructor did not return BaseException".to_owned(),
                    )
                })?);
        }
        Err(error) => return Err(vm.new_value_error(error.to_string())),
    };
    Ok(value_to_pyobj(vm, &value))
}

/// Environment queries are supplied by the embedding Rex action manager.
pub type RexEnvironmentQuery = fn(&Value, &Value, Option<Value>) -> Result<Value, RezError>;

fn unavailable_environment_query(
    _actions: &Value,
    _state: &Value,
    _query: Option<Value>,
) -> Result<Value, RezError> {
    Err(RezError::Rex(
        "Rex environment queries require an environment query callback".into(),
    ))
}

/// Execute Python code for rex with native intersects injected.
/// Uses Rust Version/VersionRange logic for 100% consistency with the resolver.
pub fn exec_py_globals_for_rex(
    code: &str,
    filename: &str,
    inject: Option<&HashMap<String, Value>>,
    exclude: &[&str],
) -> Result<HashMap<String, Value>, RezError> {
    exec_py_globals_for_rex_with_query(
        code,
        filename,
        inject,
        exclude,
        unavailable_environment_query,
    )
}

/// Execute Rex code with a query callback scoped to this execution.
pub fn exec_py_globals_for_rex_with_query(
    code: &str,
    filename: &str,
    inject: Option<&HashMap<String, Value>>,
    exclude: &[&str],
    environment_query: RexEnvironmentQuery,
) -> Result<HashMap<String, Value>, RezError> {
    let code = code.to_owned();
    let filename = filename.to_owned();
    let inject: Option<HashMap<String, Value>> = inject.cloned();
    let exclude: Vec<String> = exclude.iter().map(|s| s.to_string()).collect();

    run_on_py_worker(move |interp| {
        interp.enter(|vm| -> Result<HashMap<String, Value>, RezError> {
            let scope = vm.new_scope_with_builtins();
            let action_only = inject
                .as_ref()
                .is_some_and(|vars| vars.contains_key("_rex_data"));
            let catch_rex_errors = inject
                .as_ref()
                .and_then(|vars| vars.get("_rex_data"))
                .and_then(|data| data.get("catch_rex_errors"))
                .and_then(Value::as_bool)
                .unwrap_or(true);

            if let Some(ref vars) = inject {
                for (k, v) in vars {
                    let py_val = value_to_pyobj(vm, v);
                    scope
                        .globals
                        .set_item(k.as_str(), py_val, vm)
                        .map_err(|e| {
                            RezError::Python(format!(
                                "inject global '{}': {}",
                                k,
                                pyerr_str(vm, &e)
                            ))
                        })?;
                }
            }

            let environ_fn = vm.new_function(
                "rex_environ",
                move |actions: vm::PyObjectRef,
                      state: vm::PyObjectRef,
                      query: vm::function::OptionalArg<vm::PyObjectRef>,
                      vm: &vm::VirtualMachine| {
                    rez_environ_native(actions, state, query, vm, environment_query)
                },
            );
            scope
                .globals
                .set_item("rex_environ", environ_fn.into(), vm)
                .map_err(|error| {
                    RezError::Python(format!(
                        "inject Rex environment query: {}",
                        pyerr_str(vm, &error)
                    ))
                })?;

            scope
                .globals
                .set_item(
                    "_rez_version_native",
                    vm.new_function("_rez_version_native", rez_version_native)
                        .into(),
                    vm,
                )
                .map_err(|e| {
                    RezError::Python(format!("inject Rex version binding: {}", pyerr_str(vm, &e)))
                })?;
            let bindings = vm
                .compile(
                    VERSION_BINDINGS,
                    vm::compiler::Mode::Exec,
                    "<version-bindings>",
                )
                .map_err(|e| RezError::Python(format!("compile Rex version bindings: {e:?}")))?;
            vm.run_code_obj(bindings, scope.clone()).map_err(|e| {
                RezError::Python(format!(
                    "execute Rex version bindings: {}",
                    pyerr_str(vm, &e)
                ))
            })?;

            let intersects_fn = vm.new_function("intersects", rez_intersects_native);
            scope
                .globals
                .set_item("intersects", intersects_fn.into(), vm)
                .map_err(|e| {
                    RezError::Python(format!("inject intersects: {}", pyerr_str(vm, &e)))
                })?;

            let code_obj = vm
                .compile(&code, vm::compiler::Mode::Exec, filename.to_owned())
                .map_err(|e| {
                    let message = format!("Python compile error in {}: {:?}", filename, e);
                    if action_only {
                        RezError::Rex(message)
                    } else {
                        RezError::Python(message)
                    }
                })?;

            let package = vm.import("rez.exceptions", 0).map_err(|error| {
                RezError::Python(format!("import Rex exceptions: {}", pyerr_str(vm, &error)))
            })?;
            let exceptions = package.get_attr("exceptions", vm).map_err(|error| {
                RezError::Python(format!(
                    "load Rex exception module: {}",
                    pyerr_str(vm, &error)
                ))
            })?;
            let exception_type = |name: &'static str| -> Result<vm::builtins::PyTypeRef, RezError> {
                exceptions
                    .get_attr(name, vm)
                    .map_err(|error| {
                        RezError::Python(format!("load {name}: {}", pyerr_str(vm, &error)))
                    })?
                    .downcast::<vm::builtins::PyType>()
                    .map_err(|_| RezError::Python(format!("rez.exceptions.{name} is not a class")))
            };
            let stop_type = exception_type("RexStopError")?;
            let undefined_type = exception_type("RexUndefinedVariableError")?;
            let rex_type = exception_type("RexError")?;
            vm.run_code_obj(code_obj, scope.clone()).map_err(|error| {
                let obj: vm::PyObjectRef = error.clone().into();
                let message = pyobj_to_str(vm, &obj).unwrap_or_else(|_| pyerr_str(vm, &error));
                if obj.fast_isinstance(&stop_type) {
                    RezError::RexStop(message)
                } else if obj.fast_isinstance(&undefined_type) {
                    RezError::RexUndefinedVariable(message)
                } else if obj.fast_isinstance(&rex_type) {
                    RezError::Rex(message)
                } else {
                    let message =
                        format!("Python exec error in {filename}: {}", pyerr_str(vm, &error));
                    if action_only
                        && catch_rex_errors
                        && obj.fast_isinstance(vm.ctx.exceptions.exception_type)
                    {
                        RezError::Rex(message)
                    } else {
                        RezError::Python(message)
                    }
                }
            })?;
            let mut result = HashMap::new();
            for (key, val) in &scope.globals {
                let Ok(key_str) = pyobj_to_str(vm, &key) else {
                    continue;
                };
                if key_str.starts_with("__") {
                    continue;
                }
                if exclude.iter().any(|e| e == &key_str) || (action_only && key_str != "_actions") {
                    continue;
                }
                if is_module(vm, &val) || is_callable_skip(vm, &val) {
                    continue;
                }
                let value =
                    pyobj_to_value(vm, &val, &format!("global {key_str:?}")).map_err(|error| {
                        RezError::Python(format!("Python value conversion in {filename}: {error}"))
                    })?;
                result.insert(key_str, value);
            }
            Ok(result)
        })
    })
}

/// Execute Python code string, return filtered globals as JSON Value map.
/// `inject` — extra globals to set before exec (e.g. __name__, __file__).
/// `exclude` — keys to strip from the result (builtins, injected helpers).
pub fn exec_py_globals(
    code: &str,
    filename: &str,
    inject: Option<&HashMap<String, Value>>,
    exclude: &[&str],
) -> Result<HashMap<String, Value>, RezError> {
    let code = code.to_owned();
    let filename = filename.to_owned();
    let inject: Option<HashMap<String, Value>> = inject.cloned();
    let exclude: Vec<String> = exclude.iter().map(|s| s.to_string()).collect();

    run_on_py_worker(move |interp| {
        interp.enter(|vm| -> Result<HashMap<String, Value>, RezError> {
            let scope = vm.new_scope_with_builtins();

            // Inject pre-set globals if any
            if let Some(ref vars) = inject {
                for (k, v) in vars {
                    let py_val = value_to_pyobj(vm, v);
                    scope
                        .globals
                        .set_item(k.as_str(), py_val, vm)
                        .map_err(|e| {
                            RezError::Python(format!(
                                "inject global '{}': {}",
                                k,
                                pyerr_str(vm, &e)
                            ))
                        })?;
                }
            }

            scope
                .globals
                .set_item(
                    "_rez_version_native",
                    vm.new_function("_rez_version_native", rez_version_native)
                        .into(),
                    vm,
                )
                .map_err(|error| RezError::Python(pyerr_str(vm, &error)))?;

            // Compile + execute
            let code_obj = vm
                .compile(&code, vm::compiler::Mode::Exec, filename.to_owned())
                .map_err(|e| {
                    RezError::Python(format!("Python compile error in {}: {:?}", filename, e))
                })?;

            vm.run_code_obj(code_obj, scope.clone()).map_err(|e| {
                RezError::Python(format!(
                    "Python exec error in {}: {}",
                    filename,
                    pyerr_str(vm, &e)
                ))
            })?;

            // Collect globals → HashMap<String, Value>
            let mut result = HashMap::new();
            for (key, val) in &scope.globals {
                let Ok(key_str) = pyobj_to_str(vm, &key) else {
                    continue;
                };
                // Skip dunder, builtins, modules, callables, excluded
                if key_str.starts_with("__") {
                    continue;
                }
                if exclude.iter().any(|e| e == &key_str) {
                    continue;
                }
                if is_module(vm, &val) || is_callable_skip(vm, &val) {
                    continue;
                }
                let value =
                    pyobj_to_value(vm, &val, &format!("global {key_str:?}")).map_err(|error| {
                        RezError::Python(format!("Python value conversion in {filename}: {error}"))
                    })?;
                result.insert(key_str, value);
            }
            Ok(result)
        })
    })
}

/// Execute a .py file from disk, return globals.
pub fn exec_py_file(
    path: &Path,
    inject: Option<&HashMap<String, Value>>,
    exclude: &[&str],
) -> Result<HashMap<String, Value>, RezError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| RezError::Python(format!("read {}: {}", path.display(), e)))?;
    let fname = path.file_name().unwrap_or_default().to_string_lossy();
    exec_py_globals(&content, &fname, inject, exclude)
}

/// Start interactive Python REPL.
pub fn start_repl() -> Result<(), RezError> {
    with_py_stack(move || {
        let interp = interpreter_builder(runtime::executable()?).interpreter();

        interp
            .enter(|vm| {
                let scope = vm.new_scope_with_builtins();
                init_stdio(vm, &scope)?;
                let mut input = String::new();
                let prompt_main = ">>> ";
                let prompt_cont = "... ";

                loop {
                    let prompt = if input.is_empty() {
                        prompt_main
                    } else {
                        prompt_cont
                    };
                    eprint!("{}", prompt);

                    let mut line = String::new();
                    match std::io::stdin().read_line(&mut line) {
                        Ok(0) => break, // EOF
                        Err(e) => {
                            eprintln!("read error: {}", e);
                            break;
                        }
                        _ => {}
                    }

                    input.push_str(&line);

                    // Try to compile — if incomplete, keep reading
                    match vm.compile(&input, vm::compiler::Mode::Single, "<stdin>".to_owned()) {
                        Ok(code_obj) => {
                            if let Err(e) = vm.run_code_obj(code_obj, scope.clone()) {
                                let msg = pyerr_str(vm, &e);
                                eprintln!("{}", msg);
                            }
                            input.clear();
                        }
                        Err(_) => {
                            // Check if it's just an incomplete statement
                            if line.trim().is_empty() {
                                // Empty line in block → try to execute what we have
                                if let Ok(code_obj) = vm.compile(
                                    &input,
                                    vm::compiler::Mode::Exec,
                                    "<stdin>".to_owned(),
                                ) {
                                    if let Err(e) = vm.run_code_obj(code_obj, scope.clone()) {
                                        let msg = pyerr_str(vm, &e);
                                        eprintln!("{}", msg);
                                    }
                                } else {
                                    eprintln!("SyntaxError");
                                }
                                input.clear();
                            }
                            // Otherwise keep accumulating lines
                        }
                    }
                }
                Ok(())
            })
            .map_err(|e: vm::PyRef<vm::builtins::PyBaseException>| {
                RezError::Python(format!("REPL error: {:?}", e))
            })
    })
}

/// Run the upstream Python command-line interpreter on the shared enlarged stack.
///
/// RustPython owns argument parsing, __main__, import paths, stdio, SystemExit,
/// atexit and thread shutdown. Package-source VMs remain separate embedded scopes.
pub fn run_cli() -> Result<std::process::ExitCode, RezError> {
    with_py_stack(|| Ok(rustpython::run(interpreter_builder(runtime::executable()?))))
}

/// Run a Python script file with arguments.
pub fn run_script(path: &Path, args: &[String]) -> Result<(), RezError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| RezError::Python(format!("read {}: {}", path.display(), e)))?;
    let path_disp = path.display().to_string();
    let args = args.to_vec();

    with_py_stack(move || {
        let interp = interpreter_builder(runtime::executable()?).interpreter();

        interp
            .enter(|vm| {
                let scope = vm.new_scope_with_builtins();
                init_stdio(vm, &scope)?;

                // Set sys.argv
                let argv_code = format!(
                    "import sys; sys.argv = {:?}",
                    std::iter::once(path_disp.clone())
                        .chain(args.iter().cloned())
                        .collect::<Vec<_>>()
                );
                let argv_obj = vm
                    .compile(&argv_code, vm::compiler::Mode::Exec, "<argv>".to_owned())
                    .map_err(|e| e.into_pyexception(vm, Some(&argv_code)))?;
                vm.run_code_obj(argv_obj, scope.clone())?;

                // Run the script
                let code_obj = vm
                    .compile(&content, vm::compiler::Mode::Exec, path_disp.clone())
                    .map_err(|e| e.into_pyexception(vm, Some(&content)))?;
                vm.run_code_obj(code_obj, scope)?;
                Ok(())
            })
            .map_err(|e: vm::PyRef<vm::builtins::PyBaseException>| {
                RezError::Python(format!("script: {:?}", e))
            })
    })
}

/// Bootstrap code to ensure sys.stdout/stderr are connected to real file descriptors.
const STDIO_INIT: &str = "import sys\nimport _io\nsys.stdout = _io.TextIOWrapper(_io.FileIO(1, 'w'), line_buffering=True)\nsys.stderr = _io.TextIOWrapper(_io.FileIO(2, 'w'), line_buffering=True)\nsys.stdin = _io.TextIOWrapper(_io.FileIO(0, 'r'))\n";

/// Initialize stdio in the given scope.
fn init_stdio(vm: &vm::VirtualMachine, scope: &vm::scope::Scope) -> vm::PyResult<()> {
    let code = vm
        .compile(STDIO_INIT, vm::compiler::Mode::Exec, "<stdio>".to_owned())
        .map_err(|e| e.into_pyexception(vm, Some(STDIO_INIT)))?;
    vm.run_code_obj(code, scope.clone())?;
    Ok(())
}

/// Run a closure on a thread with a larger stack to avoid stack overflow in RustPython.
fn with_py_stack<F, T>(f: F) -> Result<T, RezError>
where
    F: FnOnce() -> Result<T, RezError> + Send + 'static,
    T: Send + 'static,
{
    let handle = std::thread::Builder::new()
        .stack_size(PY_STACK_SIZE)
        .name("rez-python".into())
        .spawn(f)
        .map_err(|e| RezError::Python(format!("spawn thread: {}", e)))?;
    handle
        .join()
        .map_err(|_| RezError::Python("Python thread panicked".into()))?
}

/// Execute a Python code string (for `rez python -c "..."`)
pub fn exec_code(code: &str) -> Result<(), RezError> {
    let code = code.to_owned();
    with_py_stack(move || {
        let interp = interpreter_builder(runtime::executable()?).interpreter();

        interp
            .enter(|vm| {
                let scope = vm.new_scope_with_builtins();
                init_stdio(vm, &scope)?;
                let code_obj = vm
                    .compile(&code, vm::compiler::Mode::Exec, "<string>".to_owned())
                    .map_err(|e| e.into_pyexception(vm, Some(&code)))?;
                vm.run_code_obj(code_obj, scope)?;
                Ok(())
            })
            .map_err(|e: vm::PyRef<vm::builtins::PyBaseException>| {
                RezError::Python(format!("{:?}", e))
            })
    })
}

/// Context bindings for late evaluation when in_context=true.
/// Mirrors Python rez ResolvedContext._get_pre_resolve_bindings().
#[derive(Clone, Debug)]
pub struct LateBindingContext {
    /// Package requests: name -> requirement string (e.g. "maya-2024")
    pub request: HashMap<String, String>,
    /// Implicit packages: name -> requirement string
    pub implicits: HashMap<String, String>,
    /// System info: platform, arch, os
    pub system: HashMap<String, String>,
    pub building: bool,
    pub testing: bool,
}

/// Evaluate late-binding source prepared by the package model.
///
/// The prepared source includes the invocation that sets `_result` and any include
/// imports. Runs with `in_context` and `this` (package data) in scope and returns
/// the result as JSON Value.
pub fn eval_prepared_late_binding(
    source: &str,
    field_name: &str,
    pkg_data: &HashMap<String, Value>,
    in_context: bool,
) -> Result<Value, RezError> {
    eval_prepared_late_binding_with_context(source, field_name, pkg_data, in_context, None)
}

/// Evaluate prepared late-binding with full context bindings (request, implicits,
/// system, etc.).
/// Use when in_context=true and the package is in a resolved context.
pub fn eval_prepared_late_binding_with_context(
    source: &str,
    field_name: &str,
    pkg_data: &HashMap<String, Value>,
    in_context: bool,
    ctx_bindings: Option<&LateBindingContext>,
) -> Result<Value, RezError> {
    let source = source.to_owned();
    let field_name = field_name.to_owned();
    let pkg_data = pkg_data.clone();
    let ctx_bindings = ctx_bindings.cloned();

    run_on_py_worker(move |interp| {
        interp.enter(move |vm| -> Result<Value, RezError> {
            let scope = vm.new_scope_with_builtins();

            // Stub for `this` — exposes package data as attributes, plus is_package, is_variant
            let this_class = r#"
class _LateThis:
    def __init__(self, d):
        self._d = d
        self.is_package = d.get("is_package", False) if hasattr(d, "get") else False
        self.is_variant = d.get("is_variant", True) if hasattr(d, "get") else True
    def __getattr__(self, k):
        if k in self._d:
            return self._d[k]
        raise AttributeError("No such attribute '%s'" % k)
"#;
            vm.run_code_obj(
                vm.compile(
                    this_class,
                    vm::compiler::Mode::Exec,
                    "<late_this>".to_owned(),
                )
                .map_err(|e| RezError::Python(format!("compile _LateThis: {:?}", e)))?,
                scope.clone(),
            )
            .map_err(|e| RezError::Python(format!("exec _LateThis: {}", pyerr_str(vm, &e))))?;

            let this_dict = vm.ctx.new_dict();
            for (k, v) in &pkg_data {
                let pv = value_to_pyobj(vm, v);
                let _ = this_dict.set_item(k.as_str(), pv, vm);
            }
            if in_context && ctx_bindings.is_some() {
                let _ = this_dict.set_item("is_package", vm.ctx.new_bool(false).into(), vm);
                let _ = this_dict.set_item("is_variant", vm.ctx.new_bool(true).into(), vm);
            }
            let late_this = scope.globals.get_item("_LateThis", vm).map_err(|e| {
                RezError::Python(format!("_LateThis not in scope: {}", pyerr_str(vm, &e)))
            })?;
            let this_instance = late_this
                .call((this_dict,), vm)
                .map_err(|e| RezError::Python(format!("_LateThis(): {}", pyerr_str(vm, &e))))?;
            scope
                .globals
                .set_item("this", this_instance.clone(), vm)
                .map_err(|e| RezError::Python(format!("set this: {}", pyerr_str(vm, &e))))?;

            // in_context — inject as Python lambda
            let in_context_code = if in_context {
                "in_context = lambda: True"
            } else {
                "in_context = lambda: False"
            };
            vm.run_code_obj(
                vm.compile(
                    in_context_code,
                    vm::compiler::Mode::Exec,
                    "<in_context>".to_owned(),
                )
                .map_err(|e| RezError::Python(format!("compile in_context: {:?}", e)))?,
                scope.clone(),
            )
            .map_err(|e| RezError::Python(format!("exec in_context: {}", pyerr_str(vm, &e))))?;

            // When in_context=true, inject request, implicits, system, building, testing
            if let Some(ref ctx) = ctx_bindings {
                let request_dict = vm.ctx.new_dict();
                for (k, v) in &ctx.request {
                    let _ =
                        request_dict.set_item(k.as_str(), vm.ctx.new_str(v.as_str()).into(), vm);
                }
                scope
                    .globals
                    .set_item("request", request_dict.into(), vm)
                    .map_err(|e| RezError::Python(format!("set request: {}", pyerr_str(vm, &e))))?;

                let implicits_dict = vm.ctx.new_dict();
                for (k, v) in &ctx.implicits {
                    let _ =
                        implicits_dict.set_item(k.as_str(), vm.ctx.new_str(v.as_str()).into(), vm);
                }
                scope
                    .globals
                    .set_item("implicits", implicits_dict.into(), vm)
                    .map_err(|e| {
                        RezError::Python(format!("set implicits: {}", pyerr_str(vm, &e)))
                    })?;

                let system_dict = vm.ctx.new_dict();
                for (k, v) in &ctx.system {
                    let _ = system_dict.set_item(k.as_str(), vm.ctx.new_str(v.as_str()).into(), vm);
                }
                scope
                    .globals
                    .set_item("system", system_dict.into(), vm)
                    .map_err(|e| RezError::Python(format!("set system: {}", pyerr_str(vm, &e))))?;

                scope
                    .globals
                    .set_item("building", vm.ctx.new_bool(ctx.building).into(), vm)
                    .map_err(|e| {
                        RezError::Python(format!("set building: {}", pyerr_str(vm, &e)))
                    })?;
                scope
                    .globals
                    .set_item("testing", vm.ctx.new_bool(ctx.testing).into(), vm)
                    .map_err(|e| RezError::Python(format!("set testing: {}", pyerr_str(vm, &e))))?;
            }

            // Exec the source
            let code_obj = vm
                .compile(&source, vm::compiler::Mode::Exec, "<late>".to_owned())
                .map_err(|e| RezError::Python(format!("compile late {}: {:?}", field_name, e)))?;
            vm.run_code_obj(code_obj, scope.clone()).map_err(|e| {
                RezError::Python(format!("exec late {}: {}", field_name, pyerr_str(vm, &e)))
            })?;

            let result = scope.globals.get_item("_result", vm).map_err(|e| {
                RezError::Python(format!("_result not in scope: {}", pyerr_str(vm, &e)))
            })?;
            pyobj_to_value(vm, &result, &format!("late-bound field {field_name:?}"))
        })
    })
}

// --- Internal helpers ---

/// Convert serde_json::Value → RustPython PyObjectRef (for injecting globals)
fn value_to_pyobj(vm: &vm::VirtualMachine, v: &Value) -> vm::PyObjectRef {
    match v {
        Value::Null => vm.ctx.none(),
        Value::Bool(b) => vm.ctx.new_bool(*b).into(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                vm.ctx.new_int(i).into()
            } else if let Some(f) = n.as_f64() {
                vm.ctx.new_float(f).into()
            } else {
                vm.ctx.none()
            }
        }
        Value::String(s) => vm.ctx.new_str(s.as_str()).into(),
        Value::Array(arr) => {
            let items: Vec<vm::PyObjectRef> = arr.iter().map(|x| value_to_pyobj(vm, x)).collect();
            vm.ctx.new_list(items).into()
        }
        Value::Object(obj) => {
            let dict = vm.ctx.new_dict();
            for (k, v) in obj {
                let pv = value_to_pyobj(vm, v);
                let _ = dict.set_item(k.as_str(), pv, vm);
            }
            dict.into()
        }
    }
}

/// Convert RustPython PyObjectRef → serde_json::Value (recursive).
fn pyobj_to_value(
    vm: &vm::VirtualMachine,
    obj: &vm::PyObjectRef,
    path: &str,
) -> Result<Value, RezError> {
    let cls_name = obj.class().name().to_string();
    // None
    if vm.is_none(obj) {
        return Ok(Value::Null);
    }
    // Bool (check before int — bool is subclass of int in Python)
    if cls_name == "bool" {
        if let Some(pyint) = obj.downcast_ref::<vm::builtins::PyInt>() {
            let zero: i64 = 0;
            return Ok(Value::Bool(pyint.as_bigint() != &zero.into()));
        }
    }
    // JSON integers support the exact signed i64 and unsigned u64 ranges.
    if cls_name == "int" {
        if let Some(pyint) = obj.downcast_ref::<vm::builtins::PyInt>() {
            let value = pyint.as_bigint();
            if let Ok(n) = i64::try_from(value.clone()) {
                return Ok(Value::Number(n.into()));
            }
            if let Ok(n) = u64::try_from(value.clone()) {
                return Ok(Value::Number(n.into()));
            }
            return Err(RezError::Python(format!(
                "Python integer at {path} is outside the JSON integer range: {value}"
            )));
        }
    }
    // serde_json numbers are finite; reject NaN and infinities instead of stringifying them.
    if cls_name == "float" {
        if let Some(pyfloat) = obj.downcast_ref::<vm::builtins::PyFloat>() {
            let value = pyfloat.to_f64();
            return serde_json::Number::from_f64(value)
                .map(Value::Number)
                .ok_or_else(|| {
                    RezError::Python(format!(
                        "Non-finite Python float at {path} cannot be represented in package metadata"
                    ))
                });
        }
    }
    // String
    if cls_name == "str" {
        if let Some(pystr) = obj.downcast_ref::<vm::builtins::PyStr>() {
            let value = pystr.to_str().ok_or_else(|| {
                RezError::Python(format!(
                    "Python string at {path} contains surrogate code points and cannot be represented",
                ))
            })?;
            return Ok(Value::String(value.to_owned()));
        }
    }
    // List → Array
    if let Some(list) = obj.downcast_ref::<vm::builtins::PyList>() {
        let items = list.borrow_vec();
        return items
            .iter()
            .enumerate()
            .map(|(index, item)| pyobj_to_value(vm, item, &format!("{path}[{index}]")))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array);
    }
    // Tuple → Array
    if let Some(tuple) = obj.downcast_ref::<vm::builtins::PyTuple>() {
        return tuple
            .iter()
            .enumerate()
            .map(|(index, item)| pyobj_to_value(vm, item, &format!("{path}[{index}]")))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array);
    }
    // Dict → Object
    if let Some(dict) = obj.downcast_ref::<vm::builtins::PyDict>() {
        let mut map = Map::new();
        for (key, value) in dict {
            let key = pyobj_to_str(vm, &key).map_err(|error| {
                RezError::Python(format!(
                    "Python dictionary key at {path} cannot be represented: {}",
                    pyerr_str(vm, &error)
                ))
            })?;
            map.insert(
                key.clone(),
                pyobj_to_value(vm, &value, &format!("{path}[{key:?}]"))?,
            );
        }
        return Ok(Value::Object(map));
    }
    // Preserve supported string conversion without silently replacing failed conversions with None.
    pyobj_to_str(vm, obj).map(Value::String).map_err(|error| {
        RezError::Python(format!(
            "Python object at {path} cannot be represented: {}",
            pyerr_str(vm, &error)
        ))
    })
}

/// Check if object is a Python module
fn is_module(vm: &vm::VirtualMachine, obj: &vm::PyObjectRef) -> bool {
    let _ = vm;
    obj.class().name().to_string() == "module"
}

/// Check if object is a callable we want to skip (functions, classes, types)
fn is_callable_skip(vm: &vm::VirtualMachine, obj: &vm::PyObjectRef) -> bool {
    let cls = obj.class().name().to_string();
    // Skip function/builtin_function/type — but keep regular data
    matches!(
        cls.as_str(),
        "function" | "builtin_function_or_method" | "type" | "classmethod" | "staticmethod"
    ) && {
        let _ = vm;
        true
    }
}

/// Extract Python exception as a string
fn pyerr_str(vm: &vm::VirtualMachine, exc: &vm::PyRef<vm::builtins::PyBaseException>) -> String {
    let obj: vm::PyObjectRef = exc.clone().into();
    if let Ok(s) = pyobj_to_str(vm, &obj) {
        let cls = obj.class().name().to_string();
        format!("{}: {}", cls, s)
    } else {
        format!("{:?}", exc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rex_environment_callback_is_scoped_to_each_worker_job() {
        fn query_one(
            actions: &Value,
            state: &Value,
            query: Option<Value>,
        ) -> Result<Value, RezError> {
            assert_eq!(actions, &serde_json::json!(["action"]));
            assert_eq!(state, &serde_json::json!({"state": true}));
            assert_eq!(query, Some(serde_json::json!("name")));
            Ok(serde_json::json!("first"))
        }
        fn query_two(_: &Value, _: &Value, _: Option<Value>) -> Result<Value, RezError> {
            Ok(serde_json::json!("second"))
        }
        let code = "result = rex_environ(['action'], {'state': True}, 'name')";
        for (query, expected) in [
            (query_one as RexEnvironmentQuery, "first"),
            (query_two as RexEnvironmentQuery, "second"),
            (query_one as RexEnvironmentQuery, "first"),
        ] {
            let result =
                exec_py_globals_for_rex_with_query(code, "<query>", None, &[], query).unwrap();
            assert_eq!(result["result"], serde_json::json!(expected));
        }
        let error = exec_py_globals_for_rex(code, "<missing-query>", None, &[]).unwrap_err();
        assert!(error.to_string().contains("environment query callback"));
    }

    #[test]
    fn rex_environment_callback_preserves_python_exception_value() {
        fn missing(_: &Value, _: &Value, query: Option<Value>) -> Result<Value, RezError> {
            assert_eq!(query, None);
            Err(RezError::RexUndefinedVariable("MISSING".to_owned()))
        }
        let result = exec_py_globals_for_rex_with_query(
            "from rez.exceptions import RexUndefinedVariableError\ntry:\n    rex_environ([], {})\nexcept RexUndefinedVariableError as error:\n    result = error.value\nelse:\n    raise AssertionError('missing exception')",
            "<missing-variable>",
            None,
            &[],
            missing,
        )
        .unwrap();
        assert_eq!(result["result"], serde_json::json!("MISSING"));
    }

    #[test]
    fn prepared_late_binding_preserves_package_and_context_bindings() {
        let package = HashMap::from([("name".to_owned(), serde_json::json!("example"))]);
        let context = LateBindingContext {
            request: HashMap::from([("tool".to_owned(), "tool-2".to_owned())]),
            implicits: HashMap::new(),
            system: HashMap::from([("platform".to_owned(), "windows".to_owned())]),
            building: true,
            testing: false,
        };
        let result = eval_prepared_late_binding_with_context(
            "_result = [this.name, in_context(), request['tool'], system['platform'], building, testing]",
            "requires",
            &package,
            true,
            Some(&context),
        )
        .unwrap();
        assert_eq!(
            result,
            serde_json::json!(["example", true, "tool-2", "windows", true, false])
        );
    }

    #[test]
    fn test_frozen_rez_exception_module_contract() {
        with_py_stack(|| {
            let interpreter = interpreter_builder(None).interpreter();
            interpreter.enter(|vm| {
                vm.run_string(
                    vm.new_scope_with_builtins(),
                    r#"
import rez
from rez import exceptions
from rez.exceptions import (
    RezError, ResourceError, ResourceContentError, PackageMetadataError,
    BuildError, BuildSystemError, BuildContextResolveError,
    ReleaseError, ReleaseVCSError, RexError, RexStopError,
    RexUndefinedVariableError, RezGuiQTImportError, convert_errors,
)
assert rez.__spec__.origin == "frozen"
assert exceptions.__spec__.origin == "frozen"
assert hasattr(rez, "__path__")
assert issubclass(RexStopError, RexError)
assert issubclass(RexUndefinedVariableError, RexError)
assert issubclass(RexError, RezError)
assert issubclass(PackageMetadataError, ResourceContentError)
assert issubclass(ResourceContentError, ResourceError)
assert issubclass(BuildSystemError, BuildError)
assert issubclass(ReleaseVCSError, ReleaseError)
assert issubclass(RezGuiQTImportError, ImportError)
error = PackageMetadataError("invalid", path="package.py", resource_key="variant")
assert str(error) == "resource type: 'variant': package definition file: package.py: invalid"
from types import SimpleNamespace
error = BuildContextResolveError(SimpleNamespace(status="failed", failure_description="unsolved"))
assert error.context.status == "failed"
assert str(error) == "The build environment could not be resolved:\nunsolved"
try:
    with convert_errors(ValueError, RexError, "command"):
        raise ValueError("bad value")
except RexError as error:
    assert error.value == "command: ValueError: bad value"
else:
    raise AssertionError("convert_errors did not raise")
with convert_errors(ValueError, RexError) as value:
    assert value is None
"#,
                    "<frozen-rez-exceptions>",
                )
                .unwrap();
            });
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_rex_imported_error_subclasses_preserve_identity() {
        for name in ["RexStopError", "RexUndefinedVariableError", "RexError"] {
            let code = format!(
                "from rez.exceptions import {name}\nclass Derived({name}): pass\nraise Derived('message')"
            );
            let error = exec_py_globals_for_rex(&code, "<subclass>", None, &[]).unwrap_err();
            match (name, error) {
                ("RexStopError", RezError::RexStop(message))
                | ("RexUndefinedVariableError", RezError::RexUndefinedVariable(message))
                | ("RexError", RezError::Rex(message)) => assert_eq!(message, "message"),
                (_, error) => panic!("incorrect conversion for {name}: {error}"),
            }
        }
    }

    #[test]
    fn test_rex_unrelated_exception_name_is_not_reclassified() {
        let error = exec_py_globals_for_rex(
            "class RexStopError(Exception): pass\nraise RexStopError('unrelated')",
            "<unrelated>",
            None,
            &[],
        )
        .unwrap_err();
        assert!(matches!(error, RezError::Python(message) if message.contains("unrelated")));
    }

    #[test]
    fn test_rex_error_policy_is_scoped_and_configurable() {
        for catch in [false, true] {
            let inject = HashMap::from([(
                "_rex_data".to_owned(),
                serde_json::json!({"catch_rex_errors": catch}),
            )]);
            let error = exec_py_globals_for_rex(
                "raise ValueError('runtime failure')",
                "<policy>",
                Some(&inject),
                &[],
            )
            .unwrap_err();
            assert_eq!(matches!(error, RezError::Rex(_)), catch);
            if !catch {
                assert!(matches!(error, RezError::Python(_)));
            }
            let error =
                exec_py_globals_for_rex("invalid Python syntax!", "<policy>", Some(&inject), &[])
                    .unwrap_err();
            assert!(matches!(error, RezError::Rex(_)));
        }
        let error = exec_py_globals(
            "raise ValueError('runtime failure')",
            "<generic>",
            None,
            &[],
        )
        .unwrap_err();
        assert!(matches!(error, RezError::Python(_)));
    }

    #[test]
    fn test_object_string_conversion_preserves_errors() {
        let error = exec_py_globals(
            "class Broken:\n    def __str__(self):\n        raise ValueError('broken metadata')\nmetadata = {'nested': [Broken()]}\n",
            "<broken-metadata>", None, &[],
        ).unwrap_err();
        assert!(matches!(error, RezError::Python(message)
            if message.contains("metadata") && message.contains("nested")
                && message.contains("broken metadata")));
        let result = exec_py_globals(
            "class Text:\n    def __str__(self):\n        return 'supported'\nmetadata = {'nested': [Text(), None]}\n",
            "<valid-metadata>", None, &[],
        ).unwrap();
        assert_eq!(
            result["metadata"],
            serde_json::json!({"nested": ["supported", null]})
        );
    }

    #[test]
    fn test_exec_basic_globals() {
        let code = r#"
name = "test_pkg"
version = "1.0.0"
x = 42
pi = 3.14
flag = True
nothing = None
"#;
        let result = exec_py_globals(code, "test.py", None, &[]).unwrap();
        assert_eq!(result["name"], Value::String("test_pkg".into()));
        assert_eq!(result["version"], Value::String("1.0.0".into()));
        assert_eq!(result["x"], Value::Number(42.into()));
        assert_eq!(result["flag"], Value::Bool(true));
        assert_eq!(result["nothing"], Value::Null);
    }

    #[test]
    fn test_exec_list_and_dict() {
        let code = r#"
items = ["a", "b", "c"]
meta = {"key": "val", "num": 10}
"#;
        let result = exec_py_globals(code, "test.py", None, &[]).unwrap();
        let items = result["items"].as_array().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], Value::String("a".into()));
        let meta = result["meta"].as_object().unwrap();
        assert_eq!(meta["key"], Value::String("val".into()));
        assert_eq!(meta["num"], Value::Number(10.into()));
    }

    #[test]
    fn test_exec_excludes_and_dunders() {
        let code = r#"
name = "pkg"
_private = "hidden"
helper = "keep"
"#;
        let result = exec_py_globals(code, "test.py", None, &["helper"]).unwrap();
        assert!(result.contains_key("name"));
        assert!(result.contains_key("_private")); // single underscore kept
        assert!(!result.contains_key("helper")); // excluded
        assert!(!result.contains_key("__builtins__")); // dunder skipped
    }

    #[test]
    fn test_exec_skips_functions() {
        let code = r#"
name = "pkg"
def my_func():
    pass
version = "1.0"
"#;
        let result = exec_py_globals(code, "test.py", None, &[]).unwrap();
        assert!(result.contains_key("name"));
        assert!(result.contains_key("version"));
        assert!(!result.contains_key("my_func")); // function skipped
    }

    #[test]
    fn test_exec_with_imports() {
        // Verifies stdlib works
        let code = r#"
import os
platform = os.name
"#;
        let result = exec_py_globals(code, "test.py", None, &[]).unwrap();
        // os.name is "nt" on Windows, "posix" on Unix
        assert!(result.contains_key("platform"));
        let plat = result["platform"].as_str().unwrap();
        assert!(plat == "nt" || plat == "posix");
        // os module itself should be filtered out
        assert!(!result.contains_key("os"));
    }

    #[test]
    fn test_exec_inject_globals() {
        let mut inject = HashMap::new();
        inject.insert("rez_version".into(), Value::String("2.0.0".into()));
        inject.insert("__file__".into(), Value::String("/tmp/test.py".into()));

        let code = r#"
name = "cfg"
ver = rez_version
"#;
        let result = exec_py_globals(code, "test.py", Some(&inject), &["rez_version"]).unwrap();
        assert_eq!(result["name"], Value::String("cfg".into()));
        assert_eq!(result["ver"], Value::String("2.0.0".into()));
        assert!(!result.contains_key("rez_version")); // excluded
        assert!(!result.contains_key("__file__")); // dunder skipped
    }

    #[test]
    fn test_exec_nested_structures() {
        let code = r#"
variants = [
    ["python-3.7"],
    ["python-3.8", "numpy-1.19"],
]
tests = {
    "unit": {
        "command": "pytest",
        "requires": ["pytest"],
    },
}
"#;
        let result = exec_py_globals(code, "test.py", None, &[]).unwrap();
        let variants = result["variants"].as_array().unwrap();
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0].as_array().unwrap().len(), 1);
        assert_eq!(variants[1].as_array().unwrap().len(), 2);
        let tests = result["tests"].as_object().unwrap();
        assert!(tests.contains_key("unit"));
    }

    #[test]
    fn test_exec_preserves_json_integer_range() {
        let result = exec_py_globals(
            "min_i64 = -(2 ** 63)\nmax_i64 = 2 ** 63 - 1\nmax_u64 = 2 ** 64 - 1",
            "test.py",
            None,
            &[],
        )
        .unwrap();

        assert_eq!(result["min_i64"], Value::Number(i64::MIN.into()));
        assert_eq!(result["max_i64"], Value::Number(i64::MAX.into()));
        assert_eq!(result["max_u64"], Value::Number(u64::MAX.into()));
    }

    #[test]
    fn test_exec_rejects_integers_outside_json_range() {
        for code in ["value = 2 ** 64", "value = -(2 ** 63) - 1"] {
            let result = exec_py_globals(code, "test.py", None, &[]).unwrap_err();
            assert!(result.to_string().contains(r#"global "value""#));
            assert!(result
                .to_string()
                .contains("outside the JSON integer range"));
        }
    }

    #[test]
    fn test_exec_rejects_non_finite_floats() {
        for value in ["nan", "inf", "-inf"] {
            let result =
                exec_py_globals(&format!("value = float({value:?})"), "test.py", None, &[])
                    .unwrap_err();
            assert!(result.to_string().contains("global"));
            assert!(result.to_string().contains("Non-finite Python float"));
        }
    }

    #[test]
    fn test_exec_reports_nested_non_finite_float_path() {
        let result = exec_py_globals(r#"value = {"items": [float("nan")]}"#, "test.py", None, &[])
            .unwrap_err();
        assert!(result.to_string().contains(r#"global "value"["items"][0]"#));
    }

    #[test]
    fn test_exec_syntax_error() {
        let code = "name = \n!!!broken";
        let result = exec_py_globals(code, "bad.py", None, &[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_exec_conditionals() {
        let code = r#"
import sys
if sys.platform == "win32":
    plat = "windows"
else:
    plat = "unix"
name = "test"
"#;
        let result = exec_py_globals(code, "test.py", None, &[]).unwrap();
        assert!(result.contains_key("plat"));
        assert!(result.contains_key("name"));
    }

    #[test]
    fn test_generic_intersects_accepts_only_requirement_strings() {
        let result = exec_py_globals_for_rex(
            "assert intersects('foo-1', '1.2')\nassert not intersects('!foo-1', '1')\nclass _VersionBinding:\n    def __str__(self): return '1'\ntry:\n    intersects(_VersionBinding(), '1')\nexcept RuntimeError:\n    accepted = True\nelse:\n    raise AssertionError('same-named fake binding accepted')\n",
            "<generic-intersects>", None, &[],
        ).unwrap();
        assert_eq!(result["accepted"], Value::Bool(true));
    }

    #[test]
    fn test_rex_intersects_propagates_surrogate_conversion_error() {
        let error = exec_py_globals_for_rex(
            r#"result = intersects("\ud800", ">=1")"#,
            "test.rex",
            None,
            &[],
        )
        .unwrap_err();

        assert!(error.to_string().contains("surrogate code points"));
    }

    #[test]
    fn test_exec_code_fn() {
        // Test the exec_code function
        let result = exec_code("x = 1 + 1");
        assert!(result.is_ok());
    }
}
