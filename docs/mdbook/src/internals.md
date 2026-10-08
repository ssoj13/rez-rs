# Internals

The [crate architecture](architecture-crates.md) names the current owners. The flows below describe shared contracts; [Plan30](https://github.com/ssoj13/rez-rs/blob/main/docs/plans/plan30.md) records their remaining compatibility and native-platform acceptance.

## Python execution

[python-runtime](https://github.com/ssoj13/rez-rs/blob/main/crates/rez/python-runtime/src/lib.rs) owns interpreter construction and execution. RustPython 0.6.0 initializes the standard library with frozen modules so execution does not depend on a build-machine stdlib path.

Package loading and configuration use this runtime through their owning adapters. Context execution supplies the Rex preamble and package/context bindings. Third-party CPython extension modules and the full Python Rez API are outside that runtime contract.

Local runtime overrides and the tests that justify them are documented in [RustPython vendoring](rustpython-vendoring.md).

## Canonical package data

[The serializer](https://github.com/ssoj13/rez-rs/blob/main/crates/rez/model/src/serialise.rs) loads Python, YAML, and TOML package definitions. Python globals are converted into reusable data, then validated into the package model. The repository layer delegates to this canonical path.

Installed Python definitions must preserve helpers and source needed by later lifecycle execution. Publication verifies loaded identity, metadata, and original source bytes rather than reconstructing arbitrary Python from a lossy metadata snapshot.

Package preprocessing and precise deferred/public API semantics retain explicit compatibility items in Plan30.

## Rex and shell rendering

[Context execution](https://github.com/ssoj13/rez-rs/blob/main/crates/rez/resolve/src/context.rs) prepares package roots, resolve data, requests, and callbacks for the shared Python binding.

Python helpers and environment proxies record actions. [The wire decoder](https://github.com/ssoj13/rez-rs/blob/main/crates/rez/rex/src/wire.rs) converts them into the shared typed action representation. The native action manager owns environment state and expansion; the selected interpreter owns shell-specific normalization and output.

This keeps one recording and decoding path for packages and context consumers. Native Cmd verbatim paths, PowerShell paths, and Unix shell paths must be normalized by their actual interpreter, not by unconditional slash replacement.

## Resolve provenance and caches

The provider supplies candidates with repository identity. The resolver builds a context containing selected packages and their handles. Canonical context loading hydrates exact resources rather than substituting whichever package happens to be found first.

Parsed metadata caches and payload caches serve different purposes. Cached payload roots may be selected for execution while original serialized handles and source bases remain unchanged. Cache policy uses the package's typed nullable settings and the configured fallback rules.

## Repository transactions

[The repository publisher](https://github.com/ssoj13/rez-rs/blob/main/crates/rez/repository/src/repository.rs) coordinates writers with a persistent per-family/version lock file. Acquiring the OS lock establishes ownership; release and drop unlock and close the handle. The file remains so existing waiters and later writers coordinate on the same object.

Build, Pip, copy, and bundle consumers reuse publication rather than each implementing package writes. Staging and journals protect the existing package when a build or publication fails.

## Acquisition and build payloads

[Shared filesystem copying](https://github.com/ssoj13/rez-rs/blob/main/crates/rez/foundation/src/filesystem.rs) owns file replacement, destination authority, link handling, and stat preservation. [Acquisition and extraction](https://github.com/ssoj13/rez-rs/blob/main/crates/rez/build-system/src/builders/README.md) stage inputs before merging or publishing.

A build adapter runs its external tool with the effective package environment, prepares metadata and payloads, then publishes through the shared repository path. Pip additionally maps wheel files and launchers, finalizes RECORD, and relocates known editable paths.

Native consumer tests must inspect the installed result after temporary work directories have been removed. Compilation and a tool's successful exit do not alone verify relocation or installed behavior.

## Deployment and source exports

[bootstrap.py](https://github.com/ssoj13/rez-rs/blob/main/bootstrap.py) stages the release executable and current source archive. [The installer](https://github.com/ssoj13/rez-rs/blob/main/packaging/install.py) requires explicit repository and optional host destinations. [CLI installation](https://github.com/ssoj13/rez-rs/blob/main/cli_install.py) owns aliases, hash manifests, backups, locks, and recovery.

The source archive excludes generated output and local environment/editor state. Public source review and source closure have separate receipts from binary compilation and runtime acceptance.
