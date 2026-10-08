cmake_minimum_required(VERSION 3.10)
project(example_vcpkg)
add_executable(example_vcpkg src/main.cpp)
install(TARGETS example_vcpkg RUNTIME DESTINATION bin)
