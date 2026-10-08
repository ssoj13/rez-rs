name = "example_extraction_sources"
version = "1.0.0"
description = "Example rez package using Extraction build system (sources.yaml)"
authors = ["rez"]
uuid = "e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"

def commands():
    env.PATH.append("{root}/bin")
    env.REZ_EXAMPLE_EXTRACTION_SOURCES_ROOT = "{root}"
