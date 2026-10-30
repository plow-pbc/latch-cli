# Builds Plow Latch's Windows installer: latch in release, placed where
# Tauri takes the window's sidecar from, then the window and its NSIS
# installer (target/release/bundle/nsis/).
$ErrorActionPreference = 'Stop'
$app = $PSScriptRoot
Push-Location "$app\..\.."
cargo build --release --locked
Pop-Location
$triple = (rustc -vV | Select-String '^host: (.+)$').Matches[0].Groups[1].Value
New-Item -ItemType Directory -Force "$app\binaries" | Out-Null
Copy-Item "$app\..\..\target\release\latch.exe" "$app\binaries\latch-$triple.exe" -Force
Push-Location $app
npx tauri build
Pop-Location
