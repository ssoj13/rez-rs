# Development guide

The root package builds the CLI. Functional libraries are separate workspace crates; see [crate architecture](architecture-crates.md) for dependency boundaries and ownership.

## Source layout

```text
src/
  main.rs                    CLI dispatch
  cli/                       resolve, query, dev, repo, and admin commands
crates/rez/
  foundation/                shared errors, paths, filesystem operations, constants
  version/                   versions, ranges, requirements
  python-runtime/            embedded interpreter and shared Python execution
  model/                     package types, configuration, serialization
  repository/                discovery, providers, binding, publication, caches
  rex/                       actions, interpreters, shell rendering, wrappers
  resolve/                   solver, contexts, suites, bundles
  build-system/              builders, acquisition, Pip, release hooks
  gui/                       optional egui application
crates/bin-patch/            ELF and Mach-O rewriting
crates/rustpython*/          vendored runtime patches and their separate tests
tests/                      CLI/process/native integration tests
examples/                   package recipes for build adapters
docs/mdbook/                user and developer book
docs/plans/                 current compatibility work queue
packaging/                  canonical staged-package installer source
```

There is no root `src/lib.rs`. CLI modules import types from the owning crates, for example `foundation::errors::Result`, `model::package::Package`, and `resolve::context::ResolvedContext`.

## Build and verify

Use Rust 1.95 or newer and a platform compiler/linker. See [installation](installation.md) for Git dependency access and helper-script prerequisites.

```bash
cargo build --locked --workspace --release
cargo test --locked --workspace --release
cargo fmt --all -- --check
cargo clippy --locked --workspace --release --all-targets -- -D warnings
python -m unittest discover -s tests -p "test_*.py"
```

Use a crate filter for a smaller test scope:

```bash
cargo test --locked -p version
cargo test --locked -p resolve solver
cargo test --locked -p model config
cargo test --locked --test cli_integration
cargo test --locked --workspace --doc
```

Root-only tests do not include moved library targets. Selecting `--workspace` also selects the GUI crate. Vendored runtime patches are excluded workspace members and keep tests under their own manifests. Ignored native builder scenarios need their external toolchains and separate execution.

For interactive development, `cargo check --locked --workspace` avoids linking. Generate API docs with `cargo doc --locked --workspace --no-deps`, or the book with `mdbook build docs/mdbook`.

## GitHub CI and releases

