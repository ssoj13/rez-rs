#!/usr/bin/env python3
"""Install a host-native Rez package and activate its CLI/source pair and aliases.

--repository-root selects the repository. --rez-root optionally activates the CLI;
without --rez-root, only the repository is changed.
Original host files are retained under Scripts/rez/.rez-rs-backups on Windows,
or bin/rez/.rez-rs-backups on Unix. A journal and OS advisory lock recover an
interrupted activation on the next invocation. Payloads must match the host:
rez.exe on Windows, rez on Linux/macOS. Activation additionally requires the
incoming executable to run on the current host before any CLI files are replaced.
The adjacent cli_install.py and system_bind.py are generated from the project;
native aliases and system packages use the incoming executable's canonical metadata.
After activation platform/arch/os are safely refreshed in the configured release
repository. Existing definitions are backed up; arbitrary callbacks fail closed.
Legacy launchers, the Python environment and original Rez sources are preserved.
"""

from __future__ import annotations

import argparse
import ast
import json
import os
import shutil
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path, PurePosixPath


STAGE_DIR = Path(__file__).resolve().parent
PACKAGE_NAME = "rez_rs"
PAYLOAD_DIR = STAGE_DIR / ("payload" if (STAGE_DIR / "payload").is_dir() else PACKAGE_NAME)
PACKAGE_TYPE = "tool"
TOOL_NAME = "rez"
TOOL_BINARY = "rez.exe" if os.name == "nt" else "rez"
EXPECTED_FILES = frozenset({"package.py", TOOL_BINARY, "rez-rs.zip"})
HOST_FILES = EXPECTED_FILES - {"package.py"}
SOURCE_ARCHIVE_ROOT = "rez-rs"
FORBIDDEN_ARCHIVE_PARTS = frozenset(
    {".claude", ".codex", ".env", ".git", ".gitnexus", ".idea", ".mcp.json",
     ".pytest_cache", ".repl_history.txt", ".venv", ".vscode", "CMakeUserPresets.json",
     "CMakeCache.txt", "CMakeFiles", ".sconsign.dblite", ".scons_node_count", ".eggs",
     "__pycache__", "dist", "node_modules", "target", "venv"}
)


class InstallError(RuntimeError):
    """Raised when the staged package or destination is invalid."""


def package_metadata(package_file: Path) -> dict[str, object]:
    """Read only literal Rez recipe fields without executing package code."""
    try:
        tree = ast.parse(package_file.read_text(encoding="utf-8"), filename=str(package_file))
    except (OSError, SyntaxError, UnicodeError) as error:
        raise InstallError(f"Cannot read Rez package recipe {package_file}: {error}") from error

    metadata: dict[str, object] = {}
    for node in tree.body:
        if not isinstance(node, ast.Assign):
            continue
        for target in node.targets:
            if isinstance(target, ast.Name) and target.id in {
                "name",
                "version",
                "package_type",
                "tools",
            }:
                try:
                    metadata[target.id] = ast.literal_eval(node.value)
                except (ValueError, TypeError, SyntaxError) as error:
                    raise InstallError(
                        f"Rez recipe field {target.id!r} must be a literal value"
                    ) from error

    if metadata.get("name") != PACKAGE_NAME:
        raise InstallError(f"Expected Rez package name {PACKAGE_NAME!r}, got {metadata.get('name')!r}")
    if metadata.get("package_type") != PACKAGE_TYPE:
        raise InstallError(
            f"Expected package_type={PACKAGE_TYPE!r}, got {metadata.get('package_type')!r}"
        )
    if metadata.get("tools") != [TOOL_NAME]:
        raise InstallError(f"Expected tools=[{TOOL_NAME!r}], got {metadata.get('tools')!r}")

    version = metadata.get("version")
    if not isinstance(version, str) or not version or version in {".", ".."}:
        raise InstallError(f"Invalid Rez package version: {version!r}")
    if Path(version).name != version or "/" in version or "\\" in version:
        raise InstallError(f"Unsafe Rez package version path: {version!r}")
    return metadata


