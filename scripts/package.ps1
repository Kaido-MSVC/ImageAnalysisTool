# Builds the zip distribution from an existing release build (same layout and checks as the
# C++ iat 1.0.0 scripts/package.ps1: zip, no installer; static CRT, no redistributable).
#
#   cargo build --release
#   pwsh -File scripts/package.ps1
#
# Needs cargo-about (THIRD_PARTY_NOTICES.txt is checked against the current Cargo.lock).
# Output: dist/ImageAnalysisTool-<version>-win-x64/ and the .zip next to it. Nothing outside
# dist/ is written. An existing package of the same version is refused rather than overwritten.
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$bin = Join-Path $root 'target\release'
$exes = 'iat.exe', 'iat-gui.exe', 'iat-worker.exe'

# Version: single source is [workspace.package] version in Cargo.toml; the built iat.exe must agree.
$toml = Get-Content -Raw -LiteralPath (Join-Path $root 'Cargo.toml')
if ($toml -notmatch '(?ms)^\[workspace\.package\].*?^version\s*=\s*"(\d+\.\d+\.\d+)"') { throw 'version not found in [workspace.package]' }
$version = $Matches[1]
foreach ($exe in $exes) {
    if (-not (Test-Path -LiteralPath (Join-Path $bin $exe))) { throw "target/release/$exe not found: run cargo build --release first" }
}
$built = (& (Join-Path $bin 'iat.exe') --version | Out-String).Trim()
if ($built -ne "iat $version") { throw "target/release is stale: iat.exe reports '$built', Cargo.toml says $version" }
foreach ($exe in $exes) {
    $v = (Get-Item -LiteralPath (Join-Path $bin $exe)).VersionInfo.FileVersion
    if ($v -ne $version) { throw "$exe has file version '$v', expected $version (Windows resources not rebuilt?)" }
}

# Static CRT: no executable may import a VC++ or UCRT runtime DLL. Import names are stored as
# ASCII in the PE file, so a byte search is enough to catch them (same check as the C++ version).
foreach ($exe in $exes) {
    $text = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes((Join-Path $bin $exe)))
    $hit = [regex]::Match($text, '(?i)(vcruntime\d+[a-z_]*\.dll|msvcp\d+[a-z_]*\.dll|ucrtbased?\.dll|api-ms-win-crt-[a-z-]+\.dll|concrt\d+\.dll|vcomp\d+\.dll)')
    if ($hit.Success) { throw "$exe imports a runtime DLL ($($hit.Value)): the build is not static CRT" }
}

$name = "ImageAnalysisTool-$version-win-x64"
$dist = Join-Path $root 'dist'
$stage = Join-Path $dist $name
$zip = "$stage.zip"
if ((Test-Path -LiteralPath $stage) -or (Test-Path -LiteralPath $zip)) { throw "$name already exists in dist/ (never overwritten)" }

# The third-party list must match the crates this Cargo.lock compiles into the executables.
$notices = Join-Path $root 'THIRD_PARTY_NOTICES.txt'
New-Item -ItemType Directory -Force $dist | Out-Null
$fresh = Join-Path $dist "THIRD_PARTY_NOTICES-$version.check.txt"
if (Test-Path -LiteralPath $fresh) { Remove-Item -LiteralPath $fresh }
& (Join-Path $PSScriptRoot 'gen_third_party.ps1') -Output $fresh
if (-not (Test-Path -LiteralPath $notices)) { throw 'THIRD_PARTY_NOTICES.txt not found: run scripts/gen_third_party.ps1' }
# Compared without line endings: git's autocrlf may have checked the committed file out with CRLF.
$lf = { param($p) [IO.File]::ReadAllText($p).Replace("`r`n", "`n") }
$same = (& $lf $fresh) -ceq (& $lf $notices)
Remove-Item -LiteralPath $fresh
if (-not $same) { throw 'THIRD_PARTY_NOTICES.txt is out of date: run scripts/gen_third_party.ps1 and commit the result' }

New-Item -ItemType Directory $stage | Out-Null
$files = [ordered]@{
    'iat.exe'                 = Join-Path $bin 'iat.exe'
    'iat-gui.exe'             = Join-Path $bin 'iat-gui.exe'
    'iat-worker.exe'          = Join-Path $bin 'iat-worker.exe'
    'README.md'               = Join-Path $root 'README.md'
    'README.zh-TW.md'         = Join-Path $root 'README.zh-TW.md'
    'CHANGELOG.md'            = Join-Path $root 'CHANGELOG.md'
    'LICENSE.txt'             = Join-Path $root 'LICENSE'
    'THIRD_PARTY_NOTICES.txt' = $notices
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
