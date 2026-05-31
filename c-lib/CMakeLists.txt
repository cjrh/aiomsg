cmake_minimum_required(VERSION 3.16)
project(aiomsg_c C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)

find_package(OpenSSL REQUIRED)
find_package(Threads REQUIRED)

add_library(aiomsg STATIC src/protocol.c src/aiomsg.c)
target_include_directories(aiomsg PUBLIC include PRIVATE src)
target_link_libraries(aiomsg PUBLIC OpenSSL::SSL OpenSSL::Crypto Threads::Threads)
target_compile_options(aiomsg PRIVATE -Wall -Wextra)

enable_testing()

add_executable(test_protocol tests/test_protocol.c src/protocol.c)
target_include_directories(test_protocol PRIVATE src)
target_compile_options(test_protocol PRIVATE -Wall -Wextra)
add_test(NAME protocol COMMAND test_protocol)

add_executable(test_integration tests/test_integration.c)
target_link_libraries(test_integration PRIVATE aiomsg)
target_compile_definitions(test_integration PRIVATE
    CERT_DIR="${CMAKE_CURRENT_SOURCE_DIR}/../conformance/certs")
add_test(NAME integration COMMAND test_integration)

add_executable(conformance_agent examples/conformance_agent.c)
target_link_libraries(conformance_agent PRIVATE aiomsg)
