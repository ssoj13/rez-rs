# Core Concepts

This chapter explains the fundamental concepts in rez-rs. Understanding these is essential for effective use of the tool.

## Package

A **package** is the fundamental unit in rez. It is a named, versioned piece of software with metadata, dependencies, and environment commands. A package is defined by a `package.py` (or `package.yaml` / `package.toml`) file.

Every package has at minimum:

- **name** -- a unique identifier (e.g., `maya`, `python`, `numpy`)
- **version** -- a semantic version (e.g., `2024.0`, `3.10.12`, `1.2.3`)

And optionally:

- **description** -- human-readable summary
- **requires** -- list of dependency requirements
- **commands** -- rex DSL code to configure the environment
- **tools** -- list of executables the package provides
- **variants** -- platform/version-specific builds

Packages are stored in repositories as directory trees:

```
<repo_path>/<name>/<version>/package.py
```

## Version

rez uses a flexible versioning scheme that is a superset of semantic versioning. Versions are dot-separated tokens where each token can be numeric or alphanumeric:

```
1.2.3          # Numeric: major.minor.patch
2024.0         # Year-based
3.10.12        # Python-style
1.0.0.beta.2   # With alphanumeric tokens
```

Versions are compared token-by-token from left to right. Numeric tokens are compared numerically; alphanumeric tokens are compared lexicographically. Numeric tokens sort before alphanumeric tokens at the same position.

An **empty version** (no version string) represents an unversioned package, which has the lowest sort order.

## Version Range

A **version range** constrains which versions of a package are acceptable. The range syntax is:

| Syntax | Meaning |
|---|---|
| `1.2.3` | Exactly version 1.2.3 |
| `1.2+` | Version 1.2 or higher (1.2.0, 1.3, 2.0, ...) |
| `<2.0` | Anything below 2.0 (1.9.9, 1.0, 0.1, ...) |
| `1.2+<2.0` | At least 1.2, below 2.0 |
| `1.2\|2.0+` | Version 1.2 OR 2.0 and above |
| `1.2..1.5` | Range from 1.2 up to (but not including) 1.5 |
| `` | Any version (unconstrained) |

Ranges support union (`|`) and intersection. The solver intersects ranges when multiple packages request the same dependency.

## Requirement

A **requirement** combines a package name with a version range. This is what appears in a package's `requires` list:

```python
requires = [
    "maya-2024+",          # maya, version 2024 or later
    "python-3.10+<3.12",   # python, version 3.10.x or 3.11.x
    "numpy",               # numpy, any version
]
```

There are three types of requirements:

### Normal Requirements

```
foo-1.2+
```

The resolver must find a version of `foo` that satisfies `1.2+`. If it cannot, the resolve fails.

### Conflict Requirements

```
!bar
```

The package `bar` must NOT be in the resolve. This is used to express incompatibility.

### Weak Requirements

```
~baz-2.0+
```

If `baz` happens to be in the resolve (pulled in by another package), it must satisfy `2.0+`. But `baz` is not required to be present. This is useful for "if you use X, make sure it is at least version Y."

## Variant

A **variant** is a platform-specific or configuration-specific build of a package. A single package version can have multiple variants, each with its own set of additional requirements:

```python
name = "numpy"
version = "1.24.0"

requires = ["python"]

variants = [
    ["python-3.9"],
    ["python-3.10"],
    ["python-3.11"],
]
```

This declares that `numpy-1.24.0` has three variants: one for Python 3.9, one for 3.10, and one for 3.11. Each variant is installed into a separate subdirectory:

```
numpy/1.24.0/
  package.py
  python-3.9/    # Variant 0 payload
    lib/
  python-3.10/   # Variant 1 payload
    lib/
  python-3.11/   # Variant 2 payload
    lib/
```

The resolver picks the variant whose requirements are compatible with the rest of the resolve. If the resolve already includes `python-3.10`, only variant 1 is eligible.

Variants can have multiple dimensions:

```python
variants = [
    ["platform-linux", "arch-x86_64", "python-3.10"],
    ["platform-linux", "arch-x86_64", "python-3.11"],
    ["platform-windows", "arch-x86_64", "python-3.10"],
    ["platform-windows", "arch-x86_64", "python-3.11"],
]
```

## Resolved Context

A **resolved context** is the output of a successful dependency resolve. It contains:

