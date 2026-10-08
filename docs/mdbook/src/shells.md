# Shell Integration

rez-rs supports 8 shell backends. When you run `rez env`, the tool resolves package dependencies, generates a shell-specific context script, and spawns a subshell that sources it. This chapter covers which shells are supported, how detection works, how to override the default, and practical details of each backend.

## Supported Shells

| Shell | Name(s) | Script Extension | Platform |
|---|---|---|---|
| Bash | `bash` | `.sh` | Linux, macOS, Windows (MSYS2/Git Bash) |
| Sh | `sh` | `.sh` | Linux, macOS |
| Zsh | `zsh` | `.sh` | Linux, macOS |
| Csh | `csh` | `.csh` | Linux, macOS |
| Tcsh | `tcsh` | `.csh` | Linux, macOS |
| Cmd | `cmd` | `.bat` | Windows |
| PowerShell | `powershell`, `pwsh` | `.ps1` | Windows, Linux, macOS |
| Git Bash | `gitbash`, `git-bash`, `git_bash` | `.sh` | Windows |

All 8 shells are compiled into the binary. There is no plugin system; shell types are selected at runtime via `ShellType::from_name()`.

## Shell Detection

When no explicit shell is specified, rez-rs auto-detects the current shell using this priority:

1. **`REZ_SHELL` environment variable** -- if set, used directly (`REZ_SHELL=bash`, `REZ_SHELL=powershell`, etc.)
2. **`default_shell` config setting** -- from `rezconfig.py` / `rezconfig.toml`, if non-empty
3. **Platform-specific detection:**

### Unix (Linux / macOS)

Reads the `$SHELL` environment variable and extracts the basename:

```
$SHELL=/bin/zsh    -> zsh
$SHELL=/usr/bin/bash -> bash
$SHELL=/bin/tcsh   -> tcsh
```

Falls back to `bash` if `$SHELL` is unset or unrecognized.

### Windows

Detection order:

1. If `$MSYSTEM` is set (indicating MSYS2 / Git Bash environment) -> `gitbash`
2. If `$COMSPEC` contains `powershell` or `pwsh` -> `powershell`
3. Otherwise -> `cmd`

## Explicit Shell Override

Use `--shell` (or `-S`) with `rez env` to force a specific shell:

```bash
# Force bash on any platform
rez env --shell bash python-3

# Force PowerShell
rez env -S powershell maya-2024 python-3.10

# Force Git Bash on Windows
rez env --shell gitbash numpy-1.26
```

The shell name is case-insensitive. Accepted aliases:

- `powershell` or `pwsh` -> PowerShell
- `gitbash`, `git-bash`, or `git_bash` -> Git Bash

## Context Script Generation

When `rez env` resolves packages, it executes each package's `commands()` function through the Rex engine, which produces a list of actions (set env var, prepend PATH, create alias, etc.). These actions are then translated into shell-specific code by the appropriate shell backend.

The generated script is written to a temporary file:

```
<temp_dir>/rez_context/context.<ext>
```

Where `<ext>` depends on the shell:

| Shell | Script File | Example Content |
|---|---|---|
| bash, sh, zsh, gitbash | `context.sh` | `export FOO="bar"` |
| csh, tcsh | `context.csh` | `setenv FOO "bar"` |
| cmd | `context.bat` | `set FOO=bar` |
| powershell | `context.ps1` | `Set-Item -Path "Env:FOO" -Value "bar"` |

### How the Subshell is Spawned

**Interactive mode** (no `-c` flag):

| Shell | Launch Command |
|---|---|
| bash, sh, gitbash | `bash --rcfile <context.sh> -i` |
| cmd | `cmd.exe /k <context.bat>` |
| powershell | `powershell -NoExit -File <context.ps1>` |
| other (csh, tcsh, zsh) | `<shell> <context_file>` |

**Non-interactive mode** (`-c <command>`):

| Shell | Launch Command |
|---|---|
| bash, sh, zsh, gitbash | `bash -c ". '<context.sh>' && <command>"` |
| cmd | `cmd.exe /c "<context.bat>" && <command>` |
| powershell | `powershell -Command ". '<context.ps1>'; <command>"` |

Example:

```bash
# Interactive: drops into subshell with python and numpy on PATH
rez env python-3.10 numpy-1.26

# Non-interactive: run a command and exit
rez env python-3 -- python -c "import numpy; print(numpy.__version__)"

# Same thing with -c flag
rez env -c "python -c 'import numpy; print(numpy.__version__)'" python-3 numpy
```

## Git Bash Special Handling on Windows

Git Bash requires special treatment on Windows because `bash.exe` can resolve to WSL's bash (located at `C:\Windows\System32\bash.exe`) instead of Git's bash.

rez-rs handles this by checking the `$EXEPATH` environment variable, which Git Bash sets to its installation directory. The executable resolution logic:

