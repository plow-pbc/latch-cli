# Installs a latch.exe you already have into %LOCALAPPDATA%\Programs\latch,
# or --bin-dir: Windows' install-binary.sh, with the same options.
#
#   powershell -ExecutionPolicy Bypass -File install-binary.ps1 [--bin-dir DIR] [--state-dir DIR] [--browser chrome|camoufox]... [--no-browser] [--client-only] [--no-login] [--non-interactive] [--no-modify-path] [path\to\latch.exe]
#
# With no path, installs the latch.exe beside this script. Options can
# appear before or after the path; --help prints this and installs nothing.
#
# A first install (no login in the state folder yet) at a terminal ends by
# running `latch setup` (log in, connect Google, and so on); --non-interactive,
# or no terminal, leaves that for later. An upgrade never runs it: setup
# could start a host the upgrade did not mean to.
#
# It also installs gog, the Google CLI a host runs for `latch google`, at the
# version this latch pins (`latch install-gog`), and a browser engine for
# `latch browser` (`latch browser setup`): Camoufox by default, as on macOS
# and Linux, or the engines `--browser` names (chrome, camoufox; repeat it
# for both). `--no-browser` installs none. Then it
# removes the helper versions an older latch installed and this one does
# not run (`latch prune-helpers`). Each lives in the state folder's plugins
# folder, ~\.latch\plugins unless --state-dir names another.
#
# --client-only installs latch alone, for a machine that only asks other
# hosts (an agent's computer): no gog or browser engine, and no setup at the
# end, only the word to run `latch login`. It leaves plugins\client-only in
# the state folder, so `latch update` installs latch alone again; an install
# without it makes this machine a host from then on. At a terminal, with
# this machine holding an agent's Plow credential ($PLOW_AGENT_TOKEN, the
# file $PLOW_TOKEN_FILE names, or $PLOW_API_BASE's proxy) and a state folder
# not logged in yet, it runs `latch login` itself; --no-login,
# --non-interactive or no terminal leaves the login to whoever runs it.
#
# When the binary directory isn't on PATH, it is added to this user's PATH
# (new terminals see it). --no-modify-path, --client-only and
# --non-interactive (`latch update`) leave PATH alone.
#
# A host on the state directory runs the latch being replaced: the new latch
# stops it first (`latch stop`). One `serve -d` started is started again
# with the new latch, with what it was last given (`serve -d --again`), once
# everything is in place. One that runs in a terminal can't be started in
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
$interactive = $true
$login = $true
$browsers = @()
$noBrowser = $false
$src = $null
# Inside a switch's clauses $args is the clause's own: the script's are kept.
$argv = $args
for ($i = 0; $i -lt $argv.Count; $i++) {
  switch -Exact ($argv[$i]) {
    '--help' { $usage; exit 0 }
    '--bin-dir' { $i++; $binDir = $argv[$i] }
    '--state-dir' { $i++; $stateDir = $argv[$i] }
    '--browser' {
      $i++
      if ($argv[$i] -notin 'chrome', 'camoufox') { [Console]::Error.WriteLine("--browser takes chrome or camoufox"); exit 2 }
      if ($argv[$i] -notin $browsers) { $browsers += $argv[$i] }
    }
    '--no-browser' { $noBrowser = $true }
    '--client-only' { $clientOnly = $true; $modifyPath = $false }
    '--non-interactive' { $interactive = $false; $modifyPath = $false }
    '--no-modify-path' { $modifyPath = $false }
    '--no-login' { $login = $false }
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
if ($noBrowser) { $browsers = @() } elseif (-not $browsers) { $browsers = @('camoufox') }
# At a terminal: one this script's user reads and types in.
$terminal = $interactive -and [Environment]::UserInteractive -and -not [Console]::IsInputRedirected -and -not [Console]::IsOutputRedirected

# What the new latch says it is; it must run here before anything changes.
$state = if ($stateDir) { @('--state-dir', $stateDir) } else { @() }
& $src --version | Out-Null
if ($LASTEXITCODE -ne 0) { Die "$src does not run on this PC; nothing was changed" }

# 0. A host on the state folder: stopped by the new latch, started again after.
$restart = $false
$status = & $src @state status --local 2>$null | Select-Object -Last 1 | ConvertFrom-Json -ErrorAction SilentlyContinue
$hosting = $status.this_machine.hosting
if ($hosting.running) {
  $stopped = & $src @state stop | Select-Object -Last 1 | ConvertFrom-Json -ErrorAction SilentlyContinue
  if ($LASTEXITCODE -ne 0 -or -not $stopped.stopped) { Die "could not stop the host on the state folder; nothing was changed" }
  if ($hosting.attached) { Say "stopped the host, which ran in a terminal; run ``latch serve`` again to start it with the new latch" }
  else { $restart = $true; Say "stopped the host; it starts again with the new latch once the install is done" }
}

# 1. latch itself.
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

# 2. PATH.
if ($modifyPath) {
  $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
  if (-not (($userPath -split ';') -contains $binDir)) {
    [Environment]::SetEnvironmentVariable('Path', (@($userPath, $binDir) | Where-Object { $_ }) -join ';', 'User')
    Say "added $binDir to your PATH; open a new terminal to use latch"
  }
}

# 3. A host's helpers: gog, the browser engines, and none of the versions
# this latch does not run. The host was stopped in step 0.
if (-not $clientOnly) {
  $gog = & $dest @state install-gog | Select-Object -Last 1 | ConvertFrom-Json -ErrorAction SilentlyContinue
  if ($LASTEXITCODE -ne 0 -or -not $gog.gog) { Die "could not install gog (see above)" }
  if ($gog.installed) { Say "installed gog $($gog.version) to $($gog.gog)" } else { Say "gog $($gog.version) already installed" }
  foreach ($engine in $browsers) {
    $set = & $dest @state browser setup --engine $engine | Select-Object -Last 1 | ConvertFrom-Json -ErrorAction SilentlyContinue
    if ($LASTEXITCODE -ne 0 -or -not $set.folder) { Die "could not install the $engine browser engine (see above)" }
    Say "$engine $($set.version) is in $($set.folder)"
  }
  & $dest @state prune-helpers | Out-Null
  if ($LASTEXITCODE -ne 0) { Say "could not remove older helper versions from $stateFolder\plugins (see above)" }
}

# 4. The host stopped in step 0, started again by the new latch.
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

# 5. A first install: no login in the state folder yet. At a terminal,
# `latch setup` takes it from here (or, latch alone, a login with this
# machine's agent credential); whatever happens there, the install is done.
if (-not (Test-Path (Join-Path $stateFolder 'config.json'))) {
  $loggedOut = { (& $dest @state status --local 2>$null | Select-Object -Last 1) -like '{"login":null,*' }
  $credential = ($env:PLOW_AGENT_TOKEN -and $env:PLOW_AGENT_TOKEN.Trim()) -or
    ($env:PLOW_TOKEN_FILE -and (Test-Path $env:PLOW_TOKEN_FILE)) -or
    ($env:PLOW_API_BASE -and $env:PLOW_API_BASE.Trim())
  if ($clientOnly) {
    if ($login -and $terminal -and $credential -and (& $loggedOut)) {
      Say "logging in with this machine's agent credential"
      & $dest @state login
      if ($LASTEXITCODE -eq 0) { Say "logged in" } else { Say "the login did not work (see above); run ``latch login`` to try again" }
    } else {
      Say "next: run ``latch login`` to log in"
    }
  } elseif (-not $terminal) {
    Say "next: run ``latch setup`` to log in and set up the rest"
  } else {
    Say "running latch setup"
    & $dest @state setup
    if ($LASTEXITCODE -ne 0) { Say "setup did not finish; run ``latch setup`` any time" }
  }
}
Say "done"
exit 0
