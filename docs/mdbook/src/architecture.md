# Architecture

rez-rs builds one CLI executable from a workspace of functional libraries. The root package has `src/main.rs` and `src/cli/`; it has no library facade. The [crate architecture report](architecture-crates.md) defines the current ownership and dependency boundaries.

## Crate responsibilities

| Crate | Responsibility |
|---|---|
| `foundation` | Shared errors, constants, path and filesystem helpers, process support. |
| `version` | Version tokens, ordering, ranges, and package requirements. |
| `python-runtime` | RustPython 0.6.0 construction, frozen standard library, Python execution. |
| `model` | Package data, configuration, canonical serialization, and platform data. |
| `repository` | Providers, discovery, binding, locked publication, cache, copy/move operations. |
| `rex` | Typed actions, environment state, shell interpreters and forwarding wrappers. |
| `resolve` | Solver, contexts, Rex package bindings, suites, bundles. |
| `build-system` | Builders, acquisition, Pip conversion and publication, release hooks. |
| `gui` | Optional graphical consumer of the shared libraries. |

Binary-patching crates under `crates/bin-patch/` handle ELF and Mach-O rewriting. Vendored RustPython patches retain their own crate directories, license files, and independent regression suites.

The CLI imports its owners directly, for example `model::config::CONFIG` or `resolve::context::ResolvedContext`. Libraries must not depend on the CLI.

## Resolve and execution flow

```text
CLI requests + typed configuration
  -> canonical package provider and repository data
  -> version constraints + solver + resolver
  -> ResolvedContext with package handles and provenance
  -> shared Python package/context bindings
  -> typed Rex action decoding
  -> native environment state and selected shell interpreter
  -> process execution or saved context
```

Configuration selects implicit requirements and package policies. The provider retains repository provenance; contexts hydrate exact handles when loading or bundling.

Package commands execute through the embedded Python runtime and the shared Rex recording path. Shell interpreters own native path normalization and escaping. A rendered script alone does not establish that a native shell accepts it; process consumers cover that boundary separately.

## Package loading

Python, YAML, and TOML definitions feed the canonical serializer and typed package model. Python definitions execute with the shared interpreter policy. Private helpers and deferred source needed by later lifecycle execution must survive conversion.

The embedded runtime includes its frozen standard library. It does not provide third-party CPython extension modules or the complete importable Python Rez API. Preprocessing and precise public API compatibility remain in [Plan30](https://github.com/ssoj13/rez-rs/blob/main/docs/plans/plan30.md).

## Publication and acquisition

```text
recipe + build dependencies + selected adapter
  -> acquire validated source inputs
  -> owned work/staging directories
  -> build command with effective environment
  -> prepared package metadata and payload
  -> shared per-family/version lock
  -> journaled publication
```

Repository locks deliberately retain their lock files after unlock. Writers coordinate through the same OS-locked file object; file presence does not indicate a held or stale lock.

Downloads, extraction, and copying use shared validation and staging policies. Native platform behavior and hostile concurrent filesystem changes have separate acceptance boundaries.

## Contexts, suites, and bundles

Saved contexts retain package identities, requests, and options. Cache roots are transient execution choices; the serialized original handles remain intact.

Suites collect contexts and expose tools through forwarding wrappers. Bundles stage package payloads, save relocatable context data, and use binary patching where needed. Copy and move use the same canonical package policy and publisher.

## Distribution

`bootstrap.py b` compiles the workspace. `bootstrap.py p` builds the CLI and stages its executable, package recipe, and current source ZIP. Installer helpers come from their canonical sources.

The source writer excludes VCS metadata, generated artifacts, local environment files, editor/agent state, and machine-specific CMake presets. The installer rejects those excluded paths in an incoming source archive.

Recorded Windows builds and runtime campaigns are described in [Plan30](https://github.com/ssoj13/rez-rs/blob/main/docs/plans/plan30.md). They do not establish complete Rez parity, GUI runtime acceptance, or native Unix/macOS consumers.
