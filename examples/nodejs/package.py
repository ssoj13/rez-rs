name = "example_nodejs"
version = "1.0.0"
description = "Example rez package using Node.js build system"
authors = ["rez"]
uuid = "f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3"

tools = ["example_nodejs"]

def commands():
    env.PATH.append("{root}/bin")
    env.NODE_PATH.append("{root}/node_modules")
