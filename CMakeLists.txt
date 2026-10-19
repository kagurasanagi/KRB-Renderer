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

# STEP 読み込み（OpenCASCADE）。見つからない場合は STEP を「読めない」扱いにして代替表示で動く
find_package(OpenCASCADE CONFIG QUIET)

find_program(GLSLC glslc HINTS "$ENV{VULKAN_SDK}/Bin" REQUIRED)

# ---------------------------------------------------------------- shaders
set(SHADER_SRC_DIR "${CMAKE_CURRENT_SOURCE_DIR}/shaders")
set(SHADER_OUT_DIR "${CMAKE_BINARY_DIR}/shaders")
file(GLOB SHADER_INCLUDES CONFIGURE_DEPENDS "${SHADER_SRC_DIR}/*.glsl")
set(SHADER_SOURCES "${SHADER_SRC_DIR}/pathtrace.comp" "${SHADER_SRC_DIR}/tonemap.comp")
set(SHADER_OUTPUTS)
foreach(src ${SHADER_SOURCES})
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
add_custom_target(krbr_shaders DEPENDS ${SHADER_OUTPUTS})

# ---------------------------------------------------------------- krbr
file(GLOB_RECURSE KRBR_SOURCES CONFIGURE_DEPENDS "${CMAKE_CURRENT_SOURCE_DIR}/src/*.cpp")
# ImGui のバックエンドは volk を使うよう自前でビルドする（vcpkg 版は Vulkan ローダーを直接リンクするため）
set(IMGUI_BACKEND_SOURCES
  "${CMAKE_CURRENT_SOURCE_DIR}/third_party/imgui_backends/imgui_impl_glfw.cpp"
  "${CMAKE_CURRENT_SOURCE_DIR}/third_party/imgui_backends/imgui_impl_vulkan.cpp")
add_executable(krbr ${KRBR_SOURCES} ${IMGUI_BACKEND_SOURCES})
add_dependencies(krbr krbr_shaders)
target_include_directories(krbr PRIVATE src third_party/imgui_backends ${STB_INCLUDE_DIRS} ${EARCUT_HPP_INCLUDE_DIRS})
target_link_libraries(krbr PRIVATE
  Vulkan::Headers volk::volk GPUOpen::VulkanMemoryAllocator
  tomlplusplus::tomlplusplus nlohmann_json::nlohmann_json
  unofficial::tinyexr::tinyexr Clipper2::Clipper2 CLI11::CLI11 glm::glm glfw imgui::imgui)
target_compile_definitions(krbr PRIVATE VK_NO_PROTOTYPES GLM_FORCE_DEPTH_ZERO_TO_ONE IMGUI_IMPL_VULKAN_USE_VOLK)
if(WIN32)
  target_link_libraries(krbr PRIVATE comdlg32)
endif()
if(WIN32)
  target_compile_definitions(krbr PRIVATE NOMINMAX WIN32_LEAN_AND_MEAN _CRT_SECURE_NO_WARNINGS)
endif()
if(MSVC)
  target_compile_options(krbr PRIVATE /utf-8 /W3 /permissive- /EHsc /Zc:__cplusplus)
endif()

if(OpenCASCADE_FOUND)
  message(STATUS "OpenCASCADE ${OpenCASCADE_VERSION}: STEP support enabled")
  target_compile_definitions(krbr PRIVATE KRBR_HAVE_OCCT=1)
  target_include_directories(krbr PRIVATE ${OpenCASCADE_INCLUDE_DIR})
  target_link_libraries(krbr PRIVATE TKDESTEP TKXCAF TKLCAF TKCAF TKRWMesh TKMesh TKBRep TKTopAlgo TKMath TKernel TKDE TKXSBase TKG3d TKGeomBase)
else()
  message(WARNING "OpenCASCADE not found: STEP models will fall back to placeholder boxes")
endif()
