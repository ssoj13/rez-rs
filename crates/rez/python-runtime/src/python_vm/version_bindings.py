class _RequirementRange:
    def __init__(self, value):
        self._s = _rez_version_native("range", str(value), "")
    def __str__(self): return self._s
    def __contains__(self, value): return _rez_version_native("contains", self._s, str(value))
    def is_any(self): return _rez_version_native("any", self._s, "")
    def intersects(self, other): return _rez_version_native("intersects", self._s, str(other))
    def issuperset(self, other): return _rez_version_native("issuperset", self._s, str(other))
    def issubset(self, other): return _rez_version_native("issubset", self._s, str(other))
    def union(self, other): return _RequirementRange(_rez_version_native("union", self._s, str(other)))
    def intersection(self, other):
        result = _rez_version_native("intersection", self._s, str(other))
        return None if result is None else _RequirementRange(result)
    def __eq__(self, other): return isinstance(other, _RequirementRange) and self._s == other._s
    def __hash__(self): return hash(self._s)

class _Requirement:
    def __init__(self, value):
        data = _rez_version_native("requirement", str(value), "")
        self.name = data["name"]
        self.range = None if data["range"] is None else _RequirementRange(data["range"])
        self.conflict = data["conflict"]
        self.weak = data["weak"]
        self._s = data["string"]
        self._safe = data["safe"]
    def __str__(self): return self._s
    def __repr__(self): return "Requirement(%r)" % self._s
    def safe_str(self): return self._safe
    def conflicts_with(self, other): return _rez_version_native("conflicts", self._s, str(other))
    def merged(self, other):
        result = _rez_version_native("merged", self._s, str(other))
        return None if result is None else _Requirement(result)
    def __eq__(self, other): return isinstance(other, _Requirement) and self._s == other._s
    def __hash__(self): return hash(self._s)
