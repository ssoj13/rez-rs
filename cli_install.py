#!/usr/bin/env python3
"""Install native CLI aliases with one recoverable transaction on Windows and Unix.

This is the canonical installer support module. Bootstrap staging copies this
file verbatim beside the external installer; generated copies are not edited.
New binaries own activation through `rez deploy`; this facade delegates to it.
The Python transaction remains a migration path for older executables and
recovery without a runnable executable. Alias names come from parser metadata.
"""
from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import subprocess
import tempfile
from pathlib import Path
from typing import Mapping

MARKER = ".rez-rs-cli.json"
PRIMARY_FILES = frozenset({"rez", "rez.exe", "rez-rs.zip"})
OLD_HOST_FILES = frozenset({"rez.exe", "rez-rs.zip"})


class InstallError(RuntimeError):
    """An unsafe target, inconsistent provenance, or incomplete transaction."""


def _safe_name(name: object) -> bool:
    return isinstance(name, str) and (
        name in PRIMARY_FILES or name == MARKER
        or re.fullmatch(r"(?:rez-[a-z0-9][a-z0-9-]*|rezolve|_rez-complete|_rez_fwd)(?:\.exe)?", name) is not None
    )


def _regular(path: Path) -> None:
    if path.is_symlink() or getattr(path, "is_junction", lambda: False)():
        raise InstallError(f"Refusing a redirected installation path: {path}")
    if path.exists() and not path.is_file():
        raise InstallError(f"Installation target is not a regular file: {path}")


def file_hashes(directory: Path, names, *, package: bool = False) -> dict[str, str]:
    """Hash only validated direct children, without following symbolic links."""
    result = {}
    for name in sorted(names):
        if not _safe_name(name) and not (package and name == "package.py"):
            raise InstallError(f"Unsafe installation filename: {name!r}")
        path = directory / name
        _regular(path)
        digest = hashlib.sha256()
        try:
            with path.open("rb") as stream:
                for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                    digest.update(chunk)
        except OSError as error:
            raise InstallError(f"Cannot hash installation file {path}: {error}") from error
        result[name] = digest.hexdigest()
    return result


def aliases(executable: Path) -> list[str]:
    """Read native alias names from the immutable incoming executable."""
    _regular(executable)
    expected = file_hashes(executable.parent, [executable.name])[executable.name]
    try:
        result = subprocess.run(
            [str(executable), "--list-cli-aliases"], capture_output=True,
            check=False, text=True, encoding="utf-8", timeout=30,
        )
        metadata = json.loads(result.stdout) if result.returncode == 0 else None
    except (OSError, ValueError, subprocess.TimeoutExpired) as error:
        raise InstallError(f"Cannot read native CLI metadata: {error}") from error
    if not isinstance(metadata, dict) or not metadata:
        raise InstallError(f"Invalid native CLI metadata: {result.stderr}")
    if any(
        not _safe_name(name) or name in PRIMARY_FILES or name == MARKER
        or not isinstance(target, str)
        for name, target in metadata.items()
    ):
        raise InstallError("Native CLI metadata contains unsafe alias names or targets")
    if file_hashes(executable.parent, [executable.name])[executable.name] != expected:
        raise InstallError("Incoming executable changed while reading native CLI metadata")
    suffix = ".exe" if executable.suffix.lower() == ".exe" else ""
    return sorted(name + suffix for name in metadata)


