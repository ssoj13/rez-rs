"""Private dependency credentials are scoped and removed after Cargo exits."""
from __future__ import annotations

import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("ci_fetch", ROOT / "ci" / "fetch_dependencies.py")
fetch = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fetch)


class DependencyCredentialsTests(unittest.TestCase):
    def test_helper_only_answers_github_and_is_removed_after_failed_fetch(self):
        saved_run = subprocess.run
        helpers = []

        def failed_fetch(command, **kwargs):
            self.assertEqual(command, ["cargo", "fetch", "--locked"])
            env = kwargs["env"]
            helper = Path(env["GIT_CONFIG_VALUE_2"].split('"')[-2])
            helpers.append(helper)
            self.assertTrue(helper.is_file())
            for host, expected in [
                ("github.com", "username=x-access-token\npassword=fake-ci-token\n"),
                ("example.com", ""),
            ]:
                result = saved_run(
                    [sys.executable, str(helper), "get"],
                    input="protocol=https\nhost=" + host + "\n\n",
                    env=env, capture_output=True, text=True, check=True,
                )
                self.assertEqual(result.stdout, expected)
            return subprocess.CompletedProcess(command, 101)

        with patch.dict(os.environ, {"DEPENDENCIES_TOKEN": "fake-ci-token"}), patch.object(
            fetch.subprocess, "run", side_effect=failed_fetch
        ):
            self.assertEqual(fetch.main(), 101)
        self.assertFalse(helpers[0].exists())

    def test_anonymous_fetch_does_not_install_a_credential_helper(self):
        with patch.dict(os.environ, {"DEPENDENCIES_TOKEN": ""}), patch.object(
            fetch.subprocess, "run", return_value=subprocess.CompletedProcess([], 0)
        ) as runner:
            self.assertEqual(fetch.main(), 0)
        env = runner.call_args.kwargs["env"]
        self.assertEqual(env["GIT_TERMINAL_PROMPT"], "0")
        self.assertEqual(env["GIT_CONFIG_COUNT"], "1")
        self.assertEqual(env["GIT_CONFIG_KEY_0"], "url.https://github.com/.insteadOf")
        self.assertEqual(env["GIT_CONFIG_VALUE_0"], "ssh://git@github.com/")


if __name__ == "__main__":
    unittest.main()
