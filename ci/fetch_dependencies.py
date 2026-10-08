#!/usr/bin/env python3
"""Fetch locked dependencies without persisting the private-repository token."""
from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def main() -> int:
    env = os.environ.copy()
    env.update(CARGO_NET_GIT_FETCH_WITH_CLI="true", GIT_TERMINAL_PROMPT="0",
               GIT_CONFIG_COUNT="1",
               GIT_CONFIG_KEY_0="url.https://github.com/.insteadOf",
               GIT_CONFIG_VALUE_0="ssh://git@github.com/")
    with tempfile.TemporaryDirectory(prefix="rez-git-auth-") as temporary:
        if env.get("DEPENDENCIES_TOKEN"):
            helper = Path(temporary) / "credential.py"
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
            env.update(GIT_CONFIG_COUNT="3",
                       GIT_CONFIG_KEY_1="credential.helper", GIT_CONFIG_VALUE_1="",
                       GIT_CONFIG_KEY_2="credential.helper",
                       GIT_CONFIG_VALUE_2=f'!"{python}" "{helper.as_posix()}"')
        else:
            print("No DEPENDENCIES_TOKEN: fetching anonymously. Private GUI repositories "
                  "require a Contents: Read token in that Actions secret.", flush=True)
        return subprocess.run(["cargo", "fetch", "--locked"], cwd=ROOT, env=env).returncode


if __name__ == "__main__":
    raise SystemExit(main())
