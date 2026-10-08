name = "example_cmake"
version = "1.0.0"
description = "Example rez package using CMake build system"
authors = ["rez"]
uuid = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4"

tools = ["example_cmake"]

def commands():
    env.PATH.append("{root}/bin")
