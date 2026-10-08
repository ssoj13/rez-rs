# SPDX-License-Identifier: Apache-2.0
"""Read the canonical configuration captured on the first native configuration access."""
from . import rs


class _Config:
    def __setattr__(self, name, value):
        raise NotImplementedError("Runtime config overrides are not supported; set REZ_CONFIG_FILE before access")

    def __getattr__(self, name):
        values = rs.config_snapshot()
        try:
            return values[name]
        except KeyError:
            raise AttributeError(name) from None

    def override(self, key, value):
        raise NotImplementedError("Configure the native runtime before import with REZ_CONFIG_FILE")


config = _Config()
