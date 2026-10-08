# Configuration

rez-rs provides configuration fields for package resolution, environment setup, build processes, caching, debugging, and appearance. This chapter explains the configuration system and documents the most important settings.

**Generate a default config:** Run `rez --write-config` to write `~/.rez/rezconfig.py` with built-in defaults and inline comments. Fails if file exists; use `--force` to overwrite.

## Configuration Loading Order

Configuration is loaded in layers, where later layers override earlier ones:

1. **Built-in defaults** -- `RezConfig::default()` in the Rust source. Every field has a sensible default so rez-rs works without any configuration file.
2. **Site rezconfig.py** -- checked first as `.py`, then `.toml`, in standard site-wide locations
3. **User rezconfig** -- `~/.rez/rezconfig.py` or `~/.rez/rezconfig.toml`
4. **REZ_CONFIG_FILE** -- environment variable pointing to a specific `.py` or `.toml` config file
5. **REZ_*_JSON environment variables** -- JSON-encoded overrides for complex values (e.g., `REZ_IMPLICIT_PACKAGES_JSON='["~platform==linux"]'`)
6. **REZ_* environment variables** -- individual overrides (e.g., `REZ_PACKAGES_PATH`); when both forms are set for one field, the plain form wins

## Configuration File Formats

### rezconfig.py (recommended)

The `.py` format is the most powerful because it allows dynamic values:

```python
import os

packages_path = [
    os.path.expanduser("~/packages"),
    "/studio/packages",
]

# Dynamic default shell based on environment
default_shell = os.environ.get("REZ_DEFAULT_SHELL", "")

# Build threads based on machine class
if os.environ.get("RENDER_NODE"):
    build_thread_count = 2
else:
    build_thread_count = "physical_cores"
```

rez-rs executes this file with the embedded RustPython VM, so standard Python operations (os, sys, pathlib) are available.

### rezconfig.toml

For simpler configurations, TOML is supported:

```toml
packages_path = ["~/packages", "/studio/packages"]
release_packages_path = "/studio/packages"
local_packages_path = "~/packages"
default_shell = "bash"
build_thread_count = "physical_cores"

[implicit_packages]
values = ["~platform=={system.platform}", "~arch=={system.arch}", "~os=={system.os}"]
```

## Key Configuration Fields

### Paths

| Field | Type | Default | Description |
|---|---|---|---|
| `packages_path` | `list[str]` | `["~/packages"]` | Package repository paths, searched in order |
| `local_packages_path` | `str` | `"~/packages"` | Where `rez build --install` puts packages |
| `release_packages_path` | `str` | `"~/packages"` | Default release path when per-builder paths not set |
| `release_bind_path` | `str?` | `"~/.rez/packages/bind"` | Where `rez bind --release` puts packages. If unset, uses `release_packages_path` |
| `release_pip_path` | `str?` | `"~/.rez/packages/pip"` | Where `rez pip` installs packages by default. Use `--local` for local path instead. If unset, uses `release_packages_path` |
| `release_build_path` | `str?` | `"~/.rez/packages/python"` | Where `rez release` deploys packages. If unset, uses `release_packages_path` |
| `tmpdir` | `str?` | `None` | Temp directory override (default: system temp) |
| `context_tmpdir` | `str?` | `None` | Where resolved context scripts are written |

### Shared source, cache, and offline settings

These settings belong to the canonical configuration owner and are also passed to Rex and build environments. Path values select one directory, not a list of directories.

| Field | Environment variable | Default | Description |
|---|---|---|---|
| `sources_path` | `REZ_SOURCES_PATH` | `None` | Root of a local source/archive mirror; recipes choose their own subdirectories |
| `wheel_cache_path` | `REZ_WHEEL_CACHE_PATH` | `None` | Local wheel directory; managed Pip falls back to `sources_path/wheels` when unset |
| `user_path` | `REZ_USER_PATH` | `None` | User data root; tool recipes preserve `cache/<tool>` beneath it |
| `offline` | `REZ_OFFLINE` | `False` | Prohibit managed network acquisition and require local inputs/caches |
| `repo_path` | `REZ_REPO_PATH` | `None` | Common publication root, below explicit destinations and per-builder release paths |
| `log_level` | `REZ_LOG_LEVEL` | `None` | Optional logging level: `ERROR`, `WARNING`, `INFO`, `DEBUG`, `TRACE`, or `OFF` |

Set `REZ_OFFLINE=true` or `REZ_OFFLINE=false`. These values are case-insensitive; numeric values and other strings are rejected for this setting. Configuration files use Python `True`/`False` or TOML `true`/`false`. Build environments receive the canonical lowercase text. Other standard Rez variables keep their existing formats, including the generated `REZ_BUILD_INSTALL` flag.

