# Writes THIRD_PARTY_NOTICES.txt with cargo-about (about.toml, about.hbs) for the crates that
# Cargo.lock compiles into iat.exe, iat-gui.exe and iat-worker.exe.
#
#   pwsh -File scripts/gen_third_party.ps1 [-Output <file>]
#
# Runs with --frozen (Cargo.lock unchanged, no network): license texts come from the crate
# sources in the local cargo registry, so the output only changes when Cargo.lock does.
# Fails when any crate's license cannot be determined or is not in about.toml's accepted list.
[CmdletBinding()]
param(
    [string]$Output
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $Output) { $Output = Join-Path $root 'THIRD_PARTY_NOTICES.txt' }

Push-Location $root
try {
    & cargo about generate --workspace --frozen --fail -c about.toml -o $Output about.hbs
    if ($LASTEXITCODE -ne 0) { throw "cargo about generate failed (exit $LASTEXITCODE)" }
} finally {
    Pop-Location
}
# Line endings: LF in the generated text, matching the other packaged text files.
$text = [IO.File]::ReadAllText($Output).Replace("`r`n", "`n")
[IO.File]::WriteAllText($Output, $text, [Text.UTF8Encoding]::new($false))
Write-Host "written: $Output"
