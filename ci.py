#!/usr/bin/env python3
"""Portable CI/release commands, invoked through bootstrap.py (Python 3.11+).

`ci` runs every source gate, then the `release` steps: stage the CLI package and
the CPython distribution, bundle one CLI ZIP and one Python ZIP per platform into
dist/release/, and verify both archives after extraction.
"""

from __future__ import annotations

import argparse
import ast
import os
import shutil
import subprocess
import sys
import tempfile
import tomllib
import zipfile
from pathlib import Path

import bootstrap

ROOT = Path(__file__).resolve().parent
RELEASE_DIR = ROOT / "dist" / "release"
TARGETS = {
    "x86_64-pc-windows-msvc": "windows-x86_64",
    "x86_64-unknown-linux-gnu": "linux-x86_64",
    "aarch64-apple-darwin": "macos-aarch64",
}
EXE = "rez.exe" if os.name == "nt" else "rez"
INSTALL_HELPERS = ("install.py", "cli_install.py", "system_bind.py")


def run(command: list[str], env: dict[str, str] | None = None, **kwargs) -> subprocess.CompletedProcess:
    print("+ " + " ".join(map(str, command)), flush=True)
    return subprocess.run(command, cwd=kwargs.pop("cwd", ROOT), env=env, check=True, **kwargs)


def rust_host() -> str:
    output = run(["rustc", "-vV"], capture_output=True, text=True).stdout
    return next(line.removeprefix("host: ") for line in output.splitlines()
                if line.startswith("host: "))


def isolated_env(home: Path) -> dict[str, str]:
    """Keep OS variables but drop inherited Rez/Python configuration."""
    env = {key: value for key, value in os.environ.items()
           if not key.upper().startswith(("REZ_", "PYTHON", "RUSTPYTHON"))}
    env.update(HOME=str(home), USERPROFILE=str(home), REZ_DISABLE_HOME_CONFIG="1",
               TMP=str(home), TEMP=str(home), TMPDIR=str(home))
    return env


def check_sources() -> None:
    run(["cargo", "fmt", "--all", "--", "--check"])
    run(["cargo", "clippy", "--locked", "--workspace", "--release", "--all-targets",
         "--", "-D", "warnings"])
    # Pip tests otherwise look up `python`, which Linux distributions name `python3`.
    run(["cargo", "test", "--locked", "--workspace", "--release"],
        dict(os.environ, REZ_TEST_PIP_PYTHON=sys.executable))
    run([sys.executable, "-m", "unittest", "discover", "-s", "tests", "-p", "test_*.py"])
    run(["mdbook", "build", "docs/mdbook"])


def stage(version: str) -> tuple[Path, Path]:
    """Build through the canonical bootstrap packager; return (CLI, Python) stage dirs."""
    if bootstrap.run_package(argparse.Namespace(force=True)):
        raise RuntimeError("bootstrap package staging failed")
    return bootstrap.PACKAGE_FAMILY_DIR / version, bootstrap.DIST_DIR / "python" / version


