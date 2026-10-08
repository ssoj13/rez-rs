# Bun Build System Example

Package built with **Bun**: `package.json` + `bunfig.toml` or `bun.lockb`.

## Detection

Rez detects a Bun package by the presence of `package.json` and (`bunfig.toml` or `bun.lockb`). If both exist, Bun takes precedence over Node.js.

## Build

```bash
rez build --install
```

Steps:
1. `bun install`
2. `bun run build`

## Structure

- `package.py` — rez package metadata
- `package.json` — scripts
- `bunfig.toml` — Bun configuration
