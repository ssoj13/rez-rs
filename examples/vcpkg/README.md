# vcpkg Build System Example

Package built with **vcpkg** (Microsoft C++ package manager): `vcpkg.json` for dependencies, CMake for the project.

## Detection

Rez detects a vcpkg package by the presence of `vcpkg.json` in the root.

## Build

```bash
rez build --install
```

Steps:
1. `vcpkg install --x-install-root={install}`

## Platform

vcpkg runs on Windows, Linux, and macOS.

## Structure

- `package.py` — rez package metadata
- `vcpkg.json` — dependencies (manifest mode)
- `CMakeLists.txt` — project build
