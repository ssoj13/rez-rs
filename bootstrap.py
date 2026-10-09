#!/usr/bin/env python3
"""Cross-platform build, packaging, verification, setup, and example commands.

Requires Python 3.10+ and Rust/Cargo for Rust commands. Uses only the Python
standard library and never invokes a shell to construct command lines.

Examples:
    python bootstrap.py build
    python bootstrap.py package
    python bootstrap.py p --force
    python bootstrap.py test -n
    python bootstrap.py check
    python bootstrap.py ci --target x86_64-pc-windows-msvc
    python bootstrap.py prepare
    python bootstrap.py pip-torch
    python bootstrap.py example cargo
    python bootstrap.py example conan --path-prepend C:/tools/conda/Scripts

The package command builds the Release CLI and creates an installable Rez
package under dist/rez_rs/<version>/ containing package.py, the Cargo-produced
CLI executable (rez.exe on Windows), and rez-rs.zip. The source archive excludes
Git metadata and generated build artifacts. Existing versions are preserved
unless --force replaces that version. The shared native-alias installer support
modules and installer are copied verbatim to dist/, outside the Rez payload.
The CPython facade and native rez.rs extension are staged separately under
dist/python/<version>/ and in its rez-rs-python.zip archive.
Host activation refreshes platform/arch/os from the native binder with backups.
The install command installs metadata-derived native aliases beside Cargo's CLI.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import zipfile
from pathlib import Path
from typing import Sequence


ROOT_DIR = Path(__file__).resolve().parent
EXAMPLES_DIR = ROOT_DIR / "examples"
DIST_DIR = ROOT_DIR / "dist"
REZ_PACKAGE_NAME = "rez_rs"
PACKAGE_FAMILY_DIR = DIST_DIR / REZ_PACKAGE_NAME
SOURCE_ARCHIVE_EXCLUDES = frozenset({
    ".claude",
    ".codex",
    ".omh",
    ".env",
    ".git",
    ".idea",
    ".mcp.json",
    ".vscode",
    "CMakeUserPresets.json",
    "CMakeCache.txt",
    "CMakeFiles",
    ".sconsign.dblite",
    ".scons_node_count",
    ".eggs",
    ".gitnexus",
    ".pytest_cache",
    ".repl_history.txt",
    ".venv",
    "__pycache__",
    "dist",
    "node_modules",
    "target",
    "venv",
})
RELEASE_REZ = ROOT_DIR / "target" / "release" / ("rez.exe" if os.name == "nt" else "rez")
EXAMPLE_NAMES = tuple(
    sorted(path.name for path in EXAMPLES_DIR.iterdir() if path.is_dir())
)


def run(
    command: Sequence[str],
    *,
    cwd: Path = ROOT_DIR,
    env: dict[str, str] | None = None,
    capture: bool = False,
) -> int:
    """Run one command and return its exit code, optionally summarizing diagnostics."""
    printable = subprocess.list2cmdline(list(command)) if os.name == "nt" else " ".join(
        repr(part) for part in command
    )
    print(f"\n> {printable}", flush=True)
    started = time.perf_counter()
    try:
        result = subprocess.run(
            list(command),
            cwd=cwd,
            env=env,
            check=False,
            capture_output=capture,
            text=capture,
        )
    except FileNotFoundError as error:
        print(f"Command not found: {error.filename}", file=sys.stderr)
        return 127
    elapsed = time.perf_counter() - started
    print(f"Completed in {elapsed:.1f}s (exit code {result.returncode})", flush=True)

    if capture and result.returncode:
        output = (result.stdout or "") + (result.stderr or "")
        diff_headers = [line for line in output.splitlines() if line.startswith("Diff in ")]
        if diff_headers:
            print(f"Formatting check found {len(diff_headers)} differences.")
            for line in diff_headers[:10]:
                print(f"  {line}")
            if len(diff_headers) > 10:
                print(f"  ... and {len(diff_headers) - 10} more")
            print("Run `cargo fmt --all` to apply rustfmt.")
        elif output:
            print(output[-6000:], end="" if output.endswith("\n") else "\n")
    return result.returncode


def cargo(*args: str) -> list[str]:
    return ["cargo", *args]


def rez_command(*args: str) -> list[str]:
    """Use the release CLI when available, otherwise let Cargo run the binary."""
    if RELEASE_REZ.is_file():
        return [str(RELEASE_REZ), *args]
    return cargo("run", "--quiet", "--bin", "rez", "--", *args)


def run_prepare(_args: argparse.Namespace) -> int:
    """Reproduce the setup commands previously stored in bootstrap.cmd."""
    steps = [
        rez_command("-vvv", "bind", "-a", "--release"),
        rez_command("-vvv", "pip", "-i", "appdirs", "PySide6", "Qt.py", "ipython", "ipdb", "numpy", "--release"),
    ]
    for command in steps:
        code = run(command)
        if code:
            return code
    return 0


def run_env(_args: argparse.Namespace) -> int:
    """Run the optional environment command that was commented out in bootstrap.cmd."""
    return run(rez_command("-vvv", "env", "appdirs", "PySide6", "Qt.py"))


def run_pip_torch(_args: argparse.Namespace) -> int:
    """Reproduce the optional PyTorch install command from test_cmd.cmd."""
    return run(
        rez_command(
            "-vvv", "pip", "-i", "torch", "torchvision", "--",
            "--index-url", "https://download.pytorch.org/whl/cu130", "-v",
        )
    )


def run_build(args: argparse.Namespace) -> int:
    profile = [] if args.debug else ["--release"]
    return run(cargo("build", "--workspace", *profile))


def cargo_package_version() -> str | None:
    """Read the root crate version from Cargo's resolved manifest metadata."""
    try:
        result = subprocess.run(
            cargo("metadata", "--no-deps", "--format-version", "1"),
            cwd=ROOT_DIR,
            check=False,
            capture_output=True,
            text=True,
        )
    except FileNotFoundError as error:
        print(f"Command not found: {error.filename}", file=sys.stderr)
        return None

    if result.returncode:
        if result.stderr:
            print(result.stderr, file=sys.stderr, end="")
        return None

    try:
        metadata = json.loads(result.stdout)
        manifest = (ROOT_DIR / "Cargo.toml").resolve()
        package = next(
            item
            for item in metadata["packages"]
            if Path(item["manifest_path"]).resolve() == manifest
        )
        return package["version"]
    except (KeyError, StopIteration, TypeError, json.JSONDecodeError) as error:
        print(f"Could not read rez-rs version from Cargo metadata: {error}", file=sys.stderr)
        return None


