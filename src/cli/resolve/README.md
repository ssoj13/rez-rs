# cli/resolve/

Commands for environment resolution, context management, and shell interaction.
These are the primary user-facing commands for creating and working with rez environments.

## Files

| File | Description |
|------|-------------|
| mod.rs | Module declarations for all six resolve commands. |
| env.rs | `rez env` -- Resolve packages and launch an interactive shell. The main entry point for users. Supports `--shell`, `--command`, `--patch`, `--exclude`, timestamp pinning, and output to file. |
| context.rs | `rez context` -- Inspect a saved resolved context (`.rxt` file). Prints resolved packages, requests, timestamps, and environment diff. |
| interpret.rs | `rez interpret` -- Execute rex commands and output shell-specific code. Translates high-level env manipulation (setenv, appendenv, alias) to bash/cmd/powershell syntax. |
| complete.rs | `rez complete` -- Generate shell completions for package names and versions. Used by shell tab-completion integration. |
| suite.rs | `rez suite` -- Manage suites (collections of resolved contexts with tool wrappers). Create, add/remove contexts, list tools, rebuild wrappers. |
| bundle.rs | `rez bundle` -- Bundle a resolved context with its packages into a relocatable directory for offline use or deployment. |

## Architecture

The `env` command is the most complex in this group. Its flow:

```
EnvArgs
  -> parse package requests as Vec<Requirement>
  -> build ResolveOptions (filters, orderers, timestamp, patching)
  -> ResolvedContext::resolve(requests, options)
     -> Resolver -> Solver (backtracking SAT-like engine)
  -> detect or select shell type
  -> ResolvedContext::execute_shell()
     -> RexExecutor generates shell-specific env setup code
     -> spawn interactive or one-shot shell process
```

The `context` command loads a previously saved `.rxt` file (JSON-serialized
`ResolvedContext`) and displays its contents without re-resolving.

The `interpret` command is lower-level -- it takes raw rex action strings and
translates them through the shell plugin system.

## Key Types Used

- `resolve::context::ResolvedContext` -- core resolved environment
- `resolve::context::ResolveOptions` -- builder for resolve configuration
- `rex::types::ShellType` -- target shell selection
- `version::Requirement` -- parsed package request
- `resolve::suite::Suite` -- suite management
- `resolve::bundle_context::BundleOptions` -- bundling configuration

## Usage from main.rs

```rust
Commands::Env(ref args) => cli::resolve::env::run(args),
Commands::Context(ref args) => cli::resolve::context::run(args),
Commands::Interpret(ref args) => cli::resolve::interpret::run(args),
// ...
```
