# Builds Plow Latch's Windows installer: latch in release, placed where
# Tauri takes the window's sidecar from, then the window and its NSIS
# installer (target/release/bundle/nsis/).
$ErrorActionPreference = 'Stop'
# A native command's failure is only in its exit code, which 'Stop' ignores:
# each one is checked, so a failed build never packages an older latch.exe.
function Native([scriptblock] $command) {
    & $command
    if ($LASTEXITCODE -ne 0) { throw "failed ($LASTEXITCODE): $command" }
}
$app = $PSScriptRoot
Push-Location "$app\..\.."
try { Native { cargo build --release --locked } } finally { Pop-Location }
$triple = (Native { rustc -vV } | Select-String '^host: (.+)$').Matches[0].Groups[1].Value
New-Item -ItemType Directory -Force "$app\binaries" | Out-Null
Copy-Item "$app\..\..\target\release\latch.exe" "$app\binaries\latch-$triple.exe" -Force
Push-Location $app
try { Native { npx tauri build } } finally { Pop-Location }
