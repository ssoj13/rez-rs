#!/usr/bin/env python3
"""Check the staged source archive and a relocated CLI before uploading it."""
from __future__ import annotations

import argparse
import ast
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def run(executable: Path, args: list[str], cwd: Path, env: dict[str, str]) -> str:
    result = subprocess.run([str(executable), *args], cwd=cwd, env=env,
                            capture_output=True, text=True, encoding="utf-8", timeout=180)
    print(result.stdout, end="", flush=True)
    if result.returncode:
        raise RuntimeError(f"{args!r}: exit {result.returncode}: {result.stderr}")
    return result.stdout


def verify(package: Path, require_python: bool) -> None:
    manifest = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    version = manifest["package"]["version"]
    executable = package / ("rez.exe" if os.name == "nt" else "rez")
    if package.name != version:
        raise RuntimeError(f"Expected staged version {version}, got {package}")
    built = ROOT / "target" / "release" / executable.name
    if executable.read_bytes() != built.read_bytes():
        raise RuntimeError("Staged executable differs from Cargo output")
    spec = importlib.util.spec_from_file_location("rez_bootstrap", ROOT / "bootstrap.py")
    bootstrap = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(bootstrap)
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
    with zipfile.ZipFile(package / "rez-rs.zip") as archive:
        names = archive.namelist()
        if len(names) != len(set(names)) or set(names) != set(expected):
            raise RuntimeError("Source archive membership differs from current sources")
        if archive.testzip() is not None:
            raise RuntimeError("Source archive CRC check failed")
        for name, source in expected.items():
            if archive.read(name) != source.read_bytes():
                raise RuntimeError(f"Source archive contains stale bytes: {name}")
    for name, source in [("install.py", ROOT / "packaging" / "install.py"),
                         ("cli_install.py", ROOT / "cli_install.py"),
                         ("system_bind.py", ROOT / "system_bind.py")]:
        if (package.parent.parent / name).read_bytes() != source.read_bytes():
            raise RuntimeError(f"Staged helper differs from canonical source: {name}")
    metadata = {}
    exec(compile((package / "package.py").read_text(encoding="utf-8"), "package.py", "exec"),
         metadata)
    if metadata["name"] != "rez_rs" or metadata["version"] != version:
        raise RuntimeError("Staged package metadata does not match Cargo")

    with tempfile.TemporaryDirectory(prefix="rez-dist-") as temporary:
        isolated = Path(temporary)
        relocated = isolated / executable.name
        shutil.copy2(executable, relocated)
        # Preserve OS variables, but remove all inherited Rez/Python configuration.
        env = {key: value for key, value in os.environ.items()
               if not key.upper().startswith(("REZ_", "PYTHON", "RUSTPYTHON"))}
        env.update(HOME=str(isolated), USERPROFILE=str(isolated),
                   REZ_DISABLE_HOME_CONFIG="1", TMP=str(isolated), TEMP=str(isolated))
        empty_path = isolated / "empty-path"
        empty_path.mkdir()
        host_path = env.get("PATH", "")
        env["PATH"] = str(empty_path)
        output = run(relocated, ["--version"], isolated, env)
        if version not in output:
            raise RuntimeError("CLI version differs from Cargo manifest")
        probe = ("import sys; sys.path[:]=[]; "
                 "import json, pathlib, re, zlib, ssl, tomllib; "
                 "assert json.__spec__.origin == 'frozen'; "
                 "assert re.fullmatch(r'[a-z]+', 'rez'); "
                 "assert zlib.decompress(zlib.compress(b'rez')) == b'rez'; "
                 "print('FROZEN_RUNTIME_OK')")
        if "FROZEN_RUNTIME_OK" not in run(relocated, ["python", "-c", probe], isolated, env):
            raise RuntimeError("Embedded Python smoke test did not complete")
        config = isolated / "rezconfig.py"
        run(relocated, ["--write-config", str(config)], isolated, env)
        ast.parse(config.read_text(encoding="utf-8"))
        if "# Package repository paths" not in config.read_text(encoding="utf-8"):
            raise RuntimeError("Generated configuration is missing comments")

        # No host Python on PATH: the mandatory platform bindings must still work.
        minimal = isolated / "minimal-packages"
        run(relocated, ["bind", "--quickstart", "--install-path", str(minimal)], isolated, env)
        for family in ("platform", "arch", "os", "rez"):
            if not list((minimal / family).glob("*/package.py")):
                raise RuntimeError(f"Quickstart omitted mandatory {family} binding")

        # Resolution launches an OS shell; keep its directory, without host Python.
        if os.name == "nt":
            shell = env.get("COMSPEC") or str(Path(env.get("SYSTEMROOT", "C:/Windows")) / "System32" / "cmd.exe")
            shell_dir = Path(shell).parent
        else:
            shell_dir = Path(shutil.which("sh", path=host_path) or "/bin/sh").parent
        env["PATH"] = os.pathsep.join([str(empty_path), str(shell_dir)])
        run(relocated, ["env", "--paths", str(minimal), "--no-cache",
                        "--shell", "cmd" if os.name == "nt" else "sh",
                        "rez", "--", "rez", "--version"], isolated, env)

        bound_python = False
        if require_python:
            env["PATH"] = host_path
            packages = isolated / "host-packages"
            run(relocated, ["bind", "--quickstart", "--install-path", str(packages)],
                isolated, env)
            if not list((packages / "python").glob("*/package.py")):
                raise RuntimeError("Quickstart did not bind the installed host Python")
            run(relocated, ["env", "--paths", str(packages), "--no-cache",
                            "--shell", "cmd" if os.name == "nt" else "bash",
                            "python", "--", "python", "--version"], isolated, env)
            bound_python = True

    digest = hashlib.sha256(executable.read_bytes()).hexdigest()
    (package / (executable.name + ".sha256")).write_text(
        f"{digest}  {executable.name}\n", encoding="utf-8")
    receipt = {"version": version, "executable": executable.name, "sha256": digest,
               "source_members": len(expected), "platform": os.name,
               "relocated_runtime": True, "quickstart_without_host_python": True,
               "quickstart_with_host_python": bound_python,
               "commit": os.environ.get("GITHUB_SHA", "")}
    (package / "verification.json").write_text(json.dumps(receipt, indent=2) + "\n",
                                               encoding="utf-8")
    print(json.dumps(receipt, indent=2))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", type=Path)
    parser.add_argument("--require-python", action="store_true")
    args = parser.parse_args()
    version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
    package = args.package or ROOT / "dist" / "rez_rs" / version
    verify(package.resolve(), args.require_python)


if __name__ == "__main__":
    main()
