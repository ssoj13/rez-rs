name = "example_python"
version = "1.0.0"
description = "Example rez package using Python setuptools build system"
authors = ["rez"]
uuid = "c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6"

tools = ["example_python"]

def commands():
    env.PATH.append("{root}/bin")
    env.PYTHONPATH.append("{root}/python")
