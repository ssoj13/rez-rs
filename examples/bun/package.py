name = "example_bun"
version = "1.0.0"
description = "Example rez package using Bun build system"
authors = ["rez"]
uuid = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4"

tools = ["example_bun"]

def commands():
    env.PATH.append("{root}/bin")
