# Rex Commands

Rex (Rez EXecution) is the domain-specific language used in package `commands()` functions to manipulate the shell environment. Rex is not a separate language -- it is Python code that uses proxy objects provided by rez to generate shell-specific output.

## Environment Variable Operations

### env.VAR.set(value)

Set an environment variable to a specific value.

```python
def commands():
    env.MY_CONFIG.set("/path/to/config")
    env.MY_PACKAGE_VERSION.set("{version}")
```

Replaces any existing value.

### env.VAR.unset()

Remove an environment variable.

```python
def commands():
    env.LEGACY_VAR.unset()
```

### env.VAR.prepend(value)

Prepend a value to a path-like environment variable.

```python
def commands():
    env.PATH.prepend("{root}/bin")
    env.PYTHONPATH.prepend("{root}/python")
    env.LD_LIBRARY_PATH.prepend("{root}/lib")
```

The separator is determined by the variable name and platform (`:` on Unix, `;` on Windows for PATH-like variables). Custom separators can be configured via `env_var_separators` in rezconfig.

### env.VAR.append(value)

Append a value to a path-like environment variable.

```python
def commands():
    env.PATH.append("{root}/optional/bin")
```

**First-reference logic**: If a variable has never been set or modified yet, the first `prepend()` or `append()` to it becomes a `set()` instead of a prepend/append operation. This prevents incorrect references to undefined variables.

### env.VAR.reset(value)

Reset an environment variable. Similar to `set`, but also tracks the reset for rez's internal bookkeeping of "resetting variables."

```python
def commands():
    env.PATH.reset("{root}/bin")
```

## String Escaping

### escape_string(value, shell_type)

Escape a string for safe use in shell commands. Each shell has its own escaping rules:

```python
from rez_rs import escape_string, ShellType

# Bash: escapes $, `, \, ", !
escaped = escape_string("$HOME/my file", ShellType.Bash)

# Cmd: escapes %, ^, &, |, <, >, etc.
escaped = escape_string("C:\\Program Files\\App", ShellType.Cmd)

# PowerShell: escapes $, `, ", ', etc.
escaped = escape_string("$env:PATH", ShellType.PowerShell)
```

This is primarily used internally by rez when generating shell code, but can be useful in custom build scripts.

## Shell Operations

### alias(name, command)

Create a shell alias.

```python
def commands():
    alias("mytool", "{root}/bin/run.sh")
    alias("mypy", "python {root}/scripts/main.py")
    alias("maya_studio", "maya -script {root}/mel/startup.mel")
```

On bash/sh/csh/tcsh, this creates a shell alias. On cmd.exe, it creates a DOSKEY macro. On PowerShell, it creates a function.

### source(path)

Source (execute) a shell script in the current shell.

```python
def commands():
    source("{root}/env/setup.sh")
```

This is shell-specific: `source` or `.` on bash, `source` on csh/tcsh, etc.

### command(cmd)

Execute a shell command.

```python
def commands():
    command("echo 'Package loaded: {name}-{version}'")
```

The command is executed in the current shell context.

## Output Operations

### info(message)

Print an informational message.

```python
def commands():
    info("Loading {name} version {version}")
```

Generates a shell echo/print command.

### error(message)

Print an error message to stderr.

```python
def commands():
    error("Warning: deprecated package, use {name}2 instead")
```

### comment(text)

Add a comment to the generated shell script.

```python
def commands():
    comment("Configure Maya plug-in paths")
    env.MAYA_PLUG_IN_PATH.prepend("{root}/plug-ins")
```

### stop(message)

Stop execution with an error message. Prevents the shell from being spawned.

```python
def commands():
    if system.platform == "windows":
        stop("This package does not support Windows")
```

### shebang(value)

Set the shebang line for the generated script (only relevant for script output mode).

```python
def commands():
    shebang("#!/bin/bash")
```

## Proxy Objects

Rex commands have access to several proxy objects that provide context about the current package, resolve, and system.

### this

The current package being configured.

| Attribute | Description | Example |
|---|---|---|
| `this.root` | Package install root directory | `/packages/maya/2024.0` |
| `this.name` | Package name | `"maya"` |
| `this.version` | Package version | `"2024.0"` |
| `this.base` | Package base directory (same as root if no variant) | `/packages/maya/2024.0` |

The `{root}`, `{name}`, and `{version}` string substitutions are shorthand for `this.root`, `this.name`, and `this.version`:

```python
# These are equivalent:
env.PATH.prepend("{root}/bin")
env.PATH.prepend("{this.root}/bin")
```

### resolve

Access to other packages in the resolved context.

```python
def commands():
    # Check if a package is in the resolve
    if resolve.get("openexr"):
        env.USE_OPENEXR.set("1")

    # Access resolved package version
    if resolve.python.version.major == "3":
        env.PY3.set("1")
