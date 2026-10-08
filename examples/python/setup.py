from setuptools import setup, find_packages

setup(
    name="example_python",
    version="1.0.0",
    packages=find_packages(),
    entry_points={
        "console_scripts": [
            "example_python = example_python:main",
        ],
    },
)
