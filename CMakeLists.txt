cmake_minimum_required(VERSION 3.22)
project(DeafOverlay LANGUAGES CXX)

# High DPI + Win32 defines for all targets
add_compile_definitions(UNICODE _UNICODE NOMINMAX)

add_subdirectory(DeafOverlay)
