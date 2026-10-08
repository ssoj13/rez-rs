# Cargo Build System Example

Package built with **Cargo** (Rust): `Cargo.toml` defines the crate.

## Detection

Rez detects a Cargo package by the presence of `Cargo.toml` in the root.

## Build

```bash
rez build --install
```

Steps:
1. `cargo build --release --target-dir={build}`
2. Copy binaries from `target/release` to `{install}/bin`

## Structure

- `package.py` — rez package metadata
- `Cargo.toml` — Rust crate configuration
- `src/main.rs` — source
