# SPDX-License-Identifier: Apache-2.0
"""Acceptance tests run against an extracted distribution, never source wrappers."""
import json
import os
from pathlib import Path
import tempfile
import unittest

import rez
from rez import rs
from rez.exceptions import PackageMetadataError, PackageNotFoundError, VersionError
from rez.packages import (
    get_developer_package, get_latest_package, get_package,
    get_variant, iter_package_families, iter_packages,
)
from rez.resolved_context import ResolvedContext
from rez.status import ResolverStatus
from rez.version import Requirement, Version, VersionRange, VersionedObject


class VersionTests(unittest.TestCase):
    def test_native_namespace_is_the_actual_extension(self):
        self.assertTrue(Path(rs.__file__).suffix in (".pyd", ".so"))
        self.assertEqual(rs.__name__, "rez.rs")
        self.assertEqual(rez.__version__, rs.__version__)
        self.assertEqual(rs.ABI_MINIMUM, "3.10")

    def test_comparison_separators_padding_and_large_tokens(self):
        self.assertEqual(Version("1.2"), Version("1-2"))
        self.assertEqual(hash(Version("1.2")), hash(Version("1-2")))
        self.assertLess(Version("01"), Version("1"))
        self.assertLess(Version("alpha2"), Version("alpha10"))
        self.assertLess(Version("9999999999999999999999"), Version("10000000000000000000000"))
        self.assertLess(Version("999"), Version.inf)

    def test_components_copy_trim_next_empty(self):
        version = Version("2.3.4")
        self.assertEqual(str(version.major), "2")
        self.assertEqual(str(version.minor), "3")
        self.assertEqual(str(version.patch), "4")
        self.assertEqual(version.as_tuple(), ("2", "3", "4"))
        self.assertEqual(str(version.trim(2)), "2.3")
        self.assertEqual(str(version.next()), "2.3.4_")
        self.assertIsNot(version.copy(), version)
        self.assertFalse(Version())
        self.assertEqual(Version().next(), Version.inf)
        with self.assertRaises(VersionError):
            Version("1..2")

    def test_ranges_membership_and_set_operations(self):
        range_ = VersionRange("1+<3")
        self.assertIn(Version("2"), range_)
        self.assertNotIn(Version("3"), range_)
        self.assertTrue(range_.issuperset(VersionRange("2")))
        self.assertEqual(str(range_ & VersionRange("2+<4")), "2+<3")
        self.assertIsNone(range_ & VersionRange("4"))
        self.assertIsNone(VersionRange().inverse())
        self.assertEqual(len(VersionRange("1|3").split()), 2)

    def test_requirements_and_versioned_objects(self):
        request = Requirement("example-1+<3")
        self.assertEqual(request.name, "example")
        self.assertFalse(request.conflict)
        self.assertTrue(request.conflicts_with(Requirement("example-4")))
        self.assertTrue(Requirement("~example-2").weak)
        obj = VersionedObject("example-2.0")
        self.assertEqual(obj.version, Version("2.0"))
        self.assertEqual(obj.as_exact_requirement(), "example==2.0")


class ConfigTests(unittest.TestCase):
    def test_read_only_configuration_does_not_silently_shadow_native_settings(self):
        from rez.config import config
        self.assertEqual(config.resolve_caching, rs.config_snapshot()["resolve_caching"])
        with self.assertRaises(NotImplementedError):
            config.packages_path = ["other"]
        with self.assertRaises(NotImplementedError):
            config.override("packages_path", ["other"])


class PackageContextTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="rez-api-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.repo = self.root / "packages"
        self.paths = [str(self.repo)]
        self.write_package("dependency", "1", "description = 'fixture dependency'\n")
        self.write_package("dependency", "2", "description = 'newer dependency'\n")
        self.write_package("application", "1", """requires = ['dependency-1']
variants = [['dependency-1']]
custom_value = 'retained'
def commands():
    env.REZRS_ACCEPTANCE.set('works')
    env.REZRS_ROOT.set('{root}')
    env.PATH.prepend('{root}/bin')
""")

    def write_package(self, name, version, extra):
        directory = self.repo / name / version
        directory.mkdir(parents=True)
        (directory / "package.py").write_text(
            f"name = {name!r}\nversion = {version!r}\n" + extra, encoding="utf-8")
        return directory

    def context(self, requests=None):
        return ResolvedContext(requests or ["application"], package_paths=self.paths,
                               add_implicit_packages=False, caching=False)

    def test_discovery_missing_packages_and_metadata(self):
        self.assertEqual({f.name for f in iter_package_families(self.paths)}, {"application", "dependency"})
        versions = [str(p.version) for p in iter_packages("dependency", paths=self.paths)]
        self.assertEqual(versions, ["2", "1"])
        self.assertEqual(str(get_latest_package("dependency", paths=self.paths).version), "2")
        package = get_package("application", "1", paths=self.paths)
        self.assertEqual(package.custom_value, "retained")
        self.assertEqual(package.base, str(self.repo / "application" / "1"))
        self.assertEqual([str(r) for r in package.requires], ["dependency-1"])
        self.assertIsNone(get_package("missing", "1", paths=self.paths))
        with self.assertRaises(PackageNotFoundError):
            get_latest_package("missing", paths=self.paths, error=True)

    def test_developer_package_uses_the_canonical_loader(self):
        path = self.root / "developer"
        path.mkdir()
        (path / "package.py").write_text("name = 'source'\nversion = '1.2'\n", encoding="utf-8")
        self.assertEqual(get_developer_package(path).qualified_name, "source-1.2")
        (path / "package.py").write_text("version = '1.2'\n", encoding="utf-8")
        with self.assertRaises(PackageMetadataError):
            get_developer_package(path)

    def test_selected_variant_and_exact_handle(self):
        context = self.context()
        self.assertTrue(context.success, context.failure_description)
        self.assertEqual(context.status, ResolverStatus.solved)
        selected = context.get_resolved_package("application")
        self.assertEqual(selected.index, 0)
        self.assertEqual(selected.parent.custom_value, "retained")
        self.assertEqual(get_variant(selected.handle).root, selected.root)
        self.assertEqual(str(context.get_resolved_package("dependency").version), "1")
        self.assertEqual([str(r) for r in context.requested_packages()], ["application"])

    def test_rex_environ_uses_parent_and_the_selected_root(self):
        context = self.context()
        selected = context.get_resolved_package("application")
        environ = context.get_environ({"PATH": "original", "CUSTOM_PARENT": "kept"})
        self.assertEqual(environ["REZRS_ACCEPTANCE"], "works")
        self.assertEqual(environ["REZRS_ROOT"].replace("\\", "/"), selected.root.replace("\\", "/"))
        self.assertTrue(environ["PATH"].replace("\\", "/").startswith(selected.root.replace("\\", "/") + "/bin"))
        self.assertEqual(context.get_environ()["REZRS_ACCEPTANCE"], "works")

    def test_context_rxt_roundtrip_is_compatible_with_canonical_handles(self):
        context = self.context()
        target = self.root / "context.rxt"
        context.save(target)
        loaded = ResolvedContext.load(target)
        self.assertTrue(loaded.success)
        self.assertEqual(loaded.to_dict()["resolved_packages"], context.to_dict()["resolved_packages"])
        self.assertEqual(loaded.get_environ()["REZRS_ACCEPTANCE"], "works")
        self.assertEqual(loaded.get_resolved_package("application").index, 0)

    def test_repository_priority_is_retained_in_context_handles(self):
        secondary = self.root / "secondary"
        directory = secondary / "dependency" / "1"
        directory.mkdir(parents=True)
        (directory / "package.py").write_text(
            "name = 'dependency'\nversion = '1'\ndescription = 'wrong repository'\n", encoding="utf-8")
        context = ResolvedContext(["dependency-1"], package_paths=[*self.paths, str(secondary)],
                                  add_implicit_packages=False, caching=False)
        target = self.root / "priority.rxt"
        context.save(target)
        loaded = ResolvedContext.load(target).get_resolved_package("dependency")
        self.assertEqual(loaded.parent.description, "fixture dependency")

    def test_failed_solve_remains_inspectable(self):
        context = self.context(["application", "dependency-2"])
        self.assertFalse(context.success)
        self.assertEqual(context.status, ResolverStatus.failed)
        self.assertIsNone(context.resolved_packages)
        self.assertTrue(context.failure_description)

    def test_unsupported_options_fail_explicitly(self):
        with self.assertRaises(NotImplementedError):
            ResolvedContext(["application"], package_paths=self.paths, callback=lambda state: None)
        with self.assertRaises(TypeError):
            ResolvedContext("application", package_paths=self.paths)


if __name__ == "__main__":
    unittest.main(argv=[__file__], verbosity=2)