def validate_source_archive(archive_path: Path) -> None:
    """Verify the source archive is intact, rooted correctly, and excludes generated/VCS trees."""
    if not zipfile.is_zipfile(archive_path):
        raise InstallError(f"Source archive is not a valid ZIP file: {archive_path}")

    try:
        with zipfile.ZipFile(archive_path) as archive:
            names = archive.namelist()
            if not names:
                raise InstallError("Source archive is empty")
            has_manifest = False
            for name in names:
                member = PurePosixPath(name)
                if member.is_absolute() or ".." in member.parts:
                    raise InstallError(f"Unsafe source archive path: {name!r}")
                if not member.parts or member.parts[0] != SOURCE_ARCHIVE_ROOT:
                    raise InstallError(f"Unexpected source archive root: {name!r}")
                if (FORBIDDEN_ARCHIVE_PARTS.intersection(member.parts)
                        or any(part.startswith(".env.") or part.endswith(".egg-info") for part in member.parts)
                        or member.parts[1:2] == ("packages",)
                        or member.parts[1:3] == ("docs", "build")):
                    raise InstallError(f"Generated or VCS content in source archive: {name!r}")
                if member.parts[1:2] == ("examples",) and (
                    any(part in {"build", ".conan", "vcpkg_installed"} for part in member.parts[2:])
                    or member.suffix.lower() in {
                        ".exe", ".obj", ".o", ".os", ".so", ".dll", ".dylib",
                        ".a", ".d", ".out", ".pdb", ".egg",
                    }
                ):
                    raise InstallError(f"Generated example output in source archive: {name!r}")
                if name == f"{SOURCE_ARCHIVE_ROOT}/Cargo.toml":
                    has_manifest = True
            if not has_manifest:
                raise InstallError("Source archive does not contain rez-rs/Cargo.toml")
            corrupt_member = archive.testzip()
            if corrupt_member is not None:
                raise InstallError(f"Corrupt source archive member: {corrupt_member}")
    except (OSError, zipfile.BadZipFile, RuntimeError) as error:
        raise InstallError(f"Cannot validate source archive {archive_path}: {error}") from error


def file_hashes(directory: Path, names=EXPECTED_FILES) -> dict[str, str]:
    """Use the canonical direct-child hash and filesystem safety policy."""
    from cli_install import InstallError as CliInstallError, file_hashes as shared_file_hashes
    try:
        return shared_file_hashes(directory, names, package=True)
    except CliInstallError as error:
        raise InstallError(str(error)) from error


def validate_package(package_dir: Path) -> dict[str, str]:
    """Validate one version directory and return its immutable file manifest."""
    if not package_dir.is_dir():
        raise InstallError(f"Package version directory does not exist: {package_dir}")
    entries = {entry.name for entry in package_dir.iterdir()}
    if entries != EXPECTED_FILES:
        raise InstallError(
            f"{package_dir} must contain exactly {sorted(EXPECTED_FILES)}, got {sorted(entries)}"
        )

    metadata = package_metadata(package_dir / "package.py")
    if metadata["version"] != package_dir.name:
        raise InstallError(
            f"Recipe version {metadata['version']!r} does not match directory {package_dir.name!r}"
        )
    validate_source_archive(package_dir / "rez-rs.zip")
    return file_hashes(package_dir)


def install_package(package_dir: Path, repository_root: Path) -> Path:
    """Install atomically without replacing a different package already at the same version."""
    source_hashes = validate_package(package_dir)
    repository_root = repository_root.resolve()
    family_dir = (repository_root / PACKAGE_TYPE / PACKAGE_NAME).resolve()
    destination = (family_dir / package_dir.name).resolve()
    if not family_dir.is_relative_to(repository_root) or not destination.is_relative_to(repository_root):
        raise InstallError("Package family/version path escapes the configured repository root")
    family_dir.mkdir(parents=True, exist_ok=True)

    if destination.exists():
        existing_hashes = validate_package(destination)
        if existing_hashes == source_hashes:
            print(f"Already installed: {destination}")
            return destination
        raise InstallError(
            f"Refusing to replace a different existing package version: {destination}"
        )

    temporary_dir = Path(tempfile.mkdtemp(prefix=f".{package_dir.name}.staging-", dir=family_dir))
    try:
        for name in sorted(EXPECTED_FILES):
            shutil.copy2(package_dir / name, temporary_dir / name)
        if file_hashes(temporary_dir) != source_hashes:
            raise InstallError("Staged package hash verification failed after copying")
        os.replace(temporary_dir, destination)
    except OSError as error:
        raise InstallError(f"Could not install Rez package at {destination}: {error}") from error
    finally:
        if temporary_dir.exists():
            shutil.rmtree(temporary_dir)

    print(f"Installed {PACKAGE_NAME}-{package_dir.name} -> {destination}")
    return destination


