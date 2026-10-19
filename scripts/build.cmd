@echo off
rem MSVC の環境を読み込んで、低優先度で configure と build を行う
setlocal
rem vcvars は VCPKG_ROOT を VS 同梱の vcpkg に書き換えるので、元の値を保存して戻す
set "KRBR_VCPKG_ROOT=%VCPKG_ROOT%"
if "%KRBR_VCPKG_ROOT%"=="" set "KRBR_VCPKG_ROOT=%USERPROFILE%\vcpkg"
set VSDEV="C:\Program Files\Microsoft Visual Studio\18\Community\VC\Auxiliary\Build\vcvars64.bat"
call %VSDEV% >nul || exit /b 1
set "VCPKG_ROOT=%KRBR_VCPKG_ROOT%"
cd /d "%~dp0.."
if not exist build\release\build.ninja (
  set "NINJA=%DevEnvDir%CommonExtensions\Microsoft\CMake\Ninja\ninja.exe"
  cmake --preset release -DCMAKE_TOOLCHAIN_FILE="%VCPKG_ROOT%\scripts\buildsystems\vcpkg.cmake" -DCMAKE_MAKE_PROGRAM="%DevEnvDir%CommonExtensions\Microsoft\CMake\Ninja\ninja.exe" || exit /b 1
)
start /belownormal /b /wait cmake --build --preset release %*
exit /b %ERRORLEVEL%
