# SPDX-License-Identifier: Apache-2.0

foreach(name encoding transport manifest)
    add_executable(test_${name} test_${name}.cpp)
    target_link_libraries(test_${name} PRIVATE openmso)
    target_include_directories(test_${name} PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}")
    add_test(NAME ${name} COMMAND test_${name})
endforeach()

# Drives the Rust demo plugin, so it is skipped unless one has been built.
set(OPENMSO_DEMO_PLUGIN "" CACHE FILEPATH
    "demo plugin binary for the cross-implementation session test")

add_executable(test_session test_session.cpp)
target_link_libraries(test_session PRIVATE openmso)
target_include_directories(test_session PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}")
add_test(NAME session COMMAND test_session "${OPENMSO_DEMO_PLUGIN}")
set_tests_properties(session PROPERTIES SKIP_RETURN_CODE 77)
