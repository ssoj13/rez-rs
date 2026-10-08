# Crate architecture

The root `rez-rs` package builds the `rez` CLI. Functional libraries are grouped under `crates/rez/`; most Cargo package names have no `rez-` prefix. The CPython bridge is the `rez-python-api` package and exports the `rs` native library. CLI code and tests import these libraries directly.

## Ownership and dependencies

The following table lists direct normal dependencies between the functional crates. Development-only dependencies, external dependencies and platform-specific binary patch crates are omitted.

| Crate | Owns | Direct local dependencies |
|---|---|---|
| `foundation` | Errors, constants, paths, logging, filesystem utilities | None |
| [version](https://github.com/ssoj13/rez-rs/blob/main/crates/rez/version/README.md) | Version tokens, ordering, ranges, bounds, requirements | `foundation` |
| `python-runtime` | Serialized RustPython VM access, frozen stdlib, package lifecycle execution | `foundation`, `version` |
| `model` | Canonical Package/Variant types, filtering/ordering, config, platform, environment, install routing, validation and serialization | `foundation`, `version`, `python-runtime` |
| [repository](https://github.com/ssoj13/rez-rs/blob/main/crates/rez/repository/README.md) | Repository providers and provenance, discovery/search, binding, publication/locks, copy/move, payload cache | `foundation`, `version`, `model` |
| [rex](https://github.com/ssoj13/rez-rs/blob/main/crates/rez/rex/README.md) | Rex actions, shared action decoding, shell interpreters, wrapper generation | `foundation`, `version`, `model` |
| [resolve](https://github.com/ssoj13/rez-rs/blob/main/crates/rez/resolve/README.md) | Solver/resolver, resolved contexts, suites, bundles, status, package tests, optional AMQP | `foundation`, `version`, `model`, `python-runtime`, `repository`, `rex` |
| [build-system](https://github.com/ssoj13/rez-rs/blob/main/crates/rez/build-system/src/builders/README.md) | Build orchestration/adapters, download/extraction, Pip, release hooks | `foundation`, `version`, `model`, `python-runtime`, `repository`, `rex`, `resolve` |
| `python-api` (`rez-python-api`) | CPython extension `rez.rs` and initial Rez-compatible Python facade | `foundation`, `version`, `model`, `repository`, `resolve` |
| `gui` | Package browser, dependency graph, tree view, solve/export panels | `foundation`, `version`, `model`, `repository`, `resolve` |

Dependencies flow toward lower layers. `foundation` and `version` support the Python runtime and model; repository and Rex depend on the model; resolve combines those components; build-system and GUI consume resolve. No functional crate depends on the root CLI package.

`model` owns loading and validation as well as data types. Package/config serialization and Python execution share this canonical layer. `PackageProvider` and candidate provenance belong to `repository`, allowing the solver to consume repository data without making repository operations depend on resolve.

The Python runtime has no dependency on model or resolve. Resolve supplies the Rex preamble callback for each call through its Python adapter. Package loading can use the lower runtime directly; context execution adds resolve's bindings. Rex owns the shared action representation and interpreters used by context and shell consumers.

## Imports and features

The root package contains [src/main.rs](https://github.com/ssoj13/rez-rs/blob/main/src/main.rs) and [src/cli/](https://github.com/ssoj13/rez-rs/blob/main/src/cli/README.md), with no library facade. Import APIs from their owning crates, for example `version::Version`, `model::package::Package`, `repository::package::discover::iter_packages`, `rex::Action`, `resolve::ResolvedContext`, and `build_system::builders::BuildProcess`. Package tests belong to `resolve::package::test`.

Cargo converts hyphens to underscores in Rust crate names: `python-runtime` becomes `python_runtime`, and `build-system` becomes `build_system`. Mechanical namespace aliases inside component crates are private implementation details. Public APIs do not forward compatibility imports from other components; consumers import types and functions from their owning crate.

The root `gui` feature remains enabled by default and activates the optional GUI dependency. Root `amqp` forwards to resolve and build-system. ELF and Mach-O patch crates remain workspace members under `crates/bin-patch`; resolve uses the relevant patcher on Linux or macOS. Vendored RustPython patch crates retain their workspace exclusions and root `[patch.crates-io]` entries.

## Build and test commands

Run these commands from the repository root:

```console
python bootstrap.py b
python bootstrap.py p
cargo test --workspace
cargo test -p version --lib
cargo test -p model --lib config
cargo test -p repository --lib
cargo test -p resolve --lib
cargo test -p build-system --lib
cargo test --test cli_integration
```

`bootstrap.py b` builds the release workspace. `bootstrap.py p` builds the release CLI and CPython bridge, then prepares separate CLI/source and Python distributions. See [Python API](python-api.md). The executable remains `rez` / `rez.exe`, and the root package remains version `0.1.0`.

Root-only `cargo test` does not execute the libraries' moved unit-test targets. Use `cargo test --workspace` for the workspace gate. The workspace selects `gui` as a member, including when the root default features are disabled; use package selection or `--exclude gui` when a check deliberately omits it. Vendored RustPython tests run through their own manifests.

Crate extraction preserves the compatibility work in [plan30](https://github.com/ssoj13/rez-rs/blob/main/docs/plans/plan30.md). Historical compiler, runtime, deployment and archive receipts refer to their original source snapshots; they do not verify the refactored workspace.

## Path authority and source identity

The shared path contract belongs to `foundation::util`. `path_key` normalizes path
prefix spelling lexically and performs no filesystem lookup.
`relative_to_authority(root, destination) -> io::Result<Option<PathBuf>>` returns
bounded relative coordinates containing only normal path components. It relates
the configured root spelling to its canonical and Windows 8.3 spellings; it does
not validate the destination's existing descendants.

Generated-directory operations apply name and reparse-point checks to each
component below that authority before creating or traversing it. A configured
root may be a junction. A generated junction below the root is rejected, as are
outside destinations and parent traversal. On Windows, `GetLongPathNameW` expands
an existing prefix only; it does not canonicalize the entire generated path.
Mapped-drive/UNC equivalence and arbitrary alias identities are outside this
contract.

Read-only executable lookup and local-package reporting reuse the authority
relation without inheriting generated-directory write validation. Pip SourceMap
keeps canonical identity for existing inputs separately from the first lexical
staging coordinates and their alias translations. Generated Pip metadata uses
its own content-relocation contract; authority checks do not rewrite its text.

Upstream Rez's [filesystem utilities](https://github.com/AcademySoftwareFoundation/rez/blob/main/src/rez/utils/filesystem.py)
resolve symlinks in `canonical_path`, and `is_subdirectory` applies `realpath` to
both paths. Its [package copying](https://github.com/AcademySoftwareFoundation/rez/blob/main/src/rez/package_copy.py)
also operates on existing package payloads. Those identity/read operations do
not supply the generated-directory write-authority contract above: resolving a
descendant first can hide a junction that must be rejected before traversal.

The Windows gate `python ci/test_workspace.py --require-short-path` runs the
locked release workspace tests once with normal temporary paths and once with a
genuine 8.3 `TEMP` alias. It fails when that alias is unavailable rather than
silently omitting the second campaign. [Plan30](../../plans/plan30.md#windows-path-authority-and-ci)
records the current local, hosted, and artifact verification boundaries.

## Scoped verification — 2026-10-06

An earlier bounded gate, `cargo test --offline -p foundation -p version`, exited 0: 41 foundation tests, 97 version tests, and three version doc-tests passed. `cargo clippy --offline -p foundation -p version --all-targets -- -D warnings` also exited 0. Foundation's shared pattern helper was extracted afterward, so these receipts do not verify that later source change.

`cargo check --offline -p model -j1` exited 0 in 8 minutes 20 seconds, including its lower-layer Python runtime dependencies. The later `cargo check --offline --locked --workspace --all-targets -j2` passed in 11.91 seconds without diagnostics; its log is `target/crate-workspace-check-7.log`. This establishes workspace compiler checking, not Rust runtime acceptance.

The fresh debug CLI build, `cargo build --offline --locked --bin rez -j1`, passed in 4 minutes 42 seconds in the original environment. The resulting CLI's `--version` check exited 0. This is a debug executable and version smoke check; it does not establish release packaging, deployment or full runtime acceptance.

`python -m unittest discover -s tests -p 'test_*.py'` passed all 18 tests in 0.411 seconds; its log is `target/crate-python-tests.log`. The final moved Rust source inventory retains all 1,511 original test functions and adds exactly three runtime callback tests, totaling 1,514 with none removed. That inventory counts source functions, not executed tests.

Final strict workspace all-target Clippy with `-D warnings` passed in 22.60 seconds; its log is `target/crate-clippy-final-fixtures.log`. The fresh `cargo test --workspace --tests --offline --locked -j1 --no-run` gate passed in 2 minutes 36 seconds and produced all 31 test executables; its log is `target/crate-final-fixture-test-build-2.log`. The preceding attempt hit MSVC LNK1318; resetting the single generated `integration4_shell.pdb` allowed the repeat to pass, without a production-source repair.

The standard Rust campaign in `target/crate-runtime-tests-final.log` exited 101. It selected 31 targets: 29 completed cleanly, the 28-test Python runtime target aborted after 21 tests because the debug test thread had a 2 MB stack, and the Python re-entry target finished with ten passed and one inspection failure caused by `TERM=dumb` stdout prompts. The 30 completed harness summaries total 1,461 passed, one failed and six ignored. The aborted target has no summary; its 21 partial passes must not be added again to the corrected target's results.

The follow-up source changes are confined to fixtures: the fresh-VM exception test uses the existing 8 MB `with_py_stack` helper, the inspection child removes `TERM`, and the native Cargo fixture passes `-j1` through `ProcessArgs`, matching the adapter's existing one-job setting. The fresh 28-test Python runtime scope passed in 1.64 seconds without a `RUST_MIN_STACK` override; its log is `target/crate-python-runtime-fixture-final.log`. The exact inspection rerun passed one test with zero failures and exited 0 after 67.56 seconds; its log is `target/crate-reentry-fixture-final.log`. The caller retained `TERM=dumb`, and the runner did not filter it: the child fixture itself removes it. The fresh native scope passed all four tests with zero failures in 31.18 seconds, exit 0; its log is `target/crate-native-fixture-final.log`. The outer `CARGO_BUILD_JOBS` was removed, so the fixture's own `ProcessArgs -j1` controls native Cargo concurrency.

Earlier separate native scopes passed four tests in 31.63 seconds and two explicitly ignored builder tests in 0.66 seconds. The accepted composite covers 1,490 unique standard Rust tests (1,461 campaign summary passes plus the corrected 28-test VM scope and one inspection test) and six explicitly executed native tests (the latest four-test native scope plus the earlier two builder tests), totaling 1,496 unique executed Rust tests with zero remaining failures in those combined scopes. Workspace doc-tests passed 14 tests with one ignored, with 8.84 seconds of compilation and about nine seconds of runtime; the 18 Python tests passed separately. Only the three fixture repairs followed the failed campaign, and their changed scopes were rerun. This is composite acceptance: the initial full campaign still exited 101, and no subsequent single full campaign passed all targets.

Runtime isolation used a temporary runner to apply private HOME, PROGRAMDATA and empty configuration only to child test processes. The compiler environment was retained. Release deployment and distribution artifacts have not been refreshed by these checks.

## Source reconciliation — 2026-10-07

The scoped checks above precede dependency refresh `4587464`. A later release package and source-closure/smoke report were committed in `24c97df`; they do not establish a repeated full runtime/Clippy gate. During the 2026-10-07 review, tracked dist payloads/helpers/report were deleted in the working tree and the earlier `target/crate-*.log` files were unavailable. The recorded checks remain saved evidence, not newly reread logs. Installed host CLI SHA256 does not match the last packaged CLI; current deployment provenance is unverified. [Plan30](https://github.com/ssoj13/rez-rs/blob/main/docs/plans/plan30.md#current-status) separates implemented code, historical checks, and current remaining work.
