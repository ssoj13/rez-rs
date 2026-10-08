#!/usr/bin/env python
"""Custom build script for example_custom."""
import os
import sys

def main():
    # REZ_BUILD_INSTALL_PATH or argv[1] when {install} is substituted
    install = sys.argv[1] if len(sys.argv) > 1 else os.environ.get("REZ_BUILD_INSTALL_PATH", "install")
    root = os.environ.get("REZ_BUILD_SOURCE_PATH", os.path.dirname(os.path.abspath(__file__)))
    bin_dir = os.path.join(install, "bin")
    os.makedirs(bin_dir, exist_ok=True)
    script = os.path.join(bin_dir, "example_custom")
    with open(script, "w") as f:
        f.write("""#!/usr/bin/env python
print("Hello from example_custom (Custom build)")
""")
    os.chmod(script, 0o755)
    print("Built example_custom ->", install)

if __name__ == "__main__":
    main()
