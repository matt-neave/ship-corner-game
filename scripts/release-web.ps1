<#
.SYNOPSIS
  Build a WebAssembly release of the game, package it for itch.io, and
  optionally push to itch via butler.

.DESCRIPTION
  Pipeline:
    1. Ensures the wasm32-unknown-unknown target is installed.
    2. Installs wasm-bindgen-cli at the version pinned in Cargo.lock
       (matches the wasm-bindgen version used by Bevy in this build).
    3. cargo build --release --target wasm32-unknown-unknown
    4. wasm-bindgen post-processing -> JS glue + sized .wasm
    5. Copies the loader HTML + assets/ + data/ into dist/web/
    6. Zips dist/web/ -> dist/ship-game-web.zip
    7. If -Push is set, calls `butler push` to upload to itch.io.

  The resulting zip can be uploaded manually via the itch.io project
  dashboard ("Uploads" -> add file, tag it as "playable in browser",
  set viewport to the canvas size). The HTML5-embed channel name for
  -Push (e.g. "html") must match what you've configured on the page.

.PARAMETER ItchTarget
  itch.io `user/game:channel` triple, used by `butler push`. Defaults
  to a placeholder you should overwrite before -Push works.

.PARAMETER Push
  When set, uploads the resulting zip via butler. Requires butler in
  PATH and a prior `butler login` so the API key is cached.

.PARAMETER WidePlay
  When set, enables the `wide_play` Cargo feature for a 360x200 arena
  instead of the default 200x200 square. Useful for shipping an AB
  variant under a separate itch.io channel.

.EXAMPLE
  pwsh scripts/release-web.ps1
  # Build + zip locally, leave the zip in dist/ for manual upload.

.EXAMPLE
  pwsh scripts/release-web.ps1 -ItchTarget "matt/ship-game:html" -Push
  # Build, zip, push to itch as the `html` channel.

.EXAMPLE
  pwsh scripts/release-web.ps1 -WidePlay -ItchTarget "matt/ship-game:html-wide" -Push
  # Ship the wide-arena variant to a separate channel for AB testing.
#>

[CmdletBinding()]
param(
    [string]$ItchTarget = "YOUR_USER/ship-game:html",
    [switch]$Push,
    [switch]$WidePlay
)

$ErrorActionPreference = 'Stop'

# Find the project root (parent of scripts/) regardless of where the
# script is invoked from. `$PSScriptRoot` is the directory containing
# THIS file.
$Root      = Split-Path -Parent $PSScriptRoot
$DistDir   = Join-Path $Root 'dist'
$WebOut    = Join-Path $DistDir 'web'
$ZipPath   = Join-Path $DistDir 'ship-game-web.zip'
$BinName   = 'ship-game'
$TargetDir = Join-Path $Root 'target\wasm32-unknown-unknown\release'
$WasmIn    = Join-Path $TargetDir "$BinName.wasm"

Write-Host "==> Building WASM release for itch.io" -ForegroundColor Cyan
Write-Host "    Root:   $Root"
Write-Host "    Output: $WebOut"
Write-Host "    Zip:    $ZipPath"

# ---- 1) Ensure the wasm target is installed --------------------------
Write-Host "==> Verifying rustup target wasm32-unknown-unknown" -ForegroundColor Cyan
$installed = rustup target list --installed
if (-not ($installed -match 'wasm32-unknown-unknown')) {
    Write-Host "    Adding target..." -ForegroundColor Yellow
    rustup target add wasm32-unknown-unknown
}

# ---- 2) Install / verify wasm-bindgen-cli at the matched version -----
# The CLI must match the wasm-bindgen library version Bevy compiled
# against, otherwise the post-processing step refuses to run. We read
# Cargo.lock to extract whatever version Bevy pulled in.
Write-Host "==> Resolving wasm-bindgen version from Cargo.lock" -ForegroundColor Cyan
$lockPath = Join-Path $Root 'Cargo.lock'
$lockText = Get-Content $lockPath -Raw
# Match the [[package]] entry whose name is exactly "wasm-bindgen"
# (NOT "wasm-bindgen-backend" or similar). The regex grabs the version
# from the following `version = "x.y.z"` line.
$match = [regex]::Match(
    $lockText,
    'name = "wasm-bindgen"\s*\nversion = "([\d\.]+)"'
)
if (-not $match.Success) {
    throw "Could not find wasm-bindgen in Cargo.lock. Run `cargo build` first?"
}
$wbVersion = $match.Groups[1].Value
Write-Host "    wasm-bindgen $wbVersion required"

