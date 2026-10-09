# rez-rs

A Rust port of the [Rez package manager](https://github.com/AcademySoftwareFoundation/rez), with extensions for package building, repository management, and shell environments.

**Experimental personal project:** I built rez-rs for my own use and to explore a Rust implementation of Rez. The scenarios tested so far work and have passed the recorded checks, but the project still needs broader testing and review. It is not yet a complete drop-in replacement for Python Rez. Validate the package definitions, configuration, Python APIs, and native platform behavior your workflow depends on. See [compatibility status](docs/plans/plan30.md) and the [publication review](docs/mdbook/src/public-release-review.md).

## What it provides

- One CLI executable, `rez` / `rez.exe`, with an embedded RustPython 0.6.0 interpreter and frozen standard library.
- Package definitions in Python, YAML, and TOML; version constraints, dependency resolution, saved contexts, and suites.
- Rex environment commands and eight shell backends: bash, sh, zsh, csh, tcsh, cmd, PowerShell, and Git Bash.
- Package build, install, release, copy, move, cache, and bundle operations.
- Build adapters for CMake, Make, Python, Pip, Cargo, Go, Zig, Node.js, Bun, SCons, vcpkg, Conan, extraction, custom commands, and no-op builds. Adapter presence does not establish complete native acceptance.
- An optional egui GUI, enabled by the default `gui` feature.

The CLI embeds its Python interpreter. Software managed by Rez and the tools selected by package recipes still need to be available. CPython extension modules such as NumPy cannot be imported into the embedded VM. A separate CPython distribution provides initial Rez-compatible imports, with Rust-specific access in `rez.rs`. Full Python API parity and the custom `rez.bld` API remain outside the accepted scope; see [Python API](docs/mdbook/src/python-api.md).

## Build

Use Rust 1.95 or newer, Git, Python 3.10 or newer for the helper scripts, and a native compiler/linker for your platform. Python 3.11 or newer is needed when the installer reads Cargo TOML configuration. Native dependencies may also need CMake.

The default GUI currently depends on Git repositories accessed through SSH. A fresh build requires access to the revisions in `Cargo.lock`; this remains a publication prerequisite. Disabling the root GUI feature does not guarantee that Cargo can resolve the workspace without those repositories.

```bash
git clone https://github.com/ssoj13/rez-rs.git
cd rez-rs
cargo build --locked --release --bin rez
```

The executable is `target/release/rez` on Linux/macOS or `target/release/rez.exe` on Windows. To compile every workspace member, including the native binary-patching crates:

```bash
python bootstrap.py b
```

Build and runtime receipts currently cover Windows. Native Linux/macOS and GUI acceptance have separate open items in [Plan30](docs/plans/plan30.md).

## Download the Windows executable

The [CI and release workflow](.github/workflows/ci.yml) checks formatting, Python helpers, documentation, release workspace tests, and strict Clippy. It stages the default-feature Windows x86_64 executable and CPython module through `bootstrap.py p`, then verifies the binaries, source archive, and extracted Python distribution before upload.

Successful trusted runs provide a `windows-x86_64` artifact containing `rez.exe`, its SHA-256 checksum, and a verification receipt. The separate `windows-python` artifact contains `rez-rs-python.zip` with `rez/rs.pyd` and compatibility wrappers; the same archive is tested on CPython 3.13 and 3.10. Pushing a `v<version>` tag matching `Cargo.toml` publishes those assets as a GitHub release; tags containing a prerelease suffix create a prerelease. Fork pull requests run source checks only while the GUI dependencies remain private. See [CI setup](docs/mdbook/src/development.md#github-ci-and-releases).

The executable carries its interpreter, standard library, configuration template, and standard binders. No companion Python source files are needed for `rez --write-config` or `rez bind --quickstart`. Quickstart binds software already installed on the host; Python, pip, and setuptools can be skipped when unavailable. A bound CPython environment still depends on its installed base Python.

## Install CLI entry points

Deploy the executable and its aliases into a directory on your PATH:

```bash
./target/release/rez deploy --bin-dir "$HOME/.local/bin" --dry-run
./target/release/rez deploy --bin-dir "$HOME/.local/bin"
```

On Windows:

```powershell
.\target\release\rez.exe deploy --bin-dir C:\tools\rez --dry-run
.\target\release\rez.exe deploy --bin-dir C:\tools\rez
```

Add the destination directory to PATH, then run `rez --version` and `rez status`. Deployment uses ownership manifests, backups, and an activation journal; review `rez deploy --help` before updating an existing installation.

## Try package resolution

Generate a configuration and bind the core system packages:

```bash
rez --write-config
rez bind --quickstart
rez env python -- python --version
```

Review the generated `~/.rez/rezconfig.py` to select your package repositories. It also documents `sources_path`, `wheel_cache_path`, `user_path`, `offline`, `repo_path`, and `log_level`, with environment overrides `REZ_SOURCES_PATH`, `REZ_WHEEL_CACHE_PATH`, `REZ_USER_PATH`, `REZ_OFFLINE`, `REZ_REPO_PATH`, and `REZ_LOG_LEVEL`. Use `REZ_OFFLINE=true` or `false`; see the [shared settings and publication precedence](docs/mdbook/src/configuration.md#shared-source-cache-and-offline-settings). Existing configurations are preserved unless `--force` is supplied. Binding creates package definitions for software detected on the host; it does not install that software.

For a package build example, see [examples](examples/README.md). Each example names its required build tools and contains a package recipe.

## Python API

Compatibility imports use `rez.version`, `rez.packages`, and `rez.resolved_context`; our native additions use `rez.rs`. `python bootstrap.py p --force` stages this separate package under `dist/python/<version>`. Extract its ZIP and add the extraction directory to `PYTHONPATH`; a `.pyd` cannot be imported from inside a ZIP. Use a separate environment from upstream Rez because both provide the `rez` package. See [supported APIs and limitations](docs/mdbook/src/python-api.md).

## Embedded Python

RustPython executes Python package definitions, configuration, and Rex commands. Most components, including stdlib and SRE, come from crates.io. Four components remain local through Cargo path patches because they retain active compatibility fixes. Regression coverage is kept in the project test suites. See [why RustPython is bundled](docs/mdbook/src/rustpython-vendoring.md).

## Test and develop

```bash
cargo test --locked --workspace --release
cargo fmt --all -- --check
cargo clippy --locked --workspace --release --all-targets -- -D warnings
python -m unittest discover -s tests -p "test_*.py"
```

`python bootstrap.py ci` runs all of these, then builds, bundles and verifies the release ZIPs exactly as GitHub CI does on Windows, Linux and macOS.

Root-only `cargo test` does not run the libraries moved into the functional crates. A workspace test run also selects the GUI member. Some native builder tests are explicitly ignored in the ordinary run and must be selected separately.

The current Windows release workspace gate passed both full campaigns: 1,529 test/doc-test executions with normal temporary paths and 1,529 with a genuine Windows 8.3 temporary-path alias, each across 47 targets with zero failures and seven ignored scenarios. This totals 3,058 executions of the same test scopes across the two path environments. Strict workspace Clippy passed, and the same Python API archive passed 15 acceptance tests on each of CPython 3.13 and 3.10. Ignored native scenarios, GUI interaction, and Unix runtime acceptance remain separate gates. [Plan30](docs/plans/plan30.md) summarizes their scope; detailed earlier receipts remain with the previous repository.

## Documentation

- [Installation](docs/mdbook/src/installation.md), [getting started](docs/mdbook/src/getting-started.md), and [configuration](docs/mdbook/src/configuration.md).
- [CLI commands](docs/mdbook/src/commands.md), [package format](docs/mdbook/src/package-format.md), and [Rex commands](docs/mdbook/src/rex-commands.md).
- [Development guide](docs/mdbook/src/development.md) and [crate architecture](docs/mdbook/src/architecture-crates.md).
- [Compatibility work queue](docs/plans/plan30.md).
- [Publication review](docs/mdbook/src/public-release-review.md), including remaining dependency and compatibility boundaries.

Build the documentation book with `mdbook build docs/mdbook`. See the [documentation index](docs/README.md) for the book layout and active work plan.

## Source packages

`python bootstrap.py p` builds the release CLI and CPython API. It stages `dist/rez_rs/<version>` with `package.py`, the executable, and `rez-rs.zip`. Use `--force` to replace an existing staged version. Installer helpers are copied beside the package.

Give the installer a repository root through `--repository-root` or `REZ_REPO_PATH`; the explicit argument takes precedence:

```bash
python dist/install.py --repository-root /path/to/repository
```

Add `--rez-root /path/to/existing/installation` only when you want host activation. Set `REZ_CONFIG_FILE` or configure the CLI before activation so system bindings use the intended repository. See [installation](docs/mdbook/src/installation.md#install-a-staged-source-package).

## License and attribution

Apache-2.0; see [LICENSE](LICENSE) and [NOTICE](NOTICE). Bundled third-party code retains its own licenses and notices. rez-rs is an independent port and is not an official release of upstream Rez.
