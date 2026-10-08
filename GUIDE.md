# rez-rs Developer Guide

**rez-rs** is a Rust port of the Python [rez](https://github.com/AcademySoftwareFoundation/rez) package manager for VFX/CG production pipelines. It manages software environments by resolving package dependencies and configuring shells with the correct environment variables.

---

## Table of Contents

- [Project Overview](#project-overview)
- [Module Reference](#module-reference)
- [CLI Commands](#cli-commands)
  - [Admin](#admin-commands)
  - [Dev](#dev-commands)
  - [Query](#query-commands)
  - [Repo](#repo-commands)
  - [Resolve](#resolve-commands)
  - [GUI](#gui-command)
- [Setup Examples](#setup-examples)
- [Bind & DCC Detectors](#bind--dcc-detectors)

---

## Project Overview

| Area | Description |
|------|-------------|
| **Language** | Rust (with embedded RustPython VM for package.py evaluation) |
| **Purpose** | Dependency resolution, environment management, package building/releasing for CG pipelines |
| **Config** | `rezconfig.toml` or `rezconfig.py` (Python-style config) |
| **Package formats** | `package.py`, `package.yaml`, `package.toml` |
| **Shells** | bash, zsh, sh, csh, tcsh, cmd, powershell |
| **Build systems** | CMake, Make, Cargo, Python, Pip, Node.js, Bun, SCons, Conan, Vcpkg, Custom, Extraction, Noop |
| **GUI** | Optional egui-based graphical interface |

---

## Module Reference

| Module | Purpose | Key Types / Functions |
|--------|---------|----------------------|
| `version/` | Version parsing, ranges, requirements | `Version`, `VersionRange`, `Requirement`, `RequirementList` |
| `package/core` | Package data model | `Package`, `Variant`, `DeveloperPackage`, `PackageFamily` |
| `package/discover` | Package discovery on disk | `iter_packages()`, `get_latest_package()`, `get_completions()` |
| `package/search` | Search by glob/regex/prefix | `PackageSearcher`, `get_reverse_dependencies()` |
| `package/filter` | Package filtering rules | `PackageFilter`, `PackageFilterList`, `Rule` (Or/And/Not/Glob/Regex) |
| `package/order` | Sort order strategies | `VersionSplitOrder`, `TimestampPackageOrder`, `PackageOrderList` |
| `package/ops` | CRUD operations on packages | `copy_package()`, `move_package()`, `remove_package()` |
| `package/bind/` | Bind system/DCC software as rez packages | `bind_package()`, `bind_all()`, 22 DCC detectors |
| `package/cache` | Local variant caching | `PackageCache`, `VariantHandle`, `CachedVariantInfo` |
| `package/test` | Package test runner | `PackageTestRunner`, `TestSpec`, `TestResult` |
| `package/maker` | Programmatic package creation | `PackageMaker` (builder pattern) |
| `repository` | Package repositories | `FsRepo`, `FsRepoCached`, `FsRepoMemcached`, `MemoryPackageRepository`, `PackageRepositoryManager` |
| `resolve/context` | Resolved environment context | `ResolvedContext` — resolve, save/load .rxt, spawn shell, get environ |
| `resolve/solver` | SAT-like dependency solver | `Solver`, `PackageProvider`, `SolverState`, `FailureReason` |
| `resolve/resolver` | High-level resolve API | `Resolver`, `resolve()`, `resolve_filtered()` |
| `shell/types` | Shell implementations | `BashShell`, `CshShell`, `CmdShell`, `PowerShellShell`, `create_shell()` |
| `shell/rex` | Rex DSL interpreter | `RexExecutor`, `ActionManager`, `Action` (setenv, appendenv, alias...) |
| `shell/wrapper` | Tool wrapper scripts | `create_wrapper()`, `create_forwarding_script()` |
| `builders/` | 13 build systems | `BuildProcess`, `BuildSystem` trait, `detect_build_system()` |
| `config` | Configuration | `RezConfig`, global `CONFIG` singleton, TOML/Python loaders |
| `serialise` | Package serialization | `load_package_data()`, `dump_package_data()`, `exec_package_py()` |
| `python_vm` | Embedded Python (RustPython) | `exec_py_globals()`, `start_repl()`, `run_script()` |
| `platform` | OS/arch detection | `Platform`, `Arch`, `OsVersion`, `SystemInfo` |
| `suite` | Suite management | `Suite`, `SuiteContext`, `SuiteTool` |
| `bundle_context` | Context bundling for deployment | `bundle_context()`, `remap_context()` |
| `amqp` | Event notifications | `publish_message()`, `publish_context_tracking()` |
| `gui` | Graphical interface (egui) | `RezGuiApp` — package browser and resolver |
| `logging` | Logging subsystem | `init()`, macros: `log_info!`, `log_debug!`, `log_trace!` |
| `errors` | Error types | `RezError` — 30+ variants covering all subsystems |
| `rez_path` | Cross-platform paths | `RezPath` — internal Unix-style, auto-converts to OS paths |
| `util` | Utilities | `stable_hash()` — deterministic FNV-1a hash |

---

## CLI Commands

### Global Options

```
rez [OPTIONS] <COMMAND>

Options:
  --write-config [PATH]   Create default rezconfig.py ("bin" = next to exe, omit = ~/.rez/)
  --force                 Overwrite existing config
  -v, --verbose           Increase log verbosity (-v INFO, -vv DEBUG, -vvv TRACE)
  -l, --log [FILE]        Redirect logs to file (default: rez.log)
```

---

### Admin Commands

#### `rez status`

Show system status and current environment info.

```
rez status [OPTIONS] [OBJECT]

Arguments:
  [OBJECT]    Object to query (tool, package, context or suite name)

Options:
  -t, --tools   List visible rez tools on PATH
```

**Examples:**
```bash
rez status                  # Full system status
rez status --tools          # List all rez tools on PATH
rez status context.rxt      # Inspect a specific context file
```

---

#### `rez config`

Query rez configuration.

```
rez config [OPTIONS] [FIELD]

Arguments:
  [FIELD]    Config field (dot notation: "packages_path", "critical_color.fore")

Options:
  --json           Output dict/list as JSON
  --search-list    List config search paths
  --source-list    List loaded config files
```

**Examples:**
```bash
rez config packages_path           # Show packages_path setting
rez config --json packages_path    # As JSON (for REZ_*_JSON env vars)
rez config --search-list           # Where rez looks for config files
rez config --source-list           # Which config files are actually loaded
rez config                         # Dump entire config as pretty JSON
```

---

#### `rez benchmark`

Run resolve benchmarks and compare results.

```
rez benchmark [OPTIONS]

Options:
  --out DIR             Output directory (default: "out")
  --iterations N        Iterations per resolve (default: 1)
  --histogram           Show ASCII histogram from previous results
  --compare RESULTS     Compare with another results directory
```

**Examples:**
```bash
rez benchmark --out bench1 --iterations 5
rez benchmark --histogram --out bench1
rez benchmark --compare bench1 --out bench2
```

---

#### `rez memcache`

Manage memcached integration for package caching.

```
rez memcache [OPTIONS]

Options:
  --flush          Flush all cache entries
  --stats          Show cache statistics
  --reset-stats    Reset server statistics
  --poll           Continuous monitoring (gets/sets per second)
  --interval SECS  Polling interval (default: 1.0)
  --warm           Warm cache with visible packages
```

Requires `memcached_uri` configured in rezconfig.

---

#### `rez python`

Run the embedded RustPython interpreter.

```
rez python [OPTIONS] [SCRIPT] [ARGS...]

Options:
  -c CODE       Execute Python code
  -V, --version Show Python version
```

**Examples:**
```bash
rez python                          # Interactive REPL
rez python -c "print('hello')"     # Execute code
rez python script.py arg1 arg2     # Run script
rez python -V                       # Show version
```

---

#### `rez selftest`

Run rez-rs self-tests.

```
rez selftest [OPTIONS] [TESTS...]

Options:
  -v, --verbose   Verbose output (can repeat)
```

Delegates to `cargo test` internally.

---

#### `rez forward`

Execute a YAML forwarding script (used internally by suite tool wrappers).

```
rez forward <YAML> [ARGS...]
```

---

### Dev Commands

#### `rez build`

Build a package from source.

```
rez build [OPTIONS]

Options:
  -s, --build-system SYS   Build system (cmake, make, python, pip, cargo, nodejs, bun, custom, noop)
  -i, --install             Install build to local packages path
  --install-path PATH       Custom install path
  -c, --clean               Clean build directory before rebuild
  -v, --variant INDEX...    Build specific variant indices (zero-indexed)
  -p, --prefix PATH         Override source directory
  --no-local                Exclude local packages from search
  --scripts                 Create build scripts only (no actual build)
  --build-args ARGS         Extra arguments for build system
  --child-build-args ARGS   Extra arguments for child build (e.g. make under cmake)
  -f, --force               Force rebuild even if variant is installed
  --fail-graph              Show resolve graph on failure
```

**Examples:**
```bash
cd ~/packages/my_tool
rez build                           # Auto-detect build system, build only
rez build -i                        # Build and install to local
rez build -i -c                     # Clean build + install
rez build -s cmake -i               # Force CMake build system
rez build -v 0 -v 1 -i             # Build only variants 0 and 1
rez build --install-path /studio/packages -i
rez build --build-args "--parallel 8"
```

---

#### `rez release`

Release a package (build + install to release path + VCS tag).

```
rez release [OPTIONS]

Options:
  -m, --message MESSAGE           Release message
  --vcs TYPE                  Force VCS type (git)
  --no-latest                 Allow releasing earlier version than latest
  --ignore-existing-tag       Release even if git tag exists
  --skip-repo-errors          Skip VCS-related errors
  --no-message                Don't prompt for release message
  --buildsys SYS              Build system to use
  -c, --clean                 Clean build directory first
  --variants INDEX...         Build specific variants
  -v, --verbose               Verbose output
```

**Examples:**
```bash
cd ~/packages/my_tool
rez release -m "Initial release"
rez release -c --clean              # Clean build + release
rez release --skip-repo-errors      # Release without VCS checks
```

**Release workflow:**
1. Pre-release hooks run
2. VCS validation (clean tree, no detached HEAD, no existing tag)
3. Version check (>= latest in release repo)
4. Build + install to release path
5. Git tag created
6. Post-release hooks run

---

#### `rez test`

Run package tests.

```
rez test [OPTIONS] [PACKAGE]

Arguments:
  [PACKAGE]    Package to test (default: developer package in cwd)

Options:
  -l, --list                List available tests
  -t, --tests NAMES...      Run specific tests (comma-separated)
  --run-on STAGE            Filter by run_on stage
  -s, --stop-on-fail        Stop on first failure
  --dry-run                 Show what would run
  --inplace                 Run in current environment
  --extra-packages PKG...   Extra packages for test environment
  -p, --paths PATHS         Package search paths
  --no-local                Exclude local packages
  -v, --verbose             Verbose output
```

**Examples:**
```bash
rez test                        # Run all tests for cwd package
rez test -l                     # List available tests
rez test -t unit,integration    # Run specific tests
rez test my_pkg                 # Test installed package
rez test --inplace              # Test in current environment
```

---

### Query Commands

#### `rez search`

Search for packages.

```
rez search [OPTIONS] [QUERY]

Options:
  -l, --latest              Only latest version per package
  --no-versions             Family names only
  --reverse PKG             Show reverse dependencies
  -f, --format FMT          Custom format (e.g. "{qualified_name} | {description}")
  --before TIME             Packages released before time
  --after TIME              Packages released after time
  -e, --errors              Only packages with errors
  --validate                Validate packages during search
  -o, --output-format FMT   Output format: plain or json
  --paths PATHS             Package search paths
  --no-local                Exclude local packages
```

**Examples:**
```bash
rez search                          # List all package families
rez search "maya*"                  # Glob search
rez search python -l                # Latest python version
rez search --reverse maya           # What depends on maya?
rez search --after -7d              # Packages released in last 7 days
rez search -f "{qualified_name} | {description}" -l
rez search --no-versions            # Family names only
rez search -o json -l               # JSON output
```

**Time format:** epoch seconds, or relative: `-10s`, `-5m`, `-2h`, `-7d`, `-1w`

---

#### `rez view`

View package details.

```
rez view [OPTIONS] <PACKAGE>

Options:
  -a, --all           Show all versions
  -b, --brief         Brief output (name + version only)
  -f, --format FMT    Output format: yaml or json (default: yaml)
  --paths PATHS       Package search paths
```

**Examples:**
```bash
rez view python                 # View latest python package
rez view python-3.9.7           # View specific version
rez view python -a              # All versions
rez view python -a -b           # All versions, brief
rez view maya -f json           # JSON output
```

---

#### `rez help-pkg`

View package help documentation.

```
rez help-pkg [OPTIONS] [PACKAGE] [SECTION]

Options:
  -l, --list          List available help sections
  -m, --manual        Open rez manual
  -v, --version VER   Package version
  --paths PATHS       Package search paths
```

**Examples:**
```bash
rez help-pkg maya            # Open first help section
rez help-pkg maya -l         # List help sections
rez help-pkg maya 2          # Open section 2
rez help-pkg -m              # Open rez manual
```

---

#### `rez depends`

Show reverse dependencies (what depends on a package).

```
rez depends [OPTIONS] <PKG>

Options:
  -d, --depth DEPTH                  Tree depth limit
  -b, --build-requires               Include build requirements
  -p, --private-build-requires       Include private build requirements
  --paths PATHS                      Package search paths
  -q, --quiet                        Quiet mode
```

**Examples:**
```bash
rez depends python              # What depends on python?
rez depends python -d 2         # Limit depth to 2
rez depends boost -b            # Include build-time dependencies
```

---

#### `rez diff`

Diff two package versions.

```
rez diff <PKG1> [PKG2]
```

If PKG2 is omitted, diffs against the latest version.

**Examples:**
```bash
rez diff maya-2024.0 maya-2024.1    # Diff two versions
rez diff maya-2024.0                # Diff against latest
```

---

#### `rez plugins`

List plugins for a package.

```
rez plugins <PKG> [--paths PATHS]
```

**Examples:**
```bash
rez plugins maya        # List Maya plugins
```

---

### Repo Commands

#### `rez bind`

Bind system software as rez packages (detect installed software and create rez package definitions).

```
rez bind [OPTIONS] [PKG]

Options:
  -q, --quickstart         Bind standard system packages (platform, arch, os, python, rez)
  -a, --all                Bind all detectable software
  -r, --release            Install to release packages path
  -i, --install-path PATH  Custom install path
  -l, --list               List available bind modules
  -s, --search             Search for module without binding
  --format FORMAT          Output format: py or yaml (default: py)
  --copies                 Use venv --copies for python (more relocatable)
  -v, --verbose            Verbose output
```

**Examples:**
```bash
rez bind -q                     # Quick start: bind platform, arch, os, python, rez
rez bind python                 # Bind python specifically
rez bind -a                     # Detect and bind everything
rez bind -l                     # List all available bind modules
rez bind maya                   # Bind Maya (auto-detect version)
rez bind houdini -r             # Bind Houdini to release path
rez bind python --copies        # Python with copied binaries (relocatable)
```

---

#### `rez cp`

Copy packages between repositories.

```
rez cp [OPTIONS] <PACKAGE>

Options:
  -d, --dest-path PATH      Destination repository
  --reversion VERSION        Copy to different version
  --rename NAME              Copy to different name
  -o, --overwrite            Overwrite existing
  --follow-symlinks          Follow symlinks
  -k, --keep-timestamp       Preserve timestamp
  -f, --force                Copy non-relocatable packages
  --allow-empty              Allow empty target repo
  --dry-run                  Dry run
  --variants INDEX...        Specific variant indices
  --paths PATHS              Source search paths
  --no-local                 Exclude local packages
  -v, --verbose              Verbose output
```

**Examples:**
```bash
rez cp python-3.9.7 -d /studio/packages
rez cp maya-2024.0 -d /studio/packages --reversion 2024.0.1
rez cp my_tool-1.0 -d /release --rename studio_tool
rez cp my_tool-1.0 -d /release --dry-run
rez cp my_tool-1.0 --variants 0 1 -d /release
```

---

#### `rez mv`

Move packages between repositories.

```
rez mv <PACKAGE> [SRC_PATH] -d <DEST_PATH>

Options:
  -k, --keep-timestamp   Preserve timestamp
  -f, --force            Move non-relocatable packages
  -v, --verbose          Verbose output
```

**Examples:**
```bash
rez mv my_tool-1.0 /local/packages -d /studio/packages
rez mv my_tool-1.0              # List repos containing this package
```

---

#### `rez rm`

Remove packages from a repository.

```
rez rm [OPTIONS] [PATH]

Options:
  -p, --package PKG        Remove specific package version
  -f, --family NAME        Remove entire package family
  --force-family           Force remove non-empty family
  --dry-run                Dry run
```

**Examples:**
```bash
rez rm -p my_tool-1.0.0 /local/packages
rez rm -f my_tool /local/packages
rez rm -f my_tool --force-family /local/packages
```

---

#### `rez pip`

Install pip packages as rez packages.

```
rez pip [OPTIONS] <PACKAGES...> [-- <PIP_ARGS>...]

Options:
  -i, --install              Actually install (required flag)
      --local                Install to local packages path instead of release
  -p, --prefix PATH          Custom install path
  --python-version VERSION   Python version to use
  --no-deps                  Skip dependencies

Global flags (-v, -l) can be placed anywhere:
  -v                         Verbosity: -v INFO, -vv DEBUG, -vvv TRACE
  -l [FILE]                  Redirect logging to file
```

**Examples:**
```bash
rez pip -i numpy                        # Install to release path (default)
rez pip -i --local numpy                # Install to local packages path
rez pip -i requests --no-deps           # Without dependencies
rez pip -i "flask>=2.0" --python-version 3.9
rez pip -i torch -- --index-url https://download.pytorch.org/whl/cu130
rez -v pip -i torch torchvision -- --index-url https://download.pytorch.org/whl/cu130
```

**How it works:**
1. Runs pip install to a temp directory
2. Parses .dist-info metadata
3. Converts pip package name to rez name
4. Maps files: `.dist-info` → `python/`, scripts → `bin/`
5. Generates `package.py` with proper metadata, requires, and variant (platform/arch/python)
6. Installs to the target repository

---

#### `rez pkg-cache`

Manage the local package cache.

```
rez pkg-cache [OPTIONS] <SUBCOMMAND>

Subcommands:
  list      List cached packages
  add       Add variant to cache
  remove    Remove variant from cache
  clean     Clean old/stalled entries
  status    Show cache summary
```

**Examples:**
```bash
rez pkg-cache status
rez pkg-cache list
rez pkg-cache clean --max-age 30
rez pkg-cache add /path/to/variant --name foo --version 1.0
rez pkg-cache remove foo 1.0
```

---

#### `rez pkg-ignore`

Ignore/unignore packages (make them invisible to resolver).

```
rez pkg-ignore [OPTIONS] <PKG> [PATH]

Options:
  -u, --unignore       Make visible again
  -a, --allow-missing  Allow ignoring non-existent packages
```

**Examples:**
```bash
rez pkg-ignore foo-1.0.0 /local/packages         # Ignore
rez pkg-ignore foo-1.0.0 /local/packages -u      # Unignore
```

---

#### `rez yaml2py`

Convert package.yaml to package.py format (outputs to stdout).

```
rez yaml2py [PATH]
```

**Examples:**
```bash
rez yaml2py                             # Convert ./package.yaml
rez yaml2py /path/to/package.yaml       # Convert specific file
rez yaml2py > package.py                # Redirect output
```

---

### Resolve Commands

#### `rez env`

Resolve packages and launch a configured shell (the **core command** of rez).

```
rez env [OPTIONS] [PACKAGES...] [-- COMMAND...]

Options:
  -S, --shell TYPE          Target shell (bash, sh, csh, tcsh, cmd, powershell, gitbash)
  --rcfile FILE             Source this file instead of standard startup
  --norc                    Skip startup scripts
  -c, --command CMD         Execute command then exit
  -s, --stdin               Read commands from stdin
  --ni, --no-implicit       Don't add implicit packages
  --nl, --no-local          Exclude local packages
  -b, --build               Create build environment
  --paths PATHS             Package search paths
  -t, --time TIME           Ignore packages after time
  --max-fails N             Abort after N failed attempts (-1 = unlimited)
  --time-limit SECS         Abort after SECS seconds (-1 = unlimited)
  -o, --output FILE         Save context to .rxt ("-" = stdout)
  -i, --input FILE          Load context from .rxt
  --exclude PATTERNS...     Package exclusion filters
  --include PATTERNS...     Package inclusion filters
  --no-filters              Disable global filters
  -p, --patch               Patch current context
  --strict                  Strict patching
  --no-cache                Skip cached resolves
  -q, --quiet               Hide welcome message
  --fail-graph              Show graph on failure
  --new-session             New process group
  --detached                Open separate terminal
  --stats                   Print solver stats
  --no-pkg-cache            Disable package caching
  --clear                   Clean environment (default)
  --inherited               Inherit parent environment
  -v, --verbose             Increase verbosity
```

**Examples:**
```bash
# Basic environment
rez env python-3.9 maya-2024

# Run a command and exit
rez env python-3.9 -- python -c "import sys; print(sys.version)"
rez env maya-2024 -c "maya -batch -script render.mel"

# Save/load contexts
rez env python-3.9 numpy -o my_env.rxt
rez env -i my_env.rxt

# Patching (modify current context)
rez env python-3.9
> rez env -p numpy                # Add numpy to current context

# Build environment
rez env -b my_tool python-3.9

# Time-based resolve
rez env python -t 1700000000     # Resolve as if it were Nov 2023
rez env maya -t -7d              # Ignore last 7 days of releases

# Shell selection
rez env -S powershell python-3.9
rez env -S bash maya-2024

# Filtering
rez env python --exclude "*.beta" --exclude "*.rc"

# Quiet mode
rez env -q python-3.9 -- python script.py
```

---

#### `rez context`

Inspect a resolved context.

```
rez context [OPTIONS] [RXT]

Options:
  --req, --print-request    Print request list
  --res, --print-resolve    Print resolve list
  --so, --source-order      Source order instead of alphabetical
  --su, --show-uris         Show URIs instead of paths
  --pg, --print-graph       Print resolve graph (dot format)
  -i, --interpret           Print resulting shell code
  -f, --format FORMAT       Output: shell name, "dict", "table", "json"
  --no-env                  Don't inherit current environment
  --diff RXT                Diff against another context
  -v, --verbose             Verbose output
```

**Examples:**
```bash
rez context                         # Show current context info
rez context my_env.rxt              # Inspect a .rxt file
rez context --print-request         # Just the request list
rez context --print-resolve         # Just resolved packages
rez context -i -f table             # Environment as table
rez context -i -f json              # Environment as JSON
rez context -i -f bash              # As bash script
rez context --diff other.rxt        # Diff two contexts
rez context --print-graph | dot -Tpng -o graph.png  # Visualize
```

---

#### `rez interpret`

Interpret rex code (rez's environment DSL).

```
rez interpret [OPTIONS] <FILE>

Options:
  -f, --format FORMAT              Output: shell name, "dict", "table"
  --no-env                         Start with empty environment
  --pv, --parent-variables VARS    Vars to update (not overwrite)
```

**Rex DSL functions:**
- `setenv("KEY", "value")` — set environment variable
- `unsetenv("KEY")` — unset variable
- `prependenv("PATH", "/new/path")` — prepend to path-like variable
- `appendenv("PATH", "/new/path")` — append to path-like variable
- `alias("name", "command")` — create shell alias
- `info("message")` — print info
- `command("cmd args")` — run a command
- `source("/path/to/script")` — source a script
- `stop("reason")` — stop execution

**Examples:**
```bash
rez interpret commands.rex -f bash
rez interpret commands.rex -f table --no-env
```

---

#### `rez bundle`

Bundle a resolved context for portable deployment.

```
rez bundle [OPTIONS] <RXT> <DEST_DIR>

Options:
  -s, --skip-non-relocatable   Skip non-relocatable packages
  -f, --force                  Force bundle non-relocatable
  --no-lib-patch               Skip rpath patching
  -v, --verbose                Verbose output
```

**Examples:**
```bash
rez env python-3.9 numpy -o my_env.rxt
rez bundle my_env.rxt /deploy/my_env
rez bundle my_env.rxt /deploy/my_env -s     # Skip non-relocatable
rez bundle my_env.rxt /deploy/my_env -f     # Force all
```

---

#### `rez suite`

Manage suites (collections of contexts that share tools on PATH).

```
rez suite [OPTIONS] [DIR]

Options:
  -l, --list              List visible suites
  -t, --tools             List tools in suite
  --which TOOL            Find tool path
  --validate              Validate suite
  --create                Create new suite
  -c, --context NAME      Context name (for context operations)
  -a, --add RXT           Add context from .rxt
  -r, --remove NAME       Remove context
  -p, --prefix PREFIX     Set context prefix
  -s, --suffix SUFFIX     Set context suffix
  --hide TOOL             Hide tool
  --unhide TOOL           Unhide tool
  --alias TOOL ALIAS      Create tool alias
  --unalias TOOL          Remove alias
  -b, --bump NAME         Bump context priority
  -v, --verbose           Verbose output
```

**Examples:**
```bash
rez suite --create /studio/suite/vfx_2024
rez suite /studio/suite/vfx_2024 -c maya -a maya_ctx.rxt
rez suite /studio/suite/vfx_2024 -c houdini -a hou_ctx.rxt
rez suite /studio/suite/vfx_2024 --tools
rez suite /studio/suite/vfx_2024 --which maya
rez suite -l                    # List all visible suites
```

---

#### `rez complete`

Shell tab-completion helper.

```
rez complete [OPTIONS] [PREFIX]

Options:
  --paths PATHS     Package search paths
  --families        Complete family names only
```

Used internally for shell completion integration.

---

### GUI Command

#### `rez gui`

Launch the graphical package browser and resolver.

```
rez gui [FILE]

Arguments:
  [FILE]    Open this .rxt file on startup
```

**Examples:**
```bash
rez gui                     # Launch empty
rez gui my_context.rxt      # Open with context
```

Requires the `gui` feature to be compiled in.

---

## Setup Examples

### Initial Setup (Quick Start)

```bash
# 1. Create default config
rez --write-config

# This creates:
#   ~/.rez/rezconfig.py
#   ~/.rez/packages/local/
#   ~/.rez/packages/int/
#   ~/.rez/packages/ext/
#   ~/.rez/packages/bind/
#   ~/.rez/packages/pip/
#   ~/.rez/packages/python/

# 2. Bind system packages
rez bind -q

# This detects and creates rez packages for:
#   platform (windows/linux/osx)
#   arch (x86_64/aarch64)
#   os (ubuntu-22.04, windows-11, etc.)
#   python (system python)
#   rez (rez itself)

# 3. Verify
rez status
rez search
```

### Studio Setup

```bash
# 1. Write config to a shared location
rez --write-config /studio/rez/rezconfig.py

# 2. Edit rezconfig.py for studio paths:
#    packages_path = [
#        "~/packages",                    # User local
#        "/studio/packages/int",          # Internal packages
#        "/studio/packages/ext",          # External/vendor
#        "/studio/packages/bind",         # System bindings
#    ]
#    local_packages_path = "~/packages"
#    release_packages_path = "/studio/packages/int"
#    package_cache_path = "/tmp/rez_cache"

# 3. Set REZ_CONFIG_FILE for all users
export REZ_CONFIG_FILE=/studio/rez/rezconfig.py

# 4. Bind platform packages
rez bind -q

# 5. Bind DCC applications
rez bind maya
rez bind houdini
rez bind nuke
rez bind blender

# 6. Install pip packages
rez pip -i numpy -r
rez pip -i PySide6 -r
rez pip -i OpenEXR -r
```

### Developer Workflow

```bash
# Create a new package
mkdir my_tool && cd my_tool

# package.py
cat > package.py << 'EOF'
name = "my_tool"
version = "1.0.0"
description = "My awesome tool"
requires = ["python-3.9+", "numpy-1.20+"]
tools = ["my_tool"]
build_command = "python {root}/install.py {install}"

def commands():
    env.PATH.prepend("{root}/bin")
    env.PYTHONPATH.prepend("{root}/python")
EOF

# Build and test locally
rez build -i
rez env my_tool -- my_tool --help

# Run tests
rez test

# Release
rez release -m "Initial release of my_tool"
```

### Using Environments

```bash
# Simple environment
rez env python-3.9 numpy

# Run a render
rez env maya-2024 arnold-7 -- mayabatch -file scene.ma -command "arnoldRender"

# Save environment for reproducibility
rez env python-3.9 numpy scipy matplotlib -o data_science.rxt

# Share with team
rez env -i data_science.rxt

# Patch an existing environment (add a package)
rez env python-3.9 numpy
> rez env -p scipy              # Adds scipy to current context

# Time travel - resolve as of last week
rez env maya -t -7d
```

### Suite Setup (Tool Management)

```bash
# Create a studio suite
rez suite --create /studio/suites/vfx_2024

# Build contexts for each DCC
rez env maya-2024 mtoa-5 -o /tmp/maya.rxt
rez env houdini-20 -o /tmp/houdini.rxt
rez env nuke-15 -o /tmp/nuke.rxt

# Add to suite
rez suite /studio/suites/vfx_2024 -c maya2024 -a /tmp/maya.rxt
rez suite /studio/suites/vfx_2024 -c houdini20 -a /tmp/houdini.rxt
rez suite /studio/suites/vfx_2024 -c nuke15 -a /tmp/nuke.rxt

# Verify
rez suite /studio/suites/vfx_2024 --tools

# Add suite to PATH
export PATH=/studio/suites/vfx_2024/bin:$PATH

# Now all DCC tools are available directly
maya          # Launches maya in resolved environment
houdini       # Launches houdini in resolved environment
nuke          # Launches nuke in resolved environment
```

### Bundle for Deployment

```bash
# Create a portable environment bundle
rez env python-3.9 numpy scipy -o render_env.rxt
rez bundle render_env.rxt /deploy/render_env

# The bundle contains all packages and a self-contained .rxt
# Can be deployed to machines without rez installed
```

### Memcached Integration

```bash
# In rezconfig.py:
#   memcached_uri = ["localhost:11211"]
#   cache_listdir = True
#   cache_package_files = True

# Check status
rez memcache --stats

# Warm cache
rez memcache --warm

# Flush when packages change
rez memcache --flush
```

---

## Bind & DCC Detectors

rez-rs includes built-in detectors for 22 DCC applications:

| Application | Bind Module | Auto-detected |
|-------------|-------------|---------------|
| Maya | `maya` | Yes (registry/paths) |
| Maya USD | `maya_usd` | Yes |
| Maya Bifrost | `maya_bifrost` | Yes |
| Maya LookDevX | `maya_lookdevx` | Yes |
| Maya Arnold | `maya_arnold` | Yes |
| Houdini | `houdini` | Yes (registry/paths) |
| Nuke | `nuke` | Yes |
| Blender | `blender` | Yes |
| Katana | `katana` | Yes |
| Mari | `mari` | Yes |
| Substance Designer | `substance_designer` | Yes |
| Substance Painter | `substance_painter` | Yes |
| Unreal Engine | `unreal` | Yes |
| Cinema 4D | `cinema4d` | Yes |
| DaVinci Resolve | `davinci_resolve` | Yes |
| Deadline | `deadline` | Yes |
| EmberGen | `embergen` | Yes |
| GeoGen | `geogen` | Yes |
| IlluGen | `illugen` | Yes |
| LiquiGen | `liquigen` | Yes |
| Meshroom | `meshroom` | Yes |
| Cursor | `cursor` | Yes |

**System packages:** `platform`, `arch`, `os`
**Tool packages:** `python`, `cmake`, `pip`, `setuptools`, `rez`, `docker`, `git`, `gh`, `gcc`, `perforce`
