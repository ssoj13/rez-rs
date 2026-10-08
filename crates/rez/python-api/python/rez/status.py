# SPDX-License-Identifier: Apache-2.0
from enum import Enum


class ResolverStatus(Enum):
    pending = 0
    solved = 1
    failed = 2
    aborted = 3
    unsolved = 4
