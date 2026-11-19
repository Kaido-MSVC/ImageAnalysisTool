# iat-core: the library front ends link (iat CLI now, a GUI later). Report model, JSON
# contract, Markdown renderer, exit-code decision, and one analysis run: read-only inputs,
# hashing, temporary files, iat-worker.exe inside a Job Object, report assembly and
# no-overwrite report writing. Holds no image decoder.
add_library(iat-core STATIC
  common/win_util.cpp
  model/report.cpp
  model/render.cpp
  core/run.cpp
  core/report_io.cpp)
target_include_directories(iat-core PUBLIC ${CMAKE_CURRENT_SOURCE_DIR})
target_link_libraries(iat-core PUBLIC nlohmann_json::nlohmann_json bcrypt)
# Single source of the tool version: project(VERSION) in the top-level CMakeLists.txt.
target_compile_definitions(iat-core PRIVATE IAT_VERSION="${PROJECT_VERSION}")

# iat_engine: container scans, decoders (PNG, TIFF, WebP, JXL, PSD/PSB, AVIF), comparison.
# Linked only into iat-worker.exe, so untrusted input is decoded inside the Job Object.
add_library(iat_engine STATIC
  engine/png_shim.c
  engine/png_scan.cpp
  engine/png_side.cpp
  engine/temp_store.cpp
  engine/tiff_scan.cpp
  engine/tiff_side.cpp
  engine/whole_image.cpp
  engine/webp_scan.cpp
  engine/webp_side.cpp
  engine/jxl_scan.cpp
  engine/jxl_side.cpp
  engine/psd_scan.cpp
  engine/psd_side.cpp
  engine/avif_scan.cpp
  engine/avif_side.cpp
  engine/analyze.cpp)
target_link_libraries(iat_engine PUBLIC iat-core PNG::PNG TIFF::TIFF ZLIB::ZLIB WebP::webp WebP::webpdecoder iat::jxl avif psapi)

# iat.exe (CLI) holds no decoder; iat-worker.exe holds the decoders.
add_executable(iat iat_main.cpp)
target_link_libraries(iat PRIVATE iat-core)

add_executable(iat-worker worker_main.cpp)
target_link_libraries(iat-worker PRIVATE iat_engine)

# Linker maps (bin/<exe>.map) record which static libraries and objects were actually
# linked; tools/gen_third_party.py reads them to build the distributed-components list.
foreach(exe IN ITEMS iat iat-worker)
  target_link_options(${exe} PRIVATE "/MAP:$<TARGET_FILE_DIR:${exe}>/$<TARGET_FILE_BASE_NAME:${exe}>.map")
endforeach()
