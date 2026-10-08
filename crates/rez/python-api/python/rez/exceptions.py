# SPDX-License-Identifier: Apache-2.0
"""Exceptions raised by the native backend."""
from .rs import (
    RezError, VersionError, PackageFamilyNotFoundError, PackageNotFoundError,
    PackageMetadataError, ResolvedContextError,
)
