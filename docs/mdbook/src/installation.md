# Installation

rez-rs is a development preview. Validate the [compatibility requirements](https://github.com/ssoj13/rez-rs/blob/main/docs/plans/plan30.md) of your packages before replacing an existing Rez installation.

## Download the Windows executable

Download the `windows-x86_64` artifact from a successful [Actions run](https://github.com/ssoj13/rez-rs/actions), or `rez.exe` from a [tagged release](https://github.com/ssoj13/rez-rs/releases). Both include a SHA-256 checksum and a verification receipt. CI currently builds Windows x86_64 with the default GUI feature.

Place `rez.exe` in a directory on PATH. Its Python interpreter, standard library, default configuration template, and standard binders are embedded; no source checkout or companion Python files are needed. Generate the commented user configuration with `rez --write-config`.

`rez bind --quickstart` creates definitions for detected software. Platform, architecture, OS, and rez bindings work without a host Python. Binding Python creates a virtual environment using installed CPython; that environment still needs its base Python installation. Missing optional software is skipped. The embedded RustPython interpreter does not install CPython, pip, or setuptools on the host.

The separate `windows-python` artifact and tagged releases also provide `rez-rs-python.zip`, containing `rez/rs.pyd` and initial Rez-compatible wrappers. Extract it before importing and use an isolated CPython environment. See [Python API installation and limits](python-api.md).

## Build from source

Use Rust 1.95 or newer, Git, and a native compiler/linker. The bootstrap scripts require Python 3.10 or newer; reading Cargo install configuration requires Python 3.11 or newer. Native dependencies may need CMake.

The default GUI uses SSH Git dependencies. Access to the locked repositories is currently needed for a fresh workspace build. See the [publication review](public-release-review.md); a cached build on a maintainer's machine does not prove anonymous buildability.

```bash
git clone https://github.com/ssoj13/rez-rs.git
cd rez-rs
cargo build --locked --release --bin rez
```

The executable is `target/release/rez` or `target/release/rez.exe`. A debug build uses `cargo build --locked --bin rez` and writes to `target/debug`.

For the full workspace, including ELF/Mach-O patch crates:

```bash
python bootstrap.py b
```

The release profile favors size and uses `panic = "abort"`. Binary size and build time vary by platform and features; the project does not promise a fixed size or duration.

## Deploy CLI entry points

Deploy the primary executable and its aliases to a directory on PATH. Without `--bin-dir`, the destination is the running executable's directory.

```bash
./target/release/rez deploy --bin-dir "$HOME/.local/bin" --dry-run
./target/release/rez deploy --bin-dir "$HOME/.local/bin"
```

```powershell
.\target\release\rez.exe deploy --bin-dir C:\tools\rez --dry-run
.\target\release\rez.exe deploy --bin-dir C:\tools\rez
# Add C:\tools\rez to your user PATH.
```

| Option | Purpose |
|---|---|
| `--bin-dir DIR` | Select the executable and alias destination. |
| `--source-zip PATH` | Include the source archive as `rez-rs.zip`. |
| `--aliases-only` | Refresh aliases without installing the primary executable. |
| `--dry-run` | Validate ownership and report proposed files without writing. |
| `--recover-only` | Recover an interrupted activation. |

Deployment uses a persistent lock, hash-verified backups, an activation journal, and an ownership manifest. Omitted owned source archives remain installed. Foreign or independently modified aliases cause an error. An unchanged running Windows executable is left in place so it can create its aliases.

To replace an older Windows executable, run the new executable from a separate directory after processes using the destination have exited. Review `rez deploy --help` for the options supported by your binary.

## Install through Cargo

```bash
python bootstrap.py install --root /path/to/cargo-install-root
```

The helper installs the CLI and its aliases under the selected root's `bin` directory. It follows Cargo installation-root precedence when no root is supplied. Use `--force` to replace a Cargo installation.

## Install a staged source package

```bash
python bootstrap.py p
python dist/install.py --repository-root /path/to/repository
```

The package family is `rez_rs`. The installer writes `tool/rez_rs/<version>` below the selected repository root and refuses to replace different bytes already installed at the same version. Set the root with `--repository-root` or `REZ_REPO_PATH`; the explicit argument takes precedence. No root is inferred when both are unset.

Host activation is optional:

```bash
python dist/install.py --repository-root /path/to/repository --rez-root /path/to/existing/installation
```

Activation requires one staged version and a host-compatible executable. It installs CLI files under `Scripts/rez` on Windows or `bin/rez` on Unix, with backups and recovery. System definitions are refreshed using the activated CLI's configuration. Set `REZ_CONFIG_FILE` or configure the CLI to select the intended system-package repository before activation. Apart from the documented `REZ_REPO_PATH` fallback, no deployment-specific environment variables or neighboring bootstrap configuration are inferred.

## Verify

```bash
rez --version
rez status
```

Then configure package paths and try the workflows you need. A successful version check does not establish Python API parity, native shell coverage, or recipe acceptance.

## Run tests

```bash
cargo test --locked --workspace --release
cargo test --locked -p version
cargo test --locked -p resolve
cargo test --locked -p model
cargo test --locked --test cli_integration
python -m unittest discover -s tests -p "test_*.py"
```

The root package has no library facade. Use `--workspace` or an owning crate to include library tests. Ignored native builder scenarios require separate execution and installed toolchains. Recorded results and remaining platform work are in [Plan30](https://github.com/ssoj13/rez-rs/blob/main/docs/plans/plan30.md).

## Upgrade and remove

Build the new version and deploy it from its build directory. Keep the ownership manifest and backups available until the upgrade has been verified.

To remove a manual installation, remove the CLI and aliases you installed and take its directory off PATH. Cargo installations can be removed with `cargo uninstall rez-rs --root /path/to/cargo-install-root`. Configuration, package repositories, caches, and deployment backups may remain; review those separately before removing them.

## Troubleshooting

If `rez` is unavailable, check PATH with `command -v rez` on Unix or `Get-Command rez` in PowerShell.

If dependency fetching requests GitHub credentials, inspect the SSH Git dependencies in `Cargo.lock`. The current publication review tracks this requirement.

If RustPython fails to compile, check `rustc --version` and use the committed lockfile with `--locked`. The runtime pins RustPython 0.6.0 with release-compatible Ruff crates.

On Windows, select `--shell cmd`, `--shell powershell`, or `--shell gitbash` explicitly when automatic shell detection does not match your terminal.
