# Introduction

**rez-rs** is a Rust implementation of the [rez](https://github.com/AcademySoftwareFoundation/rez) package manager, originally written in Python by Allan Johns. It targets the same package and environment management use cases and aims to interoperate with existing Rez data. The reference is the vendored Rez source snapshot at gitlink `78e26236cdd65bc549db6b4e5782c70fe86ed5db` (`3.3.0-12-g78e26236`); its working tree contains local changes. rez-rs is not yet a complete drop-in replacement.

## What rez-rs Does

rez-rs resolves package dependency graphs, configures shell environments, and manages package repositories. When you type `rez env maya-2024 python-3.10`, rez-rs:

1. Reads package definitions from your configured repositories
2. Runs a dependency solver to find a compatible set of packages
3. Executes each package's `commands` section (rex DSL) to build an environment
4. Spawns a new shell session with the configured environment

The result is a reproducible, isolated software environment where every tool, library, and plugin is at a known, compatible version.

## Key Facts

| Property | Value |
|---|---|
| Language | Rust (Edition 2021) |
| Binary size | Varies by platform, feature set, and release profile |
| Embedded VM | RustPython 0.6.0 for `package.py` and `rezconfig.py` execution |
| CLI commands | Resolve, query, development, repository, and administration groups; behavior is still being checked for Rez parity |
| Verification | Historical test/runtime receipts are archived; [the current work plan](https://github.com/ssoj13/rez-rs/blob/main/docs/plans/plan30.md) records accepted scope and remaining work |
| Platform acceptance | Recorded Windows checks; native Linux/macOS acceptance remains incomplete |
| Shell renderers | 8 shells: bash, sh, csh, tcsh, zsh, cmd, powershell, git-bash |
| Build adapters | CMake, Make, Python, Pip, Cargo, Go, Zig, Node.js, Bun, SCons, vcpkg, Conan, extraction, custom, no-op |
| Package formats | `package.py`, `package.yaml`, `package.yml`, `package.toml` |
| Rez compatibility baseline | Vendored Rez source snapshot `3.3.0-12-g78e26236` (gitlink `78e26236cdd65bc549db6b4e5782c70fe86ed5db`); worktree has local changes |

## Why a Rust Port?

Python Rez is a mature package manager. This personal experiment explores deploying its CLI with an embedded Python interpreter and sharing package-management logic through Rust libraries. Performance and compatibility depend on the workload; the project does not claim a general speed advantage without reproducible benchmarks.

The embedded RustPython VM runs many existing `package.py` files and `rezconfig.py` configurations. Compatibility varies by Python feature and Rez workflow; review the bug-hunt report (historical snapshot in old.rez-rs) before migrating production use.

## Project Status

This is an experimental project developed for personal use. Tested scenarios have passed their recorded checks, but broader testing and review remain necessary.

rez-rs implements a broad CLI surface for package creation, building, testing, release, resolve, and search. Command availability does not imply behavioral parity. The active parity queue and verification boundaries are tracked in [plan30](https://github.com/ssoj13/rez-rs/blob/main/docs/plans/plan30.md); the bug-hunt report (historical snapshot in old.rez-rs) preserves historical findings; evaluate each required workflow before production migration.

## How to Read This Book

- **New to rez entirely?** Start with [History](history.md) and [Core Concepts](concepts.md), then follow the [Getting Started](getting-started.md) guide.
- **Migrating from Python rez?** Read [Motivation](motivation.md), [Installation](installation.md), and [Configuration](configuration.md).
- **Integrating or extending?** See [Architecture](architecture.md), [Internals](internals.md), and [Design Decisions](design-decisions.md).
- **Contributing?** Go straight to the [Development Guide](development.md).
