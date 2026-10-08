# Pip Build System Example

Package built with **Pip**: `pip install --prefix` with post-processing (site-packages, bin, shebang).

## Detection

Rez detects a Pip package by the presence of `pyproject.toml` or `requirements.txt` in the root.

## Build

```bash
rez build --install
```

Steps:
1. `pip install --prefix={install} .`
2. Move `Lib/site-packages` → `site-packages` (Windows) or `lib/pythonX.Y/site-packages` → `site-packages` (Unix)
3. Move `Scripts` → `bin` (Windows)
4. Fix shebangs in executables

## Structure

- `package.py` — rez package metadata
- `pyproject.toml` — Python package configuration
- `example_pip.py` — module with entry point
