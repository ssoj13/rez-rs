cmake_minimum_required(VERSION 3.10)
project(example_cmake CXX)

set(CMAKE_RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin)

add_executable(example_cmake main.cpp)
install(TARGETS example_cmake RUNTIME DESTINATION bin)
