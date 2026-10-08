"""Cargo destination precedence and native alias destination stay identical."""
import argparse
import importlib.util
import os
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("rez_bootstrap", ROOT / "bootstrap.py")
bootstrap = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bootstrap)


class BootstrapInstallTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.directory = Path(self.tmp.name) / "project"
        self.directory.mkdir()
        self.cargo_home = Path(self.tmp.name) / "cargo-home"
        self.cargo_home.mkdir()
        self.run = Mock(return_value=0)
        self.install = Mock()
        self.support = types.SimpleNamespace(InstallError=RuntimeError, install_cli=self.install)
        for manager in [
            patch.object(bootstrap, "ROOT_DIR", self.directory),
            patch.object(bootstrap, "run", self.run),
            patch.dict(os.environ, {"CARGO_HOME": str(self.cargo_home)}, clear=True),
            patch.dict("sys.modules", {"cli_install": self.support}),
        ]:
            manager.start()
            self.addCleanup(manager.stop)

    def config(self, directory, content, filename="config.toml"):
        directory.mkdir(parents=True, exist_ok=True)
        (directory / filename).write_text(content, encoding="utf-8")

    def invoke(self, root=None):
        return bootstrap.run_install(argparse.Namespace(root=root, force=False))

    def destination(self, expected):
        command = self.run.call_args.args[0]
        self.assertEqual(Path(command[command.index("--root") + 1]), expected)
        binary = expected / "bin" / ("rez.exe" if os.name == "nt" else "rez")
        self.install.assert_called_once_with(binary, binary.parent)

    def test_default_cargo_home(self):
        self.assertEqual(self.invoke(), 0)
        self.destination(self.cargo_home)

    def test_explicit_root_overrides_environment_and_config(self):
        self.config(self.directory / ".cargo", '[install]\nroot="configured"\n')
        with patch.dict(os.environ, {"CARGO_INSTALL_ROOT": "environment"}):
            self.assertEqual(self.invoke(Path("explicit")), 0)
        self.destination(self.directory / "explicit")

    def test_environment_overrides_config(self):
        self.config(self.directory / ".cargo", '[install]\nroot="configured"\n')
        with patch.dict(os.environ, {"CARGO_INSTALL_ROOT": "environment"}):
            self.assertEqual(self.invoke(), 0)
        self.destination(self.directory / "environment")

    def test_deepest_config_overrides_parent_and_home(self):
        self.config(self.cargo_home, '[install]\nroot="home"\n')
        self.config(self.directory.parent / ".cargo", '[install]\nroot="parent"\n')
        self.config(self.directory / ".cargo", '[install]\nroot="project"\n')
        self.assertEqual(self.invoke(), 0)
        self.destination(self.directory / "project")

    def test_parent_config_root_is_relative_to_config_parent(self):
        self.config(self.directory.parent / ".cargo", '[install]\nroot="parent"\n')
        self.assertEqual(self.invoke(), 0)
        self.destination(self.directory.parent / "parent")

    def test_extensionless_config_precedes_toml(self):
        self.config(self.directory / ".cargo", '[install]\nroot="toml"\n')
        self.config(self.directory / ".cargo", '[install]\nroot="legacy"\n', filename="config")
        self.assertEqual(self.invoke(), 0)
        self.destination(self.directory / "legacy")

    def test_unknown_config_include_fails_before_cargo(self):
        self.config(self.directory / ".cargo", 'include=["other.toml"]\n')
        self.assertEqual(self.invoke(), 1)
        self.run.assert_not_called()
        self.install.assert_not_called()

    def test_invalid_config_root_fails_before_cargo(self):
        self.config(self.directory / ".cargo", '[install]\nroot=42\n')
        self.assertEqual(self.invoke(), 1)
        self.run.assert_not_called()

    def test_python_without_tomllib_requests_explicit_root_before_cargo(self):
        self.config(self.directory / ".cargo", '[install]\nroot="configured"\n')
        with patch.dict("sys.modules", {"tomllib": None}):
            self.assertEqual(self.invoke(), 1)
        self.run.assert_not_called()
        self.install.assert_not_called()

    def test_explicit_root_does_not_require_config_parser(self):
        self.config(self.directory / ".cargo", '[install]\nroot="configured"\n')
        with patch.dict("sys.modules", {"tomllib": None}):
            self.assertEqual(self.invoke(Path("explicit")), 0)
        self.destination(self.directory / "explicit")

    def test_cargo_failure_does_not_install_aliases(self):
        self.run.return_value = 11
        self.assertEqual(self.invoke(), 11)
        self.install.assert_not_called()


if __name__ == "__main__":
    unittest.main()
