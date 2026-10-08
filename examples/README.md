# Rez Build System Examples

Example Rez packages for the build systems supported by rez-rs.

## Structure

| Directory | Build system | Description |
|-----------|--------------|-------------|
| [cmake](cmake) | CMake | CMakeLists.txt |
| [make](make) | Make | Makefile |
| [python](python) | Python | setup.py, setuptools |
| [pip](pip) | Pip | pyproject.toml, pip install --prefix |
| [cargo](cargo) | Cargo | Cargo.toml (Rust) |
| [nodejs](nodejs) | Node.js | package.json, npm |
| [bun](bun) | Bun | package.json + bunfig.toml |
| [scons](scons) | SCons | SConstruct |
| [vcpkg](vcpkg) | vcpkg | vcpkg.json |
| [conan](conan) | Conan | conanfile.py |
| [extraction_sources](extraction_sources) | Extraction | sources.yaml |
| [extraction_config](extraction_config) | Extraction | package.config |
| [custom](custom) | Custom | build_command |
| [noop](noop) | NoOp | no build |

## Build

From the repository root, run:

```console
python bootstrap.py example <name>
```

For example:

```console
python bootstrap.py example cmake
python bootstrap.py example cargo
```

The command runs `rez build --install` with the selected example directory as its working directory. If `target/release/rez` does not exist, Cargo runs the CLI from the workspace. Add a tool directory to `PATH` for a single build with `--path-prepend`; the option may be repeated:

```console
python bootstrap.py example conan --path-prepend C:/tools/conda/Scripts
```

Other project commands are listed by `python bootstrap.py --help`. `prepare` replaces the former root setup batch file, and `pip-torch` replaces the optional PyTorch install batch file.

## Requirements / Status

| Example | Requires | Status |
|---------|----------|--------|
| cmake, python, pip, bun, noop | — | Works out of the box |
| extraction_sources, extraction_config, custom, cargo | — | Works (cargo needs `[workspace]` in Cargo.toml when inside a workspace) |
| make | `make` in PATH | Fails if make is not installed |
| nodejs | `npm` in PATH | Fails if Node.js is not installed |
| scons | `scons` in PATH | Often in Anaconda: activate the environment before building |
| vcpkg | `vcpkg` in PATH or `VCPKG_ROOT`, network | Uses `$VCPKG_ROOT/vcpkg` if set; specify `build_system = "vcpkg"` if CMakeLists.txt is present |
| conan | `conan` in PATH, network | Often in Anaconda: activate the environment before building |

## Working with Multiple Packages

See [SUITE.md](SUITE.md) for:

- **rez env pkg1 pkg2** — resolve several packages into one shell
- **rez suite** — toolset with multiple contexts and one `bin/` in PATH

## Notes

- **vcpkg** — specify `build_system = "vcpkg"` when both vcpkg.json and CMakeLists.txt exist
- **conan** — specify `build_system = "conan"` when both conanfile.py and CMakeLists.txt exist
- **Extraction (msi)** — Windows only
- **Extraction** — configuration can use `sources.yaml` or `config.extraction.downloads` in package.py
