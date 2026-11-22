# Builds the libjxl helper programs against the static libjxl 0.12.0 of the C++ iat 1.0.0 build
# (read only) and runs them (docs/decisions.md D-R18).
#
#   pwsh -File tools\libjxl_gen\build.ps1 [-CppBuild D:\projects\ImageAnalysisTool\build\release] [-Out <dir>]
#
# Toolset 14.52 with the static CRT, as the C++ build (CMakeCache: MSVC 14.52.37025, MultiThreaded).
# Outputs: crates\iat-core\data\jxl_icc_table.bin and tests\fixtures\jxl_generated\*.jxl; the
# executables and object files go to -Out (default: a temporary folder).
param(
    [string]$CppBuild = 'D:\projects\ImageAnalysisTool\build\release',
    [string]$Out = (Join-Path ([System.IO.Path]::GetTempPath()) 'iat-libjxl-gen')
)
$ErrorActionPreference = 'Stop'
$repo = Resolve-Path (Join-Path $PSScriptRoot '..\..')
$vcpkg = Join-Path $CppBuild 'vcpkg_installed\x64-windows-static-iat'
$vs = 'C:\Program Files\Microsoft Visual Studio\18\Community'
New-Item -ItemType Directory -Force $Out | Out-Null
$libs = 'jxl.lib jxl_cms.lib jxl_threads.lib hwy.lib brotlienc.lib brotlidec.lib brotlicommon.lib lcms2.lib'
$cl = "cl /nologo /std:c++17 /O2 /MT /EHsc /DJXL_STATIC_DEFINE /DJXL_CMS_STATIC_DEFINE /DJXL_THREADS_STATIC_DEFINE " +
      "/I`"$vcpkg\include`" /Fo`"$Out\\`""
$link = "/link /LIBPATH:`"$vcpkg\lib`" $libs"
$cmds = @(
    "call `"$vs\VC\Auxiliary\Build\vcvars64.bat`" -vcvars_ver=14.52 >nul",
    "$cl `"$PSScriptRoot\gen_jxl_icc_table.cpp`" /Fe`"$Out\gen_jxl_icc_table.exe`" $link",
    "$cl `"$PSScriptRoot\gen_jxl_lossy.cpp`" /Fe`"$Out\gen_jxl_lossy.exe`" $link"
) -join ' && '
cmd /c $cmds
if ($LASTEXITCODE -ne 0) { throw "build failed ($LASTEXITCODE)" }
New-Item -ItemType Directory -Force (Join-Path $repo 'crates\iat-core\data') | Out-Null
& "$Out\gen_jxl_icc_table.exe" (Join-Path $repo 'crates\iat-core\data\jxl_icc_table.bin')
if ($LASTEXITCODE -ne 0) { throw "gen_jxl_icc_table failed ($LASTEXITCODE)" }
New-Item -ItemType Directory -Force (Join-Path $repo 'tests\fixtures\jxl_generated') | Out-Null
& "$Out\gen_jxl_lossy.exe" (Join-Path $repo 'tests\fixtures\generated\s3_jxl_rgb16_600x40_srgb.truth') `
    (Join-Path $repo 'tests\fixtures\jxl_generated')
if ($LASTEXITCODE -ne 0) { throw "gen_jxl_lossy failed ($LASTEXITCODE)" }
