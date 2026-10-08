# Example Packages

The `examples/` directory in the rez-rs repository contains one rez package per build system. Each package has a `README.md` explaining how that build system works.

## Available Examples

| Directory | Build System | Description |
|-----------|--------------|-------------|
| cmake | CMake | CMakeLists.txt |
| make | Make | Makefile |
| python | Python | setup.py, setuptools |
| pip | Pip | pyproject.toml, pip install --prefix |
| cargo | Cargo | Cargo.toml (Rust) |
| nodejs | Node.js | package.json, npm |
| bun | Bun | package.json + bunfig.toml |
| scons | SCons | SConstruct |
| vcpkg | vcpkg | vcpkg.json |
| conan | Conan | conanfile.py |
| extraction_sources | Extraction | sources.yaml |
| extraction_config | Extraction | package.config |
| custom | Custom | build_command |
| noop | NoOp | No build |

## Building an Example

Install the example's external toolchain and configure a writable local package repository first. For CMake, this includes CMake and a C++ compiler. From the package source directory:

```bash
cd examples/cmake
rez build --install
```

## Notes

- **vcpkg** — The adapter is implemented; real vcpkg execution and native platform acceptance remain open.
- **Extraction (msi)** — Windows only (requires msiexec).
- **Extraction** — Two config options: `sources.yaml` in the package root, or `config.extraction.downloads` in package.py with `build_system = "extraction"`.
