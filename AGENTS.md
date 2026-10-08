# rez-rs Agent Guide

## Scope and ownership

rez-rs is an experimental Rust port of Rez, developed for personal use. The root Cargo package owns `src/main.rs` and `src/cli/`; it has no library facade. Functional libraries live under `crates/rez/`: foundation, version, python-runtime, model, repository, rex, resolve, build-system, and gui. Platform binary patching lives under `crates/bin-patch`. See [crate architecture](docs/mdbook/src/architecture-crates.md).

Upstream Rez is an external behavioral reference. No Python Rez checkout or submodule is bundled. Four patched third-party RustPython components remain under `crates/rustpython*/`; stdlib and SRE resolve to pinned crates.io 0.6.0. Preserve upstream licenses and documented patches. See [runtime ownership](docs/mdbook/src/rustpython-vendoring.md).

## Active work

[Plan30](docs/plans/plan30.md) is the only active work queue. Prior plans and repository history are retained separately in `old.rez-rs`. Inspect current source and actual consumers before inferring that an implementation is missing from an old receipt.

The current registry replacement has scoped Windows release/runtime acceptance: 27 Rust tests across six Python/SRE/TLS targets and 23 Python helper tests. Earlier migration workspace, native and Clippy receipts are historical. Do not infer full Rez compatibility, GUI runtime, native Unix acceptance, host activation, or external recipe installation from those scopes.

## Verification

```text
cargo test --locked --workspace --release
cargo fmt --all -- --check
cargo clippy --locked --workspace --release --all-targets -- -D warnings
python -m unittest discover -s tests -p "test_*.py"
mdbook build docs/mdbook
```

Root-only Cargo tests omit moved library tests. Ignored native-builder tests require external toolchains and explicit execution. `bootstrap.py b` builds the release workspace; `bootstrap.py p` builds the release CLI and stages its package/source archive. Compilation, runtime, packaging, deployment and platform acceptance are separate gates. Record exact scopes; do not extend a historical binary result to later changes.

## Shared contracts

- Reuse canonical model/config loading, package provenance, shared Rex actions, and the locked repository publisher. Keep CLI parsing at the root.
- Repository version locks use persistent `.lock.<family>-<version>` files. OS ownership is released on unlock/Drop; file presence does not mean an active lock. Do not unlink after each write.
- Embedded RustPython requires `stdlib` and `freeze-stdlib` together and shared `InterpreterBuilder::init_stdlib`. Do not hardcode Python-home paths to repair distribution.
- Preserve original verified Python package bytes, nullable package/cache policy, exact ResourceHandles, selected variant mapping, and metadata-last prepared publication.
- Keep runtime dependencies and ownership changes documented; preserve regression coverage when removing a vendor override.
- Do not launch the external Bootstrap recipe installation queue as part of documentation maintenance.

## GitNexus

Check `graph_status` before trusting queries. Use `query` and `context` to navigate unfamiliar code; run `impact` before editing a symbol and report HIGH/CRITICAL results. Use graph-aware `rename` for symbol renaming. Refresh with `reanalyze` after source edits; `detect_changes` does not re-index. Run `detect_changes` before committing and inspect the affected scope. Incremental refresh does not recompute communities/processes; use full analysis when those inventories matter.

## Documentation and packaging

The book lives under `docs/mdbook`; `docs/build` is generated. `packaging/install.py` is the canonical staged-release installer; root `cli_install.py` and `system_bind.py` are its support sources. Edit canonical files, then regenerate and verify package membership, member bytes/CRC, helper equality and exclusions. Keep local environment files, editor/agent state, VCS data and build output out of source exports.
