# SPDX-License-Identifier: Apache-2.0
"""Resolve and reconstruct environments through the canonical Rust context owner."""
from copy import deepcopy
import json
import os

from . import rs
from .config import config
from .packages import Package
from .status import ResolverStatus
from .version import Requirement


class ResolvedContext:
    def __init__(self, package_requests, verbosity=0, timestamp=None, building=False,
                 testing=False, caching=None, package_paths=None, package_filter=None,
                 package_orderers=None, max_fails=-1, add_implicit_packages=True,
                 time_limit=-1, callback=None, package_load_callback=None, buf=None,
                 suppress_passive=False, print_stats=False, package_caching=None,
                 package_cache_async=None):
        unsupported = {
            "testing": testing, "package_filter": package_filter,
            "package_orderers": package_orderers, "callback": callback,
            "package_load_callback": package_load_callback, "buf": buf,
            "suppress_passive": suppress_passive, "print_stats": print_stats,
            "package_caching": package_caching, "package_cache_async": package_cache_async,
        }
        pending = [name for name, value in unsupported.items()
                   if value is not None and (name in {"package_caching", "package_cache_async"} or value is not False)]
        if pending:
            raise NotImplementedError("Unsupported context options: " + ", ".join(pending))
        requests = [str(request) for request in package_requests]
        if isinstance(package_requests, (str, bytes)):
            raise TypeError("package_requests must be an iterable of requests, not a string")
        self._set_snapshot(rs.resolve_context(
            requests, package_paths, add_implicit_packages, building,
            config.resolve_caching if caching is None else caching,
            None if timestamp is None else int(timestamp), max_fails, time_limit, verbosity,
        ))

    def _set_snapshot(self, snapshot):
        self._snapshot = snapshot
        self._doc = snapshot["context"]

    success = property(lambda self: self.status is ResolverStatus.solved)
    status = property(lambda self: ResolverStatus[self._doc["status"]])
    failure_description = property(lambda self: self._doc["failure_description"])
    package_paths = property(lambda self: list(self._doc["package_paths"]))
    implicit_packages = property(lambda self: [Requirement(s) for s in self._doc["implicit_packages"]])
    solve_time = property(lambda self: self._doc["solve_time"])
    load_time = property(lambda self: self._doc["load_time"])
    from_cache = property(lambda self: self._doc["from_cache"])

    @property
    def resolved_packages(self):
        if not self.success:
            return None
        variants = []
        for record in self._snapshot["packages"]:
            variant = Package(record).get_variant(record["selected_index"])
            variant._record = dict(variant._record, root=record["selected_root"],
                                   handle=record["selected_handle"])
            variants.append(variant)
        return variants

    def requested_packages(self, include_implicit=False):
        result = [Requirement(s) for s in self._doc["package_requests"]]
        return result + self.implicit_packages if include_implicit else result

    def get_resolved_package(self, package_name):
        return next((p for p in self.resolved_packages or [] if p.name == package_name), None)

    def to_dict(self):
        return deepcopy(self._doc)

    def save(self, path):
        rs.context_save(json.dumps(self._doc), path)

    @classmethod
    def load(cls, path):
        context = cls.__new__(cls)
        context._set_snapshot(rs.context_load(path))
        return context

    @classmethod
    def get_current(cls):
        path = os.environ.get("REZ_RXT_FILE")
        return cls.load(path) if path else None

    def get_environ(self, parent_environ=None):
        return rs.context_environ(json.dumps(self._doc), parent_environ)

    def execute_command(self, args, parent_environ=None, **kwargs):
        raise NotImplementedError("Subprocess context execution is not supported by the facade yet")
