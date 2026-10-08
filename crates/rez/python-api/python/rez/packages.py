# SPDX-License-Identifier: Apache-2.0
"""Package discovery using the Rust repositories and exact resource provenance."""
from copy import deepcopy
import json

from . import rs
from .exceptions import PackageNotFoundError
from .version import Requirement, Version, VersionedObject


class Package:
    is_package = True
    is_variant = False

    def __init__(self, record):
        self._record = record

    name = property(lambda self: self._record["data"]["name"])
    version = property(lambda self: Version(self._record["data"].get("version", "")))
    qualified_name = property(lambda self: self._record["qualified_name"])
    base = property(lambda self: self._record["base"])
    uri = property(lambda self: self._record["source"] or self.base)
    data = property(lambda self: deepcopy(self._record["data"]))
    requires = property(lambda self: [Requirement(s) for s in self._record["data"].get("requires", [])])
    build_requires = property(lambda self: [Requirement(s) for s in self._record["data"].get("build_requires", [])])
    private_build_requires = property(lambda self: [Requirement(s) for s in self._record["data"].get("private_build_requires", [])])
    num_variants = property(lambda self: len(self._record["data"].get("variants", [])))

    def __getattr__(self, name):
        try:
            return deepcopy(self._record["data"][name])
        except KeyError:
            raise AttributeError(name) from None

    def __repr__(self):
        return f"Package({self.qualified_name!r})"

    def validated_data(self):
        return self.data

    def as_exact_requirement(self):
        return VersionedObject.construct(self.name, self.version).as_exact_requirement()

    def iter_variants(self):
        return (Variant(self, record) for record in self._record["variants"])

    def get_variant(self, index=None):
        return next((v for v in self.iter_variants() if v.index == index), None)


class DeveloperPackage(Package):
    """Metadata facade for a source package; build/re-evaluation APIs are pending."""


class Variant:
    is_package = False
    is_variant = True

    def __init__(self, parent, record):
        self.parent = parent
        self._record = record

    name = property(lambda self: self.parent.name)
    version = property(lambda self: self.parent.version)
    qualified_package_name = property(lambda self: self.parent.qualified_name)
    qualified_name = property(lambda self: self._record["qualified_name"])
    index = property(lambda self: self._record["index"])
    root = property(lambda self: self._record["root"])
    subpath = property(lambda self: self._record["subpath"])
    handle = property(lambda self: deepcopy(self._record["handle"]))
    requires = property(lambda self: [Requirement(s) for s in self._record["requires"]])

    def __getattr__(self, name):
        return getattr(self.parent, name)

    def __repr__(self):
        return f"Variant({self.qualified_name!r})"

    def get_requires(self, build_requires=False, private_build_requires=False):
        requires = self.requires
        if build_requires:
            requires += self.parent.build_requires
        if private_build_requires:
            requires += self.parent.private_build_requires
        return requires


class PackageFamily:
    def __init__(self, name, paths):
        self.name = name
        self._paths = paths

    def iter_packages(self):
        return iter_packages(self.name, paths=self._paths)


class PackageSearchPath:
    def __init__(self, paths):
        self.paths = list(paths)

    def iter_packages(self, name, range_=None):
        return iter_packages(name, range_, self.paths)


def iter_package_families(paths=None):
    return (PackageFamily(name, paths) for name in rs.package_families(paths))


def iter_packages(name, range_=None, paths=None):
    records = rs.packages(name, None if range_ is None else str(range_), paths)
    return (Package(record) for record in records)


def get_package(name, version, paths=None):
    wanted = Version(str(version))
    return next((p for p in iter_packages(name, paths=paths) if p.version == wanted), None)


def get_latest_package(name, range_=None, paths=None, error=False):
    package = next(iter_packages(name, range_, paths), None)
    if package is None and error:
        raise PackageNotFoundError(f"No matching package: {name}")
    return package


def get_package_from_string(txt, paths=None):
    obj = VersionedObject(txt)
    return get_package(obj.name, obj.version, paths)


def get_latest_package_from_string(txt, paths=None, error=False):
    request = Requirement(txt)
    if request.conflict:
        raise ValueError("A latest-package request must be positive")
    return get_latest_package(request.name, request.range, paths, error)


def get_developer_package(path, format=None):
    if format is not None:
        raise NotImplementedError("Explicit package format selection is not supported yet")
    return DeveloperPackage(rs.developer_package(path))


def get_variant(variant_handle, context=None):
    if context is not None:
        raise NotImplementedError("Context-bound deferred package attributes are not supported yet")
    handle = variant_handle.to_dict() if hasattr(variant_handle, "to_dict") else variant_handle
    package = Package(rs.package_from_handle(json.dumps(handle)))
    return package.get_variant(handle["variables"]["index"])
