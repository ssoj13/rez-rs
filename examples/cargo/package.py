name = "example_cargo"
version = "1.0.0"
description = "Example rez package using Cargo (Rust) build system"
authors = ["rez"]
uuid = "e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"

tools = ["example_cargo"]

def commands():
    env.PATH.append("{root}/bin")
