# Development helper: run an app in touchHLE, optionally click in its window, and
# save screenshots of the window at given times (seconds after launch).
param(
    [string]$Ipa = $env:TOUCHHLE_TEST_IPA,
    [double[]]$ShotAt = @(15),
    # Clicks as "seconds:x:y" with x,y as fractions (0..1) of the client area.
    [string[]]$Clicks = @(),
    # Typed text as "seconds:text"; a newline character in text presses Return.
    [string[]]$Keys = @(),
    [string]$Prefix = 'shot',
    [int]$Tail = 12,
    # Extra command-line options for touchHLE, e.g. '--gles1=gles1_on_gles2'.
    [string]$ExtraArgs = '',
    [string]$Exe = '.\target\release\touchHLE.exe'
)
$env:TOUCHHLE_NO_CRASH_DIALOG = '1'
$env:TOUCHHLE_TICK_STATS = '1'
Set-Location (Split-Path $PSScriptRoot -Parent)
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class HcWin {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern IntPtr PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
}
'@
[void][HcWin]::SetProcessDPIAware()

$p = Start-Process -FilePath $Exe -ArgumentList ("`"$Ipa`" $ExtraArgs".Trim()) -RedirectStandardOutput run_out.log -RedirectStandardError run_err.log -PassThru
$sw = [Diagnostics.Stopwatch]::StartNew()

$events = @()
foreach ($t in $ShotAt) { $events += [pscustomobject]@{ T = $t; Kind = 'shot'; X = 0; Y = 0 } }
foreach ($c in $Clicks) { $parts = $c.Split(':'); $events += [pscustomobject]@{ T = [double]$parts[0]; Kind = 'click'; X = [double]$parts[1]; Y = [double]$parts[2] } }
foreach ($k in $Keys) { $i = $k.IndexOf(':'); $events += [pscustomobject]@{ T = [double]$k.Substring(0, $i); Kind = 'keys'; X = 0; Y = 0; Text = $k.Substring($i + 1) } }
$n = 0
foreach ($e in ($events | Sort-Object T)) {
    $wait = [int](($e.T * 1000) - $sw.ElapsedMilliseconds)
    if ($wait -gt 0) { if ($p.WaitForExit($wait)) { break } }
    if ($p.HasExited) { break }
    $p.Refresh(); $h = $p.MainWindowHandle
    if ($h -eq [IntPtr]::Zero) { "no window at $($e.T)s"; continue }
    $r = New-Object HcWin+RECT; [void][HcWin]::GetClientRect($h, [ref]$r)
    if ($e.Kind -eq 'click') {
        $x = [int]($r.R * $e.X); $y = [int]($r.B * $e.Y)
        $lp = [IntPtr](($y -shl 16) -bor ($x -band 0xFFFF))
        # WM_MOUSEMOVE, WM_LBUTTONDOWN, WM_LBUTTONUP
        [void][HcWin]::PostMessage($h, 0x0200, [IntPtr]0, $lp)
        [void][HcWin]::PostMessage($h, 0x0201, [IntPtr]1, $lp)
        Start-Sleep -Milliseconds 120
        [void][HcWin]::PostMessage($h, 0x0202, [IntPtr]0, $lp)
        "click at $($e.T)s -> ($x,$y) of $($r.R)x$($r.B)"
    }
    elseif ($e.Kind -eq 'keys') {
        foreach ($ch in $e.Text.ToCharArray()) {
            if ($ch -eq "`n") {
                # WM_KEYDOWN / WM_KEYUP for VK_RETURN
                [void][HcWin]::PostMessage($h, 0x0100, [IntPtr]0x0D, [IntPtr]0x001C0001)
                [void][HcWin]::PostMessage($h, 0x0101, [IntPtr]0x0D, [IntPtr]0xC01C0001)
            }
            else {
                [void][HcWin]::PostMessage($h, 0x0102, [IntPtr][int]$ch, [IntPtr]1) # WM_CHAR
            }
            Start-Sleep -Milliseconds 80
        }
        "typed at $($e.T)s: $($e.Text.Replace("`n", '<Return>'))"
    }
    else {
        $n++
        $bmp = New-Object Drawing.Bitmap $r.R, $r.B
        $g = [Drawing.Graphics]::FromImage($bmp)
        $hdc = $g.GetHdc()
        # 1 = client area only, 2 = render full content (needed for GPU windows)
        $ok = [HcWin]::PrintWindow($h, $hdc, 3)
        $g.ReleaseHdc($hdc); $g.Dispose()
        $file = Join-Path (Get-Location) ("{0}{1}.png" -f $Prefix, $n)
        $bmp.Save($file, [Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
        "shot $n at $($e.T)s: $($r.R)x$($r.B) ok=$ok -> $file"
    }
}
Start-Sleep -Milliseconds 500
if ($p.HasExited) { 'RESULT: exited by itself' } else { 'RESULT: still running, stopped it'; Stop-Process -Id $p.Id -Force }
$all = @(Get-Content run_out.log) + @(Get-Content run_err.log)
$all | Where-Object { $_ -notmatch 'libxml2:|sqlite3:|__cxa_atexit|non-lazy symbol|fake class|^\s+R\d|^\s*R1[0-2]|host function' } | ForEach-Object { if ($_.Length -gt 220) { $_.Substring(0, 220) + ' ...' } else { $_ } } | Select-Object -Last $Tail
