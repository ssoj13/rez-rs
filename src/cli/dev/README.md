# cli/dev/

Commands for the package development workflow: building, testing, and releasing packages.
These commands operate on the current working directory (or a specified source directory)
and interact with the build system and repository infrastructure.

## Files

| File | Description |
|------|-------------|
| mod.rs | Module declarations for build, test, and release commands. |
| build.rs | `rez build` — Build a package from source. Build system: CLI `--build-system` overrides `package.build_system`; if neither specified, auto-detect from source files (cmake, make, python, pip, cargo, nodejs, bun, scons, vcpkg, conan, extraction, custom, noop). Supports `--install`, `--clean`, per-variant builds, verbose output. |
| test.rs | `rez test` -- Run tests defined in the package's `tests` attribute. Supports filtering by test name, `--list` to enumerate tests, and lifecycle hooks (pre-install, post-install, pre-release, post-release). |
| release.rs | `rez release` -- Build, install to the release repository, and optionally tag in git. Combines build + deploy into a single workflow with `--no-tag`, `--skip-tests`, and `--message` options. |

## Architecture

The build workflow:

```
rez build
  -> get_developer_package(cwd)     -- load package.py/yaml from source dir
  -> BuildProcess::new(cwd, ...)    -- detect or select build system
  -> BuildProcess::build(...)       -- per-variant: prepare dirs, set env vars, invoke builder
     -> BuildSystem::build(&BuildContext)   -- cmake/make/cargo/etc.
  -> print results
```

The release workflow extends build:

```
rez release
  -> rez build --install (to release repo)
  -> run pre-release/post-release tests if defined
  -> git tag with package version (unless --no-tag)
```

## Key Types Used

- `build_system::builders::BuildProcess` -- orchestrates full build pipeline
- `build_system::builders::BuildSystem` -- trait implemented by each build system
- `build_system::builders::BuildContext` -- input parameters for a build
- `build_system::builders::BuildResult` -- output from a build operation
- `repository::package::discover::get_developer_package` -- loads package from source dir
- `resolve::package::test::PackageTestRunner` -- executes package tests

## Usage from main.rs

```rust
Commands::Build(ref args) => cli::dev::build::run(args),
Commands::Test(ref args) => cli::dev::test::run(args),
Commands::Release(ref args) => cli::dev::release::run(args),
```
