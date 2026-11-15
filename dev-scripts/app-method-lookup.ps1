# Development helper: find the Objective-C method containing a code address in the
# app binary inside an .ipa, and dump the instruction words around it.
param(
    [string]$Ipa = $env:TOUCHHLE_TEST_IPA,
    [string]$Entry = $env:TOUCHHLE_TEST_BINARY,
    [uint32]$Addr,
    [int]$Before = 40,
    [int]$After = 4
)
Add-Type -AssemblyName System.IO.Compression.FileSystem
$z = [IO.Compression.ZipFile]::OpenRead($Ipa)
$s = $z.GetEntry($Entry).Open(); $ms = New-Object IO.MemoryStream; $s.CopyTo($ms); $s.Close(); $z.Dispose()
$b = $ms.ToArray()
function U32([int]$o) { [BitConverter]::ToUInt32($b, $o) }

# Parse segments/sections.
$ncmds = U32 16
$o = 28
$sects = @()
for ($i = 0; $i -lt $ncmds; $i++) {
    $cmd = U32 $o; $cs = U32 ($o + 4)
    if ($cmd -eq 1) {
        $nsects = U32 ($o + 48)
        for ($j = 0; $j -lt $nsects; $j++) {
            $so = $o + 56 + $j * 68
            $sects += [pscustomobject]@{
                Name = ([Text.Encoding]::ASCII.GetString($b, $so, 16)).Trim([char]0)
                Seg  = ([Text.Encoding]::ASCII.GetString($b, $so + 16, 16)).Trim([char]0)
                Addr = U32 ($so + 32); Size = U32 ($so + 36); Off = U32 ($so + 40)
            }
        }
    }
    $o += $cs
}
function FileOff([uint32]$va) {
    foreach ($x in $sects) { if ($va -ge $x.Addr -and $va -lt ($x.Addr + $x.Size) -and $x.Off -ne 0) { return [int]($va - $x.Addr + $x.Off) } }
    return -1
}
function CStr([uint32]$va) {
    $f = FileOff $va; if ($f -lt 0) { return $null }
    $e = $f; while ($b[$e] -ne 0 -and ($e - $f) -lt 200) { $e++ }
    [Text.Encoding]::ASCII.GetString($b, $f, $e - $f)
}
$text = $sects | Where-Object { $_.Name -eq '__text' } | Select-Object -First 1
$tlo = $text.Addr; $thi = $text.Addr + $text.Size

# Scan data sections for {name, types, imp} method entries.
$best = $null
foreach ($x in ($sects | Where-Object { $_.Seg -in '__DATA', '__OBJC' -and $_.Off -ne 0 })) {
    for ($p = $x.Off; $p -le ($x.Off + $x.Size - 12); $p += 4) {
        $imp = U32 ($p + 8)
        $impa = $imp -band 0xFFFFFFFE
        if ($impa -lt $tlo -or $impa -ge $thi -or $impa -gt $Addr) { continue }
        if ($best -and $impa -le $best.Imp) { continue }
        $n = U32 $p; $t = U32 ($p + 4)
        $ns = CStr $n; if (-not $ns -or $ns -notmatch '^[A-Za-z_][A-Za-z0-9_:]*$') { continue }
        $ts = CStr $t; if (-not $ts -or $ts -notmatch '^[\^vcislqCISLQfdB@#:\*\{\[rnNoORV]') { continue }
        $best = [pscustomobject]@{ Imp = $impa; Thumb = ($imp -band 1); Name = $ns; Types = $ts }
    }
}
if ($best) { 'method: {0}  types={1}  imp=0x{2:x}  thumb={3}  (+0x{4:x})' -f $best.Name, $best.Types, $best.Imp, $best.Thumb, ($Addr - $best.Imp) } else { 'no method found' }

$f = FileOff $Addr
for ($a = $Addr - 4 * $Before; $a -le $Addr + 4 * $After; $a += 4) {
    $w = U32 (FileOff $a)
    '{0:x8}: {1:x8}{2}' -f $a, $w, $(if ($a -eq $Addr) { '  <== PC' } else { '' })
}
