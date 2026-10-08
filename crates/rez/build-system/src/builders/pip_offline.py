# SPDX-License-Identifier: Apache-2.0
"""Managed pip transport policy; arbitrary build backends are not sandboxed."""
def _rez_apply_pip_offline():
    import os
    if os.environ.get("REZ_OFFLINE") != "true":
        return
    from pip._internal.network.session import PipSession
    from pip._internal.vcs.versioncontrol import VersionControl
    original_request = PipSession.request

    def request(self, method, url, *args, **kwargs):
        if not str(url).startswith("file://"):
            raise RuntimeError("REZ_OFFLINE=true: remote pip requests are disabled: " + str(url))
        return original_request(self, method, url, *args, **kwargs)

    def vcs_command(cls, *args, **kwargs):
        raise RuntimeError("REZ_OFFLINE=true: pip VCS commands are disabled")

    PipSession.request = request
    VersionControl.run_command = classmethod(vcs_command)
