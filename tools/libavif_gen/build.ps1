# Builds the libavif helper programs against the static libavif 1.4.2 + aom 3.15.2 of the C++ iat
# 1.0.0 build (read only); with -Dump also runs dump_avif_planes on the given AVIF files
# (docs/decisions.md D-R19).
#
#   pwsh -File tools\libavif_gen\build.ps1 [-Dump <out-dir> <file.avif>...] [-CppBuild <dir>] [-Out <dir>]
#
# Toolset 14.52 with the static CRT, as the C++ build. The executables go to -Out (default: a
# temporary folder): dump_avif_planes.exe and encode_avif.exe.
[CmdletBinding(PositionalBinding = $false)]
param(
    [string]$Dump,
    [Parameter(ValueFromRemainingArguments)] [string[]]$Files,
    [string]$CppBuild = 'D:\projects\ImageAnalysisTool\build\release',
    [string]$Out = (Join-Path ([System.IO.Path]::GetTempPath()) 'iat-libavif-gen')
)
$ErrorActionPreference = 'Stop'
$vcpkg = Join-Path $CppBuild 'vcpkg_installed\x64-windows-static-iat'
$vs = 'C:\Program Files\Microsoft Visual Studio\18\Community'
New-Item -ItemType Directory -Force $Out | Out-Null
$cl = "cl /nologo /std:c++17 /O2 /MT /EHsc /I`"$vcpkg\include`" /Fo`"$Out\\`""
$link = "/link /LIBPATH:`"$vcpkg\lib`" avif.lib aom.lib yuv.lib"
$cmds = @(
    "call `"$vs\VC\Auxiliary\Build\vcvars64.bat`" -vcvars_ver=14.52 >nul",
    "$cl `"$PSScriptRoot\dump_avif_planes.cpp`" /Fe`"$Out\dump_avif_planes.exe`" $link",
    "$cl `"$PSScriptRoot\encode_avif.cpp`" /Fe`"$Out\encode_avif.exe`" $link libpng16.lib zs.lib"
) -join ' && '
cmd /c $cmds
if ($LASTEXITCODE -ne 0) { throw "build failed ($LASTEXITCODE)" }
if ($Dump) {
    New-Item -ItemType Directory -Force $Dump | Out-Null
    & "$Out\dump_avif_planes.exe" $Dump @Files
    if ($LASTEXITCODE -ne 0) { throw "dump_avif_planes failed ($LASTEXITCODE)" }
}
