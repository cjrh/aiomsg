cmake_minimum_required(VERSION 3.16)
project(aiomsg_c
    VERSION 2026.7.1
    DESCRIPTION "Native C implementation of the aiomsg protocol"
    LANGUAGES C)

# Treat as top-level (build examples/tests by default) only when not a subproject.
if(CMAKE_SOURCE_DIR STREQUAL CMAKE_CURRENT_SOURCE_DIR)
    set(AIOMSG_IS_TOP_LEVEL ON)
else()
    set(AIOMSG_IS_TOP_LEVEL OFF)
endif()

option(AIOMSG_BUILD_EXAMPLES "Build the runnable examples" ${AIOMSG_IS_TOP_LEVEL})
option(AIOMSG_BUILD_TESTS "Build the test suite" ${AIOMSG_IS_TOP_LEVEL})

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)

include(GNUInstallDirs)

find_package(OpenSSL REQUIRED)
find_package(Threads REQUIRED)

# --- the library ----------------------------------------------------------
add_library(aiomsg STATIC src/protocol.c src/aiomsg.c src/ws.c)
add_library(aiomsg::aiomsg ALIAS aiomsg)
target_include_directories(aiomsg PUBLIC
    "$<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>"
    "$<INSTALL_INTERFACE:${CMAKE_INSTALL_INCLUDEDIR}>")
target_include_directories(aiomsg PRIVATE src)
target_link_libraries(aiomsg PUBLIC OpenSSL::SSL OpenSSL::Crypto Threads::Threads)
target_compile_options(aiomsg PRIVATE -Wall -Wextra)
set_target_properties(aiomsg PROPERTIES VERSION ${PROJECT_VERSION})

# --- examples --------------------------------------------------------------
if(AIOMSG_BUILD_EXAMPLES)
    foreach(ex server client tls)
        add_executable(${ex} examples/${ex}.c)
        target_link_libraries(${ex} PRIVATE aiomsg)
    endforeach()
    add_executable(conformance_agent examples/conformance_agent.c)
    target_link_libraries(conformance_agent PRIVATE aiomsg)
endif()

# --- tests -----------------------------------------------------------------
if(AIOMSG_BUILD_TESTS)
    enable_testing()

    add_executable(test_protocol tests/test_protocol.c src/protocol.c)
    target_include_directories(test_protocol PRIVATE src)
    target_compile_options(test_protocol PRIVATE -Wall -Wextra)
    add_test(NAME protocol COMMAND test_protocol)

    add_executable(test_ws tests/test_ws.c src/ws.c src/protocol.c)
    target_include_directories(test_ws PRIVATE src)
    target_link_libraries(test_ws PRIVATE OpenSSL::SSL OpenSSL::Crypto)
    target_compile_options(test_ws PRIVATE -Wall -Wextra)
    add_test(NAME ws COMMAND test_ws)

    add_executable(test_integration tests/test_integration.c)
    target_link_libraries(test_integration PRIVATE aiomsg)
    target_compile_definitions(test_integration PRIVATE
        CERT_DIR="${CMAKE_CURRENT_SOURCE_DIR}/../conformance/certs")
    add_test(NAME integration COMMAND test_integration)
endif()

# --- install + packaging ---------------------------------------------------
# Lets consumers do `find_package(aiomsg)` (CMake) or `pkg-config --cflags aiomsg`.
install(TARGETS aiomsg EXPORT aiomsgTargets
    ARCHIVE DESTINATION ${CMAKE_INSTALL_LIBDIR})
install(FILES include/aiomsg.h DESTINATION ${CMAKE_INSTALL_INCLUDEDIR})

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

configure_file(
    "${CMAKE_CURRENT_SOURCE_DIR}/cmake/aiomsg.pc.in"
    "${CMAKE_CURRENT_BINARY_DIR}/aiomsg.pc" @ONLY)
install(FILES "${CMAKE_CURRENT_BINARY_DIR}/aiomsg.pc"
    DESTINATION ${CMAKE_INSTALL_LIBDIR}/pkgconfig)
