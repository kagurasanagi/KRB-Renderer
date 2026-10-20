cmake_minimum_required(VERSION 3.25)

# STEP 変換器 DLL（krbr_step）だけをビルドする。本体（krbr.exe）は Rust 版（rust/）で、cargo build で作る。
# OpenCASCADE は C++ のライブラリなので、STEP の読み込みと三角形分割はこの DLL の中で行い、
# Rust 版は実行時に dll\step\krbr_step.dll（開発中は build/release/step/krbr_step.dll）を読み込む

# vcpkg（マニフェストモード）。release のみをビルドする独自トリプレットを使う
if(NOT DEFINED CMAKE_TOOLCHAIN_FILE AND DEFINED ENV{VCPKG_ROOT})
  set(CMAKE_TOOLCHAIN_FILE "$ENV{VCPKG_ROOT}/scripts/buildsystems/vcpkg.cmake" CACHE STRING "")
endif()
set(VCPKG_OVERLAY_TRIPLETS "${CMAKE_CURRENT_SOURCE_DIR}/triplets" CACHE STRING "")
set(VCPKG_TARGET_TRIPLET "x64-windows-rel" CACHE STRING "")
set(VCPKG_HOST_TRIPLET "x64-windows-rel" CACHE STRING "")

project(krbr_step VERSION 0.1.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)

find_package(glm CONFIG REQUIRED)
find_package(OpenCASCADE CONFIG REQUIRED)

# DLL と OpenCASCADE の DLL 一式（vcpkg が依存の DLL を横にコピーする）を build/release/step/ にまとめる
add_library(krbr_step SHARED step_dll/krbr_step.cpp step_dll/mesh_file.cpp)
target_include_directories(krbr_step PRIVATE step_dll ${OpenCASCADE_INCLUDE_DIR})
target_compile_definitions(krbr_step PRIVATE KRBR_STEP_EXPORTS NOMINMAX WIN32_LEAN_AND_MEAN _CRT_SECURE_NO_WARNINGS)
target_link_libraries(krbr_step PRIVATE glm::glm TKDESTEP TKXCAF TKLCAF TKCAF TKRWMesh TKMesh TKBRep TKTopAlgo TKMath TKernel TKDE
                                        TKXSBase TKG3d TKGeomBase)
set_target_properties(krbr_step PROPERTIES RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/step"
                                           LIBRARY_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/step")
if(MSVC)
  target_compile_options(krbr_step PRIVATE /utf-8 /W3 /permissive- /EHsc /Zc:__cplusplus)
endif()
