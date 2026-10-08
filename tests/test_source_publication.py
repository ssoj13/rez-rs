"""Source export and explicit installer destination regressions."""
from __future__ import annotations

import contextlib
import importlib.util
import io
import os
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest.mock import Mock, patch
import zipfile


ROOT = Path(__file__).resolve().parents[1]


def load_module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


bootstrap = load_module("publication_bootstrap", ROOT / "bootstrap.py")
installer = load_module("publication_installer", ROOT / "packaging" / "install.py")


class SourcePublicationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def test_export_omits_environment_history_presets_and_generated_trees(self):
        source = self.root / "source"
        public = ["Cargo.toml", "LICENSE", "README.md", "docs/mdbook/src/installation.md",
                  "crates/patch/tests/vendor/launcher.exe",
                  "crates/patch/tests/object.obj", "tests/fixtures/packages/sample/package.py"]
        private = [".env", ".env.local", "nested/.env.production",
                   ".repl_history.txt", "examples/conan/CMakeUserPresets.json",
                   ".git/config", ".claude/notes.md", ".codex/state.json", ".omh/state.json",
                   ".mcp.json", ".idea/workspace.xml", ".vscode/settings.json",
                   "docs/build/index.html", "target/debug/rez", "dist/install.py",
                   "examples/scons/main.obj", "examples/scons/example.exe",
                   "examples/scons/.sconsign.dblite",
                   "examples/python/example.egg-info/PKG-INFO",
                   "examples/cmake/build/Makefile", "examples/cmake/CMakeFiles/state",
                   "packages/private-recipe/package.py"]
        for name in public + private:
            path = source / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(name, encoding="utf-8")
        archive_path = self.root / "source.zip"
        with patch.object(bootstrap, "ROOT_DIR", source):
            bootstrap.write_source_archive(archive_path)
        with zipfile.ZipFile(archive_path) as archive:
            self.assertEqual(set(archive.namelist()), {"rez-rs/" + name for name in public})
            self.assertIsNone(archive.testzip())
        installer.validate_source_archive(archive_path)

    def test_installer_rejects_excluded_source_material(self):
        for name in [".env", ".env.secret", ".repl_history.txt", ".claude/notes.md",
                     ".omh/state.json",
                     "examples/CMakeUserPresets.json", "docs/build/index.html",
                     "examples/scons/main.obj", "examples/scons/example.exe",
                     "examples/scons/.sconsign.dblite",
                     "examples/python/example.egg-info/PKG-INFO",
                     "examples/cmake/build/Makefile", "packages/private-recipe/package.py"]:
            with self.subTest(name=name):
                archive_path = self.root / "incoming.zip"
                with zipfile.ZipFile(archive_path, "w") as archive:
                    archive.writestr("rez-rs/Cargo.toml", "[package]")
                    archive.writestr("rez-rs/" + name, "local data")
                with self.assertRaisesRegex(installer.InstallError, "Generated"):
                    installer.validate_source_archive(archive_path)

    def test_repository_argument_is_required_before_any_activation(self):
        with patch.object(sys, "argv", ["install.py"]), patch.object(
            installer, "install_host"
        ) as activate, contextlib.redirect_stderr(io.StringIO()) as stderr:
            with self.assertRaises(SystemExit) as error:
                installer.main()
        self.assertEqual(error.exception.code, 2)
        self.assertIn("--repository-root", stderr.getvalue())
        activate.assert_not_called()

    def setup_payload(self):
        repository = self.root / "repository"
        repository.mkdir()
        payload = self.root / "payload"
        (payload / "0.1.0").mkdir(parents=True)
        return repository, payload

    def test_repository_only_install_does_not_activate_host(self):
        repository, payload = self.setup_payload()
        argv = ["install.py", "--repository-root", str(repository),
                "--payload-dir", str(payload)]
        with patch.object(sys, "argv", argv), patch.dict(os.environ, {}, clear=True), patch.object(
            installer, "install_package", return_value=repository / "installed"
        ) as install, patch.object(installer, "install_host") as activate:
            self.assertEqual(installer.main(), 0)
        install.assert_called_once_with(payload.resolve() / "0.1.0", repository.resolve())
        activate.assert_not_called()

    def test_repository_environment_default_and_explicit_cli_precedence(self):
        repository, payload = self.setup_payload()
        environment_repository = self.root / "environment-repository"
        environment_repository.mkdir()
        for explicit in (False, True):
            argv = ["install.py", "--payload-dir", str(payload)]
            if explicit:
                argv.extend(["--repository-root", str(repository)])
            with patch.object(sys, "argv", argv), patch.dict(
                os.environ, {"REZ_REPO_PATH": "$REZ_TEST_DESTINATION",
                             "REZ_TEST_DESTINATION": str(environment_repository)}, clear=True
            ), patch.object(installer, "install_package") as install:
                self.assertEqual(installer.main(), 0)
            expected = repository if explicit else environment_repository
            install.assert_called_once_with(payload.resolve() / "0.1.0", expected.resolve())

    def test_repository_environment_expands_user_home(self):
        repository, payload = self.setup_payload()
        with patch.object(sys, "argv", ["install.py", "--payload-dir", str(payload)]), patch.dict(
            os.environ, {"REZ_REPO_PATH": "~/repository", "USERPROFILE": str(self.root),
                         "HOME": str(self.root)}, clear=True
        ), patch.object(installer, "install_package") as install:
            self.assertEqual(installer.main(), 0)
        install.assert_called_once_with(payload.resolve() / "0.1.0", repository.resolve())

    def test_missing_repository_fails_before_install(self):
        _, payload = self.setup_payload()
        with patch.object(sys, "argv", ["install.py", "--payload-dir", str(payload)]), patch.dict(
            os.environ, {}, clear=True
        ), patch.object(installer, "install_package") as install, contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit) as error:
                installer.main()
        self.assertEqual(error.exception.code, 2)
        install.assert_not_called()

    def test_activation_uses_cli_configuration_without_neighbour_bootstrap(self):
        repository, payload = self.setup_payload()
        host = self.root / "host"
        host.mkdir()
        destination = repository / "installed"
        refresh = Mock(return_value={"changed": []})
        support = types.SimpleNamespace(BindError=RuntimeError, refresh_system_bindings=refresh)
        argv = ["install.py", "--repository-root", str(repository),
                "--payload-dir", str(payload), "--rez-root", str(host)]
        with patch.object(sys, "argv", argv), patch.dict(os.environ, {}, clear=True), patch.object(
            installer, "install_package", return_value=destination
        ), patch.object(installer, "install_host", return_value=host / "cli") as activate, patch.dict(
            sys.modules, {"system_bind": support}
        ), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(installer.main(), 0)
        self.assertEqual(activate.call_count, 2)
        refresh.assert_called_once_with(host / "cli" / installer.TOOL_BINARY)


if __name__ == "__main__":
    unittest.main()
