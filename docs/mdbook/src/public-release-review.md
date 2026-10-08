# Publication review — 2026-10-07

rez-rs is an experimental project developed for personal use. This review prepares the current source snapshot for sharing and records the boundaries that still matter before publishing a repository or binary release.

## Completed cleanup

The initial Markdown inventory contained 143 files, including the upstream reference and repository-local agent skills. All were read for the audit, and repository text searches covered source, configuration, examples, hidden text files, installer helpers, and historical plans.

Company/deployment references were removed or anonymized, including upstream adopter/changelog text and corporate email aliases in the local reference worktree. Public author names, license notices, and unrelated upstream attribution remain. Historical receipt IDs, hashes, outcomes, and acceptance limits are retained; deployment names and local paths are now placeholders.

The README now identifies personal experimental use and distinguishes checked scenarios from full compatibility. Installation, development, architecture, and internals documentation describe the current workspace. Unsupported general speed, binary-size, full-compatibility, and old test-count claims were removed from the main onboarding pages.

The installer now requires `--repository-root`; `--rez-root` is the explicit host-activation option. It no longer infers private deployment environment variables or a neighboring bootstrap configuration. Existing callers using only environment defaults must pass these flags.

Root LICENSE and NOTICE files now match the existing Apache-2.0 declaration and source headers. Functional crate manifests declare the same license; upstream and vendored licenses remain intact.

Tracked REPL history, a machine-specific CMake preset, generated SCons binaries/object/signature files, and generated Python egg-info were removed. Byte inspection of the staged archive exposed a local path in generated SCons output before publication; the source writer now also excludes generated example output while retaining upstream binary fixtures. Source exports exclude local environment files, history, editor/agent state, generated documentation, build output, and user CMake presets. Installer archive validation enforces those exclusions.

The book and public project notes now live together under `docs/mdbook/`. The `docs/README.md` index points to Plan30, the only retained work plan; previous plans and audit reports are removed. `packaging/README.md` explains the canonical installer source. Project instruction files use `AGENTS.md`; generic private tooling documentation is excluded from source exports. Book output remains under `docs/build/`, preserving the source-writer and installer exclusion contract.

A fresh repository snapshot replaces the earlier history, which is retained separately as `old.rez-rs`. The new repository contains only the active Plan30; old plans and audit reports are removed.

The former upstream Rez reference checkout, gitlink, submodule configuration, and local submodule Git storage were removed at the user's request. Upstream Rez is an external reference; source packages no longer include its checkout.

## Verification scope

The Python test suite passes 23 tests, including five new source-export/installer regressions. Workspace Rust formatting passes. Git whitespace checks pass in the parent and reference worktrees.

The shell escaping fixture changes only its example environment-variable name; its behavior remains the space-quoting contract. The targeted Rust result and regenerated source archive receipt are recorded in `dist/publication-verification.json`. The first targeted Rust attempt hit the runner's 60-second timeout during dependency compilation; a detached continuation was used for the actual result.

The later stdlib/SRE registry replacement has a fresh default-feature Windows release build and 27 passing Python/SRE/TLS tests across six targets, with 1,566 SRE reference cases inside one test. Its current package/source closure is recorded in `dist/registry-transition-verification.json`; [runtime vendoring](rustpython-vendoring.md) records the exact boundary.

No new full runtime campaign, GUI/platform matrix, host activation, external package installation, or remote publication is claimed. Historical binary acceptance remains attached to the binary that was tested; later documentation and installer edits do not extend that acceptance.

## Remaining publication boundaries

| Item | Current evidence and next step |
|---|---|
| Git dependencies | The GUI manifest and transitive lockfile entries use SSH Git sources. Verify anonymous access to every pinned dependency, and provide an unauthenticated source transport or a licensed vendored closure before claiming a fresh public build. An offline maintainer build is insufficient. |
| Experimental compatibility | Python lifecycle/public API behavior, preprocessing, native Linux/macOS consumers, GUI runtime, and wider recipe acceptance remain in [Plan30](https://github.com/ssoj13/rez-rs/blob/main/docs/plans/plan30.md). |
| Vendored runtime | Four RustPython components retain active fixes. The unmodified stdlib and SRE copies have been replaced with pinned registry dependencies, and their regression coverage is retained. See [RustPython vendoring](rustpython-vendoring.md). |
| Crate layout | Application crates are grouped under `crates/rez/`; vendored runtime components remain at `crates/rustpython*/`. A future grouping change needs path, manifest, link, test, and archive updates. |
| Release evidence | Regenerate and verify source closure after the final source/doc edits. Build and test a fresh binary for any later production-code or runtime-dependency changes. Preserve third-party license and attribution files in the delivered closure. |

The source scan found no obvious committed credential material in its inspected patterns. It is a static review, not proof that arbitrary credentials or confidential material cannot exist. Generated archives were inspected separately.

Ordinary message variables, socket message-flag constants, cryptographic identifiers, and ctypes format-code strings retain their technical meanings; they are not company references.

Earlier GitNexus refreshes encountered a reference-submodule object lookup limitation. The local reference checkout has since been removed.
