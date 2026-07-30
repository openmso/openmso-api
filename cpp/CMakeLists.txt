# SPDX-License-Identifier: Apache-2.0
cmake_minimum_required(VERSION 3.21)
project(openmso-cpp VERSION 0.2.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

find_package(Protobuf REQUIRED)
find_package(nng CONFIG REQUIRED)

# proto_path is the directory holding the .proto files, matching the Rust
# build.rs, because the schema imports its siblings unqualified. protoc's
# output is therefore flat; the omso/capture/v1 prefix below is ours, so
# consumers include <omso/capture/v1/...> rather than a bare capture.pb.h that
# would collide with anything else on the include path.
get_filename_component(OPENMSO_PROTO_DIR
    "${CMAKE_CURRENT_SOURCE_DIR}/../proto/omso/capture/v1" ABSOLUTE)
set(OPENMSO_GEN_DIR "${CMAKE_CURRENT_BINARY_DIR}/generated")
set(OPENMSO_GEN_SUBDIR "${OPENMSO_GEN_DIR}/omso/capture/v1")

file(GLOB OPENMSO_PROTO_FILES CONFIGURE_DEPENDS "${OPENMSO_PROTO_DIR}/*.proto")
if(NOT OPENMSO_PROTO_FILES)
    message(FATAL_ERROR "no .proto files in ${OPENMSO_PROTO_DIR}")
endif()

set(OPENMSO_PROTO_SRCS)
set(OPENMSO_PROTO_HDRS)
foreach(proto IN LISTS OPENMSO_PROTO_FILES)
    get_filename_component(name "${proto}" NAME_WE)
    list(APPEND OPENMSO_PROTO_SRCS "${OPENMSO_GEN_SUBDIR}/${name}.pb.cc")
    list(APPEND OPENMSO_PROTO_HDRS "${OPENMSO_GEN_SUBDIR}/${name}.pb.h")
endforeach()

add_custom_command(
    OUTPUT ${OPENMSO_PROTO_SRCS} ${OPENMSO_PROTO_HDRS}
    COMMAND ${CMAKE_COMMAND} -E make_directory "${OPENMSO_GEN_SUBDIR}"
    COMMAND protobuf::protoc
            --proto_path=${OPENMSO_PROTO_DIR}
            --cpp_out=${OPENMSO_GEN_SUBDIR}
            ${OPENMSO_PROTO_FILES}
    DEPENDS ${OPENMSO_PROTO_FILES} protobuf::protoc
    COMMENT "protoc omso.capture.v1"
    VERBATIM
)

# Umbrella header, so which .proto a message is declared in stays an internal
# detail of the schema.
set(OPENMSO_UMBRELLA "// generated\n#pragma once\n")
foreach(hdr IN LISTS OPENMSO_PROTO_HDRS)
    get_filename_component(name "${hdr}" NAME)
    string(APPEND OPENMSO_UMBRELLA "#include <omso/capture/v1/${name}>\n")
endforeach()
file(GENERATE OUTPUT "${OPENMSO_GEN_SUBDIR}/omso.pb.h" CONTENT "${OPENMSO_UMBRELLA}")

add_library(openmso-proto STATIC ${OPENMSO_PROTO_SRCS} ${OPENMSO_PROTO_HDRS})
target_include_directories(openmso-proto PUBLIC
    "${OPENMSO_GEN_DIR}"
    ${Protobuf_INCLUDE_DIRS}
)
target_link_libraries(openmso-proto PUBLIC ${Protobuf_LIBRARIES})
set_target_properties(openmso-proto PROPERTIES POSITION_INDEPENDENT_CODE ON)

add_library(openmso STATIC
    src/client.cpp
    src/encoding.cpp
    src/error.cpp
    src/manifest.cpp
    src/transport.cpp
)

target_include_directories(openmso PUBLIC
    "${CMAKE_CURRENT_SOURCE_DIR}/include"
)

target_link_libraries(openmso PUBLIC
    openmso-proto
    nng::nng
)

set_target_properties(openmso PROPERTIES POSITION_INDEPENDENT_CODE ON)

add_library(openmso::openmso ALIAS openmso)

if(PROJECT_IS_TOP_LEVEL)
    enable_testing()
    add_subdirectory(tests)
endif()
