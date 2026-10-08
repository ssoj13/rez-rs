name = "example_pip"
version = "1.0.0"
description = "Example rez package using Pip build system"
authors = ["rez"]
uuid = "d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1"

tools = ["example_pip"]

def commands():
    env.PATH.append("{root}/bin")
    env.PYTHONPATH.append("{root}/site-packages")
