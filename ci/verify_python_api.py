#!/usr/bin/env python3
"""Verify exported Python bytes and run API acceptance in an isolated CPython process."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def verify(package: Path) -> dict:
    metadata = json.loads((package / "python-api.json").read_text(encoding="utf-8"))
    extension = package / "rez" / metadata["extension"]
    library_name = "rs.dll" if os.name == "nt" else ("librs.dylib" if sys.platform == "darwin" else "librs.so")
    if extension.read_bytes() != (ROOT / "target" / "release" / library_name).read_bytes():
        raise RuntimeError("Staged Python extension differs from Cargo output")
    expected = {"rez/" + p.name: p for p in
                (ROOT / "crates" / "rez" / "python-api" / "python" / "rez").glob("*.py")}
    expected.update({"rez/" + extension.name: extension, "LICENSE": ROOT / "LICENSE",
                     "NOTICE": ROOT / "NOTICE", "python-api.json": package / "python-api.json"})
    expected.update({"third-party-licenses/" + p.name: p for p in
                     (ROOT / "crates" / "rez" / "python-api" / "licenses").glob("*")})
    archive_path = package / "rez-rs-python.zip"
    with zipfile.ZipFile(archive_path) as archive:
        names = archive.namelist()
        if len(names) != len(set(names)) or set(names) != set(expected):
            raise RuntimeError("Python archive membership differs from the expected distribution")
        if archive.testzip() is not None:
            raise RuntimeError("Python archive CRC check failed")
        for name, source in expected.items():
            if (package / name).read_bytes() != source.read_bytes():
                raise RuntimeError(f"Staged Python package contains stale bytes: {name}")
            if archive.read(name) != source.read_bytes():
                raise RuntimeError(f"Python archive contains stale bytes: {name}")
        with tempfile.TemporaryDirectory(prefix="rez-python-dist-") as temporary:
            isolated = Path(temporary)
            archive.extractall(isolated)  # All members were matched against canonical safe names.
            tests = isolated / "test_api.py"
            shutil.copy2(ROOT / "crates" / "rez" / "python-api" / "tests" / "test_api.py", tests)
            config = isolated / "rezconfig.py"
            config.write_text("implicit_packages = []\ncache_package_files = False\ncache_listdir = False\n"
                              "resolve_caching = False\nwarn_untimestamped = False\n", encoding="utf-8")
            env = {key: value for key, value in os.environ.items()
                   if not key.upper().startswith(("REZ_", "PYTHON", "RUSTPYTHON"))}
            env.update(REZ_DISABLE_HOME_CONFIG="1", REZ_CONFIG_FILE=str(config))
            code = ("import sys,runpy; sys.path.insert(0," + repr(str(isolated)) + "); "
                    "import rez; assert rez.__version__ == " + repr(metadata["version"]) + "; "
                    "runpy.run_path(" + repr(str(tests)) + ",run_name='__main__')")
            result = subprocess.run([sys.executable, "-I", "-c", code], cwd=isolated,
                                    env=env, timeout=300)
            if result.returncode:
                raise RuntimeError(f"Extracted CPython API tests failed: exit {result.returncode}")
    digest = hashlib.sha256(archive_path.read_bytes()).hexdigest()
    (package / "rez-rs-python.zip.sha256").write_text(
        f"{digest}  rez-rs-python.zip\n", encoding="utf-8")
    versions = []
    receipt_file = package / "python-verification.json"
    if receipt_file.is_file():
        previous = json.loads(receipt_file.read_text(encoding="utf-8"))
        if previous.get("sha256") == digest:
            versions = previous.get("tested_pythons", [])
    versions = sorted(set([*versions, sys.version.split()[0]]))
    receipt = dict(metadata, tested_python=sys.version.split()[0], tested_pythons=versions, sha256=digest,
                   extension_sha256=hashlib.sha256(extension.read_bytes()).hexdigest(),
                   archive_members=len(expected), relocated_import=True)
    (package / "python-verification.json").write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(receipt, indent=2))
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", type=Path)
    args = parser.parse_args()
    if args.package is None:
        versions = [p for p in (ROOT / "dist" / "python").iterdir() if p.is_dir() and not p.name.startswith(".")]
        if len(versions) != 1:
            parser.error("Pass --package when more than one Python distribution is staged")
        package = versions[0]
    else:
        package = args.package
    verify(package.resolve())


if __name__ == "__main__":
    main()
