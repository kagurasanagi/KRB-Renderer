cmake_minimum_required(VERSION 3.25)

# vcpkg（マニフェストモード）。release のみをビルドする独自トリプレットを使う
if(NOT DEFINED CMAKE_TOOLCHAIN_FILE AND DEFINED ENV{VCPKG_ROOT})
  set(CMAKE_TOOLCHAIN_FILE "$ENV{VCPKG_ROOT}/scripts/buildsystems/vcpkg.cmake" CACHE STRING "")
endif()
set(VCPKG_OVERLAY_TRIPLETS "${CMAKE_CURRENT_SOURCE_DIR}/triplets" CACHE STRING "")
set(VCPKG_TARGET_TRIPLET "x64-windows-rel" CACHE STRING "")
set(VCPKG_HOST_TRIPLET "x64-windows-rel" CACHE STRING "")

project(krbr VERSION 0.1.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)

find_package(VulkanHeaders CONFIG REQUIRED)
find_package(volk CONFIG REQUIRED)
find_package(VulkanMemoryAllocator CONFIG REQUIRED)
find_package(tomlplusplus CONFIG REQUIRED)
find_package(nlohmann_json CONFIG REQUIRED)
find_package(tinyexr CONFIG REQUIRED)
find_package(clipper2 CONFIG REQUIRED)
find_package(CLI11 CONFIG REQUIRED)
find_package(glm CONFIG REQUIRED)
find_package(glfw3 CONFIG REQUIRED)
find_package(imgui CONFIG REQUIRED)
find_path(STB_INCLUDE_DIRS "stb_image.h" REQUIRED)
find_path(EARCUT_HPP_INCLUDE_DIRS "mapbox/earcut.hpp" REQUIRED)

# STEP 読み込み：OpenCASCADE は変換器 DLL（krbr_step、build/release/step/）の中だけで使う。
# 見つからない場合は DLL を作らず、本体は STEP を「読めない」扱いにして代替表示で動く
find_package(OpenCASCADE CONFIG QUIET)

find_program(GLSLC glslc HINTS "$ENV{VULKAN_SDK}/Bin" REQUIRED)

# ---------------------------------------------------------------- shaders
set(SHADER_SRC_DIR "${CMAKE_CURRENT_SOURCE_DIR}/shaders")
set(SHADER_OUT_DIR "${CMAKE_BINARY_DIR}/shaders")
file(GLOB SHADER_INCLUDES CONFIGURE_DEPENDS "${SHADER_SRC_DIR}/*.glsl")
set(SHADER_SOURCES "${SHADER_SRC_DIR}/pathtrace.comp" "${SHADER_SRC_DIR}/tonemap.comp"
                   "${SHADER_SRC_DIR}/denoise_prep.comp" "${SHADER_SRC_DIR}/denoise_blend.comp" "${SHADER_SRC_DIR}/vcm_light.comp"
                   "${SHADER_SRC_DIR}/vcm_camera.comp")
# 積分器（レイを飛ばすもの）は、ray query 版（.comp.spv）に加えて ray tracing pipeline 版（.rgen.spv）も作る。
# ray_query を持たない GPU（GTX 10 系など）では後者と rt_*.spv を使う
set(SHADER_RAY_SOURCES "${SHADER_SRC_DIR}/pathtrace.comp" "${SHADER_SRC_DIR}/vcm_light.comp"
                       "${SHADER_SRC_DIR}/vcm_camera.comp")
set(SHADER_RT_STAGE_SOURCES "${SHADER_SRC_DIR}/rt_hit.rchit" "${SHADER_SRC_DIR}/rt_miss.rmiss"
                            "${SHADER_SRC_DIR}/rt_shadow.rmiss")
set(SHADER_OUTPUTS)
foreach(src ${SHADER_SOURCES} ${SHADER_RT_STAGE_SOURCES})
  get_filename_component(name ${src} NAME)
  set(out "${SHADER_OUT_DIR}/${name}.spv")
  add_custom_command(
    OUTPUT ${out}
    COMMAND ${CMAKE_COMMAND} -E make_directory ${SHADER_OUT_DIR}
    COMMAND ${GLSLC} --target-env=vulkan1.3 -O -I ${SHADER_SRC_DIR} -o ${out} ${src}
    DEPENDS ${src} ${SHADER_INCLUDES}
    COMMENT "glslc ${name}"
    VERBATIM)
  list(APPEND SHADER_OUTPUTS ${out})
