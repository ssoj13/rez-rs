# Package Format

This chapter is the complete reference for rez package definition files. A package is defined by a `package.py`, `package.yaml`, or `package.toml` file at the root of its version directory.

## File Formats

rez-rs checks for package definition files in this order:

1. `package.py` -- Python format (most powerful, supports logic)
2. `package.yaml` -- YAML format (declarative, simpler)
3. `package.toml` -- TOML format (declarative, simpler)

The first file found is used. In most studios, `package.py` is the standard because it supports dynamic values and logic in the `commands()` function.

rez-rs can generate `package.py` files from other formats using `rez yaml2py` or via the internal `dump_package_data_py()` function.

## Schema Validation

rez-rs validates all package files against an internal schema when loading them. This ensures:

- Required fields are present (`name` is mandatory)
- Field types are correct (e.g., `requires` must be a list of strings)
- Values are valid (version strings, requirement syntax)
- Unknown fields are reported as warnings

Use `rez search --validate` to check all packages in your repositories for schema errors.

## Package Fields

### Core Fields

#### name (required)

```python
name = "maya"
```

The package name. Must be a valid identifier: lowercase letters, digits, and underscores. No hyphens in names (hyphens separate name from version in requirement strings).

#### version (recommended)

```python
version = "2024.0"
```

The package version string. Follows rez versioning: dot-separated tokens, each numeric or alphanumeric. If omitted, the package is "unversioned" and sorts below all versioned packages.

Common patterns:

```python
version = "1.2.3"        # semver
version = "2024.0"       # year-based
version = "3.10.12"      # Python-style
version = "1.0.0.beta"   # with alpha tokens
```

#### description

```python
description = "Autodesk Maya 3D animation software"
```

Human-readable description. Shown by `rez view`.

#### authors

```python
authors = ["Pipeline Team", "Jane Doe <jane@studio.com>"]
```

List of author strings. Informational only.

### Dependency Fields

#### requires

```python
requires = [
    "python-3.10+<3.12",
    "numpy-1.24+",
    "openexr-3+",
]
```

List of package requirements. Each entry is a requirement string: package name optionally followed by a version range.

Requirement syntax:

| Example | Meaning |
|---|---|
| `"python"` | Any version of python |
| `"python-3.10"` | Exactly python 3.10 |
| `"python-3.10+"` | Python 3.10 or later |
| `"python-3.10+<3.12"` | Python 3.10.x or 3.11.x |
| `"!legacy_tool"` | Conflict: legacy_tool must NOT be present |
| `"~openexr-3+"` | Weak: if openexr is present, it must be 3+ |

#### build_requires

```python
build_requires = [
    "cmake-3.20+",
    "gcc-11+",
]
```

Requirements needed only at build time. Not included in the resolved environment at runtime.

#### private_build_requires

```python
private_build_requires = [
    "doxygen-1.9+",
]
```

Build requirements that are not visible to other packages. Used for tools that are only needed during this package's build and should not affect dependency resolution for dependent packages.

### Variant Fields

#### variants

```python
variants = [
    ["platform-linux", "python-3.10"],
    ["platform-linux", "python-3.11"],
    ["platform-windows", "python-3.10"],
    ["platform-windows", "python-3.11"],
]
```

Each inner list is a variant -- a set of additional requirements that specialize the package for a specific configuration. Each variant is installed into its own subdirectory:

```
mypackage/1.0.0/
  package.py
  platform-linux/python-3.10/    # variant 0
  platform-linux/python-3.11/    # variant 1
  platform-windows/python-3.10/  # variant 2
  platform-windows/python-3.11/  # variant 3
```

The variant subdirectory path is constructed from the variant requirements' names and version constraints. For a variant `["python-3.10"]`, the subpath is `python-3.10/`.

If no variants are defined, the package has a single implicit variant with no extra requirements, and the payload lives directly in the version directory.

#### hashed_variants

```python
hashed_variants = True
```

When `True`, variant subdirectory names are hashed instead of using the human-readable path. This avoids filesystem path length issues with many-dimensional variants.

### Command Fields

#### commands

```python
def commands():
    env.PATH.prepend("{root}/bin")
    env.PYTHONPATH.prepend("{root}/python")
    env.MAYA_PLUG_IN_PATH.prepend("{root}/plug-ins")
    alias("mytool", "{root}/bin/mytool")
```

The main environment configuration function. Called when this package is part of a resolved context. Commands are executed in package dependency order.

Commands can also be defined as a string (for non-Python formats or simple packages):

```python
commands = """
env.PATH.prepend("{root}/bin")
env.PYTHONPATH.prepend("{root}/python")
"""
```

See [Rex Commands](rex-commands.md) for the full command reference.

#### pre_commands

```python
def pre_commands():
    env.MY_EARLY_VAR.set("initialized")
```

Executed before the main `commands()` of all packages. Used for setup that must happen before other packages configure the environment.

