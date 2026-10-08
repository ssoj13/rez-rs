# Motivation for rez-rs

Rez separates software packages from the environments that use them. rez-rs keeps that model while exploring a Rust implementation with an embedded Python interpreter.

## Simplify CLI deployment

The CLI contains RustPython and a frozen standard library, so running the package manager does not require a separately installed Python interpreter. Deploy a binary built for the target OS and architecture, then configure repositories and command aliases.

Build tools, managed applications, and Python interpreters selected by packages still have their own requirements. Platform libraries and native compatibility must also be checked for the distributed binary.

## Use Rust for shared infrastructure

Rust provides typed interfaces for package identities, version constraints, configuration, repository transactions, and Rex actions. Shared libraries let the CLI, builders, and GUI reuse those interfaces.

Ownership and type checking help catch errors during development. They do not replace runtime validation of filesystem operations, process environments, embedded Python, or external tools.

## Preserve Python package definitions

Package definitions and configuration can contain real Python logic. Executing them through RustPython preserves more of that behavior than a text parser could.

Compatibility still varies by workflow. CPython extension imports, package preprocessing, public Python Rez APIs, and custom build APIs require separate treatment. See [Plan30](https://github.com/ssoj13/rez-rs/blob/main/docs/plans/plan30.md) before assuming an existing pipeline will work unchanged.

## Measure performance on real workloads

Reducing interpreter startup overhead is one motivation for the port. Resolve time also depends on repository size, storage, caches, Python package logic, and the requested dependency graph.

The project does not publish a Python-versus-Rust speed claim without reproducible benchmark inputs and matching conditions. Build and runtime receipts measure their named scenarios; they are not general performance benchmarks.
