# Builds the installer: the app first, then the setup exe with the app built into it.
# Run: powershell -NoProfile -File tools/build-setup.ps1   ->  dist\NF-Switcher-Setup-<version>.exe
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
cargo build --release --offline --bin nf-switcher
if ($LASTEXITCODE) { throw 'building the app failed' }
$env:NFSW_APP_EXE = Join-Path $root 'target\release\nf-switcher.exe'
cargo build --release --offline --features setup --bin nf-switcher-setup
if ($LASTEXITCODE) { throw 'building the setup failed' }
Remove-Item Env:\NFSW_APP_EXE
New-Item -ItemType Directory -Force (Join-Path $root 'dist') | Out-Null
$out = Join-Path $root "dist\NF-Switcher-Setup-$version.exe"
Copy-Item (Join-Path $root 'target\release\nf-switcher-setup.exe') $out -Force
$hash = (Get-FileHash $out -Algorithm SHA256).Hash
"built $out ($([math]::Round((Get-Item $out).Length / 1KB)) KB) SHA256 $hash"
