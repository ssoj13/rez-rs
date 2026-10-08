# Design Decisions

This chapter documents the key technical decisions made during rez-rs development and the rationale behind each one. Understanding these decisions helps contributors avoid re-debating settled questions and helps users understand why things work the way they do. The RustPython 0.4 details below describe the original implementation; the current dependency is RustPython 0.6.0. Early measurements and implementation notes are historical, not current benchmark claims. The current ownership map is in [crate architecture](architecture-crates.md).

## Why Embedded RustPython Instead of Text Parsing

**Decision**: Use RustPython 0.4 to execute package.py files instead of writing a text parser.

**Alternatives considered**:

1. **Regex/text parser**: Parse the Python source text to extract variable assignments. Simpler, no dependency on RustPython.
2. **Embedded CPython**: Link against libpython and use the C API.
3. **Subprocess**: Shell out to an external Python interpreter.
4. **Restrict to YAML/TOML only**: Drop package.py support entirely.

**Why RustPython won**:

- **package.py is real Python**. Studios have packages with `import os`, `@early` decorators, conditional requires, loops generating variants, and `include()` calls. A text parser cannot handle this without reimplementing a significant subset of the Python language.
- **No external dependency**. Embedding RustPython means the binary is self-contained. Subprocess would require Python on every machine (defeating the purpose of the port). CPython would require libpython.so/python3.dll.
- **Compile-time integration**. RustPython compiles from Rust source, integrating into the same binary. No FFI, no dynamic linking, no version compatibility issues.
- **Cost**: Embedding the runtime increases binary size and compilation work. Measure initialization and command latency with reproducible workloads before making performance claims.

**Key learnings during implementation**:

- RustPython 0.4 requires the `_io` module (not `io`) for FileIO setup
- 8 MB stack thread required for stdlib imports (default 1-2 MB overflows)
- `init_stdlib()` does not set up `sys.stdout` -- manual init via `_io.TextIOWrapper` needed
- The `with_py_stack()` helper was created to handle the stack requirement consistently

## Why Single Binary (Not Daemon/Server)

**Decision**: Compile to a single binary with no background daemon process.

**Alternatives considered**:

1. **Client/server**: A rez daemon preloads package databases and caches resolves. Clients talk to it via IPC.
2. **Shared library**: A `librez.so/rez.dll` loaded by a thin CLI wrapper.

**Why single binary won**:

- **Simplicity**. No daemon lifecycle management, no IPC protocol, no port conflicts, no zombie processes. Drop the binary on a machine and it works.
- **Statelessness**. Each invocation reads config, discovers packages, resolves, and exits. No stale caches, no daemon restart after config changes.
- **Deployment**. Render farm nodes, CI runners, and artist workstations all get the same file. No service installation, no init scripts, no systemd units.
- **Independent invocations**. A CLI process can execute a command without managing a persistent daemon. Startup and command latency remain workload-dependent.

## Why Edition 2021 (Not 2024)

**Decision**: Use Rust Edition 2021.

**Rationale**: At the time of development, Edition 2024 was newly stabilized. RustPython 0.4 and several other dependencies had not been tested against the Edition 2024 changes (new lifetime elision rules, `gen` keyword, etc.). Edition 2021 is stable, well-tested, and fully supported by all dependencies. The edition can be bumped later with minimal code changes.

## Why CLI modules import their owning crates

The root package contains the binary and CLI modules, with no library facade. The CLI imports APIs from the functional workspace crates, for example `model::package::Package` and `foundation::errors::Result`.

This keeps library ownership explicit and lets other consumers reuse those libraries without depending on the executable. See [crate architecture](architecture-crates.md).

## Why HashMap<String, serde_json::Value> as Universal Data Type

**Decision**: Use `HashMap<String, serde_json::Value>` as the intermediate representation for package data.

**Alternatives considered**:

1. **Strongly typed structs only**: Parse directly into `Package` struct.
2. **Custom enum**: Define a `RezValue` enum for the intermediate representation.
3. **serde_json::Map<String, Value>**: Use serde_json's native map type.

**Why HashMap + Value won**:

- **Python compatibility**. package.py execution produces arbitrary Python objects. Converting PyObject -> serde_json::Value is straightforward (strings, ints, lists, dicts). Converting PyObject -> strongly typed struct requires knowing all possible field types up front.
- **Schema flexibility**. Packages can have custom fields beyond the standard schema. HashMap preserves unknown fields without loss.
- **Serde ecosystem**. serde_json::Value already has serialization, deserialization, and comparison. No need to write custom impls.
- **from_data() as boundary**. The `Package::from_data(HashMap<String, Value>)` constructor is the type-safety boundary. Before this point, data is loosely typed. After, it is strongly typed. This is a clean separation.

## Why Commands Stored as Raw Python Strings

**Decision**: Store package `commands` as `Option<String>` containing Python source code, not as a pre-parsed AST or action list.

**Rationale**: Commands may contain Python logic (if/for/resolve access). Storing the raw source preserves the ability to execute this logic at resolve time via RustPython. Pre-parsing to an action list would only work for the simple cases (`env.X.set("Y")`) and would require a separate code path for complex commands.

