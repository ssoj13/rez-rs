"""Normalize Python packaging standards using the pip interpreter selected by Rez.

This helper never imports Rez or publishes packages. Rust owns installation,
repository transactions, policy, metadata and conversion into Rez requirements.
The selected pip supplies its own compatible, vendored PEP 440/508 parser.
"""
import json
import sys

from pip._vendor.packaging.markers import Marker, default_environment
from pip._vendor.packaging.requirements import InvalidRequirement, Requirement
from pip._vendor.packaging.specifiers import SpecifierSet
from pip._vendor.packaging.tags import parse_tag
from pip._vendor.packaging.utils import canonicalize_name
from pip._vendor.packaging.version import Version


def rez_version(value):
    version = Version(value)
    result = ".".join(str(part) for part in version.release)
    if version.pre:
        result += "." + version.pre[0] + str(version.pre[1])
    if version.post is not None:
        result += ".post" + str(version.post)
    if version.dev is not None:
        result += ".dev" + str(version.dev)
    if version.local:
        result += "-" + version.local
    return result


def specifiers(value):
    result = []
    for spec in sorted(value, key=str):
        wildcard = spec.version.endswith(".*")
        result.append({
            "operator": spec.operator,
            "version": rez_version(spec.version[:-2] if wildcard else spec.version),
            "wildcard": wildcard,
        })
    return result


def marker_names(value):
    names = set()

    def walk(node):
        if isinstance(node, (list, tuple)):
            for child in node:
                walk(child)
        elif node.__class__.__name__ == "Variable":
            names.add(node.value)

    if value is not None:
        walk(value._markers)
    return sorted(names)


with open(sys.argv[1], encoding="utf-8") as stream:
    data = json.load(stream)
environment = default_environment()
environment.update(data["environment"])
extras = {canonicalize_name(item["name"]): set() for item in data["distributions"]}
for source in data["sources"]:
    try:
        request = Requirement(source)
    except InvalidRequirement:
        continue  # Wheels, directories and archives are pip inputs, not requirements.
    extras.setdefault(canonicalize_name(request.name), set()).update(request.extras)
parsed = {
    canonicalize_name(item["name"]): [Requirement(value) for value in item["requires_dist"]]
    for item in data["distributions"]
}


def enabled(requirement, active_extras, portable=False):
    if requirement.marker is None:
        return True
    if portable:
        def possible(node, extra):
            if isinstance(node, tuple):
                variables = {part.value for part in node
                             if part.__class__.__name__ == "Variable"}
                if variables - {"extra"}:
                    return {False, True}
                atom = Marker(" ".join(part.serialize() for part in node))
                return {atom.evaluate(dict(environment, extra=extra))}
            groups = [{True}]
            for child in node:
                if child == "or":
                    groups.append({True})
                elif child != "and":
                    values = possible(child, extra)
                    groups[-1] = {left and right for left in groups[-1] for right in values}
            return set().union(*groups)

        # Only an inactive extras guard can prove a dependency absent for every
        # environment. Unknown environment atoms remain possible in either state.
        return any(True in possible(requirement.marker._markers, extra)
                   for extra in sorted(active_extras | {""}))
    return any(
        requirement.marker.evaluate(dict(environment, extra=extra))
        for extra in sorted(active_extras | {""})
    )


# Requested dependency extras are propagated through the installed dependency graph.
# Each iteration only adds elements to finite sets; evaluation reaches a fixed point.
while True:
    changed = False
    for owner, requirements in parsed.items():
        for requirement in requirements:
            if enabled(requirement, extras[owner]):
                target = canonicalize_name(requirement.name)
                if target in extras:
                    previous = len(extras[target])
                    extras[target].update(requirement.extras)
                    changed |= len(extras[target]) != previous
    if not changed:
        break

result = []
for item in data["distributions"]:
    owner = canonicalize_name(item["name"])
    requirements = []
    for requirement in parsed[owner]:
        requirements.append({
            "name": requirement.name,
            "url": requirement.url,
            "enabled": enabled(requirement, extras[owner]),
            "marker_names": marker_names(requirement.marker),
            "specifiers": specifiers(requirement.specifier),
        })
    portable = False
    python_specifiers = []
    if data.get("variant_policy") == "none":
        tags = set().union(*(parse_tag(value) for value in item.get("wheel_tags", [])))
        portable = bool(tags) and all(
            tag.interpreter.startswith("py") and tag.abi == "none" and tag.platform == "any"
            for tag in tags
        )
        portable &= all(
            not (set(marker_names(requirement.marker)) - {"extra"})
            or not enabled(requirement, extras[owner], portable=True)
            for requirement in parsed[owner]
        )
        python_specifiers = specifiers(SpecifierSet(item.get("requires_python", "")))
    result.append({
        "name": item["name"],
        "version": rez_version(item["version"]),
        "dependencies": requirements,
        "extras": sorted(extras[owner]),
        "python_specifiers": python_specifiers,
        "portable": portable,
    })
print(json.dumps(result))
