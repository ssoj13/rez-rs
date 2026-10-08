# cli/repo/

Commands for repository operations: copying, moving, removing packages, managing
the package cache, binding system software, installing pip packages, and format conversion.
These commands modify package repositories on disk.

## Files

| File | Description |
|------|-------------|
| mod.rs | Module declarations for all nine repo commands. |
| cp.rs | `rez cp` -- Copy packages between repositories. Supports `--reversion`, `--rename`, `--overwrite`, and `--follow-symlinks`. |
| mv.rs | `rez mv` -- Move packages between repositories (copy + remove source). Same options as `cp` plus source cleanup. |
| rm.rs | `rez rm` -- Remove packages from a repository. Supports `--force` to skip confirmation and version range filtering. |
| cache.rs | `rez pkg-cache` -- Manage the local package cache. Add/remove variants, show status, clean stale entries. |
| pkg_ignore.rs | `rez pkg-ignore` -- Ignore or unignore packages from resolves by writing `.ignore` marker files. |
| bind.rs | `rez bind` -- Bind system software as rez packages. Built-in detectors for: platform, arch, os, python, pip, setuptools, rez, cmake, gcc, plus DCC apps (Maya, Houdini, Blender, etc.). `--quickstart` binds 5 core packages; `--all` detects and binds everything found on the system (dependency order). |
| pip.rs | `rez pip` -- Install pip packages as rez packages. Defaults to embedded Python (RustPython); `--external` uses system pip. |
| yaml2py.rs | `rez yaml2py` -- Convert `package.yaml` files to `package.py` format. |

## Architecture

Package operations flow:

```
rez cp foo-1.2.3 --dest-path /release
  -> parse "foo-1.2.3" as VersionedObject
  -> locate source package in packages_path
  -> package::ops::copy_package(src, dest, options)
     -> copy package.py/yaml + payload directory
     -> optionally reversion/rename

rez bind --quickstart
  -> bind_package() for [platform, arch, os, python, rez]

rez bind --all
  -> bind_all() in dependency order: system -> python stack -> tools -> DCC apps
     -> package::bind::detect_by_name() for each module
     -> write_bind_package() for each detected package
```

The `bind` module uses 9 built-in detector functions rather than dynamically
loading Python bind modules (as the original rez does). This avoids a Python
dependency for the common case.

## Key Types Used

- `repository::package::ops::CopyOptions` / `CopyResult` -- copy configuration and outcome
- `repository::package::bind::BindInfo` / `BindFormat` -- bind metadata and output format
- `repository::package::cache::PackageCache` / `CacheStatus` -- cache management
- `version::VersionedObject` -- parsed "name-version" string
- `model::serialise::FileFormat` -- output format for package files

## Usage from main.rs

```rust
Commands::Cp(ref args) => cli::repo::cp::run(args),
Commands::Mv(ref args) => cli::repo::mv::run(args),
Commands::Rm(ref args) => cli::repo::rm::run(args),
Commands::PkgCache(ref args) => cli::repo::cache::run(args),
Commands::Bind(ref args) => cli::repo::bind::run(args),
Commands::Pip(ref args) => cli::repo::pip::run(args),
// ...
```
