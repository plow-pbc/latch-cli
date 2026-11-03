# What `latch setup`'s administrator step does to a real Windows, checked on
# one where this shell is an administrator, as GitHub's Windows runners are:
# nul opens to workers, its startup task is as asked for, a standard user's
# token can't touch the task, and `latch logout --purge` takes it all back.
#
#   powershell -ExecutionPolicy Bypass -File scripts/check-null-device.ps1 [PATH-TO-LATCH]
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$latch = (Resolve-Path $(if ($args.Count) { $args[0] } else { "$here\..\latch\target\debug\latch.exe" })).Path
$standard = "$here\as-standard-user.ps1"
$name = 'latch null device'
$capability = 'S-1-15-3-1024-2072464065-3400578307-2891438376-2029114129-1094856869-133864159-224730807-2445515868'
$failed = @()
function Check($what, $holds) { if ($holds) { "ok: $what" } else { "FAILED: $what"; $script:failed += $what } }
# As a standard user, to the end whatever it says: its exit code, what it said
# in $said.
function AsStandard {
  $ErrorActionPreference = 'Continue'
  $script:said = & powershell -NoProfile -ExecutionPolicy Bypass -File $standard @args *>&1 | Out-String
  $code = $LASTEXITCODE
  Write-Host $script:said
  $code
}
function NulList { [IO.File]::GetAccessControl('\\.\NUL').GetSecurityDescriptorSddlForm('Access') }
function Entries($sddl) { @([regex]::Matches($sddl, '\([^)]*\)') | ForEach-Object { $_.Value }) | Sort-Object }

$before = NulList
"nul before: $before"
Check 'nul admits no worker before' ($before -notmatch $capability)
& $latch grant-listing | Out-Host
Check 'grant-listing succeeded' ($LASTEXITCODE -eq 0)

$after = NulList
$added = @(Compare-Object (Entries $before) (Entries $after) | Where-Object { $_.SideIndicator -eq '=>' } | ForEach-Object { $_.InputObject })
$lost = @(Compare-Object (Entries $before) (Entries $after) | Where-Object { $_.SideIndicator -eq '<=' })
Check 'nul gained one entry, for the capability, as Everyone has it' ($added.Count -eq 1 -and $added[0] -match $capability -and $added[0] -match '^\(A;;0x1201bf;;;')
Check 'nul lost no entry' ($lost.Count -eq 0)

$task = Get-ScheduledTask -TaskName $name
$action = @($task.Actions)
Check 'one action' ($action.Count -eq 1)
Check 'it runs System32 PowerShell' ($action[0].Execute -ieq "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe")
Check 'no profile, not interactive' ($action[0].Arguments -clike '-NoProfile -NonInteractive -Command *')
Check 'no Add-Type, module or file' ($action[0].Arguments -notmatch 'Add-Type|Import-Module|New-Object|-File|\$env|%|Users')
Check 'one boot trigger' (@($task.Triggers).Count -eq 1 -and @($task.Triggers)[0].CimClass.CimClassName -eq 'MSFT_TaskBootTrigger')
Check 'runs as SYSTEM' ($task.Principal.UserId -eq 'SYSTEM')
Check 'starts on battery' (-not $task.Settings.DisallowStartIfOnBatteries)
Check 'stays on battery' (-not $task.Settings.StopIfGoingOnBatteries)
Check 'says what it is' ($task.Description -match 'null device' -and $task.Author -eq 'latch')
$me = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$scheduler = New-Object -ComObject Schedule.Service
$scheduler.Connect()
$access = $scheduler.GetFolder('\').GetTask($name).GetSecurityDescriptor(4)
"task's access list: $access"
Check "no entry names who registered it" ($access -notmatch $me)
Check 'only SYSTEM and Administrators may write' (@((Entries $access) | Where-Object { $_ -notmatch ';;;(SY|BA)\)$' -and $_ -notmatch '^\(A;;(FR|GR);;;' }).Count -eq 0)
$file = "$env:SystemRoot\System32\Tasks\$name"
$owner = (Get-Acl -LiteralPath $file).Owner
"task file's owner: $owner"
Check 'no user owns its file' ($owner -match 'Administrators|SYSTEM')

Check 'a standard token cannot delete it' ((AsStandard schtasks /delete /tn $name /f) -ne 0)
Check 'a standard token cannot disable it' ((AsStandard schtasks /change /tn $name /disable) -ne 0)
Check 'a standard token cannot take its file' ((AsStandard takeown /f $file) -ne 0)
Check 'a standard token cannot change its list' ((AsStandard icacls $file /grant 'Everyone:F') -ne 0)
Check 'the task is still there and enabled' ((Get-ScheduledTask -TaskName $name).State -ne 'Disabled')
$list = (Get-Acl -LiteralPath $file).Sddl
Check 'its file still lists no user' ($list -notmatch 'WD\)' -and $list -notmatch $me)

Push-Location "$here\..\latch"
$env:LATCH_TEST_NULL_DEVICE_OPEN = '1'
$code = AsStandard cargo nextest run --locked --features fake-gog --test cli --test worker a_program_can_use_the_null_device_or_is_told_why_not everyday_tools_work_reading_only_the_area_and_the_system
Remove-Item Env:LATCH_TEST_NULL_DEVICE_OPEN
Pop-Location
Check 'a worker gets through, and git and the others run, as a standard user' ($code -eq 0 -and $said -match '2 tests run: 2 passed')

$home2 = Join-Path $env:TEMP ("latch-null-" + [guid]::NewGuid())
New-Item -ItemType Directory $home2 | Out-Null
$env:USERPROFILE, $savedProfile = $home2, $env:USERPROFILE
$env:HOME = $home2
$code = AsStandard $latch setup --status
$step = ($said | ConvertFrom-Json).steps | Where-Object { $_.step -eq 'null_device' }
Check 'setup --status reads it open with no administrator' ($step.state -eq 'done')

$purged = & $latch logout --purge --yes | Out-String
"logout --purge: $purged"
$undone = $purged | ConvertFrom-Json
Check 'purge says it removed it' ($undone.this_pc.removed -eq $true)
Check 'the task is gone' ($null -eq (Get-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue))
$end = NulList
"nul after purge: $end"
Check 'nul is back exactly as it was' ($end -ceq $before)
foreach ($root in (Get-CimInstance Win32_LogicalDisk -Filter 'DriveType=3').DeviceID) {
  Check "$root\ has no entry left" ([IO.Directory]::GetAccessControl("$root\").GetSecurityDescriptorSddlForm('Access') -notmatch $capability)
}
$env:USERPROFILE = $savedProfile
Remove-Item -Recurse -Force $home2

if ($failed.Count) { "FAILED: $($failed.Count)"; exit 1 }
'all held'
