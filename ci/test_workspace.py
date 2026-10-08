#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Run release workspace tests under explicit normal and Windows 8.3 temp roots."""

import argparse
import ctypes
import os
from pathlib import Path
import subprocess
import tempfile

COMMAND = [
    "cargo", "test", "--locked", "--offline", "--workspace", "--release",
    "--", "--test-threads=1",
]


def windows_short_path(path):
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    function = kernel32.GetShortPathNameW
    function.argtypes = [ctypes.c_wchar_p, ctypes.c_wchar_p, ctypes.c_uint32]
    function.restype = ctypes.c_uint32
    output = ctypes.create_unicode_buffer(32768)
    length = function(str(path), output, len(output))
    if length == 0:
        raise ctypes.WinError(ctypes.get_last_error())
    if length >= len(output):
        raise RuntimeError("Windows short path exceeds the supported path limit")
    return output.value


def run(root, temp, label):
    environment = os.environ.copy()
    environment.update(TEMP=str(temp), TMP=str(temp), TMPDIR=str(temp))
    print(f"Workspace tests: {label}; TEMP={temp}", flush=True)
    return subprocess.run(COMMAND, cwd=root, env=environment).returncode


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--require-short-path", action="store_true",
        help="fail if Windows does not provide a distinct 8.3 temp-root spelling",
    )
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    if args.require_short_path and os.name != "nt":
        parser.error("--require-short-path requires Windows")
    with tempfile.TemporaryDirectory(prefix="rez-workspace-temp-long-") as directory:
        normal = Path(directory).resolve()
        result = run(root, normal, "normal")
        if result:
            return result
        if os.name == "nt":
            short = windows_short_path(normal)
            if short.casefold() == str(normal).casefold():
                if args.require_short_path:
                    raise RuntimeError("This filesystem did not provide an 8.3 temp-root alias")
                print("8.3 temp-root alias unavailable; only the normal scope was run", flush=True)
            else:
                return run(root, short, "Windows 8.3")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
