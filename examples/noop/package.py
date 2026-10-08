name = "example_noop"
version = "1.0.0"
description = "Example rez package with NoOp build system (no build)"
authors = ["rez"]
uuid = "b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5"

def commands():
    env.REZ_EXAMPLE_NOOP = "1"