For publication, an explicit CLI destination wins, followed by the applicable per-builder `release_*_path`, then `repo_path`, then the normal local/release fallback. Existing default `release_bind_path`, `release_pip_path`, and `release_build_path` still count as per-builder paths; set the relevant field to `None` to use `repo_path` for that operation. Local package builds use `repo_path` before `local_packages_path`; local Pip also preserves its explicit prefix settings. The staged-release installer uses an explicit `--repository-root` before `REZ_REPO_PATH`. Resolution still uses `packages_path` / `REZ_PACKAGES_PATH`; a publication root does not replace that search list.

Managed extraction accepts local files, source-mirror inputs, and verified download-cache hits offline; it rejects a network cache miss before HTTP. Managed Pip adds `--no-index` and uses existing `PIP_FIND_LINKS`, otherwise `wheel_cache_path`, otherwise `sources_path/wheels`. Build environments also set the supported Cargo, Go, and npm offline controls. An arbitrary custom command or an external helper can have its own network behavior: `REZ_OFFLINE` is a shared acquisition policy, not an operating-system network sandbox.

Offline Python builds also require the build backend and all build dependencies to be installed in the selected build interpreter beforehand. Managed `pip install` and `pip wheel` add `--no-build-isolation`, and managed `python -m build` adds `--no-isolation`, so those commands cannot create an isolated environment that downloads its build dependencies. A warm source or wheel cache alone does not supply the backend.

The generated commented config includes all six fields. To inspect them:

```console
rez config sources_path
rez config offline
rez config repo_path
```

PBS-specific `REZ_PBS_*` settings belong to the external Python recipes, rather than additional canonical configuration fields; the build environment forwards them when present. External recipes live in the separate private `rez-rs-packages` repository. Their `rez_build` helper is supplied separately and must implement this shared contract; `rez.rs` does not provide it.

### Extensions

| Field | Type | Default | Description |
|---|---|---|---|
| `bind_module_path` | `list[str]` | `[]` | Extra dirs scanned for custom bind modules (*.py); names appear in `rez bind --list` (binding requires Python rez) |

### Package Resolution

| Field | Type | Default | Description |
|---|---|---|---|
| `implicit_packages` | `list[str]` | `["~platform==...", "~arch==...", "~os==..."]` | Requirements auto-added to every resolve |
| `variant_select_mode` | `str` | `"version_priority"` | How to pick among multiple valid variants |
| `allow_unversioned_packages` | `bool` | `true` | Whether packages without versions are allowed |
| `prune_failed_graph` | `bool` | `true` | Simplify failure messages by pruning irrelevant nodes |
| `error_on_missing_variant_requires` | `bool` | `false` | Error if variant requires reference unknown packages |
| `package_filter` | `json?` | `None` | Filter rules for excluding packages from resolves |
| `package_orderers` | `json?` | `None` | Custom ordering rules for package version selection |
| `platform_map` | `dict` | `{}` | Map platform strings (e.g., `{"os": {"centos-7": "linux-7"}}`) |

### Environment Resolution

| Field | Type | Default | Description |
|---|---|---|---|
| `default_shell` | `str` | `""` | Shell to use for `rez env` (empty = auto-detect) |
| `parent_variables` | `list[str]` | `[]` | Env vars inherited from parent environment |
| `all_parent_variables` | `bool` | `false` | Inherit ALL parent env vars |
| `resetting_variables` | `list[str]` | `[]` | Env vars to reset (remove) before applying commands |
| `all_resetting_variables` | `bool` | `false` | Reset ALL env vars |
| `set_prompt` | `bool` | `true` | Modify shell prompt to show resolved packages |
| `prefix_prompt` | `bool` | `true` | Prefix prompt (true) vs replace (false) |
| `package_commands_sourced_first` | `bool` | `true` | If true: source package commands before shell init (.bashrc); if false: after |
| `env_var_separators` | `dict` | `{"PATH": ":", ...}` | Custom separators for specific env vars |
| `pathed_env_vars` | `list[str]` | `["PATH", "PYTHONPATH", ...]` | Env vars that contain paths (for proper joining) |
| `standard_system_paths` | `list[str]` | platform-specific | System paths always included in PATH |

### Shells

rez-rs supports 8 shell types:

| Shell | Platforms | Notes |
|---|---|---|
| `bash` | Linux, macOS, Windows (WSL/Git Bash) | Most common in VFX |
| `sh` | Linux, macOS | POSIX-compatible |
| `zsh` | Linux, macOS | Z shell |
| `csh` | Linux, macOS | Legacy, some studios still use it |
| `tcsh` | Linux, macOS | Enhanced csh |
| `cmd` | Windows | Windows Command Prompt |
| `powershell` / `pwsh` | Windows, Linux, macOS | Same type; both names accepted |
| `gitbash` | Windows | Git for Windows bash |

Shell detection priority on Windows: Git Bash EXEPATH -> cmd.exe -> PowerShell.

### Caching

