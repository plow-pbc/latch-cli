# Installs a latch.exe you already have into %LOCALAPPDATA%\Programs\latch,
# or --bin-dir: Windows' install-binary.sh, with the same options.
#
#   powershell -ExecutionPolicy Bypass -File install-binary.ps1 [--bin-dir DIR] [--state-dir DIR] [--client-only] [--non-interactive] [--no-modify-path] [path\to\latch.exe]
#
# With no path, installs the latch.exe beside this script. Options can
# appear before or after the path; --help prints this and installs nothing.
# --browser, --no-browser and --no-login are accepted and do nothing here.
#
# --client-only installs latch alone, for a machine that only asks other
# hosts (an agent's computer). It leaves plugins\client-only in the state
# folder, so `latch update` installs latch alone again; an install without
# it makes this machine a host from then on.
#
# When the binary directory isn't on PATH, it is added to this user's PATH
# (new terminals see it). --no-modify-path, --client-only and
# --non-interactive (`latch update`) leave PATH alone.
#
# A host on the state directory runs the latch being replaced: the new latch
# stops it first (`latch stop`). One `serve -d` started is started again
# with the new latch, with what it was last given (`serve -d --again`), once
# the new latch is in place. One that runs in a terminal can't be started in
# that terminal: the install says to run `latch serve` there again.
#
# Windows can't replace a program while it runs, and the latch running
# `latch update` is the one being replaced: it is moved aside, to
# latch.exe.old-<random> in the same folder, which a later install removes.
$ErrorActionPreference = 'Stop'

$usage = (Get-Content $PSCommandPath | Select-Object -Skip 1 -First 7) -replace '^# ?', ''
$binDir = Join-Path $env:LOCALAPPDATA 'Programs\latch'
$stateDir = $null
$modifyPath = $true
$clientOnly = $false
$src = $null
# Inside a switch's clauses $args is the clause's own: the script's are kept.
$argv = $args
for ($i = 0; $i -lt $argv.Count; $i++) {
  switch -Exact ($argv[$i]) {
    '--help' { $usage; exit 0 }
    '--bin-dir' { $i++; $binDir = $argv[$i] }
    '--state-dir' { $i++; $stateDir = $argv[$i] }
    '--browser' { $i++ }
    '--client-only' { $clientOnly = $true; $modifyPath = $false }
    { $_ -in '--non-interactive', '--no-modify-path' } { $modifyPath = $false }
    { $_ -in '--no-browser', '--no-login' } { }
    default {
      if ($argv[$i] -like '--*') { [Console]::Error.WriteLine("unknown option $($argv[$i])"); exit 2 }
      $src = $argv[$i]
    }
  }
}
if (-not $src) { $src = Join-Path $PSScriptRoot 'latch.exe' }
$src = (Resolve-Path $src).Path
function Say($text) { [Console]::Error.WriteLine("latch install: $text") }
function Die($text) { Say $text; exit 1 }

# What the new latch says it is; it must run here before anything changes.
$state = if ($stateDir) { @('--state-dir', $stateDir) } else { @() }
& $src --version | Out-Null
if ($LASTEXITCODE -ne 0) { Die "$src does not run on this PC; nothing was changed" }

# A host on the state folder: stopped by the new latch, started again after.
$restart = $false
$status = & $src @state status --local 2>$null | Select-Object -Last 1 | ConvertFrom-Json -ErrorAction SilentlyContinue
$hosting = $status.this_machine.hosting
if ($hosting.running) {
  $stopped = & $src @state stop | Select-Object -Last 1 | ConvertFrom-Json -ErrorAction SilentlyContinue
  if ($LASTEXITCODE -ne 0 -or -not $stopped.stopped) { Die "could not stop the host on the state folder; nothing was changed" }
  if ($hosting.attached) { Say "stopped the host, which ran in a terminal; run ``latch serve`` again to start it with the new latch" }
  else { $restart = $true; Say "stopped the host; it starts again with the new latch once the install is done" }
}

New-Item -ItemType Directory -Force $binDir | Out-Null
$dest = Join-Path $binDir 'latch.exe'
# Leftovers from earlier installs go, unless something still runs them.
Get-ChildItem $binDir -Filter 'latch.exe.old-*' -ErrorAction SilentlyContinue |
  ForEach-Object { Remove-Item $_.FullName -Force -ErrorAction SilentlyContinue }
$aside = $null
if ((Test-Path $dest) -and ((Resolve-Path $dest).Path -ne $src)) {
  $aside = Join-Path $binDir ('latch.exe.old-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
  Move-Item $dest $aside
}
if ((Resolve-Path -ErrorAction SilentlyContinue $dest).Path -ne $src) {
  try { Copy-Item $src $dest }
  catch {
    if ($aside) { Move-Item $aside $dest -Force }
    Die "could not put latch in $binDir ($($_.Exception.Message)); the old latch is back in place"
  }
}
Say "installed $dest"

# Whether latch is installed alone, kept where `latch update` looks (latch
# makes the state folder private when it first uses it).
$stateFolder = if ($stateDir) { $stateDir } elseif ($env:LATCH_STATE_DIR) { $env:LATCH_STATE_DIR } else { Join-Path $HOME '.latch' }
$record = Join-Path $stateFolder 'plugins\client-only'
if ($clientOnly) {
  New-Item -ItemType Directory -Force (Split-Path $record) | Out-Null
  New-Item -ItemType File -Force $record | Out-Null
} else {
  Remove-Item -Force $record -ErrorAction SilentlyContinue
}

if ($modifyPath) {
  $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
  if (-not (($userPath -split ';') -contains $binDir)) {
    [Environment]::SetEnvironmentVariable('Path', (@($userPath, $binDir) | Where-Object { $_ }) -join ';', 'User')
    Say "added $binDir to your PATH; open a new terminal to use latch"
  }
}

if ($restart) {
  # Through ShellExecute, which hands the host none of this script's handles:
  # a host started with `&` would hold this script's output pipe open, and
  # whoever reads that output would wait for the host to end.
  $started = Start-Process -FilePath $dest -ArgumentList (@($state | ForEach-Object { "`"$_`"" }) + @('serve', '-d', '--again')) -WindowStyle Hidden -PassThru
  # Only `serve -d` itself: -Wait would wait for the host it starts, too.
  $started.WaitForExit()
  if ($started.ExitCode -ne 0) { Die "the new latch is installed, but the host did not start again; run ``latch serve -d --again``" }
  Say "started the host again with the new latch"
}
exit 0
