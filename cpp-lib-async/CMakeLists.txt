cmake_minimum_required(VERSION 3.16)
project(aiomsg_cpp_async CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

find_package(OpenSSL REQUIRED)
find_package(Threads REQUIRED)

# Standalone Asio is header-only and not packaged on most systems, so vendor it.
# A system copy (e.g. /usr/include/asio.hpp) is used if present.
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

add_library(aiomsg STATIC src/protocol.cpp src/socket.cpp)
target_include_directories(aiomsg PUBLIC include)
target_include_directories(aiomsg PRIVATE src ${ASIO_INCLUDE_DIR})
target_compile_definitions(aiomsg PUBLIC ASIO_STANDALONE ASIO_NO_DEPRECATED)
target_link_libraries(aiomsg PUBLIC OpenSSL::SSL OpenSSL::Crypto Threads::Threads)
target_compile_options(aiomsg PRIVATE -Wall -Wextra)
# Asio's coroutine + SSL headers are needed by callers (examples/tests), so
# expose the include dir on the public interface too.
target_include_directories(aiomsg PUBLIC ${ASIO_INCLUDE_DIR})

add_executable(conformance_agent examples/conformance_agent.cpp)
target_link_libraries(conformance_agent PRIVATE aiomsg)

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