def source_archive_path_is_excluded(relative: Path) -> bool:
    """Exclude local state and generated example outputs while retaining binary fixtures."""
    if any(part in SOURCE_ARCHIVE_EXCLUDES or part.startswith(".env.")
           or part.endswith(".egg-info") for part in relative.parts):
        return True
    if relative.parts[:1] == ("packages",):
        return True
    if relative.parts[:2] == ("docs", "build"):
        return True
    if relative.parts[:1] == ("examples",):
        if any(part in {"build", ".conan", "vcpkg_installed"} for part in relative.parts[1:]):
            return True
        if relative.suffix.lower() in {
            ".exe", ".obj", ".o", ".os", ".so", ".dll", ".dylib",
            ".a", ".d", ".out", ".pdb", ".egg",
        }:
            return True
    return False


def write_source_archive(destination: Path) -> None:
    """Archive the current project sources without VCS or generated build files."""
    with zipfile.ZipFile(
        destination,
        mode="w",
        compression=zipfile.ZIP_DEFLATED,
        compresslevel=9,
    ) as archive:
        for current, directories, files in os.walk(ROOT_DIR):
            directories[:] = sorted(
                name for name in directories
                if not source_archive_path_is_excluded((Path(current) / name).relative_to(ROOT_DIR))
            )
            current_path = Path(current)
            for filename in sorted(files):
                source = current_path / filename
                if source_archive_path_is_excluded(source.relative_to(ROOT_DIR)):
                    continue
                archive.write(source, Path("rez-rs") / source.relative_to(ROOT_DIR))