- The original request (list of requirements)
- The resolved package list (exact name-version pairs with variant indices)
- The resulting environment (all env var changes from all packages' commands)
- Metadata (timestamp, resolve time, platform info)

Contexts can be:

- **Executed** -- spawn a shell with the configured environment
- **Saved** to a `.rxt` file -- a JSON file capturing the entire resolve
- **Loaded** from a `.rxt` file -- reconstruct the environment later
- **Diffed** -- compare two contexts to see what changed
- **Patched** -- modify an existing resolve by adding/changing requests

The `.rxt` file format makes environments reproducible. You can save the context from a successful render, attach it to a bug report, and another person can load it to reproduce the exact same environment.

## Rex Commands

**Rex** (Rez EXecution) is a domain-specific language for manipulating environment variables. It is embedded in `package.py` `commands()` functions:

```python
def commands():
    env.PATH.prepend("{root}/bin")
    env.PYTHONPATH.prepend("{root}/python")
    env.MY_CONFIG.set("/path/to/config")
    alias("mytool", "python {root}/scripts/run.py")
```

Rex commands are abstract -- they produce shell-specific output depending on the target shell. The same `commands()` function generates correct bash, cmd.exe, PowerShell, or zsh code.

rez-rs supports 8 shell types: bash, sh, csh, tcsh, zsh, cmd, powershell, and git-bash. Each shell has its own rex command interpreter that generates the correct shell-specific syntax.

Available proxy objects in rex:

- **`env`** -- environment variable manipulation
- **`this`** -- the current package (`this.root`, `this.name`, `this.version`)
- **`resolve`** -- access to other resolved packages (`resolve.python.version`)
- **`request`** -- the original resolve request
- **`system`** -- system info (`system.platform`, `system.arch`, `system.os`)

See [Rex Commands](rex-commands.md) for the full reference.

## Repository

A **repository** is a directory containing packages. rez-rs supports two repository types:

### Filesystem Repository

The standard repository. Packages are directories on disk:

```
/studio/packages/
  maya/2024.0/package.py
  python/3.10.12/package.py
  mylib/2.0.0/package.py
```

### Memory Repository

An in-memory repository used for testing and package building. Packages are stored as data structures rather than files.

Multiple repositories are configured via `packages_path` in `rezconfig.py`:

```python
packages_path = [
    "~/packages",          # Highest priority -- local dev packages
    "/studio/packages",    # Studio-wide shared packages
    "/opt/rez/packages",   # System packages
]
```

When resolving, rez searches repositories in order. The first repository containing a matching package wins.

## Package Family

A **package family** is the collection of all versions of a package with the same name. For example, `python` is a family containing versions `3.9.18`, `3.10.12`, `3.11.8`, etc.

Families are used in package ordering (e.g., prefer latest version within a family) and in search operations.

## Implicit Packages

**Implicit packages** are requirements automatically added to every resolve. They typically constrain the platform:

```python
implicit_packages = [
    "~platform=={system.platform}",
    "~arch=={system.arch}",
    "~os=={system.os}",
]
```

These are weak requirements (`~`): if a package in the resolve depends on `platform`, the implicit requirement ensures only the current platform is selected. Without implicts, a resolve might incorrectly pick a Linux variant of a package on a Windows machine.

## Ephemeral Packages

**Ephemeral packages** (prefixed with `_.`) exist only during a resolve and are not installed anywhere. They are used to pass configuration flags:

```bash
rez env myapp _.debug=1
```

A package can reference ephemerals in its commands:

```python
def commands():
    if "_.debug" in request:
        env.MY_APP_DEBUG.set("1")
```

Ephemerals provide a way to parameterize resolves without creating actual packages.

## Suite

A **suite** is a collection of resolved contexts with tool aliases. When multiple environments need to coexist (e.g., a Maya context and a Nuke context), a suite provides a unified set of tool wrappers that activate the correct context when invoked:

```bash
rez suite --create mysuite
rez suite --add maya-2024 --context "maya_ctx" mysuite
rez suite --add nuke-15 --context "nuke_ctx" mysuite
```

The suite directory contains wrapper scripts for each tool that activate the appropriate context.

## Bundle

A **bundle** is a self-contained copy of a resolved context, including all package payloads. It can be transferred to an offline machine (e.g., a render farm node without network access to the package repository) and used to recreate the environment locally.

```bash
rez bundle mycontext.rxt /path/to/bundle

# Create a relocatable bundle with patched library paths
rez bundle --patch-libs mycontext.rxt /path/to/bundle
```

The `--patch-libs` flag patches ELF (Linux) and Mach-O (macOS) binaries in the bundle to use relative library paths (RPATH/$ORIGIN), making the bundle fully relocatable to any filesystem location.
