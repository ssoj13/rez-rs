#!/usr/bin/env python3
"""Fetch locked dependencies using temporary, repository-scoped credentials."""
from __future__ import annotations

import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
SSH_DEPENDENCIES = (
    ("DEPENDENCY_NODES_SSH_KEY", "ssoj13/nodes-rs.git", "rez-dependency-nodes"),
    ("DEPENDENCY_BOX_SSH_KEY", "ssoj13/box-rs.git", "rez-dependency-box"),
    ("DEPENDENCY_WIDGETS_SSH_KEY", "ssoj13/wgpu-widgets-rs.git", "rez-dependency-widgets"),
    ("DEPENDENCY_LAYOUT_SSH_KEY", "ssoj13/graph-layout-rs.git", "rez-dependency-layout"),
)


def configure_credentials(env: dict[str, str], temporary: Path) -> dict[str, str]:
    """Configure child Git credentials; the caller owns the temporary directory."""
    configured = env.copy()
    key_names = [name for name, _, _ in SSH_DEPENDENCIES]
    use_ssh = any(env.get(name, "").strip() for name in key_names)
    if use_ssh:
        missing = [name for name in key_names if not env.get(name, "").strip()]
        if missing:
            raise ValueError("All four dependency SSH keys are required; missing: "
                             + ", ".join(missing))

    # Replace inherited command-scoped configuration rather than merging secrets
    # or helpers from an unrelated caller.
    for name in list(configured):
        if name == "GIT_CONFIG_COUNT" or name.startswith(
                ("GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_")):
            del configured[name]
    configured.update(CARGO_NET_GIT_FETCH_WITH_CLI="true", GIT_TERMINAL_PROMPT="0")
    entries = [("url.https://github.com/.insteadOf", "ssh://git@github.com/"),
               ("credential.helper", "")]

    if use_ssh:
        sections = []
        known_hosts = (ROOT / "ci" / "github_known_hosts").as_posix()
        for name, repository, alias in SSH_DEPENDENCIES:
            key = temporary / (alias + ".key")
            key.write_text(env[name].strip() + "\n", encoding="utf-8", newline="\n")
            key.chmod(0o600)
            sections.append(
                f"Host {alias}\n"
                "    HostName github.com\n"
                "    User git\n"
                f'    IdentityFile "{key.as_posix()}"\n'
                "    IdentitiesOnly yes\n"
                "    BatchMode yes\n"
                "    StrictHostKeyChecking yes\n"
                "    HostKeyAlias github.com\n"
                f'    UserKnownHostsFile "{known_hosts}"\n'
                "    GlobalKnownHostsFile /dev/null\n")
            entries.append((f"url.ssh://git@{alias}/{repository}.insteadOf",
                            f"ssh://git@github.com/{repository}"))
        config = temporary / "ssh_config"
        config.write_text("\n".join(sections), encoding="utf-8", newline="\n")
        config.chmod(0o600)
        configured["GIT_SSH_COMMAND"] = "ssh -F " + shlex.quote(config.as_posix())
        configured["GIT_SSH_VARIANT"] = "ssh"
        configured.pop("GIT_SSH", None)
        configured.pop("DEPENDENCIES_TOKEN", None)
    elif configured.get("DEPENDENCIES_TOKEN"):
        helper = temporary / "credential.py"
        helper.write_text(
            "import os, sys\n"
            "request = dict(line.rstrip('\\n').split('=', 1) "
            "for line in sys.stdin if '=' in line)\n"
            "if sys.argv[1:] == ['get'] and request.get('protocol') == 'https' "
            "and request.get('host') == 'github.com':\n"
            "    print('username=x-access-token')\n"
            "    print('password=' + os.environ['DEPENDENCIES_TOKEN'])\n",
            encoding="utf-8")
        # Git runs credential helpers through its POSIX shell, also on Windows.
        python = Path(sys.executable).as_posix()
        entries.append(("credential.helper",
                        f'!"{python}" "{helper.as_posix()}"'))
    else:
        configured.pop("DEPENDENCIES_TOKEN", None)

    for name in key_names:
        configured.pop(name, None)
    configured["GIT_CONFIG_COUNT"] = str(len(entries))
    for index, (name, value) in enumerate(entries):
        configured[f"GIT_CONFIG_KEY_{index}"] = name
        configured[f"GIT_CONFIG_VALUE_{index}"] = value
    return configured


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="rez-git-auth-") as temporary:
        try:
            env = configure_credentials(os.environ.copy(), Path(temporary))
        except ValueError as error:
            print(str(error), file=sys.stderr, flush=True)
            return 2
        if "GIT_SSH_COMMAND" not in env and not env.get("DEPENDENCIES_TOKEN"):
            print("No dependency credentials: fetching anonymously. Private GUI "
                  "repositories require four deploy keys or DEPENDENCIES_TOKEN.",
                  flush=True)
        return subprocess.run(["cargo", "fetch", "--locked"], cwd=ROOT, env=env).returncode


if __name__ == "__main__":
    raise SystemExit(main())