#### post_commands

```python
def post_commands():
    env.MY_FINAL_CHECK.set("validated")
```

Executed after the main `commands()` of all packages. Used for finalization or validation.

#### pre_build_commands

```python
def pre_build_commands():
    env.CMAKE_PREFIX_PATH.prepend("{root}")
```

Executed before building. Sets up the build environment.

#### pre_test_commands

```python
def pre_test_commands():
    env.TEST_DATA_PATH.set("{root}/test_data")
```

Executed before running tests.

### Tool Fields

#### tools

```python
tools = ["maya", "mayapy", "mayabatch"]
```

List of executable tools this package provides. Informational -- used by suites to create wrapper scripts and by `rez search` to find packages by tool name.

### Plugin Fields

#### has_plugins

```python
has_plugins = True
```

Indicates this package supports plugins (other packages can extend it).

#### plugin_for

```python
plugin_for = ["maya"]
```

Declares this package is a plugin for the listed packages.

### Build Fields

#### build_system

```python
build_system = "cmake"
```

Explicitly specify the build system. If omitted, rez detects it from project files.

Valid values: `cmake`, `make`, `python`, `pip`, `cargo`, `nodejs`, `bun`, `scons`, `vcpkg`, `conan`, `extraction`, `custom`, `noop`.

If `build_system` is set and the CLI does not pass `--build-system`, rez uses the package value. This allows Extraction packages that configure sources via `package.config` (without `sources.yaml`) to be built correctly.

#### build_command

```python
build_command = "python setup.py install --prefix={install_path}"
```

Custom build command. Used when `build_system` is `custom` or when you want to override the detected build system's default command.

Available substitutions:

- `{root}` -- package source directory
- `{install_path}` -- where to install the built package

#### requires_rez_version

```python
requires_rez_version = "0.1.0+"
```

Minimum rez version required to use this package.

### Metadata Fields

#### uuid

```python
uuid = "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
```

Unique identifier for the package family. Used to detect renamed packages.

#### help

```python
# Simple help string
help = "https://docs.mypackage.com"

# Or structured help with multiple sections
help = [
    ["User Guide", "https://docs.mypackage.com/guide"],
    ["API Reference", "https://docs.mypackage.com/api"],
    ["Changelog", "https://docs.mypackage.com/changelog"],
]
```

Help information. Can be a single URL/string or a list of `[label, url]` pairs. Displayed by `rez help-pkg`.

#### relocatable

```python
relocatable = True
```

Whether the package can be moved to a different filesystem location without breaking. Default is controlled by the `default_relocatable` config setting.

#### cachable

```python
cachable = True
```

Whether the package can be cached locally. Default is controlled by the `default_cachable` config setting.

### Test Fields

#### tests

```python
tests = {
    "unit": {
        "command": "python -m pytest tests/ -v",
        "requires": ["pytest-7+"],
    },
    "lint": {
        "command": "ruff check python/",
        "requires": ["ruff"],
    },
    "integration": {
        "command": "python tests/integration/run.py",
        "requires": ["pytest", "requests"],
        "on_variants": {"type": "requires", "value": ["python-3.10"]},
    },
}
```

Dictionary of named test configurations. Each test has:

- `command` -- shell command to run
- `requires` (optional) -- additional packages needed for this test
- `on_variants` (optional) -- restrict which variants this test applies to

### Release Fields

#### timestamp

```python
timestamp = 1706745600
```

Unix timestamp of when the package was released. Usually set automatically by `rez release`.

#### changelog

```python
changelog = "Fixed memory leak in texture loader"
```

Release changelog text. Usually extracted from VCS history by `rez release`.

#### revision

```python
revision = {"branch": "main", "commit": "abc123def456"}
```

VCS revision information. Set automatically by `rez release`.

#### previous_version

```python
previous_version = "1.0.0"
```

The version this release supersedes. Set automatically.

#### vcs

```python
vcs = "git"
```

Version control system. Set automatically.

### Config Override

#### config

```python
config = {
    "release_packages_path": "/special/packages",
}
```

Per-package configuration overrides. These override the global rezconfig for operations involving this package.

**Extraction build system:** Use `config.extraction.downloads` to define archive sources when not using `sources.yaml`:

```python
config = {
    "extraction": {
        "downloads": [
            {"path": "archive.zip"},
            {"url": "https://example.com/foo.zip", "file_name": "foo.zip", "checksum": {"sha256": "..."}},
        ],
    },
}
build_system = "extraction"
```

Each entry may have `path` (local file), `url` (HTTP download with optional resume), `file_name`, and `checksum` (e.g. `{"sha256": "hex"}`).

## Decorators

### @early

```python
@early()
def requires():
    import os
    base = ["python-3.10+"]
    if os.environ.get("USE_GPU"):
        base.append("cuda-11+")
    return base
```

