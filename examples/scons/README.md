# SCons Build System Example

Package built with **SCons**: `SConstruct` defines build targets.

## Detection

Rez detects a SCons package by the presence of `SConstruct` or `SConscript` in the root.

## Build

```bash
rez build --install
```

Steps:
1. `scons -jN` (PREFIX={install})
2. `scons install` (PREFIX={install})

## Structure

- `package.py` — rez package metadata
- `SConstruct` — SCons script
- `main.c` — source
