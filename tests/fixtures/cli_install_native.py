"""Qualify the shared installer with an actual native rez executable, in isolation."""
from __future__ import annotations

import importlib.util
import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("rez_cli_install", ROOT / "cli_install.py")
installer = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(installer)


def main() -> None:
    executable = Path(sys.argv[1]).resolve()
    with tempfile.TemporaryDirectory(prefix="rez-native-alias-") as temporary:
        root = Path(temporary)
        source = root / "source"
        source.mkdir()
        incoming = source / ("rez.exe" if executable.suffix.lower() == ".exe" else "rez")
        shutil.copy2(executable, incoming)
        archive = source / "rez-rs.zip"
        archive.write_bytes(b"isolated transaction fixture")
        destination = root / "Scripts" / "rez"
        payloads = {incoming.name: incoming, archive.name: archive}
        installer.install_cli(incoming, destination, payloads=payloads)
        metadata = json.loads((destination / installer.MARKER).read_text(encoding="utf-8"))
        names = sorted(metadata["hashes"])
        before = installer.file_hashes(destination, [*names, installer.MARKER])
        installer.install_cli(incoming, destination, payloads=payloads)
        assert installer.file_hashes(destination, before) == before
        suffix = incoming.suffix
        pip = subprocess.run(
            [str(destination / ("rez-pip" + suffix)), "--help"],
            capture_output=True, text=True, encoding="utf-8", timeout=60,
        )
        assert pip.returncode == 0, pip.stderr
        assert "pip" in pip.stdout.lower(), pip.stdout
        python = subprocess.run(
            [str(destination / ("rez-python" + suffix)), "-I", "-S", "-c",
             "import subprocess,sys; assert sys.flags.isolated; assert sys.flags.no_site; "
             "assert sys.argv[1:]==['--version','two words']; "
             "assert subprocess.run([sys.executable,'-I','-S','-c','raise SystemExit(7)']).returncode==7; "
             "print('INSTALLED_NATIVE_PYTHON_OK')",
             "--version", "two words"],
            capture_output=True, text=True, encoding="utf-8", timeout=90,
        )
        assert python.returncode == 0, python.stderr
        assert "INSTALLED_NATIVE_PYTHON_OK" in python.stdout, python.stdout
        assert not (destination / ".rez-rs-activation").exists()
        print("NATIVE_CLI_INSTALL_OK " + json.dumps({
            "aliases": len(names) - len(payloads), "idempotent": True,
            "pip_help": True, "native_python": True, "nested_python_exit": 7,
        }, sort_keys=True))


if __name__ == "__main__":
    main()
