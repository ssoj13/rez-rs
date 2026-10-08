# builders/

Build system plugins and build process orchestration. This module provides
a unified interface for building rez packages using various build systems.

This module belongs to the `build-system` crate. Pip publication and release hooks
share that crate; model, repository, Rex, and resolve supply common contracts.
See the [workspace architecture](../../../../../docs/mdbook/src/architecture-crates.md).

## Files

| File | Description |
|------|-------------|
| mod.rs | Module root. Contains `BuildSystemType` enum (15 variants), `BuildSystem` trait, `BuildContext` / `BuildResult` structs, `BuildProcess` orchestrator, detection logic, factory (`create_build_system()`), and `set_standard_build_vars()` helper. |
| cmake.rs | `CMakeBuildSystem` — CMake builds. Detects `CMakeLists.txt`; keeps logical install prefix and physical staging separate. Child build system: Make. |
| make.rs | `MakeBuildSystem` — GNU Make builds. Detects `Makefile` / `makefile`. |
| python.rs | `PythonBuildSystem` — Legacy `setup.py` builds when a valid PEP 517 declaration does not take precedence. Uses owned source/install staging. |
| pip.rs | `PipBuildSystem` — Valid PEP 517 projects or standalone `requirements.txt`. Prepares `pip install --target` payloads for publication; non-install mode writes wheels. |
| pip_utils.rs | Shared effective command lookup, SourceMap staging, selected Pip parser/runner policy, layout and shebang relocation helpers. |
| cargo_build.rs | `CargoBuildSystem` — Rust/Cargo. Detects `Cargo.toml`; maps compiler-reported binaries and dynamic libraries plus explicit artifacts. |
| go.rs | `GoBuildSystem` — Go modules. Detects `go.mod`; stages multiple commands under `go-bin`. |
| zig.rs | `ZigBuildSystem` — Zig projects. Detects `build.zig`; preserves the staged install layout. |
| native.rs | Shared typed options, artifact validation, owned payload preparation and collision checks. |
| metadata.rs | Joint build-requirement metadata and source/installed provenance for batch consumers. |
| nodejs.rs | `NodeJsBuildSystem` — Node.js. Detects `package.json`. Runs `npm install` + `npm run build`. |
| bun.rs | `BunBuildSystem` — Bun. Detects `package.json` + `bunfig.toml` or `bun.lockb`. Preferred over NodeJs. |
| scons.rs | `SConsBuildSystem` — SCons. Detects `SConstruct` or `SConscript`. |
| vcpkg.rs | `VcpkgBuildSystem` — Manifest dependencies/ports from `vcpkg.json`, with owned install/buildtrees/packages roots and prepared publication. |
| conan.rs | `ConanBuildSystem` — Conan. Detects `conanfile.py` or `conanfile.txt`. |
| extraction.rs | `ExtractionBuildSystem` — Extract zip/tar/tar.gz/tar.xz/msi. Config from `sources.yaml` or `package.config.extraction.downloads`. |
| download.rs | HTTP download with cache, resumable Range support, optional checksum (sha256/sha1). |
| custom.rs | `CustomBuildSystem` — Arbitrary `build_command` from package. |
| noop.rs | `NoOpBuildSystem` — No-op. For pure-data packages. |

## Architecture

The build system uses a trait-object pattern:

```
BuildProcess::new(working_dir, build_command, build_system_name, config)
  -> explicit build-system name, else custom build_command, else manifest detection
     Filter Make when CMake is present and NodeJs when Bun is present
     Reject multiple remaining matches; use NoOp when none match
  -> create_build_system() -> Box<dyn BuildSystem>
  -> capture config.build_directory; None config uses CONFIG
  -> set_package(package, optional developer source) -> set_package_info(..., optional directory override)
     relative directory -> source/directory
     absolute directory -> directory/package/version

BuildProcess::build(install_path, clean, install, variant_indices, force, write_build_scripts)
  -> reject an absolute configured build root without package identity before I/O
  -> for each variant:
     -> append the shared variant subpath
     -> prepare build dir; cleaning rejects the source directory or its ancestor
     -> set REZ_BUILD_* env vars from these same paths
     -> BuildSystem::build(&BuildContext), or supported build-script launcher
     -> collect BuildResult
   -> install + all selected variants successful + not script-generation mode:
      -> repository::publish_package validates and merges metadata under a lock
      -> owned prepared payloads and canonical package metadata share publication
```

