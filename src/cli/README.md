# cli/

CLI command implementations for the `rez` binary. All 30 subcommands are organized
into five logical groups. Each command module follows the same pattern: a clap-derived
`XxxArgs` struct and a `pub fn run(&XxxArgs) -> Result<()>` entry point.

## Structure

| Subgroup | Directory | Commands | Purpose |
|----------|-----------|----------|---------|
| resolve | [cli/resolve/](resolve/README.md) | env, context, interpret, complete, suite, bundle | Environment resolution and shell interaction |
| query | [cli/query/](query/README.md) | search, view, help-pkg, depends, diff, plugins | Read-only package information lookups |
| dev | [cli/dev/](dev/README.md) | build, test, release | Package development workflow |
| repo | [cli/repo/](repo/README.md) | cp, mv, rm, pkg-cache, pkg-ignore, bind, pip, yaml2py | Repository mutations and package management |
| admin | [cli/admin/](admin/README.md) | config, status, selftest, benchmark, memcache, python, forward | System diagnostics, configuration, and tools |

## Files

| File | Description |
|------|-------------|
| mod.rs | Declares `pub mod` for all five subgroup modules. |

## Architecture

CLI modules import functional libraries by their owning crate names, such as
`model`, `repository`, `resolve`, and `build_system`. The root package contains
the binary (`main.rs`); see [crate architecture](../../docs/mdbook/src/architecture-crates.md).
The dispatch flow is:

```
main.rs
  Cli::parse()           -- clap parses argv
  Commands enum match    -- routes to cli::<group>::<cmd>::run(&args)
    cli::resolve::env::run(&EnvArgs)
    cli::query::search::run(&SearchArgs)
    ...etc
```

Each command module is self-contained:

1. Define `XxxArgs` struct with `#[derive(Args)]`
2. Implement `pub fn run(args: &XxxArgs) -> Result<()>`
3. Use types from the owning workspace crate for business logic

Error handling: all `run()` functions return `Result<()>`. Errors propagate to
`main()` which prints them to stderr and exits with code 1.

## Key Design Decisions

1. **Five groups, not flat**: Commands are grouped by domain rather than listed
   in a single directory, improving navigation in a 30-command codebase.
2. **No shared CLI state**: Each command is stateless -- no global CLI context
   object. Configuration comes from `model::config::CONFIG`.
3. **clap derive**: All argument parsing uses clap's derive macros for
   compile-time validation and auto-generated help text.
4. **Hidden commands**: `rez forward` is marked `hide = true` in clap --
   it is an internal implementation detail for suite wrapper scripts.