The `@early` decorator causes the function to be evaluated at package load time (when the package.py is first read). The function's return value replaces the field. This is useful for dynamic dependencies that depend on the environment.

### @late

```python
@late()
def commands():
    env.PATH.prepend("{root}/bin")
    # This is evaluated at resolve time, not load time
```

The `@late` decorator causes the function to be evaluated at resolve time rather than load time. This is the default for `commands`, `pre_commands`, and `post_commands`, so `@late` is rarely needed explicitly.

## include()

```python
include("common_commands")

name = "mypackage"
version = "1.0.0"

def commands():
    common_setup()  # Function from included module
    env.PATH.prepend("{root}/bin")
```

The `include()` function imports shared code from `_include/` directories found in `packages_path`. This allows reusing common functions across multiple packages.

The search order is:

1. `<packages_path[0]>/_include/common_commands.py`
2. `<packages_path[1]>/_include/common_commands.py`
3. ...

The first matching file is loaded.

## YAML Format

`package.yaml` example:

```yaml
name: maya_plugin
version: 2.1.0
description: Custom Maya plugin

requires:
  - maya-2024+
  - python-3.10+<3.12

variants:
  - [platform-linux, arch-x86_64]
  - [platform-windows, arch-x86_64]

tools:
  - my_maya_tool

commands: |
  env.MAYA_PLUG_IN_PATH.prepend("{root}/plug-ins")
  env.PYTHONPATH.prepend("{root}/python")
```

YAML limitations: no `@early`/`@late` decorators, no `include()`, no Python logic in commands (string commands only).

## TOML Format

`package.toml` example:

```toml
name = "maya_plugin"
version = "2.1.0"
description = "Custom Maya plugin"

requires = ["maya-2024+", "python-3.10+<3.12"]

variants = [
    ["platform-linux", "arch-x86_64"],
    ["platform-windows", "arch-x86_64"],
]

tools = ["my_maya_tool"]

commands = """
env.MAYA_PLUG_IN_PATH.prepend("{root}/plug-ins")
env.PYTHONPATH.prepend("{root}/python")
"""
```

Same limitations as YAML.

## Complete Example: Complex Package

```python
name = "studio_maya"
version = "2024.1.3"
description = "Studio Maya distribution with custom plugins and configuration"

authors = ["Pipeline Team"]

uuid = "d4e5f6a7-b8c9-0123-4567-89abcdef0123"

requires = [
    "maya-2024.0+<2025",
    "python-3.10+<3.12",
    "pymel-1.4+",
    "numpy-1.24+",
    "~openexr-3+",       # Weak: if present, must be 3+
    "!legacy_pipeline",  # Conflict: incompatible with old pipeline
]

build_requires = [
    "cmake-3.20+",
]

variants = [
    ["platform-linux", "arch-x86_64"],
    ["platform-windows", "arch-x86_64"],
]

tools = ["studio_maya", "sm_batch", "sm_render"]

has_plugins = True

help = [
    ["User Guide", "https://wiki.studio.com/maya"],
    ["API Docs", "https://docs.studio.com/studio_maya/api"],
]

relocatable = False
cachable = True

tests = {
    "unit": {
        "command": "mayapy -m pytest {root}/tests/unit",
        "requires": ["pytest-7+"],
    },
    "integration": {
        "command": "mayapy {root}/tests/integration/run.py",
        "requires": ["pytest-7+"],
    },
}

def pre_commands():
    env.STUDIO_MAYA_VERSION.set("{version}")

def commands():
    env.PATH.prepend("{root}/bin")
    env.PYTHONPATH.prepend("{root}/python")
    env.MAYA_PLUG_IN_PATH.prepend("{root}/maya/plug-ins")
    env.MAYA_SCRIPT_PATH.prepend("{root}/maya/scripts")
    env.MAYA_SHELF_PATH.prepend("{root}/maya/shelves")
    env.XBMLANGPATH.prepend("{root}/maya/icons")

    alias("studio_maya", "maya -script {root}/scripts/startup.mel")
    alias("sm_batch", "mayabatch -script {root}/scripts/startup.mel")

    if resolve.get("openexr"):
        env.MAYA_RENDER_SETUP.set("exr_enabled")

def post_commands():
    env.STUDIO_ENV_READY.set("1")
```

## Repository Layout

The standard filesystem layout for a package repository:

```
/packages/
  _include/                    # Shared include modules
    common_commands.py
    studio_defaults.py
  maya/
    2024.0/
      package.py
      bin/
      python/
      plug-ins/
    2024.1/
      package.py
      ...
  python/
    3.10.12/
      package.py
    3.11.8/
      package.py
  numpy/
    1.24.0/
      package.py
      python-3.10/             # variant 0
        lib/python3.10/site-packages/
      python-3.11/             # variant 1
        lib/python3.11/site-packages/
  platform/
    linux/
      package.py
    windows/
      package.py
```
