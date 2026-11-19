# Locate the vcpkg-installed (static) dependencies and expose them as targets.
find_package(PNG REQUIRED)
find_package(TIFF REQUIRED)
find_package(WebP CONFIG REQUIRED)
find_package(nlohmann_json CONFIG REQUIRED)
find_package(doctest CONFIG REQUIRED)
# libavif[aom] (0.5.0): its config pulls libyuv and AOM through find_dependency.
find_package(libavif CONFIG REQUIRED)

# libjxl ships only pkg-config files in this port; mirror libjxl.pc and
# libjxl_cms.pc (JXL_STATIC_DEFINE, JXL_CMS_STATIC_DEFINE, HWY_STATIC_DEFINE).
foreach(lib jxl jxl_cms hwy brotlidec brotlienc brotlicommon lcms2)
  string(TOUPPER "${lib}" var)
  find_library(IAT_${var}_LIB NAMES ${lib} REQUIRED NO_DEFAULT_PATH
               PATHS "${VCPKG_INSTALLED_DIR}/${VCPKG_TARGET_TRIPLET}/lib")
endforeach()

# Guard: every dependency must come from this project's vcpkg tree. PATH on this
# machine also exposes Strawberry's libtiff (tiff.dll + TiffConfig.cmake).
set(_iat_root "${VCPKG_INSTALLED_DIR}/${VCPKG_TARGET_TRIPLET}")
foreach(v PNG_LIBRARY_RELEASE TIFF_LIBRARY_RELEASE ZLIB_LIBRARY_RELEASE WebP_DIR nlohmann_json_DIR doctest_DIR
          PNG_PNG_INCLUDE_DIR TIFF_INCLUDE_DIR libavif_DIR AOM_DIR libyuv_DIR)
  if(NOT DEFINED ${v} OR "${${v}}" STREQUAL "")
    message(FATAL_ERROR "dependency guard: ${v} is not set")
  endif()
  cmake_path(ABSOLUTE_PATH ${v} NORMALIZE OUTPUT_VARIABLE _p)
  string(FIND "${_p}" "${_iat_root}" _pos)
  if(NOT _pos EQUAL 0)
    message(FATAL_ERROR "dependency guard: ${v}=${${v}} is outside ${_iat_root}")
  endif()
endforeach()
if(Tiff_DIR)
  message(FATAL_ERROR "dependency guard: libtiff came from a CMake config package (${Tiff_DIR})")
endif()

add_library(iat_jxl INTERFACE)
add_library(iat::jxl ALIAS iat_jxl)
target_include_directories(iat_jxl INTERFACE "${VCPKG_INSTALLED_DIR}/${VCPKG_TARGET_TRIPLET}/include")
target_compile_definitions(iat_jxl INTERFACE JXL_STATIC_DEFINE JXL_CMS_STATIC_DEFINE HWY_STATIC_DEFINE)
target_link_libraries(iat_jxl INTERFACE
  ${IAT_JXL_LIB} ${IAT_JXL_CMS_LIB} ${IAT_LCMS2_LIB} ${IAT_HWY_LIB}
  ${IAT_BROTLIDEC_LIB} ${IAT_BROTLIENC_LIB} ${IAT_BROTLICOMMON_LIB})
