@echo off
rem Shared build environment for this project only. Called from build.ps1 inside
rem a child cmd.exe; it never changes the user's persistent PATH or settings.
rem
rem Optional environment variables (set them before running build.ps1):
rem   IAT_VS            Visual Studio 2026 root (default: the standard Community install path)
rem   IAT_VCPKG_CACHE   folder for vcpkg binary cache, downloads and registries; when unset,
rem                     vcpkg uses its own defaults
if not defined IAT_VS set "IAT_VS=C:\Program Files\Microsoft Visual Studio\18\Community"
set "IAT_TOOLSET=14.52"
if defined IAT_VCPKG_CACHE (
  set "VCPKG_DEFAULT_BINARY_CACHE=%IAT_VCPKG_CACHE%\archives"
  set "VCPKG_DOWNLOADS=%IAT_VCPKG_CACHE%\downloads"
  set "X_VCPKG_REGISTRIES_CACHE=%IAT_VCPKG_CACHE%\registries"
)
set "VCPKG_DISABLE_METRICS=1"
rem vcvarsall returns 0 even when the requested toolset directory is missing, so its
rem exit code is not trusted: the loaded toolset is verified below instead.
call "%IAT_VS%\VC\Auxiliary\Build\vcvarsall.bat" x64 10.0.26100.0 -vcvars_ver=%IAT_TOOLSET% >nul
if not defined VCToolsVersion (
  echo error: vcvarsall did not set VCToolsVersion; MSVC %IAT_TOOLSET% is not available 1>&2
  exit /b 1
)
if not "%VCToolsVersion:~0,6%"=="%IAT_TOOLSET%." (
  echo error: loaded MSVC toolset %VCToolsVersion% is not %IAT_TOOLSET%.x 1>&2
  exit /b 1
)
if not defined VCPKG_ROOT (
  echo error: VCPKG_ROOT is not set after loading the MSVC environment; install the vcpkg component of Visual Studio 1>&2
  exit /b 1
)
where cl.exe >nul 2>&1
if errorlevel 1 (
  echo error: cl.exe not found after loading MSVC %VCToolsVersion% 1>&2
  exit /b 1
)
where ninja.exe >nul 2>&1
if errorlevel 1 (
  echo error: ninja.exe not found on PATH 1>&2
  exit /b 1
)
exit /b 0
