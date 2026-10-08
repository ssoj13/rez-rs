name = "example_scons"
version = "1.0.0"
description = "Example rez package using SCons build system"
authors = ["rez"]
uuid = "b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5"

tools = ["example_scons"]

def commands():
    env.PATH.append("{root}/bin")
