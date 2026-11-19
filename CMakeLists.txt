cmake_minimum_required(VERSION 3.28)
project(ImageAnalysisTool VERSION 1.0.0 LANGUAGES C CXX)

if(NOT MSVC)
  message(FATAL_ERROR "This project is built with MSVC only.")
endif()

# After compiler detection: never resolve packages from PATH-derived prefixes,
# package registries or system paths (PATH here contains Strawberry's libtiff).
set(CMAKE_FIND_USE_SYSTEM_ENVIRONMENT_PATH OFF)
set(CMAKE_FIND_USE_PACKAGE_REGISTRY OFF)
set(CMAKE_FIND_USE_SYSTEM_PACKAGE_REGISTRY OFF)
set(CMAKE_FIND_USE_CMAKE_SYSTEM_PATH OFF)

set(CMAKE_C_STANDARD 17)
set(CMAKE_CXX_STANDARD 23)  # standard only; no C++23-specific language or library features are used
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
# The project uses no C++ modules: skip CMake's module dependency scanning and the
# synthesized `import std` targets CMake 4.5 otherwise adds for C++20 and later.
set(CMAKE_CXX_SCAN_FOR_MODULES OFF)
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/bin")

add_compile_options(/utf-8 /W4 /permissive- $<$<COMPILE_LANGUAGE:CXX>:/Zc:__cplusplus> /Zi)
add_link_options(/DEBUG:FULL /OPT:REF /OPT:ICF)
add_compile_definitions(UNICODE _UNICODE NOMINMAX WIN32_LEAN_AND_MEAN _CRT_SECURE_NO_WARNINGS)

include(cmake/IatDeps.cmake)

add_subdirectory(src)
add_subdirectory(tests/gen)
# Stage-0 feasibility probes: kept only in the private development archive.
if(EXISTS ${CMAKE_SOURCE_DIR}/probes/CMakeLists.txt)
  add_subdirectory(probes)
endif()

enable_testing()
add_executable(iat_unit tests/unit/test_core.cpp)
target_link_libraries(iat_unit PRIVATE iat_engine doctest::doctest)
target_compile_definitions(iat_unit PRIVATE IAT_EXPECTED_VERSION="${PROJECT_VERSION}")
add_test(NAME unit COMMAND iat_unit)
# The Python launcher is looked up on PATH explicitly: CMAKE_FIND_USE_SYSTEM_ENVIRONMENT_PATH
# is OFF above so that libraries never resolve outside vcpkg, which would otherwise hide py.exe.
find_program(IAT_PY NAMES py PATHS ENV PATH NO_DEFAULT_PATH NO_CMAKE_FIND_ROOT_PATH
             DOC "Python launcher (py.exe) for the e2e test")
if(IAT_PY)
  set(IAT_FIXTURE_DIR ${CMAKE_SOURCE_DIR}/tests/fixtures/generated)
  add_test(NAME fixtures COMMAND ${IAT_PY} -3.13 -I ${CMAKE_SOURCE_DIR}/tools/gen_fixtures.py
           ${IAT_FIXTURE_DIR} --codec-bin ${CMAKE_BINARY_DIR}/bin)
  set_tests_properties(fixtures PROPERTIES FIXTURES_SETUP iat_fixtures)
  add_test(NAME e2e COMMAND ${IAT_PY} -3.13 -I ${CMAKE_SOURCE_DIR}/tests/e2e/run_e2e.py
           ${CMAKE_BINARY_DIR}/bin ${IAT_FIXTURE_DIR} ${CMAKE_BINARY_DIR}/e2e)
  set_tests_properties(e2e PROPERTIES FIXTURES_REQUIRED iat_fixtures)
else()
  message(WARNING "py.exe not found on PATH: the fixtures and e2e tests are not registered")
endif()
