# Why RustPython is bundled

Rez package definitions, configuration, and Rex commands contain executable Python. Rust implements the package manager's native logic; RustPython executes the Python parts without requiring a separately installed interpreter to run the CLI.

The application-facing adapter is [python-runtime](https://github.com/ssoj13/rez-rs/blob/main/crates/rez/python-runtime/Cargo.toml), one of the functional crates under `crates/rez/`. It pins `rustpython = "=0.6.0"` with both `stdlib` and `freeze-stdlib`. Most runtime dependencies come from crates.io.

## Why local crates remain

The root [Cargo manifest](https://github.com/ssoj13/rez-rs/blob/main/Cargo.toml) overrides four released RustPython components through `[patch.crates-io]`. These are third-party sources with retained licenses and documented changes.

| Local crate | Remaining purpose |
|---|---|
| [rustpython](https://github.com/ssoj13/rez-rs/blob/main/crates/rustpython/PATCHES.md) | Redirected stdin, inspection/REPL behavior, CLI metadata, stream and exit handling; compatible parser and registry-stdlib pins. |
| [rustpython-vm](https://github.com/ssoj13/rez-rs/blob/main/crates/rustpython-vm/PATCHES.md) | Watch the packaged frozen-module source tree during builds; release-compatible Ruff and registry-SRE pins. |
| [rustpython-common](https://github.com/ssoj13/rez-rs/blob/main/crates/rustpython-common/PATCHES.md) | Correct buffered zlib decompression, output limits, dictionary handling and input state. |
| [rustpython-host_env](https://github.com/ssoj13/rez-rs/blob/main/crates/rustpython-host_env/PATCHES.md) | Certificate self-signature verification and TLS receive-progress guards; scoped lint repair. |

Removing these overrides would also remove active fixes. Do not infer that a released crate passes these consumers merely because its version matches.

## Registry stdlib and SRE — 2026-10-07

`rustpython-stdlib` and `rustpython-sre_engine` now resolve to crates.io 0.6.0. Their path overrides and local source copies are removed. The owning runtime manifests pin these dependencies to `=0.6.0`; the root also declares SRE as a test dependency.

Before replacement, all 82 stdlib production files (including snapshots and its build script) and all four SRE production files matched the published release byte for byte. The stdlib manifest was identical; SRE's only manifest addition registered our prefix regression. The [stdlib history](rustpython-stdlib-history.md) and [SRE history](rustpython-sre-history.md) retain the provenance.

The two prefix regressions now live in [tests/sre_prefix.rs](https://github.com/ssoj13/rez-rs/blob/main/tests/sre_prefix.rs) and run against the registry crate. The [embedded corpus](https://github.com/ssoj13/rez-rs/blob/main/tests/sre_compatibility.rs) retains 1,566 reference cases across seven APIs. Existing Python relocation, reentry, compression and TLS consumers check the combined runtime with the four active patches.

Cargo.lock changes only the two dependency sources/checksums and the root test dependency. Other locked package versions and dependency edges are preserved, including Ruff 0.16.5 and malachite-bigint 0.12.0. A fresh offline, locked, default-feature Windows release build passes all 27 tests across six targets: compression (1), Python relocation (3), reentry (11), the embedded SRE corpus (1), prefix capture (2), and TLS framing/certificate behavior (9). The corpus test checks 1,566 Python cases; those cases are not additional Rust tests. Workspace formatting and 23 Python tests pass. This is scoped replacement acceptance, not a new full workspace runtime, Clippy, GUI-runtime or cross-platform campaign. Current package/source closure is recorded in `dist/registry-transition-verification.json`; earlier dist receipts retain their original source and binary boundaries.

## Reduce the vendored surface

Compare each local change with the selected release, remove an override only when that release supplies the required behavior, and rerun the affected Python CLI, relocation, compression, regex, and TLS consumers. Keep independent regression tests when removing a source copy.

The current layout groups application crates under `crates/rez/` but leaves the four patched third-party runtime sources at `crates/rustpython*/`. A future layout cleanup can group them under a vendor directory. Such a move also needs updates to path patches, workspace exclusions, independent manifests, documentation links, archive membership, and verification.
