# Development helper: resolve addresses in the app binary to symbol names using
# its symbol table, and follow pointer chains stored in the file.
param(
    [string]$Ipa = $env:TOUCHHLE_TEST_IPA,
    [string]$Entry = $env:TOUCHHLE_TEST_BINARY,
    [uint32[]]$Addr = @(),
    [uint32[]]$Deref = @(),
    [string]$Find = ''
)
Add-Type -AssemblyName System.IO.Compression.FileSystem
$z = [IO.Compression.ZipFile]::OpenRead($Ipa)
$s = $z.GetEntry($Entry).Open(); $ms = New-Object IO.MemoryStream; $s.CopyTo($ms); $s.Close(); $z.Dispose()
$b = $ms.ToArray()
function U32([int]$o) { [BitConverter]::ToUInt32($b, $o) }
$ncmds = U32 16; $o = 28; $sects = @(); $symoff = 0; $nsyms = 0; $stroff = 0
for ($i = 0; $i -lt $ncmds; $i++) {
    $cmd = U32 $o; $cs = U32 ($o + 4)
    if ($cmd -eq 1) {
        $n = U32 ($o + 48)
        for ($j = 0; $j -lt $n; $j++) {
            $so = $o + 56 + $j * 68
            $sects += [pscustomobject]@{ Name = ([Text.Encoding]::ASCII.GetString($b, $so, 16)).Trim([char]0); Addr = U32 ($so + 32); Size = U32 ($so + 36); Off = U32 ($so + 40) }
        }
    }
    elseif ($cmd -eq 2) { $symoff = U32 ($o + 8); $nsyms = U32 ($o + 12); $stroff = U32 ($o + 16) }
    $o += $cs
}
function FileOff([uint32]$va) {
    foreach ($x in $sects) { if ($va -ge $x.Addr -and $va -lt ($x.Addr + $x.Size) -and $x.Off -ne 0) { return [int]($va - $x.Addr + $x.Off) } }
    return -1
}
function SectOf([uint32]$va) { foreach ($x in $sects) { if ($va -ge $x.Addr -and $va -lt ($x.Addr + $x.Size)) { return $x.Name } } return '?' }
$syms = New-Object System.Collections.Generic.List[object]
for ($i = 0; $i -lt $nsyms; $i++) {
    $e = $symoff + $i * 12
    $strx = U32 $e; $type = $b[$e + 4]; $val = U32 ($e + 8)
    if (($type -band 0xE0) -ne 0 -or ($type -band 0x0E) -ne 0x0E) { continue } # skip debug and undefined
    $p = $stroff + $strx; $q = $p; while ($b[$q] -ne 0) { $q++ }
    $syms.Add([pscustomobject]@{ Val = $val; Name = [Text.Encoding]::ASCII.GetString($b, $p, $q - $p) })
}
$sorted = $syms | Sort-Object Val
"defined symbols: $($sorted.Count)"
function SymFor([uint32]$va) {
    $best = $null
    foreach ($x in $sorted) { if ($x.Val -le $va) { $best = $x } else { break } }
    if ($best) { '{0}+0x{1:x} [{2}]' -f $best.Name, ($va - $best.Val), (SectOf $va) } else { '? [{0}]' -f (SectOf $va) }
}
foreach ($a in $Addr) { '0x{0:x}: {1}' -f $a, (SymFor $a) }
foreach ($a in $Deref) {
    $f = FileOff $a
    if ($f -lt 0) { '0x{0:x}: not in file' -f $a; continue }
    $v = U32 $f
    '[0x{0:x}] ({1}) = 0x{2:x} -> {3}' -f $a, (SymFor $a), $v, (SymFor $v)
    $f2 = FileOff $v
    if ($f2 -ge 0) { $v2 = U32 $f2; '   [0x{0:x}] = 0x{1:x} -> {2}' -f $v, $v2, (SymFor $v2) }
}
if ($Find) { $sorted | Where-Object { $_.Name -match $Find } | Select-Object -First 60 | ForEach-Object { '0x{0:x} {1}' -f $_.Val, $_.Name } }
