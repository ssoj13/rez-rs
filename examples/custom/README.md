# Custom Build System Example

Package built with **Custom**: arbitrary command from `build_command` in `package.py`.

## Detection

Rez uses Custom when `build_command` is specified in the package. This overrides auto-detection.

## Build

```bash
rez build --install
```

Steps:
- `python {root}/build.py {install}` (placeholder substitution)

## Placeholders

- `{root}` — path to package source
- `{install}` — install path
- `{build}` — build directory path

## Structure

- `package.py` — `build_command = "python {root}/build.py {install}"`
- `build.py` — build script
