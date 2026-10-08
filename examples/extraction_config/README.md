# Extraction Build System Example (package.config)

Package built with **Extraction**: archive is extracted to the install path. Configuration is in `package.config`.

## Detection

Rez detects an Extraction package by the presence of `sources.yaml`/`sources.yml`. With explicit `build_system = "extraction"` you can use `config` only.

## Build

```bash
rez build --install
```

Steps:
1. Read `config.extraction.downloads` from package
2. For each source — extract archive
3. Copy to install path

## Structure

- `package.py` — metadata and `config.extraction.downloads`
- `archive.zip` — local archive

## Config format

```python
config = {
    "extraction": {
        "downloads": [
            {"path": "archive.zip"},
            {"url": "https://...", "file_name": "foo.zip", "checksum": {"sha256": "..."}}
        ]
    }
}
```
