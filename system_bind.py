#!/usr/bin/env python3
"""Refresh only host platform/arch/os definitions using the activated Rez CLI.

Generation uses the canonical native bind implementation in a temporary repository.
Existing literal extension metadata is retained; custom callbacks, variants and
ambiguous definitions fail closed. Persistent repository locks coordinate with
native writers. Every changed definition has an immutable backup and hash receipt.
"""
from __future__ import annotations

import ast
from contextlib import ExitStack, contextmanager
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

FAMILIES = ("platform", "arch", "os")
MANAGED = frozenset({"name", "version", "description", "requires", "tools", "commands"})
# Reviewed 2026-10-02: Bootstrap117/0010.prep/1050.rez_git/src/rez/bind/platform.py.
# Only this exact legacy callback may migrate to the native shared OS baseline.
LEGACY_PLATFORM_COMMANDS = "088b814075289f915fcfd91229bdfc199ac9d8453c1a81c06304447466b20980"


class BindError(RuntimeError):
    """The configured destination or an existing definition is unsafe."""


def _safe(path: Path, root: Path) -> None:
    if not path.absolute().is_relative_to(root.absolute()):
        raise BindError(f"Path escapes system repository: {path}")
    current = path
    while True:
        if current.is_symlink() or getattr(current, "is_junction", lambda: False)():
            raise BindError(f"Redirected system-bind path: {current}")
        if current == root:
            break
        current = current.parent


def _definition(source: str, filename: Path) -> tuple[dict, dict, list[str]]:
    tree = ast.parse(source, filename=str(filename))
    values, callbacks, extensions = {}, {}, []
    for node in tree.body:
        if isinstance(node, ast.Expr) and isinstance(node.value, ast.Constant) and isinstance(node.value.value, str):
            continue
        if isinstance(node, ast.Assign) and len(node.targets) == 1 and isinstance(node.targets[0], ast.Name):
            key = node.targets[0].id
            if key in values or key in callbacks:
                raise BindError(f"Duplicate package attribute {key}: {filename}")
            try:
                values[key] = ast.literal_eval(node.value)
            except (ValueError, TypeError) as error:
                raise BindError(f"Dynamic package attribute {key}: {filename}") from error
            if key not in MANAGED:
                extensions.append(ast.get_source_segment(source, node))
        elif isinstance(node, ast.FunctionDef) and node.name == "commands" and not node.decorator_list:
            if node.name in callbacks or node.name in values:
                raise BindError(f"Duplicate commands: {filename}")
            if node.args.posonlyargs or node.args.args or node.args.kwonlyargs or node.args.vararg or node.args.kwarg:
                raise BindError(f"Custom commands signature requires review: {filename}")
            body = "\n".join(ast.dump(statement, include_attributes=False) for statement in node.body)
            callbacks[node.name] = hashlib.sha256(body.encode("utf-8")).hexdigest()
        else:
            raise BindError(f"Custom package lifecycle requires review: {filename}:{node.lineno}")
    if values.get("variants") or values.get("tools"):
        raise BindError(f"System package has variants or tools: {filename}")
    return values, callbacks, extensions