[The workflow](https://github.com/ssoj13/rez-rs/blob/main/.github/workflows/ci.yml) runs on pushes, pull requests from this repository, and manual dispatch. One matrix job builds natively on Windows x86_64, Linux x86_64 (`ubuntu-latest`, glibc) and macOS arm64. Each row runs a single entry point, the same one used locally:

```console
python bootstrap.py ci --target x86_64-pc-windows-msvc --python <extra-python>
```

[`ci.py`](https://github.com/ssoj13/rez-rs/blob/main/ci.py) runs formatting, strict release Clippy, release workspace tests, the Python helper tests and the book build. It then stages the packages through `bootstrap.py package`, after tests, so the staged extension is the final Cargo output, and writes two ZIPs per platform to `dist/release/`:

- `rez-rs-v<version>-<platform>.zip`: `install.py`, its support modules, and `rez_rs/<version>/` (`package.py`, the CLI, `rez-rs.zip` sources), ready for `python install.py --repository-root <repo>`.
- `rez-rs-python-v<version>-<platform>.zip`: the abi3 `rez` package with `rez.rs`.

Both archives are verified after extraction. The CLI ZIP is installed into an empty repository with its own `install.py`; the embedded source archive must equal the current sources; the relocated CLI must run its frozen interpreter, write a configuration, bind and resolve the quickstart packages with no host Python on `PATH`, and then bind and resolve the host Python. The Python ZIP runs the API acceptance suite in isolated interpreters: the build CPython (3.13 in CI) and each `--python` (3.10 in CI). `python bootstrap.py release` runs only the packaging and verification steps.

The GUI needs private repositories (`nodes-rs`, `box-rs`, `wgpu-widgets-rs`, `graph-layout-rs`). CI reads them with one fine-grained token, `DEPENDENCIES_TOKEN`, which has **Contents: Read** on those repositories. A workflow step rewrites `ssh://git@github.com/ssoj13/` to authenticated HTTPS for Git, so `Cargo.lock` is unchanged. Fork pull requests do not receive the secret, so they skip the build.

Deterministic PowerShell process fixtures set `$OutputEncoding` (pipeline text), `[Console]::InputEncoding` (initial redirected native stdin) and `[Console]::OutputEncoding` (console output) to UTF-8 without a BOM. Windows PowerShell's .NET Framework [Process initialization](https://raw.githubusercontent.com/microsoft/referencesource/main/System/services/monitoring/system/diagnosticts/Process.cs) creates the redirected stdin writer with `Console.InputEncoding` and enables `AutoFlush`, so the [StreamWriter](https://raw.githubusercontent.com/microsoft/referencesource/main/mscorlib/system/io/streamwriter.cs) can emit a preamble before any pipeline text; `$OutputEncoding` cannot remove it later.

For a release, update the Cargo package version (and `rez-python-api` with it) and the lockfile, commit, and push a matching tag such as `v0.1.0`. The tag must equal `v` plus the Cargo version. After all three platforms pass, the release job publishes the six ZIPs. A tag such as `v0.1.0-alpha.1` requires the same prerelease version in Cargo and creates a GitHub prerelease.

## Add a CLI command

Choose its group under `src/cli/`, define a Clap argument struct and `run` function, register the module in the group's `mod.rs`, and add dispatch metadata in `src/main.rs`.

```rust
use clap::Args;
use foundation::errors::Result;

#[derive(Args, Debug)]
pub struct ExampleArgs {
    pub package: String,
}

pub fn run(args: &ExampleArgs) -> Result<()> {
    println!("{}", args.package);
    Ok(())
}
```

CLI metadata also supplies native aliases. Check the [CLI installation tests](https://github.com/ssoj13/rez-rs/blob/main/tests/test_cli_install.py) when changing entry-point behavior. Use behavioral tests for parsing, error paths, and command output where they establish the contract.

## Add a build adapter

Build-system implementations live in `crates/rez/build-system/src/builders/`. Read the current `BuildSystem` trait, `BuildContext`, adapter selection, and prepared-publication flow before adding a module. Reuse shared environment selection, filesystem copying, destination authority, and repository publication.

A successful external tool process does not alone establish a publishable package. Native consumers should verify installed payloads after temporary work directories have been removed and should verify that failed rebuilds preserve the existing package.

## Compare upstream behavior

The upstream [Rez repository](https://github.com/AcademySoftwareFoundation/rez) is an external behavioral reference. This project does not include its source checkout or register it as a submodule. Historical comparison receipts used commit `78e26236cdd65bc549db6b4e5782c70fe86ed5db`; obtain that snapshot separately when reproducing those comparisons.

| Upstream source | Rust owner |
|---|---|
| `version/_version.py`, `version/_requirement.py` | `crates/rez/version/src/` |
| `packages.py`, `package_resources.py` | `crates/rez/model/src/package/` |
| `config.py`, `rezconfig.py` | `crates/rez/model/src/config.rs` |
| `serialise.py` | `crates/rez/model/src/serialise.rs` |
| `rex.py`, `rex_bindings.py`, `shells.py` | `crates/rez/rex/src/` and context bindings in `resolve` |
| `solver.py`, `resolver.py`, `resolved_context.py` | `crates/rez/resolve/src/` |
| Repository and package operations | `crates/rez/repository/src/` |
| `build_system.py`, `build_process.py` | `crates/rez/build-system/src/builders/` |

## Conventions

Keep business behavior in its owning library and CLI parsing in the root binary. Shared library errors use `foundation::errors::{Result, RezError}`; add context when propagating filesystem and process failures.

Use standard Rust naming and formatting. Preserve third-party license headers and notices. Tests should compare behavior, including invalid inputs and actual consumers, rather than merely mirror implementation details.

Check the graph's freshness before relying on GitNexus queries, assess callers before changing shared symbols, and refresh the graph after edits. Retain the exact scope of a verification result: compilation, a unit test, a process consumer, and native-platform acceptance establish different things.

[Plan30](https://github.com/ssoj13/rez-rs/blob/main/docs/plans/plan30.md) owns current compatibility work. Previous plans and audit reports are retained only in the separate old repository.
