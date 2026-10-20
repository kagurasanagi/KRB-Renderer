@echo off
rem Build the STEP converter DLL (build\release\step\krbr_step.dll, OpenCASCADE) with CMake.
rem krbr.exe itself is built with cargo: cd rust && cargo build --release
rem Loads the MSVC environment and runs configure + build at low priority.
rem   build.cmd            : incremental build (configures on first run)
rem   build.cmd --clean    : delete build\release, then configure + build
rem   other arguments are passed to "cmake --build"
rem (ASCII only: cmd.exe reads this file in the system code page)
setlocal enabledelayedexpansion
rem vcvars overwrites VCPKG_ROOT with the VS-bundled vcpkg, so save and restore it
set "KRBR_VCPKG_ROOT=%VCPKG_ROOT%"
if "!KRBR_VCPKG_ROOT!"=="" set "KRBR_VCPKG_ROOT=%USERPROFILE%\vcpkg"

rem Locate Visual Studio or Build Tools with vswhere (any edition)
set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
set "VSDIR="
if exist "!VSWHERE!" (
  for /f "usebackq delims=" %%i in (`"!VSWHERE!" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "VSDIR=%%i"
)
if "!VSDIR!"=="" (
  echo error: Visual Studio C++ tools not found. Install VS 2022/2026 or Build Tools with the C++ workload. 1>&2
  exit /b 1
)
rem (vcvars64 of Build Tools prints a harmless "'vswhere.exe' is not recognized" on stderr)
call "!VSDIR!\VC\Auxiliary\Build\vcvars64.bat" >nul 2>&1 || exit /b 1
set "VCPKG_ROOT=!KRBR_VCPKG_ROOT!"
cd /d "%~dp0.."

set "ARGS="
:args
if "%~1"=="" goto args_done
if "%~1"=="--clean" (
  if exist build\release rmdir /s /q build\release
) else (
  set "ARGS=!ARGS! %1"
)
shift
goto args

:args_done
if exist build\release\build.ninja goto build
rem Configure. Ninja: VS-bundled, then PATH, then winget
set "NINJA="
if exist "!DevEnvDir!CommonExtensions\Microsoft\CMake\Ninja\ninja.exe" set "NINJA=!DevEnvDir!CommonExtensions\Microsoft\CMake\Ninja\ninja.exe"
if "!NINJA!"=="" for %%i in (ninja.exe) do set "NINJA=%%~$PATH:i"
if "!NINJA!"=="" if exist "%LOCALAPPDATA%\Microsoft\WinGet\Links\ninja.exe" set "NINJA=%LOCALAPPDATA%\Microsoft\WinGet\Links\ninja.exe"
if "!NINJA!"=="" echo error: ninja.exe not found. Run: winget install Ninja-build.Ninja 1>&2
if "!NINJA!"=="" exit /b 1
cmake --preset release -DCMAKE_TOOLCHAIN_FILE="!VCPKG_ROOT!\scripts\buildsystems\vcpkg.cmake" -DCMAKE_MAKE_PROGRAM="!NINJA!"
if errorlevel 1 exit /b 1

:build
start /belownormal /b /wait cmake --build --preset release !ARGS!
exit /b %ERRORLEVEL%
