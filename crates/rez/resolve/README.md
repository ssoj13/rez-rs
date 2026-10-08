# resolve

Dependency resolution engine. This module contains the SAT-like solver that computes
compatible package sets, the high-level resolver wrapper, and the resolved context
that represents a fully resolved environment ready to be materialized as a shell.

This crate also owns suites, bundles, status, package test execution, and optional
AMQP tracking. Provider implementations and candidate provenance belong to
`repository`. See the [workspace architecture](../../../docs/mdbook/src/architecture-crates.md).
Files below are relative to `src/`.

## Files

| File | Description |
|------|-------------|
| lib.rs | Module declarations and re-exports of the crate's own `context::*` and `resolver::*` APIs. |
| solver.rs | Core solver engine. Phase-based backtracking algorithm: extract common deps, intersect with existing scopes, reduce conflicting variants, split on exhaustion. Types: `Solver`, `SolverState`, `Reduction`, `DependencyConflict`, `FailureReason`, `SolverFailure`; consumes repository-owned `PackageProvider` and `PackageVariant` directly. |
| resolver.rs | High-level resolver wrapper. `Resolver` struct with builder-pattern configuration. Maps `SolverStatus` to `ResolverStatus`. Extracts `ResolvedPackageInfo` from solver output. Computes cache keys. |
| context.rs | Resolved context -- the main user-facing object. `ResolvedContext` stores resolved packages, requests, timestamps, and environment changes. Can spawn shells, save/load `.rxt` files, and generate patched requests. `ResolveOptions` builder. `ContextChanges` for env var diff tracking. |

## Architecture

The resolution pipeline:

```
User requests: ["maya-2024", "python-3.10+"]
         |
         v
  ResolvedContext::resolve(requests, options)
         |
         v
  Resolver::new(requests, provider)
    .package_filter(filter)
    .package_orderers(orderers)
    .resolve()
         |
         v
  Solver::new(requests, provider)
    Phase loop:
      1. extract()    -- find common deps across variant scopes
      2. intersect()  -- merge extracted deps with existing scopes
      3. reduce()     -- remove variants that conflict
      4. if exhausted -> split() a scope and try both halves
      5. if solved -> done
      6. if all paths fail -> FailureReason
         |
         v
  Vec<ResolvedPackageInfo>
         |
         v
  ResolvedContext
    .execute_shell()   -- spawn bash/cmd/powershell with env vars
    .save(path)        -- serialize to .rxt JSON file
```

The repository-owned `PackageProvider` trait abstracts package data access. Two implementations:

- `FilesystemPackageProvider` -- reads from disk repositories
- `MemoryPackageProvider` -- in-memory for tests

The solver supports progress callbacks (`SolverCallbackReturn`) for UI feedback
and timeout enforcement.

## Key Types / Traits

| Type | Description |
|------|-------------|
| `Solver` | Core backtracking solver. Maintains scope list, split stack, reduction history. |
| `SolverState` | Snapshot of solver state for caching/debugging. |
| `repository::provider::PackageProvider` | Trait for supplying packages/variants to the solver. |
| `repository::provider::PackageVariant` | A specific variant offered to the solver. |
| `Resolver` | Builder-pattern wrapper around `Solver`. Produces `ResolvedPackageInfo`. |
| `ResolvedPackageInfo` | Resolved variant: name, version, variant_index, requires, root, commands. |
| `ResolvedContext` | Full resolved environment. Save/load `.rxt`, spawn shells, track env changes. |
| `ResolveOptions` | Builder for configuring a resolve (paths, filters, orderers, timestamp, etc.). |
| `Reduction` | Record of a variant removed during solve due to dependency conflict. |
| `FailureReason` | Why a solve failed: TotalReduction, DependencyConflicts, or Cycle. |

## Usage

```rust
use resolve::context::{ResolvedContext, ResolveOptions};
use version::Requirement;

let requests = vec![
    Requirement::new("maya-2024")?,
    Requirement::new("python-3.10+<3.12")?,
];
let ctx = ResolvedContext::resolve(requests, ResolveOptions::new())?;
ctx.print_info();
ctx.execute_shell(None)?;
```
