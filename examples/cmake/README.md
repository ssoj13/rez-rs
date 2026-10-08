# CMake Build System Example

Package built with **CMake**: `CMakeLists.txt` defines the project and install targets.

## Detection

Rez detects a CMake package by the presence of `CMakeLists.txt` in the root.

## Build

```bash
rez build --install
```

Steps:
1. `cmake -S . -B build -DCMAKE_INSTALL_PREFIX={install}`
2. `cmake --build build --config Release --parallel N`
3. `cmake --install build`

## Structure

- `package.py` — rez package metadata
- `CMakeLists.txt` — CMake configuration
- `main.cpp` — source, builds to `bin/example_cmake`
