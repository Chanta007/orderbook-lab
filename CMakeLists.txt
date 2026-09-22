# Optional second build of the same four programs.
# Say this if asked why both exist: Make is what CI runs, including the
# Python tests. This file is only the C++ binaries plus one ctest named
# core. It does not run test_ws_frames.py and it does not run the fixture.
cmake_minimum_required(VERSION 3.16)
project(orderbook-lab LANGUAGES CXX)
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
include_directories(${CMAKE_SOURCE_DIR}/include)
find_package(Threads REQUIRED)
add_executable(feedd src/feedd.cpp)
add_executable(tui src/tui.cpp)
add_executable(headless src/headless.cpp)
add_executable(test_core tests/test_core.cpp)
foreach(t feedd tui headless test_core)
  target_link_libraries(${t} PRIVATE Threads::Threads)
endforeach()
enable_testing()
add_test(NAME core COMMAND test_core)
