# Make Build System Example

Package built with **Make**: `Makefile` defines `all` and `install` targets.

## Detection

Rez detects a Make package by the presence of `Makefile` or `makefile` in the root.

## Build

```bash
rez build --install
```

Steps:
1. `make -jN` (or `make install PREFIX={install} -jN` when installing)

## Structure

- `package.py` — rez package metadata
- `Makefile` — build targets, uses `PREFIX` and `DESTDIR`
- `main.c` — source, builds to `bin/example_make`
