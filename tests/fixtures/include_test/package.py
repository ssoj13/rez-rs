name = "test_include"
version = "1.0.0"

# Include common configuration
include("common")

# Use values from included module
authors = [default_author]
help = default_license
requires = common_requires + ["numpy"]

tools = ["test_tool"]
