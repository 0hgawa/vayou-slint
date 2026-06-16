# Build the release, sign vayou.exe, and emit latest.json for the in-app
# self-updater; also build Vayou-Setup.exe for first-time installs.
#
#   powershell -ExecutionPolicy Bypass -File build.ps1
#
# The self-updater downloads and swaps ONLY vayou.exe (verified against the
# minisign key embedded in the app - src/update.rs UPDATE_PUBKEY). The NSIS
# installer is just for the first install (ships vayou.exe + libmpv-2.dll +
# ffmpeg.exe + shortcuts).
#
# Signing uses rsign2 (`cargo install rsign2`); the key in .keys is rsign-format
# and produces a minisign-compatible signature. It was generated passwordless
# (`rsign generate -W`), so signing here is non-interactive.
#   -Key   path to the rsign/minisign secret key (default: D:\Apps\.keys\vayou.key)
#   -Repo  GitHub owner/repo the release is published to (for the asset URL)
param(
    [string]$Key  = "D:\Apps\.keys\vayou.key",
    [string]$Repo = "0hgawa/vayou-slint"
)
$ErrorActionPreference = "Stop"

$root = "D:\Apps\vayou-slint"
$exe  = "$root\target\release\vayou.exe"

# 0. Build the release binary.
Write-Host "Building release..."
& cargo build --release --manifest-path "$root\Cargo.toml"
if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
if (-not (Test-Path $exe)) { throw "vayou.exe not found after build." }

# The installer bundles the two runtime sidecars - fail early if they're missing
# from the release dir (they're not committed; drop them in per the README).
foreach ($dep in @("libmpv-2.dll", "ffmpeg.exe")) {
    if (-not (Test-Path "$root\target\release\$dep")) {
        throw "$dep is missing from target\release - copy it there before packaging."
    }
}

# Single source of truth: read the version from Cargo.toml [package] and feed it
# to both the installer (below) and latest.json (further down), so nothing carries
# a hand-synced version string.
$cargoLines = Get-Content "$root\Cargo.toml"
$pkgLine = ($cargoLines | Select-String -Pattern '^\[package\]' | Select-Object -First 1).LineNumber
$version = ($cargoLines[$pkgLine..($cargoLines.Count - 1)] |
    Select-String -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches.Groups[1].Value
if (-not $version) { throw "couldn't read [package] version from Cargo.toml" }
$version4 = "$version.0"

# 1. First-install installer (NSIS). Found on PATH or a standard NSIS install;
#    set $env:MAKENSIS to point at makensis.exe explicitly.
$makensis = if ($env:MAKENSIS) { $env:MAKENSIS } else {
    @(
        (Get-Command makensis -ErrorAction SilentlyContinue).Source,
        (Join-Path ${env:ProgramFiles(x86)} "NSIS\makensis.exe"),
        (Join-Path $env:ProgramFiles "NSIS\makensis.exe")
    ) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
}
if (-not $makensis) {
    throw "makensis not found. Install NSIS (https://nsis.sourceforge.io) or set `$env:MAKENSIS to its path."
}
$nsi   = Join-Path $PSScriptRoot "vayou.nsi"
$setup = Join-Path $PSScriptRoot "Vayou-Setup.exe"
# Drive the installer's version from Cargo.toml (the .nsi defaults match but defer
# to these /D overrides).
& $makensis "/DAPP_VERSION=$version" "/DAPP_VERSION_4=$version4" $nsi
if ($LASTEXITCODE -ne 0) { throw "makensis failed ($LASTEXITCODE)" }
if (-not (Test-Path $setup)) { throw "Vayou-Setup.exe was not produced." }

# 2. Sign vayou.exe (rsign2) - this is what the self-updater verifies.
$rsign = (Get-Command rsign -ErrorAction SilentlyContinue).Source
if (-not $rsign) {
    throw "rsign not found on PATH. Install it: 'cargo install rsign2'."
}
if (-not (Test-Path $Key)) { throw "secret key not found at $Key (override with -Key)." }

$sig = "$exe.minisig"
if (Test-Path $sig) { Remove-Item $sig -Force }
# -W: the key was generated passwordless, so signing is non-interactive.
& $rsign sign -W -s $Key -x $sig $exe
if ($LASTEXITCODE -ne 0) { throw "rsign signing failed ($LASTEXITCODE)" }
# .NET read: Get-Content -Raw attaches PSPath note-properties that ConvertTo-Json
# would serialize as an object instead of the raw .minisig string.
$signature = [System.IO.File]::ReadAllText($sig)

# 3. Emit latest.json (reusing $version read above).
$manifest = [ordered]@{
    version   = $version
    pub_date  = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    platforms = [ordered]@{
        "windows-x86_64" = [ordered]@{
            # GitHub serves the newest release's asset from this stable path.
            url       = "https://github.com/$Repo/releases/latest/download/vayou.exe"
            signature = $signature
        }
    }
}
$json = $manifest | ConvertTo-Json -Depth 6
$latest = Join-Path $PSScriptRoot "latest.json"
# UTF-8 without BOM: PS 5.1's `-Encoding utf8` adds a BOM and serde_json
# (the app's feed parser) rejects it.
[System.IO.File]::WriteAllText($latest, $json, (New-Object System.Text.UTF8Encoding $false))

Write-Host ""
Write-Host "Done:"
Write-Host "  installer (first install) -> $setup"
Write-Host "  app binary (self-update)  -> $exe"
Write-Host "  signature                 -> $sig"
Write-Host "  manifest                  -> $latest  (v$version)"
Write-Host ""
Write-Host "Upload Vayou-Setup.exe, vayou.exe and latest.json as assets on the GitHub release."
