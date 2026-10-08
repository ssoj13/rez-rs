"""Private dependency credentials are scoped and removed after Cargo exits."""
from __future__ import annotations

import importlib.util
import io
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
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
        self.assertEqual(env["GIT_CONFIG_COUNT"], "2")
        self.assertEqual(env["GIT_CONFIG_KEY_0"], "url.https://github.com/.insteadOf")
        self.assertEqual(env["GIT_CONFIG_VALUE_0"], "ssh://git@github.com/")
        self.assertEqual(env["GIT_CONFIG_KEY_1"], "credential.helper")
        self.assertEqual(env["GIT_CONFIG_VALUE_1"], "")

    def test_ssh_keys_are_isolated_and_host_verification_is_pinned(self):
        original = {
            name: f"fake-private-key-{index}"
            for index, (name, _, _) in enumerate(fetch.SSH_DEPENDENCIES)
        }
        original.update(DEPENDENCIES_TOKEN="ignored-token",
                        GIT_CONFIG_COUNT="1",
                        GIT_CONFIG_KEY_0="credential.helper",
                        GIT_CONFIG_VALUE_0="unsafe-inherited-helper",
                        GIT_CONFIG_KEY_99="unused-inherited-config",
                        GIT_SSH="unsafe-inherited-ssh")
        with tempfile.TemporaryDirectory(prefix="rez auth test ") as temporary:
            directory = Path(temporary)
            env = fetch.configure_credentials(original, directory)
            self.assertEqual(env["GIT_CONFIG_COUNT"], "6")
            self.assertEqual(env["GIT_CONFIG_VALUE_1"], "")
            self.assertNotIn("GIT_CONFIG_KEY_99", env)
            self.assertNotIn("DEPENDENCIES_TOKEN", env)
            self.assertNotIn("GIT_SSH", env)
            self.assertEqual(env["GIT_SSH_VARIANT"], "ssh")
            config = directory / "ssh_config"
            self.assertIn(config.as_posix(), env["GIT_SSH_COMMAND"])
            sections = config.read_text(encoding="utf-8").strip().split("\n\n")
            self.assertEqual(len(sections), 4)
            for index, (name, repository, alias) in enumerate(fetch.SSH_DEPENDENCIES):
                self.assertNotIn(name, env)
                key = directory / (alias + ".key")
                self.assertEqual(key.read_text(encoding="utf-8"),
                                 original[name] + "\n")
                if os.name != "nt":
                    self.assertEqual(key.stat().st_mode & 0o777, 0o600)
                section = sections[index]
                for setting in [
                    f"Host {alias}", "HostName github.com", "User git",
                    f'IdentityFile "{key.as_posix()}"', "IdentitiesOnly yes",
                    "BatchMode yes", "StrictHostKeyChecking yes",
                    "HostKeyAlias github.com",
                    f'UserKnownHostsFile "{(ROOT / "ci/github_known_hosts").as_posix()}"',
                    "GlobalKnownHostsFile /dev/null",
                ]:
                    self.assertIn(setting, section)
                self.assertEqual(section.count("IdentityFile"), 1)
                result = subprocess.run(
                    ["git", "ls-remote", "--get-url",
                     f"ssh://git@github.com/{repository}"],
                    env=env, capture_output=True, text=True, check=True)
                self.assertEqual(result.stdout.strip(),
                                 f"ssh://git@{alias}/{repository}")
            public = subprocess.run(
                ["git", "ls-remote", "--get-url",
                 "ssh://git@github.com/ssoj13/gpu-info-rs.git"],
                env=env, capture_output=True, text=True, check=True)
            self.assertEqual(public.stdout.strip(),
                             "https://github.com/ssoj13/gpu-info-rs.git")
            self.assertFalse((directory / "credential.py").exists())
        self.assertEqual(original["DEPENDENCIES_TOKEN"], "ignored-token")

    def test_partial_ssh_keys_fail_without_disclosing_values_or_writing_files(self):
        name = fetch.SSH_DEPENDENCIES[0][0]
        partial = {name: "fake-secret-must-not-appear"}
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            with self.assertRaisesRegex(ValueError, "All four dependency SSH keys") as error:
                fetch.configure_credentials(partial, directory)
            self.assertNotIn(partial[name], str(error.exception))
            self.assertEqual(list(directory.iterdir()), [])
        with patch.dict(os.environ, partial, clear=True), patch.object(
            fetch.subprocess, "run"
        ) as runner, patch.object(fetch.sys, "stderr", io.StringIO()) as errors:
            self.assertEqual(fetch.main(), 2)
        runner.assert_not_called()
        self.assertNotIn(partial[name], errors.getvalue())

    def test_ssh_key_files_are_removed_after_failed_fetch(self):
        files = []
        supplied = {name: f"fake-key-{index}"
                    for index, (name, _, _) in enumerate(fetch.SSH_DEPENDENCIES)}

        def failed_fetch(command, **kwargs):
            env = kwargs["env"]
            self.assertEqual(command, ["cargo", "fetch", "--locked"])
            for name, _, alias in fetch.SSH_DEPENDENCIES:
                self.assertNotIn(name, env)
                # Extract each key path from the actual configuration rather
                # than reconstructing the TemporaryDirectory location.
                config_command = env["GIT_SSH_COMMAND"]
                config = Path(shlex.split(config_command)[2])
                key = config.parent / (alias + ".key")
                files.extend([key, config])
                self.assertTrue(key.is_file())
            self.assertNotIn("DEPENDENCIES_TOKEN", env)
            return subprocess.CompletedProcess(command, 101)

        with patch.dict(os.environ, supplied, clear=True), patch.object(
            fetch.subprocess, "run", side_effect=failed_fetch
        ):
            self.assertEqual(fetch.main(), 101)
        self.assertTrue(files)
        self.assertTrue(all(not path.exists() for path in files))


if __name__ == "__main__":
    unittest.main()
