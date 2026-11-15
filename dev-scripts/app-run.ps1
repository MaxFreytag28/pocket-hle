# Development helper: rebuild touchHLE, run an app for a while, print the end of its log.
param(
    [string]$Ipa = $env:TOUCHHLE_TEST_IPA,
    [int]$Seconds = 45,
    [int]$Tail = 45,
    [switch]$NoBuild
)
$env:Path = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' + [Environment]::GetEnvironmentVariable('Path', 'User')
$env:CMAKE_POLICY_VERSION_MINIMUM = '3.5'
$env:TOUCHHLE_NO_CRASH_DIALOG = '1'
Set-Location (Split-Path $PSScriptRoot -Parent)
if (-not $NoBuild) {
    cargo build --release *> build_patch.log
    "build exit=$LASTEXITCODE"
    if ($LASTEXITCODE -ne 0) {
        Select-String -Path build_patch.log -Pattern '^error' -Context 0, 18 | Select-Object -First 6 | ForEach-Object { $_.ToString() }
        return
    }
}
$p = Start-Process -FilePath .\target\release\touchHLE.exe -ArgumentList "`"$Ipa`"" -RedirectStandardOutput run_out.log -RedirectStandardError run_err.log -PassThru
$null = $p.WaitForExit($Seconds * 1000)
if ($p.HasExited) { 'RESULT: exited by itself' } else { "RESULT: still running after $Seconds s, stopped it"; Stop-Process -Id $p.Id -Force }
$all = @(Get-Content run_out.log) + @(Get-Content run_err.log)
"total lines: $($all.Count)"
$all | Where-Object { $_ -notmatch '__cxa_atexit|non-lazy symbol|fake class' } | Select-Object -Last $Tail
