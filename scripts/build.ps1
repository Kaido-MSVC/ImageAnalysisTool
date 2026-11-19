# Project build entry point. The MSVC environment (toolset 14.52, SDK
# 10.0.26100.0) is loaded inside a child cmd.exe only; the caller's PATH and
# persistent settings are never changed.
#
#   pwsh -File scripts/build.ps1 [-Preset <name>] [-ConfigureOnly]
#
# -Preset defaults to "release" (CMakePresets.json). Machine-specific presets go in the
# git-ignored CMakeUserPresets.json (see CMakeUserPresets.example.json).
# Optional environment: IAT_CMAKE (cmake.exe), IAT_VS and IAT_VCPKG_CACHE (see vcpkg-env.cmd).
[CmdletBinding()]
param(
    [string]$Preset = 'release',
    [switch]$ConfigureOnly
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$cmake = if ($env:IAT_CMAKE) { $env:IAT_CMAKE } else { 'C:\Program Files\CMake\bin\cmake.exe' }
if (-not (Test-Path -LiteralPath $cmake)) { throw "cmake.exe not found at '$cmake' (set IAT_CMAKE)" }
$toolset = '14.52'

if ($env:IAT_VCPKG_CACHE) {
    foreach ($dir in 'archives', 'downloads', 'registries') {
        New-Item -ItemType Directory -Force (Join-Path $env:IAT_VCPKG_CACHE $dir) | Out-Null
    }
}

function Invoke-ChildCmd([string[]]$Lines) {
    $script = Join-Path ([IO.Path]::GetTempPath()) "iat-build-$PID-$([guid]::NewGuid().ToString('N')).cmd"
    Set-Content -LiteralPath $script -Encoding ascii -Value $Lines
    try {
        $out = & cmd.exe /d /c $script
        return [pscustomobject]@{ Output = $out; ExitCode = $LASTEXITCODE }
    }
    finally {
        Remove-Item -LiteralPath $script -ErrorAction SilentlyContinue
    }
}

# Preflight: vcvarsall returns 0 even when the requested toolset is missing, so the
# environment it actually produced is inspected (in a child process; this process's
# environment is not touched) and verified here.
$probe = Invoke-ChildCmd @(
    '@echo off'
    "call `"$root\scripts\vcpkg-env.cmd`" >nul 2>&1"
    'echo IAT_ENV_EXIT=%ERRORLEVEL%'
    'echo IAT_VCTOOLS=%VCToolsVersion%'
    'for /f "delims=" %%p in (''where cl.exe 2^>nul'') do echo IAT_CL=%%p'
)
$vctools = ($probe.Output | Where-Object { $_ -like 'IAT_VCTOOLS=*' } | Select-Object -First 1) -replace '^IAT_VCTOOLS=', ''
$cl = ($probe.Output | Where-Object { $_ -like 'IAT_CL=*' } | Select-Object -First 1) -replace '^IAT_CL=', ''
$problem = $null
if (-not $vctools -or $vctools -eq '%VCToolsVersion%') { $problem = "VCToolsVersion is not set after loading the MSVC environment" }
elseif (-not $vctools.Trim().StartsWith("$toolset.")) { $problem = "VCToolsVersion is '$($vctools.Trim())', expected $toolset.x" }
elseif (-not $cl -or -not (Test-Path -LiteralPath $cl.Trim())) { $problem = "cl.exe not found for MSVC $($vctools.Trim())" }
elseif ($cl.Trim() -notlike "*\MSVC\$($vctools.Trim())\*") { $problem = "cl.exe '$($cl.Trim())' does not belong to MSVC $($vctools.Trim())" }
if ($problem) {
    [Console]::Error.WriteLine("error: MSVC $toolset toolset check failed: $problem")
    exit 1
}
Write-Host "MSVC toolset $($vctools.Trim()) ($($cl.Trim()))"

$lines = @(
    '@echo off'
    "call `"$root\scripts\vcpkg-env.cmd`" || exit /b 1"
    "cd /d `"$root`""
    "`"$cmake`" --preset $Preset || exit /b 1"
)
if (-not $ConfigureOnly) {
    $lines += "`"$cmake`" --build --preset $Preset || exit /b 1"
}

$script = Join-Path ([IO.Path]::GetTempPath()) "iat-build-$PID.cmd"
Set-Content -LiteralPath $script -Encoding ascii -Value $lines
try {
    & cmd.exe /d /c $script
    if ($LASTEXITCODE -ne 0) {
        [Console]::Error.WriteLine("error: build failed with exit code $LASTEXITCODE")
        exit 1
    }
}
finally {
    Remove-Item -LiteralPath $script -ErrorAction SilentlyContinue
}
