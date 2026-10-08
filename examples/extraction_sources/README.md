# Extraction Build System Example (sources.yaml)

Package built with **Extraction**: archive is extracted to the install path. Configuration is in `sources.yaml`.

## Detection

Rez detects an Extraction package by the presence of `sources.yaml` or `sources.yml` in the root.

## Build

```bash
rez build --install
```

Steps:
1. Read `sources.yaml` → `downloads`
2. For each `path` (or `url`) — extract zip/tar/tar.gz/tar.xz
3. Copy contents to install path
4. If a single root directory — use it as the root

## Supported formats

- zip, tar, tar.gz, tgz, tar.xz
- **msi** — Windows only (requires msiexec)

## Structure

- `package.py` — rez package metadata
- `sources.yaml` — `downloads: [{ path: "archive.zip" }]` or `url`
- `archive.zip` — local archive
