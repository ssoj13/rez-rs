# src/

Root source directory for rez-rs -- a Rust port of the Python rez package manager.
The root package contains the CLI binary (`main.rs` + `cli/`).
CLI subcommands dispatch through a single `clap` enum in `main.rs`. Functional
implementations live in workspace crates under `crates/rez/`; see the
[crate architecture](../docs/mdbook/src/architecture-crates.md).

## Files

| File | Description |
|------|-------------|
| main.rs | Binary crate entry point. Defines `Cli` (clap), `Commands` enum, and the `main()` dispatch. |

The implementation files previously in this directory have these owners. Paths
in the first column are relative to `../crates/rez/`.

| File | Description |
|------|-------------|
| model/src/config.rs | Configuration system. Loads settings from built-in defaults, rezconfig.py/toml, user overrides, and `REZ_*` env vars. Exports global `CONFIG` via `LazyLock`. |
| foundation/src/rez_path.rs | Path handling. `RezPath` stores Unix-format paths; `to_os()` converts at OS boundary (fs, Command). |
| foundation/src/constants.rs | Enums and constants shared across modules: `SolverStatus`, `ResolverStatus`, `VariantSelectMode`, `BuildType`, `PatchLock`, `SuiteVisibility`, package attribute definitions. |
| foundation/src/errors.rs | `RezError` enum (thiserror-derived) covering version, config, package, resolve, shell, build, and Python VM errors. Also defines `Result<T> = std::result::Result<T, RezError>`. |
| model/src/serialise.rs | Package file I/O. Reads `package.py` (via embedded RustPython), `package.yaml`, and `package.toml`. Writes package data in any format. Handles `include()` resolution across `_include/` dirs. |
| repository/src/repository.rs | Package repository abstraction. `PackageRepository` trait, `FsRepo` (reads from disk), `MemoryPackageRepository` (in-memory for tests), `PackageRepositoryManager` (multi-repo aggregation). |
| model/src/platform.rs | Platform detection. `Platform` enum (Windows/Linux/MacOS), `Arch` struct, `OsVersion`, `SystemInfo`. Global `SYSTEM` singleton. |
| python-runtime/src/lib.rs | Embedded RustPython 0.6.0 VM, package lifecycle execution, scripts and REPL. Model and resolve provide their consumer adapters. |
| resolve/src/status.rs | System status reporting. `Status` struct collecting rez version, platform, arch, OS, shell, user, hostname, config paths, and package paths. Used by `rez status`. |
| resolve/src/suite.rs | Suite management. `Suite` struct stores multiple resolved contexts with priority-based tool conflict resolution, aliasing, and wrapper script generation. Serialized as `suite.json`. |
| resolve/src/bundle_context.rs | Context bundling. Copies a resolved context and its package payloads into a self-contained relocatable directory. `BundleOptions` and `BundleResult`. |

The old `command.rs` registry used `RezCommand`, `from_name()` and `ALL`; current
command dispatch lives in `main.rs`.

## Subdirectories

| Directory | Description |
|-----------|-------------|
| [cli/](cli/README.md) | CLI command implementations, grouped into 5 submodules: resolve, query, dev, repo, admin. |

Component guides live alongside their crates:

- [version](../crates/rez/version/README.md): tokens, versions, ranges, bounds, requirements.
- [repository](../crates/rez/repository/README.md): discovery, search, binding, help, maker, operations, and cache; model owns canonical package types and resolve owns package tests.
- [resolve](../crates/rez/resolve/README.md): solver, resolver, resolved contexts, suites, bundles, and status.
- [rex](../crates/rez/rex/README.md): Rex engine, shell backends, and tool wrappers.
- [builders](../crates/rez/build-system/src/builders/README.md): adapters, detection, orchestration, and execution.

## Architecture

```
main.rs  ->  cli/*  ->  crates/rez/* libraries
```

The libraries form a layered dependency graph documented in the
[crate dependency table](../docs/mdbook/src/architecture-crates.md#ownership-and-dependencies).
`version` depends on `foundation`; Python runtime depends on those two crates;
model adds canonical loading, configuration and platform support. Repository
and Rex depend on model, resolve combines their APIs, and build-system/GUI
consume resolve. CLI modules and integration tests import those owning crates
directly, for example `version::Version` and `resolve::ResolvedContext`.

## Key Design Decisions

1. **Single binary**: All commands compile into one `rez` / `rez.exe` executable with embedded Python stdlib.
2. **Embedded Python**: RustPython 0.6.0 VM is embedded for `package.py` parsing and `rezconfig.py` loading, avoiding external Python dependency.
3. **Edition 2021**: Chosen over 2024 for broader ecosystem compatibility.
4. **CLI imports**: CLI modules use the owning workspace crates directly; the root package has no library facade.
5. **Trait objects**: `Box<dyn Shell>`, `Box<dyn BuildSystem>`, `Box<dyn PackageOrder>` for runtime polymorphism where needed.