endforeach()
foreach(src ${SHADER_RAY_SOURCES})
  get_filename_component(base ${src} NAME_WE)
  set(out "${SHADER_OUT_DIR}/${base}.rgen.spv")
  add_custom_command(
    OUTPUT ${out}
    COMMAND ${CMAKE_COMMAND} -E make_directory ${SHADER_OUT_DIR}
    COMMAND ${GLSLC} --target-env=vulkan1.3 -O -fshader-stage=rgen -DKRBR_RT_PIPELINE=1 -I ${SHADER_SRC_DIR} -o ${out} ${src}
    DEPENDS ${src} ${SHADER_INCLUDES}
    COMMENT "glslc ${base}.rgen (ray tracing pipeline)"
    VERBATIM)
  list(APPEND SHADER_OUTPUTS ${out})
endforeach()
add_custom_target(krbr_shaders DEPENDS ${SHADER_OUTPUTS})

# ---------------------------------------------------------------- krbr_core（本体とテストで共有）
file(GLOB_RECURSE KRBR_SOURCES CONFIGURE_DEPENDS "${CMAKE_CURRENT_SOURCE_DIR}/src/*.cpp")
list(REMOVE_ITEM KRBR_SOURCES "${CMAKE_CURRENT_SOURCE_DIR}/src/main.cpp"
                              "${CMAKE_CURRENT_SOURCE_DIR}/src/model/mesh_file.cpp"
                              "${CMAKE_CURRENT_SOURCE_DIR}/src/step_dll/krbr_step.cpp")
# .krbm の読み書き（本体と変換器 DLL で共有。他に依存しない）
add_library(krbr_meshfile STATIC src/model/mesh_file.cpp)
target_include_directories(krbr_meshfile PUBLIC src)
target_link_libraries(krbr_meshfile PUBLIC glm::glm)
if(MSVC)
  target_compile_options(krbr_meshfile PUBLIC /utf-8 /W3 /permissive- /EHsc /Zc:__cplusplus)
endif()
# ImGui のバックエンドは volk を使うよう自前でビルドする（vcpkg 版は Vulkan ローダーを直接リンクするため）
set(IMGUI_BACKEND_SOURCES
  "${CMAKE_CURRENT_SOURCE_DIR}/third_party/imgui_backends/imgui_impl_glfw.cpp"
  "${CMAKE_CURRENT_SOURCE_DIR}/third_party/imgui_backends/imgui_impl_vulkan.cpp")
add_library(krbr_core STATIC ${KRBR_SOURCES} ${IMGUI_BACKEND_SOURCES})
add_dependencies(krbr_core krbr_shaders)
target_include_directories(krbr_core PUBLIC src third_party/imgui_backends ${STB_INCLUDE_DIRS} ${EARCUT_HPP_INCLUDE_DIRS})
target_link_libraries(krbr_core PUBLIC
  krbr_meshfile Vulkan::Headers volk::volk GPUOpen::VulkanMemoryAllocator
  tomlplusplus::tomlplusplus nlohmann_json::nlohmann_json
  unofficial::tinyexr::tinyexr Clipper2::Clipper2 CLI11::CLI11 glm::glm glfw imgui::imgui)
target_compile_definitions(krbr_core PUBLIC VK_NO_PROTOTYPES GLM_FORCE_DEPTH_ZERO_TO_ONE IMGUI_IMPL_VULKAN_USE_VOLK)
if(WIN32)
  target_link_libraries(krbr_core PUBLIC comdlg32)
  target_compile_definitions(krbr_core PUBLIC NOMINMAX WIN32_LEAN_AND_MEAN _CRT_SECURE_NO_WARNINGS)
endif()
if(MSVC)
  target_compile_options(krbr_core PUBLIC /utf-8 /W3 /permissive- /EHsc /Zc:__cplusplus)
endif()

