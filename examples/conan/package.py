name = "example_conan"
version = "1.0.0"
description = "Example rez package using Conan build system"
authors = ["rez"]
uuid = "d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1"
build_system = "conan"

tools = ["example_conan"]

def commands():
    env.PATH.append("{root}/bin")