1. Read `$EXEPATH` (e.g., `C:\Program Files\Git\bin`)
2. Go up one level to the Git root (e.g., `C:\Program Files\Git`)
3. Look for `usr/bin/bash.exe` under the Git root
4. If found, use that full path; otherwise fall back to `bash.exe` on PATH

This ensures the correct bash.exe is used even when WSL is installed.

## Environment Variable Syntax

Each shell uses different syntax for referencing environment variables:

| Shell | Variable Reference | Example |
|---|---|---|
| bash, sh, zsh, gitbash | `${KEY}` | `${PATH}` |
| csh, tcsh | `${KEY}` | `${PATH}` |
| cmd | `%KEY%` | `%PATH%` |
| powershell | `${Env:KEY}` | `${Env:PATH}` |

### Path Separators

| Shell | Separator |
|---|---|
| cmd, powershell | `;` (always) |
| bash, sh, zsh, csh, tcsh, gitbash | `:` on Unix, `;` on Windows |

## Escape Handling

Each shell backend has its own string escaping rules:

### Bash / Sh / Git Bash

Single-quote wrapping with `'\''` for literal single quotes:

```
hello       -> 'hello'
it's here   -> 'it'\''s here'
```

### Csh / Tcsh

Single-quote wrapping, with `!` escaped (`\!`) since csh treats `!` as a history expansion character:

```
hello       -> 'hello'
hello!      -> 'hello\!'
```

### Zsh

Single-quote wrapping, with `%` doubled (`%%`) to prevent prompt expansion:

```
hello       -> 'hello'
100%        -> '100%%'
```

### Cmd

Special characters (`&`, `<`, `>`, `|`, `^`) are escaped with `^`:

```
a&b     -> a^&b
a<b>c   -> a^<b^>c
a^b     -> a^^b
```

### PowerShell

Quotes are escaped with backtick, dollar signs are escaped to prevent variable expansion:

```
he said "hi"  -> he said `"hi`"
$HOME         -> `$HOME
```

## Aliases

Alias implementation varies by shell:

| Shell | Alias Mechanism |
|---|---|
| bash, sh, zsh, gitbash | Exported function: `function name() { cmd "$@"; };export -f name;` |
| cmd | `doskey name=cmd $*` |
| powershell | Function wrapper: `function name() { cmd @args }` |

## Shell Completions

rez-rs includes built-in tab-completion support for bash, zsh, and PowerShell.

### Generating Completion Scripts

Use `rez complete` for package name completions:

```bash
# Complete package names starting with "py"
rez complete py

# Complete family names only (no version expansion)
rez complete --families py
```

### Installing Shell Completions

**Bash** -- add to `~/.bashrc`:

```bash
eval "$(rez complete --shell bash)"
```

Or source the built-in completion script which registers `_rez_complete_fn`:

```bash
_rez_complete_fn()
{
    local cur prev opts
    COMPREPLY=()
    cur="${COMP_WORDS[COMP_CWORD]}"
    if [ $COMP_CWORD -eq 1 ]; then
        opts="env build test config search view ..."
        COMPREPLY=( $(compgen -W "${opts}" -- ${cur}) )
    fi
}
complete -F _rez_complete_fn rez
```

**Zsh** -- add to `~/.zshrc` or place in `$fpath`:

```zsh
#compdef rez
_rez() {
    local -a commands
    commands=(
        'env:Spawn shell with resolved context'
        'build:Build packages'
        'test:Run package tests'
        ...
    )
    _describe 'rez commands' commands
}
_rez "$@"
```

**PowerShell** -- add to your `$PROFILE`:

```powershell
Register-ArgumentCompleter -CommandName rez -ScriptBlock {
    param($commandName, $wordToComplete, $commandAst, $fakeBoundParameters)
    $commands = @('env', 'build', 'test', 'config', 'search', ...)
    $commands | Where-Object { $_ -like "$wordToComplete*" } | ForEach-Object {
        [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_)
    }
}
```

**Cmd** -- cmd.exe does not support programmable tab completion. No completion script is generated.

## Configuration

Shell-related configuration in `rezconfig.py`:

```python
# Default shell for rez env (overridden by --shell flag)
default_shell = ""   # empty = auto-detect

# Example: always use PowerShell on Windows
# default_shell = "powershell"
```

The `REZ_SHELL` environment variable also overrides the detected shell (checked before `default_shell`).

## Wrapper Scripts

When rez creates tool wrappers for suites, the wrapper script format depends on the target shell:

| Shell | Wrapper Template |
|---|---|
| bash, sh, zsh, gitbash | `#!/bin/bash` / `source "<context>"` / `exec <tool> "$@"` |
| cmd | `@echo off` / `call "<context>"` / `<tool> %*` |
| powershell | `. "<context>"` / `& <tool> @args` |

Wrapper scripts are written to the suite's `bin/` directory with the appropriate extension (`.sh`, `.cmd`, `.ps1`). On Unix, scripts are set to mode `0755` (executable).