# ---------------------------------------------------------------- krbr_step（STEP 変換器 DLL）
# 本体は OpenCASCADE をリンクしない。DLL と OpenCASCADE の DLL 一式は build/release/step/ にまとめ、
# 本体は実行時に exe と同じフォルダの step/krbr_step.dll を探して読み込む
if(OpenCASCADE_FOUND)
  message(STATUS "OpenCASCADE ${OpenCASCADE_VERSION}: building STEP converter DLL (step/krbr_step.dll)")
  add_library(krbr_step SHARED src/step_dll/krbr_step.cpp)
  target_include_directories(krbr_step PRIVATE src ${OpenCASCADE_INCLUDE_DIR})
  target_compile_definitions(krbr_step PRIVATE KRBR_STEP_EXPORTS NOMINMAX WIN32_LEAN_AND_MEAN _CRT_SECURE_NO_WARNINGS)
  target_link_libraries(krbr_step PRIVATE krbr_meshfile TKDESTEP TKXCAF TKLCAF TKCAF TKRWMesh TKMesh TKBRep TKTopAlgo TKMath TKernel TKDE TKXSBase TKG3d TKGeomBase)
  set_target_properties(krbr_step PROPERTIES RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/step"
                                             LIBRARY_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/step")
  add_dependencies(krbr_core krbr_step)
else()
  message(WARNING "OpenCASCADE not found: STEP converter DLL is not built; STEP models will fall back to placeholder boxes")
endif()

# ノイズ除去（OIDN の公式ビルド済みバイナリを third_party/oidn に置く。無ければノイズ除去なしで動く）
set(OIDN_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/third_party/oidn")
find_package(OpenImageDenoise CONFIG QUIET PATHS "${OIDN_ROOT}" NO_DEFAULT_PATH)
if(OpenImageDenoise_FOUND)
  message(STATUS "OpenImageDenoise ${OpenImageDenoise_VERSION}: denoising enabled")
  target_compile_definitions(krbr_core PUBLIC KRBR_HAVE_OIDN=1)
  target_link_libraries(krbr_core PUBLIC OpenImageDenoise)
  set(OIDN_RUNTIME_DLLS
    "${OIDN_ROOT}/bin/OpenImageDenoise.dll" "${OIDN_ROOT}/bin/OpenImageDenoise_core.dll"
    "${OIDN_ROOT}/bin/OpenImageDenoise_device_cuda.dll" "${OIDN_ROOT}/bin/OpenImageDenoise_device_cpu.dll"
    "${OIDN_ROOT}/bin/tbb12.dll")
else()
  message(WARNING "OpenImageDenoise not found in ${OIDN_ROOT}: denoising disabled")
endif()

# ---------------------------------------------------------------- krbr（コマンド）
add_executable(krbr src/main.cpp)
target_link_libraries(krbr PRIVATE krbr_core)
if(OIDN_RUNTIME_DLLS)
  add_custom_command(TARGET krbr POST_BUILD COMMAND ${CMAKE_COMMAND} -E copy_if_different ${OIDN_RUNTIME_DLLS} $<TARGET_FILE_DIR:krbr>)
endif()

# ---------------------------------------------------------------- テスト
# 単体テストと、オフスクリーン描画を使う検証（白い炉テストなど）。ウィンドウは開かない
find_package(doctest CONFIG REQUIRED)
enable_testing()
file(GLOB KRBR_TEST_SOURCES CONFIGURE_DEPENDS "${CMAKE_CURRENT_SOURCE_DIR}/tests/*.cpp")
add_executable(krbr_tests ${KRBR_TEST_SOURCES})
target_link_libraries(krbr_tests PRIVATE krbr_core doctest::doctest)
# テストが描く基板と環境マップ（このフォルダの外にある、ユーザー所有の読み取り専用データ）
set(KRBR_TEST_PCB "E:/program/kachkojir_RK3506G2/kicad/kachkojir_RK3506G2.kicad_pcb" CACHE FILEPATH "テストで使う .kicad_pcb")
set(KRBR_TEST_HDR "E:/desktop/hdr/brown_photostudio_02_4k.exr" CACHE FILEPATH "テストで使う環境マップ（.hdr / .exr）")
target_compile_definitions(krbr_tests PRIVATE KRBR_SOURCE_DIR="${CMAKE_CURRENT_SOURCE_DIR}"
  KRBR_TEST_PCB="${KRBR_TEST_PCB}" KRBR_TEST_HDR="${KRBR_TEST_HDR}")
add_test(NAME krbr_tests COMMAND krbr_tests)
if(OIDN_RUNTIME_DLLS)
  add_custom_command(TARGET krbr_tests POST_BUILD COMMAND ${CMAKE_COMMAND} -E copy_if_different ${OIDN_RUNTIME_DLLS} $<TARGET_FILE_DIR:krbr_tests>)
endif()
