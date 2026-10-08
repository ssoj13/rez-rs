# CLI Commands

rez-rs provides commands organized into five categories: resolve, query, dev, repo, and admin. Every command follows the pattern:

```
rez <command> [options] [arguments]
```

## rez deploy

Deploy the running native CLI and its copied command aliases. The destination defaults to the running executable's directory; deployment does not require Python initialization or Rez configuration.

```powershell
rez deploy --dry-run
rez deploy --bin-dir C:\rez3\Scripts\rez
rez deploy --bin-dir C:\rez3\Scripts\rez --source-zip .\rez-rs.zip
rez deploy --recover-only
rez deploy --help
```

`--aliases-only` omits primary executable installation. The installer preserves an existing owned source archive when `--source-zip` is omitted and refuses foreign or independently modified aliases. `--recover-only` cannot be combined with incoming-payload or dry-run options. See [installation](installation.md#deploy-cli-entry-points) for ownership, recovery and Windows executable handling. Use a binary built from source containing this command; older installations may not provide it. Recorded acceptance is scoped in [Plan30](https://github.com/ssoj13/rez-rs/blob/main/docs/plans/plan30.md).

## Resolve Commands

Commands for resolving environments and working with resolved contexts.

### rez env

Resolve packages and launch an interactive shell environment.

```bash
# Enter environment with specific packages
rez env maya-2024 python-3.10

# Run a command inside the environment (instead of interactive shell)
rez env maya-2024 -- maya -batch -file scene.ma

# Patch an existing context (add/modify packages)
rez env --patch mylib-3.0+

# Use a specific shell
rez env --shell bash maya-2024

# Dry-run: show what would be resolved without entering the shell
rez env --dry-run maya-2024 python-3.10
```

`rez env` is the most commonly used command. It resolves all requested packages and their transitive dependencies, executes the rex commands from each package, and spawns a new shell session with the resulting environment.

### rez context

Inspect a resolved context (`.rxt` file).

```bash
# Show context from an .rxt file
rez context mycontext.rxt

# Show the current context (if inside a resolved shell)
rez context

# Show specific fields
rez context --field packages mycontext.rxt
rez context --field request mycontext.rxt
```

### rez interpret

Interpret rex commands and output shell code.

```bash
# Interpret a rex command string
rez interpret "env.PATH.prepend('/usr/local/bin')"

# Output for a specific shell
rez interpret --shell bash "env.FOO.set('bar')"

# Read from a file
rez interpret --file commands.rex
```

This is useful for debugging rex commands or generating shell snippets from rex code.

### rez complete

Generate shell completions for package names.

```bash
# Generate completion for a partial package name
rez complete maya

# Generate completions for bash
rez complete --shell bash
```

### rez suite

Manage rez suites -- collections of resolved contexts with unified tool access.

```bash
# Create a new suite
rez suite --create mysuite

# Add a context to the suite
rez suite --add "maya-2024 mylib-2+" --context maya_ctx mysuite

# List suite contents
rez suite --list mysuite

# Remove a context
rez suite --remove maya_ctx mysuite
```

### rez bundle

Bundle a resolved context for offline use.

```bash
# Bundle an existing .rxt file
rez bundle mycontext.rxt /path/to/bundle

# Bundle with library patching for relocatable binaries
rez bundle --patch-libs mycontext.rxt /path/to/bundle
```

Bundles are self-contained: they include both the resolved context and all package files, so they can be used on machines without access to the original package repositories.

The `--patch-libs` flag patches ELF (Linux) and Mach-O (macOS) binaries in the bundle to use relative library paths, making the bundle fully relocatable.

## Query Commands

Commands for searching and inspecting packages.

### rez search

Search for packages across all configured repositories.

```bash
# List all available packages
rez search

# Search by name (glob patterns supported)
rez search maya
rez search "my*"

# Search in specific paths
rez search --paths ~/packages

# Show versions for a package
rez search maya --versions

# Filter by package type
rez search --type python

# Format output
rez search --format json maya

# Filter by timestamp
rez search --before 2026-01-01 --after 2025-01-01

# Validate packages (check for schema errors)
rez search --validate
```

### rez view

View detailed information about a specific package.

```bash
# View latest version
rez view maya

# View specific version
rez view maya-2024.0

# Show all fields
rez view --all maya-2024.0
```

Output includes name, version, description, requires, variants, commands, tools, and other metadata fields.

### rez help-pkg

Display help information for a package.

```bash
# Show package help (from the 'help' field in package.py)
rez help-pkg maya

# Open help URL in browser
rez help-pkg --open maya
```

### rez depends

Perform reverse dependency lookup -- find packages that depend on a given package.

```bash
# What depends on python?
rez depends python

# What depends on python-3.10?
rez depends python-3.10

# Search in specific paths
rez depends --paths /studio/packages python
```

### rez diff

Compare two package versions.

```bash
# Compare two versions of the same package
rez diff mylib-1.0.0 mylib-2.0.0

# Compare a package against its latest version
rez diff mylib-1.0.0
```

### rez plugins

List package plugins.

```bash
# Show packages that have plugins
rez plugins maya

# List all plugin packages
rez plugins
```

## Dev Commands

Commands for building, testing, and releasing packages.

### rez build

Build the current package.

```bash
# Build in the current directory
rez build

# Install after building
rez build --install

# Clean build (remove previous build artifacts)
rez build --clean

# Specify build system explicitly
rez build --build-system cmake

# Set number of build threads
rez build --jobs 8

# Verbose build output
rez build --verbose

# Force rebuild even if variant is already installed
rez build --install --force
```

By default, `rez build --install` skips variants that are already installed. Use `--force` / `-f` to rebuild and reinstall.

The build system is selected in this order:

1. **CLI** — `rez build --build-system <name>` (overrides everything)
2. **Package** — `build_system = "..."` in package.py (used when CLI does not specify)
3. **Auto-detect** — from project files

| File | Build System |
|---|---|
| `CMakeLists.txt` | CMake |
| `Cargo.toml` | Cargo |
| `setup.py` without a valid PEP 517 build-system table | Python (setuptools) |
| `pyproject.toml` with `[build-system]` | Pip |
| `requirements.txt` without `setup.py` or `pyproject.toml` | Pip requirements install |
| `bunfig.toml` or `bun.lockb` + `package.json` | Bun |
| `package.json` | Node.js |
| `SConstruct` / `SConscript` | SCons |
| `vcpkg.json` | Vcpkg |
| `conanfile.py` / `conanfile.txt` | Conan |
| `sources.yaml` / `sources.yml` | Extraction |
| `Makefile` | Make |
| `build_command` in package.py | Custom |
| (none) | NoOp |

Detection priority: CMake > Cargo > Pip > Python > Bun > Node.js > SCons > Vcpkg > Conan > Extraction > Make. If both `bunfig.toml` and `package.json` exist, Bun takes priority over Node.js.

### rez test

Run package tests defined in the `tests` field of `package.py`.

```bash
# Run all tests
rez test

# Run a specific test
rez test unit

# List available tests
rez test --list
```

Tests are defined in `package.py`:

```python
tests = {
    "unit": {
        "command": "python -m pytest tests/",
        "requires": ["pytest"],
    },
    "lint": {
        "command": "flake8 python/",
        "requires": ["flake8"],
    },
}
```

### rez release

Build and deploy a package to the release repository.

```bash
# Release current package
rez release

# Release with a message
rez release --message "Bug fix for animation curve export"

# Skip VCS tagging
rez release --no-tag

# Release to a specific path
rez release --path /studio/packages
```

Release performs: build -> install to release_packages_path -> git tag (if in a git repo).

## Repo Commands

Commands for managing package repositories.

### rez cp

Copy packages between repositories.

```bash
# Copy a specific version
rez cp maya-2024.0 /dest/packages

# Copy all versions of a package
rez cp maya /dest/packages

# Dry run
rez cp --dry-run maya-2024.0 /dest/packages
```

### rez mv

Move packages between repositories.

```bash
# Move a package
rez mv mylib-1.0.0 /archive/packages
```

### rez rm

Remove packages from a repository.

```bash
# Remove a specific version
rez rm mylib-1.0.0

# Remove with force (no confirmation)
rez rm --force mylib-1.0.0
```

### rez pkg-cache

Manage the local package cache.

```bash
# Show cache status
rez pkg-cache --status

# Clean the cache
rez pkg-cache --clean

# Show cache statistics
rez pkg-cache --stats
```

### rez pkg-ignore

Ignore or unignore packages from resolves.

```bash
# Ignore a package (excluded from future resolves)
rez pkg-ignore mylib-1.0.0

# Unignore
rez pkg-ignore --unignore mylib-1.0.0

# List ignored packages
rez pkg-ignore --list
```

Ignoring a package creates a `.ignore` marker file in the package directory. The resolver skips packages with this marker.

### rez bind

Bind system software as rez packages. Detects installed tools and creates package definitions. `rez bind rez` copies the rez binary into the package for use in resolved environments.

```bash
# Bind all common system packages (platform, arch, os, python, rez)
rez bind --quickstart

# Detect and bind all available software (full scan: DCC apps, tools, etc.)
rez bind --all

# Bind a specific package type
rez bind platform
rez bind python
rez bind rez
rez bind cmake

# Bind to a specific repository
rez bind -i ~/packages python

# For python: copy venv binaries (--copies) instead of symlinks; more relocatable
rez bind python --copies

# Verbose output (show what is detected)
rez bind --verbose python
```

Available bind modules: `platform`, `arch`, `os`, `python`, `pip`, `setuptools`, `rez`, `cmake`, `gcc`, plus DCC apps (maya, houdini, blender, nuke, etc.). Use `rez bind --list` to see all. With `--all`, modules are bound in dependency order (system → python stack → tools → DCC). Variant subdirs follow `default_hashed_variants` from rezconfig. Custom modules from `bind_module_path` in rezconfig appear in `--list`; binding them requires Python rez (not yet implemented in rez-rs).

### rez pip

Install pip packages as rez packages. Installs to release path by default.

```bash
# Install a pip package (release path by default)
rez pip -i requests

# Install to local packages path
rez pip -i --local requests

# Install to a specific path
rez pip -i requests -p ~/packages

# Pass extra args to pip after "--"
rez pip -i torch torchvision -- --index-url https://download.pytorch.org/whl/cu130

# Global flags (-v, -l) work anywhere in the command
rez -v pip -i numpy
rez pip -i numpy -vv
```

### rez yaml2py

Convert `package.yaml` to `package.py` format.

```bash
# Convert a single file
rez yaml2py package.yaml

# Convert and write output
rez yaml2py package.yaml --output package.py
```

## Admin Commands

Commands for system administration and diagnostics.

### rez config

Query or display configuration settings.

```bash
# Show all config
rez config

# Show a specific setting
rez config packages_path

# Show in JSON format
rez config --json packages_path
```

### rez status

Show rez system status.

```bash
rez status
```

Output includes:

- rez-rs version
- Platform, architecture, OS
- Detected shell
- Package paths
- Python version (embedded RustPython)
- Configuration file locations

### rez selftest

Run the rez-rs test suite.

```bash
# Run all tests
rez selftest

# Run with verbose output
rez selftest --verbose
```

This executes `cargo test` on the rez-rs source.

### rez benchmark

Run benchmarking suite for package resolves.

```bash
# Run default benchmark
rez benchmark

# Run with specific number of iterations
rez benchmark --iterations 100

# Compare two runs
rez benchmark --compare baseline.json

# Output results to file
rez benchmark --output results.json
```

Benchmarks measure resolve time for various dependency graph sizes and display histograms.

### rez memcache

Manage Memcached servers used for shared caching. Requires `memcached_uri` in rezconfig; if empty, exits with "memcaching is not enabled."

```bash
# Default: show summary table (uptime, hits, misses, hit ratio)
rez memcache

# Detailed statistics per server
rez memcache --stats

# Flush all cached data
rez memcache --flush

# Warm cache (scan packages_path to populate)
rez memcache --warm
rez memcache --warm -v   # verbose

# Poll for connectivity (placeholder)
rez memcache --poll
```

See [Caching and Memcached](caching.md) for setup and usage.

### rez python

Run the embedded Python interpreter.

```bash
# Start interactive REPL
rez python

# Execute a code string
rez python -c "print('hello from RustPython')"

# Run a script file
rez python script.py
```

The embedded Python is RustPython 0.6.0 with stdlib. It can import standard library modules (os, sys, json, pathlib, etc.) but cannot use C-extension packages (numpy, etc.).

### rez forward

Execute a forwarding YAML script (hidden command, used internally).

```bash
rez forward script.yaml
```

This is a hidden command used by suite wrappers and internal tooling. It reads a YAML file describing a command to forward to another rez context.
