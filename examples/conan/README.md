# Conan Build System Example

Package built with **Conan**: `conanfile.py` defines dependencies and build.

## Detection

Rez detects a Conan package by the presence of `conanfile.py` or `conanfile.txt` in the root.

## Build

```bash
rez build --install
```

Steps:
1. `conan build . -of={install}` (for conanfile.py)

## Structure

- `package.py` — rez package metadata
- `conanfile.py` — Conan recipe
- `CMakeLists.txt`, `src/main.cpp` — project
