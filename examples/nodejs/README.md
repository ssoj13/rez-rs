# Node.js Build System Example

Package built with **Node.js / npm**: `package.json` defines the project and scripts.

## Detection

Rez detects a Node.js package by the presence of `package.json` (if no bunfig.toml or bun.lockb — otherwise Bun is used).

## Build

```bash
rez build --install
```

Steps:
1. `npm install`
2. `npm run build`

## Structure

- `package.py` — rez package metadata
- `package.json` — npm configuration, `build` script
- `bin/example_nodejs.js` — executable script