CMake filters its Make child and Bun filters NodeJs. An explicit build-system
name takes precedence over `build_command`; the command selects Custom when no
explicit name is supplied. Other simultaneous manifest matches remain an error.

`set_package` accepts a string `package.config.build_directory` override and
returns an error for other types. It delegates identity validation and path
selection to `set_package_info`. Repeated setup recalculates from the original
configured directory, so it does not append package/version twice. Removing a
package override restores the original configured directory. `--clean` checks
canonical target/source paths before recursive deletion; all production callers
supply the source path to `prepare_build_dir`.

Standard `REZ_BUILD_*` environment variables are set for all builders:
`REZ_BUILD_ENV`, `REZ_BUILD_PATH`, `REZ_BUILD_SOURCE_PATH`,
`REZ_BUILD_PROJECT_NAME`, `REZ_BUILD_PROJECT_VERSION`,
`REZ_BUILD_VARIANT_INDEX`, `REZ_BUILD_INSTALL_PATH`, etc.

## Shared configuration and acquisition policy

The canonical configuration exports `REZ_SOURCES_PATH`, `REZ_WHEEL_CACHE_PATH`,
`REZ_USER_PATH`, `REZ_OFFLINE`, `REZ_REPO_PATH`, and `REZ_LOG_LEVEL` to child
build environments. Configured values also reach generated launchers and Rex
environments; present `REZ_PBS_*` recipe settings are preserved. Package identity
continues to use the standard `REZ_BUILD_PROJECT_NAME` and
`REZ_BUILD_PROJECT_VERSION`.

`REZ_OFFLINE=true` requires managed extraction to use a local file, source mirror,
or verified cache hit, and managed Pip to use `--no-index` with local find-links.
Cargo, Go, and npm receive their supported offline environment controls.
`REZ_OFFLINE=false` is the default; only case-insensitive `true`/`false` values
are accepted. This policy does not sandbox arbitrary custom commands or external
build helpers. The separately supplied recipe helper `rez_build` must honor the
same names; it is not implemented by `rez.rs`.

Offline Python builds require the backend and all build dependencies to be
installed in the selected build interpreter. Managed `pip install` and `pip wheel`
add `--no-build-isolation`; managed `python -m build` adds `--no-isolation`.
These commands therefore use the existing interpreter instead of creating an
isolated build environment that can download dependencies. A warm archive or
wheel cache does not replace this prerequisite.

