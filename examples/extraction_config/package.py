name = "example_extraction_config"
version = "1.0.0"
description = "Example rez package using Extraction build system (package.config)"
authors = ["rez"]
uuid = "f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3"
build_system = "extraction"

config = {
    "extraction": {
        "downloads": [
            {"path": "archive.zip"}
        ]
    }
}

def commands():
    env.PATH.append("{root}/bin")
    env.REZ_EXAMPLE_EXTRACTION_CONFIG_ROOT = "{root}"
