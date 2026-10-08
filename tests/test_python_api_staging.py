"""Python distribution publication preserves bytes and previous output on build failure."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zipfile

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("api_bootstrap", ROOT / "bootstrap.py")
bootstrap = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bootstrap)


class PythonApiStagingTests(unittest.TestCase):
    def test_stage_excludes_python_cache_and_archives_actual_extension_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources = root / "crates/rez/python-api/python/rez"
            sources.mkdir(parents=True)
            (sources / "__init__.py").write_text("from . import rs\n", encoding="utf-8")
            cache = sources / "__pycache__"
            cache.mkdir()
            (cache / "private.pyc").write_bytes(b"local cache")
            library_name = "rs.dll" if bootstrap.os.name == "nt" else (
                "librs.dylib" if bootstrap.sys.platform == "darwin" else "librs.so")
            library = root / "target/release" / library_name
            library.parent.mkdir(parents=True)
            library.write_bytes(b"compiled extension fixture")
            for name in ("LICENSE", "NOTICE"):
                (root / name).write_text(name, encoding="utf-8")
            with patch.object(bootstrap, "ROOT_DIR", root), patch.object(
                bootstrap, "DIST_DIR", root / "dist"
            ), patch.object(bootstrap, "run", return_value=0):
                self.assertEqual(bootstrap.stage_python_api("1.0"), 0)
            with zipfile.ZipFile(root / "dist/python/1.0/rez-rs-python.zip") as archive:
                self.assertFalse(any("__pycache__" in name for name in archive.namelist()))
                extension = "rez/rs.pyd" if bootstrap.os.name == "nt" else "rez/rs.abi3.so"
                self.assertEqual(archive.read(extension), library.read_bytes())
                self.assertEqual(archive.read("rez/__init__.py"), (sources / "__init__.py").read_bytes())

    def test_failed_extension_build_preserves_previous_python_distribution(self):
        with tempfile.TemporaryDirectory() as temporary:
            dist = Path(temporary)
            previous = dist / "python/1.0/rez/rs.pyd"
            previous.parent.mkdir(parents=True)
            previous.write_bytes(b"previous verified extension")
            with patch.object(bootstrap, "DIST_DIR", dist), patch.object(
                bootstrap, "run", return_value=101
            ):
                self.assertEqual(bootstrap.stage_python_api("1.0"), 101)
            self.assertEqual(previous.read_bytes(), b"previous verified extension")


if __name__ == "__main__":
    unittest.main()
