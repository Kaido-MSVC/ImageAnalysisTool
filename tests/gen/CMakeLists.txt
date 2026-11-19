# Test-image generators used by tools/gen_fixtures.py (--codec-bin) and tests/e2e/run_e2e.py.
# They only encode or write files from known ground truth; they never decode. Not shipped.
add_executable(gen_codec_fixture gen_codec_fixture.cpp)
target_link_libraries(gen_codec_fixture PRIVATE iat::jxl WebP::webp avif)

add_executable(gen_large_png gen_large_png.cpp)
target_link_libraries(gen_large_png PRIVATE ZLIB::ZLIB)

add_executable(gen_large_tiff gen_large_tiff.cpp)
target_link_libraries(gen_large_tiff PRIVATE ZLIB::ZLIB)

foreach(t gen_codec_fixture gen_large_png gen_large_tiff)
  target_link_libraries(${t} PRIVATE psapi)
endforeach()