def stage_python_api(version: str) -> int:
    """Build an abi3 extension and atomically stage the separate Python distribution."""
    import sysconfig
    if sys.implementation.name != "cpython" or sys.version_info < (3, 10) or sysconfig.get_config_var("Py_GIL_DISABLED"):
        print("Packaging requires CPython 3.10+ with the GIL enabled.", file=sys.stderr)
        return 2
    extension_env = os.environ.copy()
    extension_env["PYO3_PYTHON"] = sys.executable
    extension_env["PYO3_BUILD_EXTENSION_MODULE"] = "1"
    code = run(cargo("build", "--locked", "--release", "-p", "rez-python-api"), env=extension_env)
    if code:
        return code
    library_name = "rs.dll" if os.name == "nt" else ("librs.dylib" if sys.platform == "darwin" else "librs.so")
    extension_name = "rs.pyd" if os.name == "nt" else "rs.abi3.so"
    library = ROOT_DIR / "target" / "release" / library_name
    if not library.is_file():
        print(f"CPython extension was not produced: {library}", file=sys.stderr)
        return 1
    family = DIST_DIR / "python"
    destination = family / version
    family.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=f".{version}-", dir=family) as temporary:
        staging = Path(temporary) / version
        package = staging / "rez"
        package.mkdir(parents=True)
        for source in sorted((ROOT_DIR / "crates" / "rez" / "python-api" / "python" / "rez").glob("*.py")):
            shutil.copy2(source, package / source.name)
        shutil.copy2(library, package / extension_name)
        for name in ("LICENSE", "NOTICE"):
            shutil.copy2(ROOT_DIR / name, staging / name)
        licenses = staging / "third-party-licenses"
        licenses.mkdir()
        for source in sorted((ROOT_DIR / "crates" / "rez" / "python-api" / "licenses").glob("*")):
            shutil.copy2(source, licenses / source.name)
        import platform
        import struct
        metadata = {
            "version": version, "module": "rez.rs", "extension": extension_name,
            "python_abi": "abi3", "minimum_python": "3.10",
            "platform": sys.platform, "machine": platform.machine(), "pointer_bits": struct.calcsize("P") * 8,
        }
        (staging / "python-api.json").write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")
        archive_path = staging / "rez-rs-python.zip"
        members = [*sorted(package.iterdir()), *sorted(licenses.iterdir()),
                   staging / "LICENSE", staging / "NOTICE", staging / "python-api.json"]
        with zipfile.ZipFile(archive_path, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
            for source in members:
                archive.write(source, source.relative_to(staging))
        previous = Path(temporary) / "previous"
        if destination.exists():
            os.replace(destination, previous)
        try:
            os.replace(staging, destination)
        except OSError:
            if previous.exists():
                os.replace(previous, destination)
            raise
    print(f"CPython package staged at {destination}")
    return 0


def run_package(args: argparse.Namespace) -> int:
    """Build a Release executable and create an installable Rez package in dist/."""
    version = cargo_package_version()
    if version is None:
        return 1

    package_dir = PACKAGE_FAMILY_DIR / version
    if package_dir.exists() and not args.force:
        print(
            f"Package already exists: {package_dir}. "
            "Pass --force to replace that version.",
            file=sys.stderr,
        )
        return 2

    code = run(cargo("build", "--locked", "--release", "--bin", "rez"))
    if code:
        return code
    if not RELEASE_REZ.is_file():
        print(f"Release executable was not produced: {RELEASE_REZ}", file=sys.stderr)
        return 1

    code = stage_python_api(version)
    if code:
        return code

    try:
        DIST_DIR.mkdir(parents=True, exist_ok=True)
        # Installer support is generated from the canonical module, outside payload.
        with tempfile.TemporaryDirectory(prefix=".installer-", dir=DIST_DIR) as support_tmp:
            for support_source in (
                ROOT_DIR / "cli_install.py", ROOT_DIR / "system_bind.py",
                ROOT_DIR / "packaging" / "install.py",
            ):
                support_staged = Path(support_tmp) / support_source.name
                shutil.copy2(support_source, support_staged)
                if hashlib.sha256(support_staged.read_bytes()).digest() != hashlib.sha256(support_source.read_bytes()).digest():
                    raise OSError("Installer support changed during staging")
                os.replace(support_staged, DIST_DIR / support_source.name)
        PACKAGE_FAMILY_DIR.mkdir(parents=True, exist_ok=True)
        staging_root = Path(
            tempfile.mkdtemp(prefix=f".{version}-", dir=PACKAGE_FAMILY_DIR)
        )
    except OSError as error:
        print(f"Could not create package staging directory: {error}", file=sys.stderr)
        return 1

    staged_package = staging_root / version
    previous_package = staging_root / "previous"
    binary_name = RELEASE_REZ.name
    tool_name = RELEASE_REZ.stem

    try:
        staged_package.mkdir()
        shutil.copy2(RELEASE_REZ, staged_package / binary_name)
        write_source_archive(staged_package / "rez-rs.zip")
        (staged_package / "package.py").write_text(
            f'''name = {REZ_PACKAGE_NAME!r}
version = {version!r}
description = "Rust implementation of the Rez package manager."
package_type = "tool"
requires = []
tools = [{tool_name!r}]
tests = {{"version": {{"command": "{tool_name} --version"}}}}


def commands():
    env.PATH.prepend("{{this.root}}")
''',
            encoding="utf-8",
        )

        if package_dir.exists():
            os.replace(package_dir, previous_package)
        try:
            os.replace(staged_package, package_dir)
        except OSError:
            if previous_package.exists():
                os.replace(previous_package, package_dir)
            raise

        if previous_package.exists():
            shutil.rmtree(previous_package)
    except (OSError, zipfile.BadZipFile) as error:
        print(f"Could not stage Rez package at {package_dir}: {error}", file=sys.stderr)
        return 1
    finally:
        shutil.rmtree(staging_root, ignore_errors=True)

    print(f"Rez package staged at {package_dir}")
    return 0


def run_test(args: argparse.Namespace) -> int:
    command = cargo("test", "--workspace")
    if not args.debug:
        command.append("--release")

    test_args = list(args.cargo_args)
    if test_args[:1] == ["--"]:
        test_args.pop(0)
    if args.nocapture:
        test_args.insert(0, "--nocapture")
    if test_args:
        command.extend(["--", *test_args])
    return run(command)


def run_check(_args: argparse.Namespace) -> int:
    code = run(cargo("fmt", "--all", "--", "--check"), capture=True)
    if code:
        return code
    return run(cargo("clippy", "--workspace", "--all-targets", "--", "-D", "warnings"))


def run_install(args: argparse.Namespace) -> int:
    """Install the CLI and native aliases into the same verified Cargo root."""
    cargo_home = Path(os.environ.get("CARGO_HOME") or Path.home() / ".cargo")
    if not cargo_home.is_absolute():
        cargo_home = ROOT_DIR / cargo_home
    selected_root = args.root or os.environ.get("CARGO_INSTALL_ROOT")
    if selected_root is None:
        config_directories = [*(directory / ".cargo" for directory in [ROOT_DIR, *ROOT_DIR.parents]), cargo_home]
        for directory in config_directories:
            config_file = directory / "config"
            if not config_file.is_file():
                config_file = directory / "config.toml"
            if not config_file.is_file():
                continue
            try:
                import tomllib
                with config_file.open("rb") as stream:
                    config = tomllib.load(stream)
                if "include" in config:
                    raise ValueError("Cargo config includes require an explicit installation root")
                install_root = config.get("install", {}).get("root")
                if install_root is not None:
                    if not isinstance(install_root, str) or not install_root:
                        raise ValueError("install.root must be a non-empty path string")
                    configured_root = Path(install_root)
                    selected_root = configured_root if configured_root.is_absolute() else directory.parent / configured_root
                    break
            except (ImportError, OSError, ValueError, TypeError, AttributeError) as error:
                print(f"Cannot determine Cargo installation root from {config_file}: {error}. "
                      "Use bootstrap.py install --root DIR.", file=sys.stderr)
                return 1
    install_root = Path(selected_root) if selected_root is not None else cargo_home
    if not install_root.is_absolute():
        install_root = ROOT_DIR / install_root
    command = cargo("install", "--path", str(ROOT_DIR), "--root", str(install_root))
    if args.force:
        command.append("--force")
    code = run(command)
    if code:
        return code
    from cli_install import InstallError, install_cli

    executable = install_root / "bin" / ("rez.exe" if os.name == "nt" else "rez")
    try:
        install_cli(executable, executable.parent)
    except InstallError as error:
        print(f"Cannot install native CLI aliases: {error}", file=sys.stderr)
        return 1
    return 0


def run_clean(_args: argparse.Namespace) -> int:
    return run(cargo("clean"))


def run_example(args: argparse.Namespace) -> int:
    example_dir = EXAMPLES_DIR / args.name
    if not example_dir.is_dir():
        print(f"Example directory does not exist: {example_dir}", file=sys.stderr)
        return 2

    env = os.environ.copy()
    if args.path_prepend:
        current = env.get("PATH", "")
        env["PATH"] = os.pathsep.join(
            [*(str(path) for path in args.path_prepend), current] if current else
            [str(path) for path in args.path_prepend]
        )
    return run(rez_command("build", "--install"), cwd=example_dir, env=env)


def run_portable(args: argparse.Namespace) -> int:
    """Keep CI/release orchestration behind the same bootstrap entry point."""
    command = [sys.executable, str(ROOT_DIR / "ci.py"), args.command]
    if args.target:
        command += ["--target", args.target]
    for python in args.python:
        command += ["--python", python]
    return run(command)


def make_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Cross-platform build, packaging, and development commands for rez-rs."
    )
    commands = parser.add_subparsers(dest="command", required=True)

    build_parser = commands.add_parser("build", aliases=["b"], help="Build the rez workspace.")
    build_parser.add_argument("-d", "--debug", action="store_true", help="Build without the release profile.")
    build_parser.set_defaults(handler=run_build)

    package_parser = commands.add_parser(
        "package",
        aliases=["p"],
        help=f"Build the Release CLI, CPython API, and dist/{REZ_PACKAGE_NAME}/<version> sources.",
    )
    package_parser.add_argument(
        "--force",
        action="store_true",
        help=f"Replace this version in dist/{REZ_PACKAGE_NAME}.",
    )
    package_parser.set_defaults(handler=run_package)

    test_parser = commands.add_parser("test", aliases=["t"], help="Run workspace tests.")
    test_parser.add_argument("-d", "--debug", action="store_true", help="Run tests without the release profile.")
    test_parser.add_argument("-n", "--nocapture", action="store_true", help="Show test output.")
    test_parser.add_argument("cargo_args", nargs=argparse.REMAINDER, help="Additional Cargo test arguments.")
    test_parser.set_defaults(handler=run_test)

    check_parser = commands.add_parser("check", aliases=["c"], help="Check formatting and run Clippy.")
    check_parser.set_defaults(handler=run_check)

    for name, help_text in (
        ("ci", "Run all source gates, then build, bundle and verify the release ZIPs."),
        ("release", "Build, bundle and verify the release ZIPs in dist/release."),
    ):
        portable_parser = commands.add_parser(name, help=help_text)
        portable_parser.add_argument("--target", help="Rust target triple; must match the host.")
        portable_parser.add_argument("--python", action="append", default=[],
                                     help="Extra CPython interpreter for the Python ZIP tests.")
        portable_parser.set_defaults(handler=run_portable)

    prepare_parser = commands.add_parser("prepare", help="Bind system packages and install the common Python packages.")
    prepare_parser.set_defaults(handler=run_prepare)

    env_parser = commands.add_parser("env", help="Run the optional sample Rez environment command.")
    env_parser.set_defaults(handler=run_env)

    torch_parser = commands.add_parser("pip-torch", help="Install torch and torchvision from the CUDA 13.0 wheel index.")
    torch_parser.set_defaults(handler=run_pip_torch)

    install_parser = commands.add_parser("install", aliases=["i"], help="Install rez-rs into Cargo's bin directory.")
    install_parser.add_argument("-f", "--force", action="store_true", help="Replace an existing installation.")
    install_parser.add_argument("--root", type=Path, help="Installation root shared by Cargo and native CLI aliases.")
    install_parser.set_defaults(handler=run_install)

    clean_parser = commands.add_parser("clean", aliases=["cl"], help="Remove Cargo build artifacts.")
    clean_parser.set_defaults(handler=run_clean)

    example_parser = commands.add_parser("example", help="Build and install one example package.")
    example_parser.add_argument("name", choices=EXAMPLE_NAMES, help="Example directory under examples/.")
    example_parser.add_argument(
        "--path-prepend",
        action="append",
        type=Path,
        default=[],
        metavar="DIR",
        help="Prepend a directory to PATH for this build (may be repeated).",
    )
    example_parser.set_defaults(handler=run_example)

    return parser


def main(argv: Sequence[str] | None = None) -> int:
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(line_buffering=True)
    args = make_parser().parse_args(argv)
    return args.handler(args)


if __name__ == "__main__":
    raise SystemExit(main())