The cost of executing commands depends on their Python logic and interpreter initialization; benchmark the actual package workload.

## Why Package Ordering Uses Per-Family Caching

**Decision**: `TimestampPackageOrder` caches version orderings per package family.

**Rationale**: The solver queries package versions many times during backtracking. Without caching, each query would re-sort versions by timestamp. The per-family cache stores the sorted order once and reuses it. This is especially important for large repositories where a package family might have hundreds of versions.

The `PackageOrderList` has a manual `Debug` impl because it contains `Box<dyn PackageOrder>`, which does not automatically derive Debug. The manual impl prints the list contents without requiring Debug on the trait object.

## Why Blanket Impl for ActionInterpreter on `Box<T>`

**Decision**: Implement `ActionInterpreter for Box<T> where T: ActionInterpreter + ?Sized`.

**Rationale**: The RexExecutor needs to work with `Box<dyn Shell>`, which is a trait object. The Shell trait extends ActionInterpreter. Without the blanket impl, you cannot call ActionInterpreter methods on a `Box<dyn Shell>` directly. The blanket impl delegates all method calls to the inner type, enabling:

```rust
let shell: Box<dyn Shell> = create_shell(ShellType::Bash);
shell.interpret_action(&action)?;  // Works because of blanket impl
```

## Why Resolver Callback Uses take() and Raw Pointer

**Decision**: The resolver callback handling uses `Option::take()` to move the callback out of `self` before invoking it, then restores it after.

**Rationale**: The solver callback needs to be called during the solve loop, but the callback is stored in `self`. Calling `self.callback(...)` while `self` is borrowed mutably for the loop body violates Rust's borrowing rules. The pattern:

```rust
let mut cb = self.callback.take();  // Move out of self
if let Some(ref mut f) = cb {
    let result = f(status);         // Call without borrowing self
}
self.callback = cb;                 // Restore
```

This is a standard Rust pattern for callbacks that need to be called from methods that also mutate other parts of `self`.

## Why 9 Build System Types

**Decision**: Support CMake, Make, Python (setuptools), Pip, Cargo, Node.js, Bun, Custom, and NoOp as build systems.

**Rationale**: The Python rez supports CMake and Python as primary build systems. rez-rs extends this to cover the modern software ecosystem:

- **CMake, Make**: Traditional C/C++ builds, standard in VFX
- **Python (setuptools)**: Python packages, standard in VFX
- **Pip**: Python packages installed via pip, for external PyPI packages
- **Cargo**: Rust packages (rez-rs is itself a Cargo project)
- **Node.js**: Increasingly used for web-based pipeline tools
- **Bun**: Fast Node.js alternative, growing in use
- **Custom**: User-defined build command for anything else
- **NoOp**: Packages with no build step (data packages, config packages)

Detection is priority-based: CMake > Cargo > Python > Bun > Node.js > Make. If both `bun.lockb` and `package.json` exist, Bun is chosen (since Bun generates `package.json` compatibility files).

## Why Not Async

**Decision**: Use synchronous I/O throughout, no async runtime.

**Rationale**: rez-rs is a CLI tool with short-lived processes. The overhead of an async runtime (tokio, async-std) adds binary size, complexity, and compile time. The primary bottleneck is CPU-bound (solver backtracking), not I/O-bound. Filesystem operations are fast on local drives, and even with network filesystems, the number of concurrent I/O operations is small (reading a few dozen package.py files).

If network package repositories or parallel solving are added in the future, async may be reconsidered.

## Why thiserror 2.0 (Not anyhow)

**Decision**: Use `thiserror 2.0` for error definitions, not `anyhow`.

**Rationale**: rez-rs is a library + binary. Library crates should use typed errors so callers can match on specific error variants. `thiserror` generates typed error enums with proper Display and Error impls. `anyhow` is designed for applications where you just want to propagate errors without matching on them. Since the library crate needs typed errors and the binary crate can simply `?`-propagate them, `thiserror` is the better choice.

## Why Unix Paths Internally (RezPath)

**Decision**: Store all paths in Unix format (`/`) internally, convert to OS-native only at the boundary (fs operations, `Command::env`, scripts).

**Rationale**:

- **Consistency**. Config files, .rxt, logs, and CLI output use the same path format across Linux, macOS, and Windows. No mixed `C:\Users\example\.rez/packages` (backslash + forward slash).
- **Portability**. Context files and configs written on Windows can be read on Linux without path format surprises.
- **Single conversion point**. The `RezPath` type has `to_os()` as the only place where native paths are produced. All fs::, subprocess env, and script paths go through this.
- **path-slash crate**. Uses `PathBuf::from_slash()` and `to_slash_lossy()` for reliable conversion. Handles non-Unicode paths via the lossy variant.

Implementation: `config.expand_path()` and `expanded_packages_path()` return `RezPath`. Use `config.expanded_packages_path_os()` for `Vec<PathBuf>` at the fs/Command boundary (repository, provider, build). Callers otherwise invoke `.to_os()` before passing to repository, solver, or filesystem APIs.
