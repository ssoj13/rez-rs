# SPDX-License-Identifier: Apache-2.0
"""Rez version primitives delegated to the canonical Rust version crate."""
from functools import total_ordering

from . import rs
from .exceptions import VersionError

ParseException = VersionError


@total_ordering
class AlphanumericVersionToken:
    def __init__(self, token):
        self._text = str(token)
        info = rs.version_info(self._text)
        if len(info["tokens"]) != 1 or info["is_inf"]:
            raise VersionError("Expected one alphanumeric version token")

    def __str__(self):
        return self._text

    def __repr__(self):
        return f"{type(self).__name__}({self._text!r})"

    def __eq__(self, other):
        if not isinstance(other, AlphanumericVersionToken):
            return NotImplemented
        return rs.version_compare(self._text, other._text) == 0

    def __lt__(self, other):
        if not isinstance(other, AlphanumericVersionToken):
            return NotImplemented
        return rs.version_compare(self._text, other._text) < 0

    def __hash__(self):
        return hash(self._text)

    def next(self):
        return type(self)(rs.version_transform(self._text, "next"))


@total_ordering
class Version:
    def __init__(self, ver_str="", make_token=AlphanumericVersionToken):
        if make_token is not AlphanumericVersionToken:
            raise NotImplementedError("Custom token factories are not supported yet")
        info = rs.version_info("[INF]" if ver_str is None else str(ver_str))
        self._text = info["text"]
        self._tokens = tuple(info["tokens"])
        self._is_inf = info["is_inf"]

    def __str__(self):
        return self._text

    def __repr__(self):
        return f"Version({self._text!r})"

    def __eq__(self, other):
        if not isinstance(other, Version):
            return NotImplemented
        return rs.version_compare(self._text, other._text) == 0

    def __lt__(self, other):
        if not isinstance(other, Version):
            return NotImplemented
        return rs.version_compare(self._text, other._text) < 0

    def __hash__(self):
        return hash((self._is_inf, self._tokens))

    def __bool__(self):
        return self._is_inf or bool(self._tokens)

    def __len__(self):
        return 0 if self._is_inf else len(self._tokens)

    def __getitem__(self, index):
        return AlphanumericVersionToken(self._tokens[index])

    def copy(self):
        return Version(self._text)

    def trim(self, len_):
        return Version(rs.version_transform(self._text, "trim", len_))

    def next(self):
        return Version(rs.version_transform(self._text, "next"))

    def as_tuple(self):
        return self._tokens

    def _component(self, index):
        return self[index] if len(self) > index else None

    major = property(lambda self: self._component(0))
    minor = property(lambda self: self._component(1))
    patch = property(lambda self: self._component(2))


Version.inf = Version("[INF]")


class VersionRange:
    def __init__(self, range_str="", make_token=AlphanumericVersionToken, invalid_bound_error=True):
        if make_token is not AlphanumericVersionToken or not invalid_bound_error:
            raise NotImplementedError("Custom range parsers are not supported yet")
        if range_str is None:
            raise TypeError("Use None to represent an empty range result")
        self._info = rs.range_info(str(range_str))
        self._text = self._info["text"]

    def __str__(self):
        return self._text

    def __repr__(self):
        return f"VersionRange({self._text!r})"

    def __eq__(self, other):
        if not isinstance(other, VersionRange):
            return NotImplemented
        return self._text == other._text

    def __hash__(self):
        return hash(self._text)

    def __len__(self):
        return self._info["bounds"]

    def __contains__(self, item):
        if isinstance(item, VersionRange):
            return self.issuperset(item)
        return self.contains_version(item)

    def _op(self, operation, other=""):
        return rs.range_operation(self._text, operation, str(other))

    def contains_version(self, version):
        return self._op("contains", version)

    def is_any(self):
        return self._info["is_any"]

    def lower_bounded(self):
        return self._info["lower_bounded"]

    def upper_bounded(self):
        return self._info["upper_bounded"]

    def bounded(self):
        return self.lower_bounded() and self.upper_bounded()

    def issuperset(self, other):
        return self._op("issuperset", other)

    def issubset(self, other):
        return self._op("issubset", other)

    def intersects(self, other):
        return self._op("intersects", other)

    @staticmethod
    def _result(value):
        return None if value is None else VersionRange(value)

    def intersection(self, other):
        return self._result(self._op("intersection", other))

    def union(self, other):
        return self._result(self._op("union", other))

    def inverse(self):
        return self._result(self._op("inverse"))

    def split(self):
        return [VersionRange(value) for value in self._op("split")]

    def span(self):
        return VersionRange(self._op("span"))

    __and__ = intersection
    __or__ = union
    __invert__ = inverse


class Requirement:
    def __init__(self, s, invalid_bound_error=True):
        if not invalid_bound_error:
            raise NotImplementedError("Permissive bound parsing is not supported yet")
        self._info = rs.requirement_info(str(s))

    name = property(lambda self: self._info["name"])
    conflict = property(lambda self: self._info["conflict"])
    weak = property(lambda self: self._info["weak"])
    range = property(lambda self: VersionRange._result(self._info["range"]))

    def __str__(self):
        return self._info["text"]

    def __repr__(self):
        return f"Requirement({str(self)!r})"

    def __eq__(self, other):
        if not isinstance(other, Requirement):
            return NotImplemented
        return (self.name, self.range, self.conflict) == (other.name, other.range, other.conflict)

    def __hash__(self):
        return hash((self.name, self.range, self.conflict))

    def conflicts_with(self, other):
        return rs.requirement_conflicts(str(self), str(other))

    def safe_str(self):
        return str(self)


class VersionedObject:
    def __init__(self, s):
        self._info = rs.versioned_info(str(s))

    name = property(lambda self: self._info["name"])
    version = property(lambda self: Version(self._info["version"]))

    def __str__(self):
        return self._info["text"]

    def __repr__(self):
        return f"VersionedObject({str(self)!r})"

    def __eq__(self, other):
        if not isinstance(other, VersionedObject):
            return NotImplemented
        return (self.name, self.version) == (other.name, other.version)

    def __hash__(self):
        return hash((self.name, self.version))

    def as_exact_requirement(self):
        return self._info["exact_requirement"]

    @classmethod
    def construct(cls, name, version=None):
        return cls(name if version is None or not version else f"{name}-{version}")
