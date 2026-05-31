cmake_minimum_required(VERSION 3.16)
project(aiomsg_cpp_sync CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

find_package(OpenSSL REQUIRED)
find_package(Threads REQUIRED)

add_library(aiomsg STATIC src/protocol.cpp src/socket.cpp)
target_include_directories(aiomsg PUBLIC include)
target_include_directories(aiomsg PRIVATE src)
target_link_libraries(aiomsg PUBLIC OpenSSL::SSL OpenSSL::Crypto Threads::Threads)
target_compile_options(aiomsg PRIVATE -Wall -Wextra)

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
