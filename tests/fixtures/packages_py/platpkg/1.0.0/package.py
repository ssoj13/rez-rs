import sys
name = "platpkg"
version = "1.0.0"
if sys.platform == "win32":
    requires = ["msvc"]
else:
    requires = ["gcc"]
