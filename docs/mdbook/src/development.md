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

[The workflow](https://github.com/ssoj13/rez-rs/blob/main/.github/workflows/ci.yml) runs on pushes, pull requests, and manual dispatch. Source checks run on Ubuntu; the release binary and runtime acceptance run on Windows x86_64. Ordinary workspace tests exclude explicitly ignored native scenarios, and this workflow does not establish GUI interaction or Linux/macOS runtime acceptance.

The default GUI currently needs four private repositories: `ssoj13/nodes-rs`, `ssoj13/box-rs`, `ssoj13/wgpu-widgets-rs`, and `ssoj13/graph-layout-rs`. Trusted CI uses a separate read-only deploy key for each repository, stored as `DEPENDENCY_NODES_SSH_KEY`, `DEPENDENCY_BOX_SSH_KEY`, `DEPENDENCY_WIDGETS_SSH_KEY`, and `DEPENDENCY_LAYOUT_SSH_KEY` in rez-rs Actions Secrets. Supply all four together. The fetch helper maps each exact dependency URL to its own SSH identity, verifies GitHub's pinned public host keys from `ci/github_known_hosts`, and removes credentials before subsequent build steps. Public dependencies use HTTPS; the lockfile remains unchanged.

Alternatively, provide a fine-grained token with **Contents: Read** access to those four repositories as `DEPENDENCIES_TOKEN`. Its temporary HTTPS credential helper only answers requests for github.com. Private key files and token helpers live in a temporary directory removed when Cargo exits, including failure paths. The four key values and the token are removed from Cargo's environment in SSH mode; no private credential is stored in source or Cargo configuration. Host-key pins were obtained from GitHub's HTTPS `/meta` endpoint. Update them explicitly after verifying a GitHub host-key rotation.

Fork pull requests run formatting, Python helper tests, and the book build. Full Windows builds run on pushes, manual dispatch, and pull requests from this repository.

The Windows job runs `python ci/test_workspace.py --require-short-path` for full locked release workspace tests in normal and genuine Windows 8.3 temporary-path environments, then strict Clippy. It builds and stages the final binaries afterward through `python bootstrap.py p --force`, so packaging uses the post-test extension output. `python ci/verify_dist.py --require-python` checks source archive membership, bytes and CRCs, canonical installer helpers, binary equality, a relocated frozen interpreter, generated configuration, quickstart with no host Python on PATH, and a bound installed Python consumer.

Cargo cache restore and save use separate steps. Trusted non-PR jobs attempt to save the compiled cache after the workspace attempt, including runtime-test failures. This preserves a completed cold compilation for later runs; a cache hit or successful cache save does not establish test or artifact acceptance.

PowerShell process fixtures set both `$OutputEncoding` (native stdin pipes) and `[Console]::OutputEncoding` (console output) to UTF-8 without a BOM. Setting console encoding alone does not control bytes sent through a native stdin pipe. A separate regression explicitly selects caller UTF-8 encodings with and without a BOM, compares rendered-command stdin bytes with an ordinary native invocation, and verifies that rendering preserves the caller's `$OutputEncoding` object. It checks Unicode and the raw byte comparison before asserting the baseline BOM prefix; production shell rendering is unchanged.

The separate Python archive under `dist/python/<version>` includes `rez/rs.pyd` and the compatibility facade. `python ci/verify_python_api.py` checks its exported bytes and runs acceptance after extraction in an isolated CPython process. CI tests the same archive on CPython 3.13 and 3.10; download `windows-python` for those files. See [Python API ownership and supported imports](python-api.md).

Download the `windows-x86_64` artifact from a successful Actions run. It contains the verified `rez.exe` from `dist/rez_rs/<version>`, `rez.exe.sha256`, and `verification.json`. Verify the executable in PowerShell:

```powershell
(Get-FileHash ./rez.exe -Algorithm SHA256).Hash
Get-Content ./rez.exe.sha256
```

For a release, update the Cargo package version and lockfile, commit the change, and push a matching tag, such as `v0.1.0`. After all gates pass, the release job publishes the executable and Python archive, their checksums, and receipts. Update the `rez-python-api` crate version together with the root Cargo package version. A tag such as `v0.1.0-alpha.1` requires the same prerelease version in Cargo and creates a GitHub prerelease.

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
