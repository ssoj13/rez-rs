# Python Build System Example

Package built with **Python setuptools**: `setup.py` defines the package and entry points.

## Detection

Rez detects a Python package by the presence of `setup.py` or `pyproject.toml` with setuptools in `[build-system]`.

## Build

```bash
rez build --install
```

Steps:
1. `python setup.py build --build-base={build}`
2. `python setup.py install --prefix={install}`

## Structure

- `package.py` — rez package metadata
- `setup.py` — setuptools configuration, entry points
- `example_python/` — Python module