| Field | Type | Default | Description |
|---|---|---|---|
| `resolve_caching` | `bool` | `true` | Cache resolve results |
| `cache_package_files` | `bool` | `true` | Cache parsed package definitions |
| `cache_listdir` | `bool` | `true` | Cache directory listings |
| `resource_caching_maxsize` | `int` | `-1` | Max cache size (-1 = unlimited) |
| `memcached_uri` | `list[str]` | `[]` | Memcached server URIs |
| `default_hashed_variants` | `bool` | `false` | Use SHA1 hash for variant subdirs (false = readable platform-X/arch-Y paths) |
| `cache_packages_path` | `str?` | `None` | Local package cache directory |
| `read_package_cache` | `bool` | `true` | Read from local package cache |
| `write_package_cache` | `bool` | `true` | Write to local package cache |

### Build/Release

| Field | Type | Default | Description |
|---|---|---|---|
| `build_directory` | `str` | `"build"` | Build output directory name |
| `build_thread_count` | `int\|str` | `"physical_cores"` | Thread count: number or "physical_cores"/"logical_cores" |
| `release_hooks` | `list[str]` | `[]` | Hook scripts to run on release |
| `prompt_release_message` | `bool` | `false` | Prompt for release message |
| `make_package_temporarily_writable` | `bool` | `true` | Temporarily make install dir writable during install |
| `default_build_process` | `str` | `"local"` | Build process: "local" or "remote" |

### Debugging

| Field | Type | Default | Description |
|---|---|---|---|
| `debug_file_loads` | `bool` | `false` | Log package file loading |
| `debug_plugins` | `bool` | `false` | Log plugin loading |
| `debug_package_release` | `bool` | `false` | Log release operations |
| `debug_resources` | `bool` | `false` | Log resource loading |
| `debug_package_exclusions` | `bool` | `false` | Log why packages are excluded |
| `debug_resolve_memcache` | `bool` | `false` | Log resolve cache operations |
| `debug_all` | `bool` | `false` | Enable all debug logging |
| `warn_all` | `bool` | `false` | Enable all warnings |
| `catch_rex_errors` | `bool` | `true` | Catch and report rex execution errors |

### Appearance

| Field | Type | Default | Description |
|---|---|---|---|
| `quiet` | `bool` | `false` | Suppress non-error output |
| `show_progress` | `bool` | `true` | Show progress indicators |
| `color_enabled` | `bool` | `true` | Enable colored output |
| `editor` | `str?` | `None` | Preferred text editor |
| `browser` | `str?` | `None` | Preferred web browser |
| `difftool` | `str?` | `None` | Preferred diff tool |

### Suite Settings

| Field | Type | Default | Description |
|---|---|---|---|
| `suite_visibility` | `str` | `"always"` | When suite tools are visible |
| `rez_tools_visibility` | `str` | `"always"` | When rez tools are visible in suites |
| `suite_alias_prefix_char` | `char` | `'+'` | Prefix character for suite aliases |

## Environment Variable Overrides

Any configuration field can be overridden with an environment variable. The naming convention is:

```
REZ_<FIELD_NAME_UPPERCASED>
```

Examples:

```bash
export REZ_PACKAGES_PATH="~/mypackages:/shared/packages"
export REZ_DEFAULT_SHELL="bash"
export REZ_BUILD_THREAD_COUNT=8
export REZ_QUIET=1
```

For complex values (lists, dicts), use the `_JSON` suffix:

```bash
export REZ_IMPLICIT_PACKAGES_JSON='["~platform==linux", "~arch==x86_64"]'
export REZ_PLATFORM_MAP_JSON='{"centos-7": "linux-7"}'
```

## Querying Configuration

Use `rez config` to inspect the current configuration:

```bash
# Show all configuration
rez config

# Show a specific field
rez config packages_path

# Show the resolved value (after all overrides)
rez config default_shell
```

## Example: Studio Configuration

A typical VFX studio `rezconfig.py`:

```python
import os

# Package repositories: local dev -> show-specific -> studio-wide
show = os.environ.get("SHOW", "default")
packages_path = [
    os.path.expanduser("~/packages"),
    f"/shows/{show}/packages",
    "/studio/packages",
    "/studio/packages/external",
]

release_packages_path = f"/shows/{show}/packages"
local_packages_path = os.path.expanduser("~/packages")

implicit_packages = [
    "~platform=={system.platform}",
    "~arch=={system.arch}",
    "~os=={system.os}",
]

default_shell = "bash"
set_prompt = True
prefix_prompt = True

# Build configuration
build_thread_count = "physical_cores"
build_directory = "build"

# Caching for performance
resolve_caching = True
cache_packages_path = os.path.expanduser("~/.rez/cache")
memcached_uri = ["memcache01:11211", "memcache02:11211"]

# Debugging (disabled in production)
debug_all = False
warn_untimestamped = True
```
