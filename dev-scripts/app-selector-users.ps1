# Development helper: for a list of Objective-C method names (selectors),
# find which methods of an app use them.
#
# This is for triaging the output of `--dump=linking-info`: an app refers to
# many selectors touchHLE doesn't implement, but usually most of those uses
# are in libraries bundled with the app that never run. This shows who the
# users are, so the ones in the app's own code can be picked out.
#
# It works on 32-bit ARM binaries that still have their symbol table, by
# looking for the addresses of selector references in the literal pools of
# the code. Prints one line per selector: the name, then the classes whose
# methods use it.
param(
    [string]$Ipa = $env:TOUCHHLE_TEST_IPA,
    [string]$Entry = $env:TOUCHHLE_TEST_BINARY,
    # Text file with one selector per line.
    [Parameter(Mandatory = $true)][string]$Selectors,
    # Only show selectors used by at least one class NOT matching this.
    [string]$IgnoreClasses = ''
)
Add-Type -AssemblyName System.IO.Compression.FileSystem
$z = [IO.Compression.ZipFile]::OpenRead($Ipa)
$s = $z.GetEntry($Entry).Open(); $ms = New-Object IO.MemoryStream; $s.CopyTo($ms); $s.Close(); $z.Dispose()
$b = $ms.ToArray()

$ncmds = [BitConverter]::ToUInt32($b, 16); $o = 28
$sects = @(); $symoff = 0; $nsyms = 0; $stroff = 0
for ($i = 0; $i -lt $ncmds; $i++) {
    $cmd = [BitConverter]::ToUInt32($b, $o); $cs = [BitConverter]::ToUInt32($b, $o + 4)
    if ($cmd -eq 1) {
        $n = [BitConverter]::ToUInt32($b, $o + 48)
        for ($j = 0; $j -lt $n; $j++) {
            $so = $o + 56 + $j * 68
            $sects += [pscustomobject]@{
                Name = ([Text.Encoding]::ASCII.GetString($b, $so, 16)).Trim([char]0)
                Addr = [BitConverter]::ToUInt32($b, $so + 32); Size = [BitConverter]::ToUInt32($b, $so + 36)
                Off = [BitConverter]::ToUInt32($b, $so + 40)
            }
        }
    }
    elseif ($cmd -eq 2) {
        $symoff = [BitConverter]::ToUInt32($b, $o + 8); $nsyms = [BitConverter]::ToUInt32($b, $o + 12)
        $stroff = [BitConverter]::ToUInt32($b, $o + 16)
    }
    $o += $cs
}
function FileOff([uint32]$va) {
    foreach ($x in $sects) { if ($va -ge $x.Addr -and $va -lt ($x.Addr + $x.Size) -and $x.Off -ne 0) { return [int]($va - $x.Addr + $x.Off) } }
    return -1
}
function CStr([int]$f) { $e = $f; while ($b[$e] -ne 0) { $e++ }; [Text.Encoding]::ASCII.GetString($b, $f, $e - $f) }

# Selector reference slots of the wanted selectors.
$wanted = New-Object 'System.Collections.Generic.HashSet[string]'
Get-Content $Selectors | ForEach-Object { if ($_.Trim()) { [void]$wanted.Add($_.Trim()) } }
$selrefs = $sects | Where-Object { $_.Name -eq '__objc_selrefs' } | Select-Object -First 1
$slotName = New-Object 'System.Collections.Generic.Dictionary[uint32,string]'
for ($p = 0; $p -lt $selrefs.Size; $p += 4) {
    $target = [BitConverter]::ToUInt32($b, $selrefs.Off + $p)
    $f = FileOff $target
    if ($f -lt 0) { continue }
    $name = CStr $f
    if ($wanted.Contains($name)) { $slotName[[uint32]($selrefs.Addr + $p)] = $name }
}

# Method symbols, sorted by address, for finding the method containing an
# address.
$symAddrs = New-Object 'System.Collections.Generic.List[uint32]'
$symNames = New-Object 'System.Collections.Generic.List[string]'
$pairs = for ($i = 0; $i -lt $nsyms; $i++) {
    $e = $symoff + $i * 12
    $type = $b[$e + 4]
    if (($type -band 0xE0) -ne 0 -or ($type -band 0x0E) -ne 0x0E) { continue }
    [pscustomobject]@{ Addr = [BitConverter]::ToUInt32($b, $e + 8); Name = CStr ($stroff + [BitConverter]::ToUInt32($b, $e)) }
}
foreach ($pair in ($pairs | Sort-Object Addr)) { $symAddrs.Add($pair.Addr); $symNames.Add($pair.Name) }

# Scan the code for literal-pool entries that are one of those slots.
$text = $sects | Where-Object { $_.Name -eq '__text' } | Select-Object -First 1
$users = @{}
for ($p = 0; $p -lt $text.Size - 3; $p += 4) {
    $word = [BitConverter]::ToUInt32($b, $text.Off + $p)
    $name = $null
    if (-not $slotName.TryGetValue($word, [ref]$name)) { continue }
    $index = $symAddrs.BinarySearch([uint32]($text.Addr + $p))
    if ($index -lt 0) { $index = (-bnot $index) - 1 }
    $owner = if ($index -ge 0) { $symNames[$index] } else { '?' }
    # "-[Class method]" or "+[Class(Category) method]" -> Class
    $class = if ($owner -match '^[-+]\[([A-Za-z0-9_]+)') { $Matches[1] } else { $owner }
    if (-not $users.ContainsKey($name)) { $users[$name] = New-Object 'System.Collections.Generic.HashSet[string]' }
    [void]$users[$name].Add($class)
}

foreach ($name in ($wanted | Sort-Object)) {
    $classes = if ($users.ContainsKey($name)) { @($users[$name] | Sort-Object) } else { @() }
    if ($IgnoreClasses) {
        $own = @($classes | Where-Object { $_ -notmatch $IgnoreClasses })
        if ($own.Count -eq 0) { continue }
        '{0}  <-  {1}' -f $name, ($own -join ', ')
    }
    else {
        '{0}  <-  {1}' -f $name, ($(if ($classes.Count) { $classes -join ', ' } else { '(no direct use found)' }))
    }
}