@contextmanager
def _lock(path: Path):
    # A byte-range lock overlaps the native whole-file lock on Windows.
    with path.open("a+b") as stream:
        stream.seek(0)
        try:
            if os.name == "nt":
                import msvcrt
                msvcrt.locking(stream.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl
                fcntl.flock(stream.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as error:
            raise BindError(f"Repository writer owns {path}") from error
        try:
            yield
        finally:
            stream.seek(0)
            if os.name == "nt":
                msvcrt.locking(stream.fileno(), msvcrt.LK_UNLCK, 1)
            else:
                fcntl.flock(stream.fileno(), fcntl.LOCK_UN)


def _run(executable: Path, args: list[str], env: dict[str, str]) -> str:
    result = subprocess.run(
        [str(executable), *args], env=env, capture_output=True,
        text=True, encoding="utf-8", timeout=60, check=False,
    )
    if result.returncode:
        raise BindError(f"Rez {' '.join(args[:2])} failed ({result.returncode}): {result.stderr[-2000:]}")
    return result.stdout


def refresh_system_bindings(
    executable: Path, *, env: dict[str, str] | None = None,
    config_fallback: Path | None = None, repository: Path | None = None,
) -> dict:
    """Generate and safely publish the three system packages; return a receipt.

    An explicit repository supports controlled maintenance. Normal bootstrap
    queries release_packages_path from the effective Rez config.
    """
    executable = executable.absolute()
    environment = dict(os.environ if env is None else env)
    configs = environment.get("REZ_CONFIG_FILE", "").split(os.pathsep)
    if not any(value and Path(value).is_file() for value in configs):
        if config_fallback is None or not config_fallback.is_file():
            raise BindError("No deployed or fallback REZ_CONFIG_FILE; refusing default repository routing")
        environment["REZ_CONFIG_FILE"] = str(config_fallback)
    source_hash = hashlib.sha256(executable.read_bytes()).hexdigest()
    plugins = json.loads(_run(executable, ["config", "plugins", "--json"], environment))
    filesystem = plugins
    for key in ("package_repository", "filesystem"):
        if not isinstance(filesystem, dict):
            raise BindError("Invalid filesystem repository plugin configuration")
        filesystem = filesystem.get(key, {})
    if not isinstance(filesystem, dict) or filesystem.get("package_filenames", ["package"]) != ["package"]:
        raise BindError("System bind refresh requires the canonical package.py definition stem")
    if repository is None:
        value = _run(executable, ["config", "release_packages_path"], environment).strip()
        if not value or value == "None" or "\n" in value or "\r" in value:
            raise BindError("Rez config did not return one release repository path")
        repository = Path(value).expanduser()
    repository = repository.absolute()
    if not repository.is_dir():
        raise BindError(f"System repository does not exist: {repository}")
    _safe(repository, repository)

    with tempfile.TemporaryDirectory(prefix="rez-rs-system-bind-") as temporary:
        stage = Path(temporary)
        _run(executable, ["bind", "os", "--install-path", str(stage), "--format", "py"], environment)
        if hashlib.sha256(executable.read_bytes()).hexdigest() != source_hash:
            raise BindError("Activated Rez executable changed during system bind generation")
        generated = {}
        if {entry.name for entry in stage.iterdir()} != set(FAMILIES):
            raise BindError("Canonical os bind did not generate exactly platform, arch and os")
        for family in FAMILIES:
            versions = list((stage / family).iterdir())
            if len(versions) != 1 or not versions[0].is_dir():
                raise BindError(f"Expected exactly one generated {family} version")
            version = versions[0].name
            if not re.fullmatch(r"[A-Za-z0-9_][A-Za-z0-9_.-]*", version) or version.endswith("."):
                raise BindError(f"Unsafe generated system version: {version!r}")
            definition = versions[0] / "package.py"
            if {entry.name for entry in versions[0].iterdir()} != {"package.py"}:
                raise BindError(f"Unexpected generated system payload: {versions[0]}")
            source = definition.read_text(encoding="utf-8")
            values, callbacks, _ = _definition(source, definition)
            if values.get("name") != family or values.get("version") != version:
                raise BindError(f"Generated package identity mismatch: {definition}")
            if family == "platform" and (not callbacks or "system.environ" not in source):
                raise BindError("Activated Rez platform binder lacks the canonical system baseline")
            generated[family] = (version, source, values, callbacks)

        receipt = {"schema": 1, "executable_sha256": source_hash, "repository": str(repository), "packages": []}
        with ExitStack() as locks:
            for family, (version, _, _, _) in generated.items():
                lock = repository / f".lock.{family}-{version}"
                _safe(lock, repository)
                locks.enter_context(_lock(lock))
            updates = []
            for family, (version, source, values, callbacks) in generated.items():
                directory = repository / family / version
                target = directory / "package.py"
                _safe(target, repository)
                if directory.exists():
                    if not directory.is_dir():
                        raise BindError(f"System version is not a directory: {directory}")
                    definitions = [p for p in directory.iterdir() if p.name.startswith("package.")]
                    if any(p.name != "package.py" for p in definitions):
                        raise BindError(f"Ambiguous system package definitions: {directory}")
                previous = target.read_bytes() if target.exists() else None
                if previous is not None:
                    old, old_callbacks, extensions = _definition(previous.decode("utf-8"), target)
                    if old.get("name") != family or old.get("version") != version:
                        raise BindError(f"Existing system package identity mismatch: {target}")
                    legacy = family == "platform" and old_callbacks == {"commands": LEGACY_PLATFORM_COMMANDS}
                    if old.get("commands") or (old_callbacks and old_callbacks != callbacks and not legacy):
                        raise BindError(f"Custom system commands require review: {target}")
                    if old.get("requires") and old.get("requires") != values.get("requires"):
                        raise BindError(f"Custom system requirements require review: {target}")
                    if extensions:
                        source = source.rstrip() + "\n\n" + "\n\n".join(extensions)
                incoming = (source.rstrip() + "\n").encode("utf-8")
                record = {"family": family, "version": version,
                          "previous": hashlib.sha256(previous).hexdigest() if previous is not None else None,
                          "incoming": hashlib.sha256(incoming).hexdigest(),
                          "changed": previous != incoming}
                receipt["packages"].append(record)
                if record["changed"]:
                    updates.append((target, previous, incoming, record))

            if updates:
                backups = repository / ".rez-rs-backups" / "system-bind"
                _safe(backups, repository)
                backups.mkdir(parents=True, exist_ok=True)
                backup = Path(tempfile.mkdtemp(prefix="transaction-", dir=backups))
                for target, previous, incoming, record in updates:
                    relative = target.relative_to(repository)
                    if previous is not None:
                        saved = backup / relative
                        saved.parent.mkdir(parents=True, exist_ok=True)
                        shutil.copy2(target, saved)
                        if hashlib.sha256(saved.read_bytes()).hexdigest() != record["previous"]:
                            raise BindError(f"System backup verification failed: {saved}")
                    saved_incoming = backup / "incoming" / relative
                    saved_incoming.parent.mkdir(parents=True, exist_ok=True)
                    saved_incoming.write_bytes(incoming)
                receipt["backup"] = str(backup)
                with (backup / "manifest.json").open("w", encoding="utf-8") as manifest:
                    json.dump(receipt, manifest, indent=2)
                    manifest.flush()
                    os.fsync(manifest.fileno())
                for target, previous, incoming, record in updates:
                    if (target.read_bytes() if target.exists() else None) != previous:
                        raise BindError(f"System metadata changed before publication: {target}")
                    _safe(target, repository)
                    target.parent.mkdir(parents=True, exist_ok=True)
                    with tempfile.NamedTemporaryFile(prefix=".system-bind-", dir=target.parent, delete=False) as output:
                        pending = Path(output.name)
                        output.write(incoming)
                        output.flush()
                        os.fsync(output.fileno())
                    try:
                        os.replace(pending, target)
                    finally:
                        pending.unlink(missing_ok=True)
                    if hashlib.sha256(target.read_bytes()).hexdigest() != record["incoming"]:
                        raise BindError(f"Published system metadata hash mismatch: {target}")
                (backup / "complete").write_text("verified\n", encoding="utf-8")
        return receipt
