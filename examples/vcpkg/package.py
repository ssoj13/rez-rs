name = "example_vcpkg"
version = "1.0.0"
description = "Example rez package using vcpkg build system"
authors = ["rez"]
uuid = "c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6"
build_system = "vcpkg"

tools = ["example_vcpkg"]

def commands():
    env.PATH.append("{root}/bin")
