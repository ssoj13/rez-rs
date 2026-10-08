name = "example_custom"
version = "1.0.0"
description = "Example rez package using Custom build system"
authors = ["rez"]
uuid = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4"

tools = ["example_custom"]

# Кастомная команда сборки — имеет приоритет над автоопределением
# Use build.py; {root} and {install} are expanded by rez
build_command = "python {root}/build.py {install}"

def commands():
    env.PATH.append("{root}/bin")
