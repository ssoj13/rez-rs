from conan import ConanFile
from conan.tools.cmake import CMake, cmake_layout


class ExampleConanConan(ConanFile):
    name = "example_conan"
    version = "1.0.0"
    settings = "os", "compiler", "build_type", "arch"
    generators = "CMakeDeps", "CMakeToolchain"

    def layout(self):
        cmake_layout(self)

    def build(self):
        cmake = CMake(self)
        cmake.configure()
        cmake.build()