```

`resolve.<package_name>` gives access to the resolved version of that package. If the package is not in the resolve, accessing it raises an error -- use `resolve.get("name")` to safely check.

### request

The original resolve request.

```python
def commands():
    # Check if a specific package was explicitly requested
    if "debug_mode" in request:
        env.MY_DEBUG.set("1")
```

Useful for checking ephemerals:

```python
def commands():
    if "_.verbose" in request:
        env.VERBOSE.set("1")
```

### system

System information.

| Attribute | Description | Example |
|---|---|---|
| `system.platform` | Platform name | `"linux"`, `"windows"`, `"osx"` |
| `system.arch` | CPU architecture | `"x86_64"`, `"aarch64"` |
| `system.os` | Operating system with version | `"ubuntu-22.04"`, `"windows-10"` |

```python
def commands():
    if system.platform == "linux":
        env.LD_LIBRARY_PATH.prepend("{root}/lib")
    elif system.platform == "windows":
        env.PATH.prepend("{root}/lib")
    elif system.platform == "osx":
        env.DYLD_LIBRARY_PATH.prepend("{root}/lib")
```

## String Substitutions

Within rex command string arguments, the following substitutions are available:

| Substitution | Expands to |
|---|---|
| `{root}` | Package root directory |
| `{name}` | Package name |
| `{version}` | Package version string |
| `{base}` | Package base directory |

These are expanded before the command is rendered to shell code.

## Python Logic in Commands

Since rex commands are Python code, you can use any Python logic:

```python
def commands():
    import os

    # Conditional based on environment
    if os.environ.get("STUDIO") == "main":
        env.PATH.prepend("{root}/bin/production")
    else:
        env.PATH.prepend("{root}/bin/development")

    # Loop over tool names
    for tool in ["render", "composite", "edit"]:
        alias(f"studio_{tool}", f"python {{root}}/tools/{tool}.py")

    # Access resolve information
    py_ver = resolve.python.version
    env.PYTHONPATH.prepend(f"{{root}}/python{py_ver.major}.{py_ver.minor}")
```

Note: when using f-strings with `{root}`, double the braces (`{{root}}`) because f-strings consume the first level of braces.

## String Commands vs Function Commands

Commands can be defined as either a function or a string:

### Function form (recommended for package.py)

```python
def commands():
    env.PATH.prepend("{root}/bin")
    env.FOO.set("bar")
```

### String form (required for YAML/TOML, optional for .py)

```python
commands = """
env.PATH.prepend("{root}/bin")
env.FOO.set("bar")
"""
```

Both forms are equivalent. The function form is preferred in `package.py` because it gets syntax highlighting and IDE support.

## Action Types (Internal)

Internally, rex commands are represented as an `Action` enum in `src/shell/rex.rs`:

| Action | Rex Command |
|---|---|
| `Setenv` | `env.VAR.set(value)` |
| `Unsetenv` | `env.VAR.unset()` |
| `Resetenv` | `env.VAR.reset(value)` |
| `Prependenv` | `env.VAR.prepend(value)` |
| `Appendenv` | `env.VAR.append(value)` |
| `Alias` | `alias(name, cmd)` |
| `Source` | `source(path)` |
| `Command` | `command(cmd)` |
| `Info` | `info(msg)` |
| `Error` | `error(msg)` |
| `Comment` | `comment(text)` |
| `Shebang` | `shebang(value)` |
| `Stop` | `stop(msg)` |

These actions are interpreted by a shell-specific `ActionInterpreter` to produce the correct shell syntax. Each shell type (bash, cmd, PowerShell, etc.) has its own interpreter that renders actions into the correct shell commands.

## Execution Order

When a resolved context is executed, commands run in this order:

1. **pre_commands** from all packages (in dependency order)
2. **commands** from all packages (in dependency order)
3. **post_commands** from all packages (in dependency order)

Within each phase, packages are processed in dependency order: a package's commands run before the commands of packages that depend on it.