def install_host(package_dir: Path | None, rez_root: Path, *, recover_only: bool = False) -> Path:
    """Recover first, then activate the CLI/source pair and native entry points."""
    from cli_install import InstallError as CliInstallError, install_cli

    host_dir = rez_root / ("Scripts" if os.name == "nt" else "bin") / "rez"
    if not host_dir.resolve().is_relative_to(rez_root.resolve()):
        raise InstallError(f"Activation path escapes the configured Rez root: {host_dir}")
    try:
        if recover_only:
            return install_cli(None, host_dir, recover_only=True)
        if package_dir is None:
            raise InstallError("Activation requires a package version directory")
        source_hashes = validate_package(package_dir)
        result = install_cli(
            package_dir / TOOL_BINARY, host_dir,
            payloads={name: package_dir / name for name in HOST_FILES},
        )
        if file_hashes(host_dir, HOST_FILES) != {name: source_hashes[name] for name in HOST_FILES}:
            raise InstallError("Activated CLI/source hash verification failed")
    except CliInstallError as error:
        raise InstallError(str(error)) from error
    print(f"Activated CLI/source and native aliases -> {host_dir}")
    return result


def main() -> int:
    """Recover host activation first, then validate/install staged package versions."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repository-root",
        type=Path, required=True,
        help="Existing repository root in which to install tool/rez_rs/<version>.",
    )
    parser.add_argument(
        "--rez-root", type=Path,
        help="Activate under this existing root; omit for repository-only installation.",
    )
    parser.add_argument(
        "--payload-dir", type=Path, default=PAYLOAD_DIR,
        help="Staged versions directory; defaults to payload/ or the adjacent rez_rs/ family.",
    )
    args = parser.parse_args()

    try:
        rez_value = args.rez_root
        rez_root = Path(rez_value).expanduser().resolve() if rez_value else None
        if rez_root is not None:
            if not rez_root.is_dir():
                parser.error(f"Rez root does not exist or is not a directory: {rez_root}")
            install_host(None, rez_root, recover_only=True)

        repository_root = args.repository_root.expanduser().resolve()
        if not repository_root.is_dir():
            parser.error(f"Repository root does not exist or is not a directory: {repository_root}")

        payload_dir = args.payload_dir.expanduser().resolve()
        if not payload_dir.is_dir():
            parser.error(f"Staged payload directory does not exist: {payload_dir}")
        package_dirs = sorted(path for path in payload_dir.iterdir() if path.is_dir())
        if not package_dirs:
            parser.error(f"No staged Rez package versions found in {payload_dir}")
        if rez_root is not None and len(package_dirs) != 1:
            parser.error("Host activation requires exactly one staged package version")

        for package_dir in package_dirs:
            destination = install_package(package_dir, repository_root)
            if rez_root is not None:
                executable = install_host(destination, rez_root) / TOOL_BINARY
                from system_bind import BindError, refresh_system_bindings
                try:
                    receipt = refresh_system_bindings(
                        executable,
                    )
                except (BindError, SyntaxError, UnicodeError, subprocess.SubprocessError) as error:
                    raise InstallError(f"Cannot refresh system packages: {error}") from error
                print("System bindings: " + json.dumps(receipt, sort_keys=True))
    except (InstallError, OSError) as error:
        print(f"rez-rs install failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
