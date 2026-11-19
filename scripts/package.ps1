# Builds the zip distribution (D15: zip, no installer; D16: static CRT, no redistributable)
# from an existing Release build. Run scripts/build.ps1 first.
#
#   pwsh -File scripts/package.ps1 [-Downloads <dir>] [-SpdxTemplates <dir>]
#
# -Downloads: vcpkg downloads folder holding the source archives (libjxl PATENTS/AUTHORS).
#   Default: $env:VCPKG_DOWNLOADS, else $env:IAT_VCPKG_CACHE\downloads, else
#   %LOCALAPPDATA%\vcpkg\downloads.
# -SpdxTemplates: folder of SPDX license templates (<id>.txt) the license texts are
#   checked against. Default: $env:IAT_SPDX_TEMPLATES.
# The vcpkg installed tree is read from build/release/CMakeCache.txt (VCPKG_INSTALLED_DIR).
#
# Output: dist/ImageAnalysisTool-<version>-win-x64/ and the .zip next to it.
# Nothing outside dist/ is written. An existing package of the same version is
# refused rather than overwritten.
[CmdletBinding()]
param(
    [string]$Downloads,
    [string]$SpdxTemplates
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$bin = Join-Path $root 'build\release\bin'

$cacheFile = Join-Path $root 'build\release\CMakeCache.txt'
if (-not (Test-Path -LiteralPath $cacheFile)) { throw 'build/release/CMakeCache.txt not found: run scripts/build.ps1 first' }
$installedLine = Select-String -LiteralPath $cacheFile -Pattern '^VCPKG_INSTALLED_DIR:[A-Z]+=(.+)$' | Select-Object -First 1
if (-not $installedLine) { throw 'VCPKG_INSTALLED_DIR not found in build/release/CMakeCache.txt' }
$installed = $installedLine.Matches[0].Groups[1].Value.Trim()
if (-not $Downloads) {
    $Downloads = if ($env:VCPKG_DOWNLOADS) { $env:VCPKG_DOWNLOADS }
                 elseif ($env:IAT_VCPKG_CACHE) { Join-Path $env:IAT_VCPKG_CACHE 'downloads' }
                 else { Join-Path $env:LOCALAPPDATA 'vcpkg\downloads' }
}
if (-not $SpdxTemplates) { $SpdxTemplates = $env:IAT_SPDX_TEMPLATES }
if (-not $SpdxTemplates) { throw 'SPDX template folder not given: pass -SpdxTemplates or set IAT_SPDX_TEMPLATES' }

# Version: single source is project(VERSION) in CMakeLists.txt; the built iat.exe must agree.
$cm = Get-Content -Raw -LiteralPath (Join-Path $root 'CMakeLists.txt')
if ($cm -notmatch 'project\(\s*ImageAnalysisTool\s+VERSION\s+(\d+\.\d+\.\d+)') { throw 'project(VERSION) not found in CMakeLists.txt' }
$version = $Matches[1]
$built = (& (Join-Path $bin 'iat.exe') --version).Trim()
if ($built -ne "iat $version") { throw "build/release is stale: iat.exe reports '$built', CMakeLists.txt says $version" }

# The third-party list must match what this build actually linked (linker maps).
& py -3.13 -I (Join-Path $root 'tools\gen_third_party.py') $root --installed $installed `
    --triplet x64-windows-static-iat --bin $bin --downloads $Downloads --spdx-templates $SpdxTemplates --check
if ($LASTEXITCODE -ne 0) { throw 'third-party notices are out of date: rerun tools/gen_third_party.py without --check' }

# Static CRT (D16): neither executable may import a VC++ or UCRT runtime DLL. Import
# names are stored as ASCII in the PE file, so a byte search is enough to catch them.
foreach ($exe in 'iat.exe', 'iat-worker.exe') {
    $text = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes((Join-Path $bin $exe)))
    $hit = [regex]::Match($text, '(?i)(vcruntime\d+[a-z_]*\.dll|msvcp\d+[a-z_]*\.dll|ucrtbased?\.dll|api-ms-win-crt-[a-z-]+\.dll|concrt\d+\.dll|vcomp\d+\.dll)')
    if ($hit.Success) { throw "$exe imports a runtime DLL ($($hit.Value)): the build is not static CRT" }
}

$name = "ImageAnalysisTool-$version-win-x64"
$dist = Join-Path $root 'dist'
$stage = Join-Path $dist $name
$zip = "$stage.zip"
if ((Test-Path -LiteralPath $stage) -or (Test-Path -LiteralPath $zip)) { throw "$name already exists in dist/ (never overwritten)" }
New-Item -ItemType Directory -Force $stage | Out-Null

$files = [ordered]@{
    'iat.exe'                 = Join-Path $bin 'iat.exe'
    'iat-worker.exe'          = Join-Path $bin 'iat-worker.exe'
    'README.md'               = Join-Path $root 'README.md'
    'README.zh-TW.md'         = Join-Path $root 'README.zh-TW.md'
    'USAGE.md'                = Join-Path $root 'docs\usage.md'
    'CHANGELOG.md'            = Join-Path $root 'CHANGELOG.md'
    'LICENSE.txt'             = Join-Path $root 'LICENSE'
    'THIRD_PARTY_NOTICES.txt' = Join-Path $root 'THIRD_PARTY_NOTICES.txt'
}
foreach ($k in $files.Keys) { Copy-Item -LiteralPath $files[$k] -Destination (Join-Path $stage $k) }
Copy-Item -LiteralPath (Join-Path $root 'licenses') -Destination (Join-Path $stage 'licenses') -Recurse

# SHA256SUMS.txt over every file in the package (relative paths, forward slashes).
$sums = Get-ChildItem -LiteralPath $stage -Recurse -File | Sort-Object FullName | ForEach-Object {
    $rel = $_.FullName.Substring($stage.Length + 1).Replace('\', '/')
    '{0}  {1}' -f (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(), $rel
}
Set-Content -LiteralPath (Join-Path $stage 'SHA256SUMS.txt') -Value $sums -Encoding utf8NoBOM

Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip -CompressionLevel Optimal
Write-Host "package: $zip"
Write-Host ("sha256:  " + (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant())
