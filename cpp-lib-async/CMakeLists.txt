cmake_minimum_required(VERSION 3.16)
project(aiomsg_cpp_async
    VERSION 1.0.0
    DESCRIPTION "Native C++ (Asio coroutines) implementation of the aiomsg protocol"
    LANGUAGES CXX)

if(CMAKE_SOURCE_DIR STREQUAL CMAKE_CURRENT_SOURCE_DIR)
    set(AIOMSG_IS_TOP_LEVEL ON)
else()
    set(AIOMSG_IS_TOP_LEVEL OFF)
endif()

option(AIOMSG_BUILD_EXAMPLES "Build the runnable examples" ${AIOMSG_IS_TOP_LEVEL})
option(AIOMSG_BUILD_TESTS "Build the test suite" ${AIOMSG_IS_TOP_LEVEL})

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

include(GNUInstallDirs)

find_package(OpenSSL REQUIRED)
find_package(Threads REQUIRED)

# Standalone Asio is header-only and not packaged on most systems, so vendor it.
# A system copy (e.g. /usr/include/asio.hpp) is used if present. The include
# directory is a build-time detail of this port; consumers of the installed
# library supply their own Asio (the CMake package config locates it for them).
find_path(ASIO_INCLUDE_DIR asio.hpp)
if(NOT ASIO_INCLUDE_DIR)
    include(FetchContent)
    FetchContent_Declare(
        asio
        GIT_REPOSITORY https://github.com/chriskohlhoff/asio.git
        GIT_TAG asio-1-30-2
        GIT_SHALLOW TRUE
    )
    FetchContent_MakeAvailable(asio)
    set(ASIO_INCLUDE_DIR ${asio_SOURCE_DIR}/asio/include)
endif()

# --- the library ----------------------------------------------------------
add_library(aiomsg STATIC src/protocol.cpp src/socket.cpp src/ws.cpp)
add_library(aiomsg::aiomsg ALIAS aiomsg)
target_include_directories(aiomsg PUBLIC
    "$<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>"
    "$<INSTALL_INTERFACE:${CMAKE_INSTALL_INCLUDEDIR}>")
# Asio's headers are needed by callers too, but only as a build-tree path here;
# the installed package config re-discovers Asio on the consumer's machine.
target_include_directories(aiomsg PUBLIC "$<BUILD_INTERFACE:${ASIO_INCLUDE_DIR}>")
target_include_directories(aiomsg PRIVATE src)
target_compile_definitions(aiomsg PUBLIC ASIO_STANDALONE ASIO_NO_DEPRECATED)
target_link_libraries(aiomsg PUBLIC OpenSSL::SSL OpenSSL::Crypto Threads::Threads)
target_compile_options(aiomsg PRIVATE -Wall -Wextra)
set_target_properties(aiomsg PROPERTIES VERSION ${PROJECT_VERSION})

# --- examples --------------------------------------------------------------
if(AIOMSG_BUILD_EXAMPLES)
    foreach(ex server client tls)
        add_executable(${ex} examples/${ex}.cpp)
        target_link_libraries(${ex} PRIVATE aiomsg)
    endforeach()
    add_executable(conformance_agent examples/conformance_agent.cpp)
    target_link_libraries(conformance_agent PRIVATE aiomsg)
endif()

# --- tests -----------------------------------------------------------------
if(AIOMSG_BUILD_TESTS)
    enable_testing()

    add_executable(test_protocol tests/test_protocol.cpp)
    target_link_libraries(test_protocol PRIVATE aiomsg)
    target_include_directories(test_protocol PRIVATE src)
    add_test(NAME test_protocol COMMAND test_protocol)

    add_executable(test_integration tests/test_integration.cpp)
    target_link_libraries(test_integration PRIVATE aiomsg)
    target_compile_definitions(test_integration PRIVATE
        CERT_DIR="${CMAKE_CURRENT_SOURCE_DIR}/../conformance/certs")
    add_test(NAME test_integration COMMAND test_integration)

    add_executable(test_ws tests/test_ws.cpp src/ws.cpp)
    target_link_libraries(test_ws PRIVATE OpenSSL::Crypto)
    target_include_directories(test_ws PRIVATE src)
    add_test(NAME test_ws COMMAND test_ws)
endif()

# --- install + packaging ---------------------------------------------------
install(TARGETS aiomsg EXPORT aiomsgTargets
    ARCHIVE DESTINATION ${CMAKE_INSTALL_LIBDIR})
install(DIRECTORY include/aiomsg DESTINATION ${CMAKE_INSTALL_INCLUDEDIR})

install(EXPORT aiomsgTargets
    FILE aiomsgTargets.cmake
    NAMESPACE aiomsg::
    DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/aiomsg)

include(CMakePackageConfigHelpers)
configure_package_config_file(
    "${CMAKE_CURRENT_SOURCE_DIR}/cmake/aiomsgConfig.cmake.in"
    "${CMAKE_CURRENT_BINARY_DIR}/aiomsgConfig.cmake"
    INSTALL_DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/aiomsg)
write_basic_package_version_file(
    "${CMAKE_CURRENT_BINARY_DIR}/aiomsgConfigVersion.cmake"
    VERSION ${PROJECT_VERSION}
    COMPATIBILITY SameMajorVersion)
install(FILES
    "${CMAKE_CURRENT_BINARY_DIR}/aiomsgConfig.cmake"
    "${CMAKE_CURRENT_BINARY_DIR}/aiomsgConfigVersion.cmake"
    DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/aiomsg)