def install_cli(
    executable: Path | None, directory: Path, *,
    payloads: Mapping[str, Path] | None = None, recover_only: bool = False,
) -> Path:
    """Recover first, then atomically install copied aliases and optional payload files.

    Prior alias ownership is established by a recorded immutable hash manifest.
    Foreign alias files fail closed. Existing primary payload files are backed up.
    Recovery does not require an incoming executable, ZIP, or interpreter launch.
    """
    directory = directory.absolute()
    payloads = dict(payloads or {})
    if any(name not in PRIMARY_FILES for name in payloads):
        raise InstallError("Unsafe primary payload filenames")
    alias_names = None
    if executable is not None and not recover_only:
        executable = executable.absolute()
        alias_names = aliases(executable)
        suffix = ".exe" if os.name == "nt" else ""
        if "rez-deploy" + suffix in alias_names:
            primary = "rez" + suffix
            if set(payloads) - {primary, "rez-rs.zip"}:
                raise InstallError("Native payload filenames must match the host platform")
            if primary in payloads:
                incoming_hash = file_hashes(executable.parent, [executable.name])[executable.name]
                source = payloads[primary]
                if file_hashes(source.parent, [source.name])[source.name] != incoming_hash:
                    raise InstallError("Primary payload does not match the incoming executable")
            command = [str(executable), "deploy", "--bin-dir", str(directory)]
            if primary not in payloads:
                command.append("--aliases-only")
            if "rez-rs.zip" in payloads:
                command.extend(["--source-zip", str(payloads["rez-rs.zip"].absolute())])
            try:
                result = subprocess.run(
                    command, capture_output=True, check=False, text=True, encoding="utf-8",
                )
            except OSError as error:
                raise InstallError(f"Cannot run native CLI deployment: {error}") from error
            if result.returncode != 0:
                raise InstallError(f"Native CLI deployment failed: {result.stderr.strip()}")
            return directory
    transaction = directory / ".rez-rs-activation"
    backups = directory / ".rez-rs-backups"
    if recover_only and not transaction.exists():
        return directory
    directory.mkdir(parents=True, exist_ok=True)
    if directory.is_symlink() or getattr(directory, "is_junction", lambda: False)():
        raise InstallError(f"Installation directory is redirected: {directory}")
    lock_path = directory / ".rez-rs-install.lock"
    _regular(lock_path)
    with lock_path.open("a+b") as lock:
        if lock.tell() == 0:
            lock.write(b"\0")
            lock.flush()
        lock.seek(0)
        try:
            if os.name == "nt":
                import msvcrt
                msvcrt.locking(lock.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl
                fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as error:
            raise InstallError(f"Another installation owns {lock_path}") from error
        try:
            for state in [transaction, backups]:
                if state.resolve() != directory.resolve() / state.name:
                    raise InstallError(f"Installation state directory is redirected: {state}")

            def restore(record: dict) -> None:
                previous, incoming = record.get("previous"), record.get("incoming")
                if not isinstance(previous, dict) or not isinstance(incoming, dict):
                    raise InstallError("Invalid activation recovery manifest")
                names = set(previous) | set(incoming)
                if record.get("schema") == 2:
                    if not incoming or any(not _safe_name(name) for name in names):
                        raise InstallError("Invalid activation recovery filenames")
                    if MARKER not in incoming:
                        raise InstallError("Missing native alias ownership manifest")
                elif not set(previous).issubset(OLD_HOST_FILES) or set(incoming) != OLD_HOST_FILES:
                    raise InstallError("Invalid legacy activation recovery filenames")
                if any(
                    not isinstance(digest, str) or re.fullmatch(r"[0-9a-f]{64}", digest) is None
                    for digest in [*previous.values(), *incoming.values()]
                ):
                    raise InstallError("Invalid activation recovery hashes")
                identity = hashlib.sha256(json.dumps(previous, sort_keys=True).encode()).hexdigest()
                backup = backups / identity
                if backup.resolve() != backups.resolve() / identity:
                    raise InstallError("Activation backup is redirected")
                if not backup.is_dir() or file_hashes(backup, previous) != previous:
                    raise InstallError(f"Activation originals are missing or corrupted: {backup}")
                for name in sorted(names):
                    target = directory / name
                    _regular(target)
                    current = file_hashes(directory, [name])[name] if target.exists() else None
                    if current is not None and current not in {previous.get(name), incoming.get(name)}:
                        raise InstallError(f"Activation target changed independently: {target}")
                    if name in previous:
                        if current == previous[name]:
                            continue
                        temporary = transaction / ("restore-" + name)
                        _regular(temporary)
                        shutil.copy2(backup / name, temporary)
                        if hashlib.sha256(temporary.read_bytes()).hexdigest() != previous[name]:
                            raise InstallError(f"Restored file hash mismatch: {name}")
                        os.replace(temporary, target)
                    elif target.exists():
                        target.unlink()
                if file_hashes(directory, previous) != previous:
                    raise InstallError("Activation rollback hash verification failed")

            if transaction.exists():
                try:
                    _regular(transaction / "journal.json")
                    record = json.loads((transaction / "journal.json").read_text(encoding="utf-8"))
                except (OSError, ValueError) as error:
                    raise InstallError(f"Review incomplete activation at {transaction}: {error}") from error
                if not isinstance(record, dict):
                    raise InstallError("Invalid activation recovery journal")
                restore(record)
                shutil.rmtree(transaction)
                # Recovery may replace an executable located inside the destination.
                alias_names = None
            if recover_only:
                return directory
            if executable is None:
                raise InstallError("Installation requires an incoming executable")

            if alias_names is None:
                alias_names = aliases(executable)
            sources = {name: executable for name in alias_names}
            sources.update(payloads)
            source_hashes = {
                source: file_hashes(source.parent, [source.name])[source.name]
                for source in set(sources.values())
            }
            incoming = {name: source_hashes[source] for name, source in sources.items()}
            marker_path = directory / MARKER
            _regular(marker_path)
            owned = {}
            if marker_path.exists():
                try:
                    ownership = json.loads(marker_path.read_text(encoding="utf-8"))
                    owned = ownership["hashes"]
                except (OSError, ValueError, KeyError, TypeError) as error:
                    raise InstallError(f"Invalid native alias ownership manifest: {error}") from error
                if (not isinstance(owned, dict) or ownership.get("schema") != 1
                        or any(not _safe_name(name) or name == MARKER for name in owned)
                        or any(not isinstance(digest, str) or re.fullmatch(r"[0-9a-f]{64}", digest) is None for digest in owned.values())):
                    raise InstallError("Unsafe native alias ownership manifest")
                current_owned = file_hashes(directory, owned)
                primary = "rez.exe" if os.name == "nt" else "rez"
                executable_hash = source_hashes[executable]
                for name, expected in owned.items():
                    actual = current_owned[name]
                    # A manually copied primary is allowed only when explicitly incoming.
                    # Never extend this exception to aliases or source archives.
                    if actual != expected and not (
                        name == primary and name in payloads
                        and incoming[name] == executable_hash == actual
                    ):
                        raise InstallError(f"Previously installed native CLI file changed independently: {name}")
                    if name in PRIMARY_FILES and name not in incoming:
                        sources[name] = directory / name
                        incoming[name] = actual
            for name in alias_names:
                target = directory / name
                _regular(target)
                if target.exists() and name not in owned:
                    raise InstallError(f"Refusing to replace a foreign CLI alias: {target}")
            marker_bytes = (json.dumps({"schema": 1, "hashes": incoming}, sort_keys=True) + "\n").encode()
            incoming[MARKER] = hashlib.sha256(marker_bytes).hexdigest()
            names = set(owned) | set(incoming)
            for name in names:
                _regular(directory / name)
            previous = file_hashes(directory, [name for name in names if (directory / name).exists()])
            if previous == incoming:
                return directory
            identity = hashlib.sha256(json.dumps(previous, sort_keys=True).encode()).hexdigest()
            backup = backups / identity
            backups.mkdir(exist_ok=True)
            if backup.exists():
                if backup.resolve() != backups.resolve() / identity:
                    raise InstallError("Activation backup is redirected")
                if not backup.is_dir() or {x.name for x in backup.iterdir()} != set(previous):
                    raise InstallError(f"Conflicting activation backup: {backup}")
                if file_hashes(backup, previous) != previous:
                    raise InstallError("Conflicting activation backup hashes")
            else:
                temporary_backup = Path(tempfile.mkdtemp(prefix=".staging-", dir=backups))
                try:
                    for name in previous:
                        shutil.copy2(directory / name, temporary_backup / name)
                    if file_hashes(temporary_backup, previous) != previous:
                        raise InstallError("Activation originals changed during backup")
                    os.replace(temporary_backup, backup)
                finally:
                    if temporary_backup.exists():
                        shutil.rmtree(temporary_backup)
            pending = Path(tempfile.mkdtemp(prefix=".rez-rs-activation.staging-", dir=directory))
            try:
                record = {"schema": 2, "previous": previous, "incoming": incoming}
                with (pending / "journal.json").open("w", encoding="utf-8") as journal:
                    json.dump(record, journal, sort_keys=True)
                    journal.flush()
                    os.fsync(journal.fileno())
                os.replace(pending, transaction)
            finally:
                if pending.exists():
                    shutil.rmtree(pending)
            try:
                for name, source in sources.items():
                    _regular(transaction / name)
                    shutil.copy2(source, transaction / name)
                _regular(transaction / MARKER)
                (transaction / MARKER).write_bytes(marker_bytes)
                if file_hashes(transaction, incoming) != incoming:
                    raise InstallError("Incoming CLI files changed during staging")
                if file_hashes(directory, [name for name in names if (directory / name).exists()]) != previous:
                    raise InstallError("Activation originals changed before commit")
                for name in sorted(incoming, key=lambda name: (name == MARKER, name)):
                    # The running primary can be locked on Windows; identical bytes need no replacement.
                    if previous.get(name) != incoming[name]:
                        os.replace(transaction / name, directory / name)
                for name in set(previous) - set(incoming):
                    (directory / name).unlink()
                if file_hashes(directory, incoming) != incoming:
                    raise InstallError("Activated CLI/source/alias hash verification failed")
            except Exception as error:
                try:
                    restore(record)
                except Exception as recovery_error:
                    raise InstallError(f"Activation failed ({error}); recovery pending at {transaction}: {recovery_error}") from recovery_error
                shutil.rmtree(transaction)
                raise InstallError(f"Activation failed and originals restored: {error}") from error
            shutil.rmtree(transaction)
        finally:
            lock.seek(0)
            if os.name == "nt":
                import msvcrt
                msvcrt.locking(lock.fileno(), msvcrt.LK_UNLCK, 1)
            else:
                import fcntl
                fcntl.flock(lock.fileno(), fcntl.LOCK_UN)
    return directory
