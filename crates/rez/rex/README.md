# rex

Shell integration layer. This module provides the rex DSL engine for environment
manipulation, shell type plugins for generating platform-specific code, and tool
wrapper script generation for suites.

The `rex` crate depends on `model`, `version`, and `foundation`. Context orchestration lives in `resolve`; shared action decoding lives in `src/wire.rs`. Consumers import `rex` directly. See the [workspace architecture](../../../docs/mdbook/src/architecture-crates.md). Files below are relative to `src/`.

## Files

| File | Description |
|------|-------------|
| lib.rs | Module declarations and re-exports. Re-exports `types::*` and `rex::*`. |
| rex.rs | Rex DSL engine -- the core of rez's environment manipulation system. `Action` enum (13 variants: setenv, unsetenv, prependenv, appendenv, alias, source, command, etc.). `ActionInterpreter` trait for shell-specific output. `RexExecutor` orchestrates action execution with bindings. `EnvironmentDict` tracks env state. `PythonInterpreter` applies changes to a HashMap. `OutputInterpreter` records actions as generic strings. `EphemeralsDict` for ephemeral variables. `VariantBinding` for read-only variant attributes in rex code. |
| types.rs | Shell type plugins. `ShellType` enum (Bash, Sh, Csh, Tcsh, Cmd, PowerShell, Gitbash). `Shell` trait with `ActionInterpreter` bound. Concrete implementations: `BashShell`, `CmdShell`, `PowerShellShell`. Factory: `create_shell(ShellType) -> Box<dyn Shell>`. Detection: `detect_shell() -> ShellType`. `StartupCapabilities` for shell feature queries. |
| wrapper.rs | Tool wrapper generation. `Wrapper` struct (suite path, context name, tool name, prefix/suffix). `WrapperScript` generates shell-specific wrapper content. `create_wrapper()` writes wrapper scripts to suite bin/. `create_forwarding_script()` generates simple forwarding scripts. Shell-specific templates for bash, cmd, powershell. |

## Architecture

The rex system is rez's central mechanism for environment manipulation:

```
Package commands (rex code)       Shell plugins
  "env.PATH.prepend('{root}/bin')"     BashShell / CmdShell / PowerShellShell
         |                                    |
         v                                    v
  RexExecutor                          ActionInterpreter trait
    -> parse rex string                   .setenv(key, val) -> "export KEY=val"
    -> emit Action enum                   .prependenv(key, val) -> "export KEY=val:$KEY"
    -> dispatch to interpreter            .alias(name, cmd) -> "alias name='cmd'"
         |                                    |
         v                                    v
  shell-specific output code          spawned shell process
```

The `ActionInterpreter` trait is the key abstraction. Each shell plugin implements
it to translate generic `Action` values into shell-specific syntax. There is a
blanket impl for `Box<T>` enabling `Box<dyn Shell>` usage.

The `Shell` trait extends `ActionInterpreter` with shell lifecycle methods:
startup script paths, executable name, file extension, spawn configuration.

## Key Types / Traits

| Type | Description |
|------|-------------|
| `Action` | Enum of 13 rex commands (setenv, unsetenv, prependenv, appendenv, alias, etc.). |
| `ActionInterpreter` | Trait: translate `Action` to shell-specific string output. |
| `Shell` | Trait: extends `ActionInterpreter` with shell lifecycle (startup, spawn, etc.). |
| `ShellType` | Enum: Bash, Sh, Csh, Tcsh, Cmd, PowerShell, Gitbash. |
| `RexExecutor` | High-level executor combining action manager + bindings. |
| `EnvironmentDict` | Tracks current env var state during rex execution. |
| `OutputStyle` | Script file vs eval output mode. |
| `Wrapper` | Tool wrapper descriptor for suite bin/ scripts. |

## Usage

```rust
use rex::{create_shell, detect_shell, ShellType};
use rex::rex::{Action, RexExecutor};

// Detect and create shell
let shell_type = detect_shell();
let shell = create_shell(shell_type);

// Execute rex actions
let actions = vec![
    Action::Setenv { key: "FOO".into(), value: "bar".into() },
    Action::Prependenv { key: "PATH".into(), value: "/opt/bin".into() },
];
```