def bundle(cli_dir: Path, python_dir: Path, version: str, platform: str) -> tuple[Path, Path]:
    """Write the installable CLI ZIP (installer + one package version) and the Python ZIP."""
    RELEASE_DIR.mkdir(parents=True, exist_ok=True)
    cli_zip = RELEASE_DIR / f"rez-rs-v{version}-{platform}.zip"
    with zipfile.ZipFile(cli_zip, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        for name in INSTALL_HELPERS:
            archive.write(bootstrap.DIST_DIR / name, name)
        for source in sorted(cli_dir.iterdir()):
            # ZipFile.write keeps the Unix mode, so the executable bit survives.
            archive.write(source, f"{cli_dir.parent.name}/{version}/{source.name}")
    python_zip = RELEASE_DIR / f"rez-rs-python-v{version}-{platform}.zip"
    shutil.copy2(python_dir / "rez-rs-python.zip", python_zip)
    return cli_zip, python_zip


def check_source_archive(archive_path: Path) -> None:
    """The embedded source archive must equal the current non-excluded sources."""
    expected = {}
    for current, directories, files in os.walk(ROOT):
        directories[:] = [name for name in directories if not
                          bootstrap.source_archive_path_is_excluded(
                              (Path(current) / name).relative_to(ROOT))]
        for name in files:
            source = Path(current) / name
            relative = source.relative_to(ROOT)
            if not bootstrap.source_archive_path_is_excluded(relative):
                expected["rez-rs/" + relative.as_posix()] = source
    with zipfile.ZipFile(archive_path) as archive:
        names = archive.namelist()
        if len(names) != len(set(names)) or set(names) != set(expected):
            raise RuntimeError("Source archive membership differs from current sources")
        for name, source in expected.items():
            if archive.read(name) != source.read_bytes():
                raise RuntimeError(f"Source archive contains stale bytes: {name}")


def verify_cli(cli_zip: Path, version: str) -> None:
    """Install the extracted ZIP into an empty repository, then smoke-test the installed CLI."""
    with tempfile.TemporaryDirectory(prefix="rez-release-") as directory:
        root = Path(directory)
        bundle_dir = root / "bundle"
        with zipfile.ZipFile(cli_zip) as archive:
            if archive.testzip() is not None:
                raise RuntimeError("release ZIP CRC check failed")
            archive.extractall(bundle_dir)
        repository = root / "repository"
        repository.mkdir()
        env = isolated_env(root)
        host_path = env.get("PATH", "")
        # Not -I: that implies -P, and install.py imports its adjacent support modules.
        run([sys.executable, str(bundle_dir / "install.py"),
             "--repository-root", str(repository)], env)
        installed = [path for path in repository.rglob(EXE) if path.parent.name == version]
        if len(installed) != 1:
            raise RuntimeError(f"Expected one installed {EXE}, found {installed}")
        check_source_archive(installed[0].parent / "rez-rs.zip")

        # Relocated executable with an empty PATH: embedded runtime only.
        relocated = root / EXE
        shutil.copy2(installed[0], relocated)
        relocated.chmod(0o755)
        empty = root / "empty-path"
        empty.mkdir()
        env["PATH"] = str(empty)

        def rez(*args: str) -> str:
            return run([str(relocated), *args], env, cwd=root, capture_output=True,
                       text=True, encoding="utf-8", timeout=180).stdout

        if version not in rez("--version"):
            raise RuntimeError("CLI version differs from Cargo manifest")
        probe = ("import sys; sys.path[:]=[]; "
                 "import json, pathlib, re, zlib, ssl, tomllib; "
                 "assert json.__spec__.origin == 'frozen'; "
                 "assert re.fullmatch(r'[a-z]+', 'rez'); "
                 "assert zlib.decompress(zlib.compress(b'rez')) == b'rez'; "
                 "print('FROZEN_RUNTIME_OK')")
        if "FROZEN_RUNTIME_OK" not in rez("python", "-c", probe):
            raise RuntimeError("Embedded Python smoke test did not complete")
        config = root / "rezconfig.py"
        rez("--write-config", str(config))
        text = config.read_text(encoding="utf-8")
        ast.parse(text)
        if "# Package repository paths" not in text:
            raise RuntimeError("Generated configuration is missing comments")

        # Mandatory platform bindings must work without a host Python.
        minimal = root / "minimal-packages"
        rez("bind", "--quickstart", "--install-path", str(minimal))
        for family in ("platform", "arch", "os", "rez"):
            if not list((minimal / family).glob("*/package.py")):
                raise RuntimeError(f"Quickstart omitted mandatory {family} binding")
        # Resolution launches an OS shell: keep only its directory on PATH.
        if os.name == "nt":
            shell = env.get("COMSPEC") or str(Path(env.get("SYSTEMROOT", "C:/Windows")) / "System32" / "cmd.exe")
            shell_dir, shell_name = Path(shell).parent, "cmd"
        else:
            shell_dir, shell_name = Path(shutil.which("sh", path=host_path) or "/bin/sh").parent, "sh"
        env["PATH"] = os.pathsep.join([str(empty), str(shell_dir)])
        rez("env", "--paths", str(minimal), "--no-cache", "--shell", shell_name,
            "rez", "--", "rez", "--version")

        # With the host Python on PATH, quickstart must bind it and resolve it.
        env["PATH"] = host_path
        packages = root / "host-packages"
        rez("bind", "--quickstart", "--install-path", str(packages))
        if not list((packages / "python").glob("*/package.py")):
            raise RuntimeError("Quickstart did not bind the installed host Python")
        rez("env", "--paths", str(packages), "--no-cache",
            "--shell", "cmd" if os.name == "nt" else "bash",
            "python", "--", "python", "--version")
    print(f"Verified CLI release ZIP: {cli_zip}", flush=True)


def verify_python(python_zip: Path, version: str, pythons: list[str]) -> None:
    """Import the extracted distribution and run the API tests on every interpreter."""
    api = ROOT / "crates" / "rez" / "python-api"
    with zipfile.ZipFile(python_zip) as archive:
        if archive.testzip() is not None:
            raise RuntimeError("Python ZIP CRC check failed")
        for python in pythons:
            with tempfile.TemporaryDirectory(prefix="rez-python-release-") as directory:
                root = Path(directory)
                archive.extractall(root)
                tests = root / "test_api.py"
                shutil.copy2(api / "tests" / "test_api.py", tests)
                config = root / "rezconfig.py"
                config.write_text("implicit_packages = []\ncache_package_files = False\n"
                                  "cache_listdir = False\nresolve_caching = False\n"
                                  "warn_untimestamped = False\n", encoding="utf-8")
                env = isolated_env(root)
                env["REZ_CONFIG_FILE"] = str(config)
                code = (f"import sys, runpy; sys.path.insert(0, {str(root)!r}); "
                        f"import rez; assert rez.__version__ == {version!r}, rez.__version__; "
                        f"runpy.run_path({str(tests)!r}, run_name='__main__')")
                run([python, "-I", "-c", code], env, cwd=root, timeout=300)
    print(f"Verified Python release ZIP on {len(pythons)} interpreter(s): {python_zip}", flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("ci", "release"))
    parser.add_argument("--target", choices=TARGETS)
    parser.add_argument("--python", action="append", default=[],
                        help="extra CPython 3.10+ interpreter for the Python ZIP tests")
    args = parser.parse_args()
    host = rust_host()
    target = args.target or host
    if target not in TARGETS:
        parser.error(f"unsupported release host: {host}")
    # Builds are host-native: every release platform has a native CI runner.
    if target != host:
        parser.error(f"{target} requires a native runner, got {host}")

    version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
    if os.environ.get("GITHUB_REF_TYPE") == "tag":
        tag = os.environ.get("GITHUB_REF_NAME")
        if tag != f"v{version}":
            parser.error(f"release tag {tag!r} must match Cargo.toml: v{version}")

    if args.command == "ci":
        check_sources()
    cli_dir, python_dir = stage(version)
    cli_zip, python_zip = bundle(cli_dir, python_dir, version, TARGETS[target])
    verify_cli(cli_zip, version)
    verify_python(python_zip, version, [sys.executable, *args.python])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