Explicit publication destinations and per-builder release paths take precedence
over `REZ_REPO_PATH`. Existing per-builder defaults still apply; resolution
continues to use `REZ_PACKAGES_PATH`. See the [configuration contract and
precedence](../../../../../docs/mdbook/src/configuration.md#shared-source-cache-and-offline-settings).

## Key Types / Traits

| Type | Description |
|------|-------------|
| `BuildSystemType` | Enum: CMake, Make, Python, Pip, Cargo, Go, Zig, NodeJs, Bun, SCons, Vcpkg, Conan, Extraction, Custom, NoOp. |
| `BuildSystem` | Trait: `name()`, `build_type()`, `is_valid()`, `build()`, `install()`, `child_build_system()`, `supports_build_scripts()`, `write_build_scripts()`. |
| `BuildContext` | Input: source/build/install paths, build type, variant index, build/child arguments, install/script flags, context path, threads, environment and package config. |
| `BuildResult` | Output: success, build_path, install_path, owned prepared_payload, build_env_script, elapsed_secs, error, extra_files. |
| `BuildProcess` | Orchestrator: detects build system, manages variants, sets env vars, invokes builder. |

## Adapter options

Package `config` supplies backend mappings through the shared BuildContext.
Cargo, Go and Zig reject unknown option fields.

| Mapping | Fields and defaults |
|---------|---------------------|
| `config.cargo` | `manifest_path="Cargo.toml"`, `profile="release"`, `locked=true`, `workspace=false`, `features=[]`, `bins=[]`, `artifacts=[]` |
| `config.go` | `manifest_path="go.mod"`, `targets=["./..."]`, `ldflags=null`, `tags=[]`, `cgo=false`, `artifacts=[]` |
| `config.zig` | `manifest_path="build.zig"`, `steps=["install"]`, `optimize="ReleaseSafe"`, `artifacts=[]` |
| `config.nodejs` / `config.bun` | Optional `artifacts`; omission retains the legacy `dist` layout, while an explicit list replaces inference. |
| `config.python` / `config.pip` | `source_inputs`: exact file/directory inputs relative to the project or absolute; project ancestors are rejected. Pip also accepts a string `python` selecting its interpreter. |

Each artifact has `source`, `destination` and optional `from` (`"build"` by
default, or `"source"`). Paths must be safe relative paths. Shared preparation
checks missing inputs, root escapes, directory cycles and destination collisions
before creating an owned payload. Cargo reports binaries under `bin` and dynamic
libraries under `lib`; explicit mappings can place a reported library elsewhere.
Go always adds `go-bin` to `bin`; Zig retains all top-level `zig-out` entries.
Go disables cgo by default only when no explicit or inherited `CGO_ENABLED` exists.
Zig accepts `Debug`, `ReleaseSafe`, `ReleaseFast` and `ReleaseSmall`.

CMake reserves source/build paths and install/staging prefixes. It configures
with the final logical prefix and stages installation under `cmake-install`,
rejecting absolute install destinations in the generated install script. Custom
`install(CODE/SCRIPT)` side effects remain outside that guard. vcpkg resolves
through effective `VCPKG_ROOT`/PATH and reserves its three owned output roots,
including response-file options after `--`. Dry-run/download-only or cache-only
results without installed ports cannot publish a Rez package.

Pip validates the selected launcher before execution or source staging. Supported
launchers are canonical Python entry scripts or verified selected-distlib Windows
launchers; custom or unknown wrappers return an explicit capability error.
Selecting `config.pip.python` does not bypass launcher validation. SourceMap uses
the selected Pip parser and runner for source discovery, interpreter selection
and final arguments; editable references are relocated before shared publication.
Fresh Windows compiler/runtime and private CMake/Cargo/Go/Pip consumers pass;
relative-CMake and launcher-rejection consumers also pass. The composite covers
1,490 unique executed tests. Strict Clippy and packaging pass after formatting
and three redundant-borrow removals; that full runtime checkpoint predates those
edits. The final installed CLI passes isolated Python JSON/SSL/zlib smoke checks
and the repeated moved-bundle consumer. Actual vcpkg and native Unix/macOS
consumers, full Bootstrap117 and Python API parity retain separate gates in
[plan30](../../../../../docs/plans/plan30.md). Final source/archive hashes are recorded in
`dist/verification.json` after canonical ZIP regeneration with the final docs.

## Usage

```rust
use build_system::builders::{BuildProcess, BuildSystemType, create_build_system};

// Auto-detect and build
let mut bp = BuildProcess::new(source_dir, None, None, None)?;
bp.set_package(package, None)?; // Supply Some(developer_package) to retain source lifecycle.
let results = bp.build(
    install_dir,
    /* clean */ true,
    /* install */ true,
    /* variant_indices */ None,
    /* force */ false,
    /* write_build_scripts */ false,
)?;

// Explicit build system
let bs = create_build_system(source_dir, BuildSystemType::CMake, None);
let result = bs.build(&ctx)?;
```