# Install the CLI at that exact version if it's not already there.
# `wasm-bindgen --version` prints "wasm-bindgen X.Y.Z" — we compare
# the last token. `cargo install` is idempotent; this branch just
# saves the 30-60s reinstall when it's already current.
$haveCli = $false
try {
    $cliVer = (& wasm-bindgen --version 2>&1) -replace '^wasm-bindgen\s+', ''
    if ($cliVer.Trim() -eq $wbVersion) { $haveCli = $true }
} catch { }
if (-not $haveCli) {
    Write-Host "==> Installing wasm-bindgen-cli@$wbVersion" -ForegroundColor Cyan
    cargo install --version $wbVersion wasm-bindgen-cli
} else {
    Write-Host "    wasm-bindgen-cli $wbVersion already installed" -ForegroundColor Green
}

# ---- 3) Cargo build --------------------------------------------------
Write-Host "==> cargo build --release --target wasm32-unknown-unknown" -ForegroundColor Cyan
$features = @()
if ($WidePlay) { $features += 'wide_play' }
$featureArgs = @()
if ($features.Count -gt 0) {
    $featureArgs = @('--features', ($features -join ','))
}
cargo build --release --target wasm32-unknown-unknown @featureArgs
if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }
if (-not (Test-Path $WasmIn)) {
    throw "Expected wasm artifact not found at: $WasmIn"
}

# ---- 4) wasm-bindgen post-process -----------------------------------
# `--target web` emits an ES module loader (`<name>.js`) that calls
# `init()` to fetch and instantiate the `.wasm`. Matches the contract
# our `web/index.html` relies on.
Write-Host "==> wasm-bindgen post-processing" -ForegroundColor Cyan
if (Test-Path $WebOut) { Remove-Item $WebOut -Recurse -Force }
New-Item -ItemType Directory -Path $WebOut | Out-Null
wasm-bindgen `
    --target web `
    --no-typescript `
    --out-dir $WebOut `
    --out-name $BinName `
    $WasmIn
if ($LASTEXITCODE -ne 0) { throw "wasm-bindgen failed (exit $LASTEXITCODE)" }

# ---- 5) Stage loader HTML + assets + data ---------------------------
# Bevy's AssetPlugin fetches assets via relative HTTP requests when
# running in a browser, so the directory layout under the served root
# must mirror the source tree. The `data/` folder holds translations.
Write-Host "==> Staging static files" -ForegroundColor Cyan
Copy-Item (Join-Path $Root 'web\index.html') (Join-Path $WebOut 'index.html') -Force
# `assets/` is optional — the game uses procedural rendering, no
# bundled images / fonts at this point — but copy if present so a
# future asset folder ships automatically.
$assetsSrc = Join-Path $Root 'assets'
if (Test-Path $assetsSrc) {
    Copy-Item $assetsSrc (Join-Path $WebOut 'assets') -Recurse -Force
}
$dataSrc = Join-Path $Root 'data'
if (Test-Path $dataSrc) {
    Copy-Item $dataSrc (Join-Path $WebOut 'data') -Recurse -Force
}

# ---- 6) Zip ----------------------------------------------------------
# itch.io accepts a single .zip for HTML5 uploads; the page's "embed
# settings" panel sets which file is the entry point (index.html by
# default).
Write-Host "==> Zipping $WebOut -> $ZipPath" -ForegroundColor Cyan
if (Test-Path $ZipPath) { Remove-Item $ZipPath -Force }
Compress-Archive -Path (Join-Path $WebOut '*') -DestinationPath $ZipPath -CompressionLevel Optimal

# ---- 7) Optional: butler push ---------------------------------------
if ($Push) {
    Write-Host "==> butler push $ZipPath $ItchTarget" -ForegroundColor Cyan
    if ($ItchTarget -like 'YOUR_USER/*') {
        throw "Refusing to push with placeholder ItchTarget. Pass -ItchTarget user/game:channel."
    }
    butler push $ZipPath $ItchTarget
    if ($LASTEXITCODE -ne 0) { throw "butler push failed (exit $LASTEXITCODE)" }
} else {
    Write-Host ""
    Write-Host "==> Done. Zip ready at:" -ForegroundColor Green
    Write-Host "    $ZipPath"
    Write-Host ""
    Write-Host "To upload, either:" -ForegroundColor Gray
    Write-Host "  - Drag-drop the zip onto the itch.io project page (under Uploads)" -ForegroundColor Gray
    Write-Host "    and tick 'This file will be played in the browser'." -ForegroundColor Gray
    Write-Host "  - Or rerun with: -Push -ItchTarget user/game:channel" -ForegroundColor Gray
}
