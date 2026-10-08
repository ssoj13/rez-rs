"""Small-file transaction regressions; no native build or external installation."""
from __future__ import annotations

import hashlib
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import cli_install


class CliInstallTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="rez-cli-contract-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "source"
        self.source.mkdir()
        self.directory = self.root / "target"
        self.primary = "rez.exe" if os.name == "nt" else "rez"
        self.alias = "rez-pip.exe" if os.name == "nt" else "rez-pip"
        self.executable = self.source / self.primary
        self.executable.write_bytes(b"old executable")
        self.archive = self.source / "rez-rs.zip"
        self.archive.write_bytes(b"source archive")
        self.metadata = patch.object(cli_install, "aliases", return_value=[self.alias])
        self.metadata.start()
        self.addCleanup(self.metadata.stop)

    def install(self, archive=False):
        payloads = {self.primary: self.executable}
        if archive:
            payloads["rez-rs.zip"] = self.archive
        return cli_install.install_cli(self.executable, self.directory, payloads=payloads)

    def ownership(self):
        return json.loads((self.directory / cli_install.MARKER).read_text())["hashes"]

    def test_manual_primary_refresh_preserves_owned_archive_and_is_idempotent(self):
        self.install(archive=True)
        self.executable.write_bytes(b"new executable")
        (self.directory / self.primary).write_bytes(b"new executable")
        self.install()
        self.assertEqual((self.directory / self.alias).read_bytes(), b"new executable")
        self.assertEqual((self.directory / "rez-rs.zip").read_bytes(), b"source archive")
        self.assertEqual(self.ownership()["rez-rs.zip"], hashlib.sha256(b"source archive").hexdigest())
        marker = self.directory / cli_install.MARKER
        before = marker.stat().st_mtime_ns
        self.install()
        self.assertEqual(marker.stat().st_mtime_ns, before)
        self.assertFalse((self.directory / ".rez-rs-activation").exists())

    def test_alias_only_refresh_retains_owned_primary_and_archive(self):
        self.install(archive=True)
        cli_install.install_cli(self.executable, self.directory)
        self.assertIn(self.primary, self.ownership())
        self.assertIn("rez-rs.zip", self.ownership())
        self.assertEqual((self.directory / self.primary).read_bytes(), b"old executable")

    def test_manual_primary_does_not_allow_changed_alias_or_archive(self):
        for changed in (self.alias, "rez-rs.zip"):
            with self.subTest(changed=changed):
                self.directory = self.root / changed.replace(".", "_")
                self.install(archive=True)
                self.executable.write_bytes(b"new executable")
                (self.directory / self.primary).write_bytes(b"new executable")
                (self.directory / changed).write_bytes(b"independent change")
                with self.assertRaises(cli_install.InstallError):
                    self.install()
                self.assertEqual((self.directory / changed).read_bytes(), b"independent change")
                self.executable.write_bytes(b"old executable")

    def test_foreign_alias_is_never_adopted(self):
        self.directory.mkdir()
        alias = self.directory / self.alias
        alias.write_bytes(b"foreign alias")
        with self.assertRaises(cli_install.InstallError):
            self.install()
        self.assertEqual(alias.read_bytes(), b"foreign alias")
        self.assertFalse((self.directory / cli_install.MARKER).exists())

    def test_manual_primary_first_install_without_ownership_marker(self):
        self.directory.mkdir()
        (self.directory / self.primary).write_bytes(self.executable.read_bytes())
        self.install()
        self.assertEqual(self.ownership()[self.alias], self.ownership()[self.primary])

    def test_schema_two_recovery_accepts_compact_native_ownership_marker(self):
        self.install(archive=True)
        previous = cli_install.file_hashes(
            self.directory, [*self.ownership(), cli_install.MARKER],
        )
        identity = hashlib.sha256(json.dumps(previous, sort_keys=True).encode()).hexdigest()
        backup = self.directory / ".rez-rs-backups" / identity
        backup.mkdir()
        for name in previous:
            (backup / name).write_bytes((self.directory / name).read_bytes())
        new_hashes = dict(sorted(self.ownership().items()))
        new_hashes[self.primary] = hashlib.sha256(b"new executable").hexdigest()
        new_hashes[self.alias] = new_hashes[self.primary]
        native_marker = (
            json.dumps({"schema": 1, "hashes": new_hashes}, separators=(",", ":")) + "\n"
        ).encode()
        incoming = {**new_hashes, cli_install.MARKER: hashlib.sha256(native_marker).hexdigest()}
        transaction = self.directory / ".rez-rs-activation"
        transaction.mkdir()
        (transaction / "journal.json").write_text(json.dumps({
            "schema": 2, "previous": previous, "incoming": incoming,
        }))
        (self.directory / self.primary).write_bytes(b"new executable")
        (self.directory / cli_install.MARKER).write_bytes(native_marker)
        cli_install.install_cli(None, self.directory, recover_only=True)
        self.assertEqual(cli_install.file_hashes(self.directory, previous), previous)
        self.assertFalse(transaction.exists())

    def test_native_delegation_uses_advertised_deploy_and_has_no_fallback_on_failure(self):
        suffix = ".exe" if os.name == "nt" else ""
        with patch.object(cli_install, "aliases", return_value=["rez-deploy" + suffix]), patch.object(
            cli_install.subprocess, "run"
        ) as run:
            run.return_value.returncode = 0
            cli_install.install_cli(
                self.executable, self.directory,
                payloads={self.primary: self.executable, "rez-rs.zip": self.archive},
            )
            argv = run.call_args.args[0]
            self.assertEqual(argv[:2], [str(self.executable), "deploy"])
            self.assertIn("--bin-dir", argv)
            self.assertIn("--source-zip", argv)
            self.assertNotIn("--aliases-only", argv)
            self.assertFalse(self.directory.exists())
            run.return_value.returncode = 1
            run.return_value.stderr = "activation refused"
            with self.assertRaisesRegex(cli_install.InstallError, "activation refused"):
                self.install()
            self.assertFalse(self.directory.exists())


if __name__ == "__main__":
    unittest.main()
