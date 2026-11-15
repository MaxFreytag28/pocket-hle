# Development helper: gather the web build into one folder that can be served.
#
# Run dev-scripts/web-build.ps1 first. For local testing, -TestApp copies an
# app file into the folder as test-app.ipa, which web/index.html loads when
# opened with ?autoload. The folder is ignored by git; never publish it with
# an app in it.
param(
    [string]$Out = 'web-dist',
    [string]$TestApp = $env:TOUCHHLE_TEST_IPA
)
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root
$release = 'target\wasm32-unknown-emscripten\release'

New-Item -ItemType Directory -Force $Out | Out-Null
Copy-Item "$release\touchHLE.js", "$release\touchHLE.wasm" $Out -Force
# The resource package is written next to the linker's own output, which cargo
# doesn't copy. Take the one from the most recent link.
$data = Get-ChildItem "$release\build\touchHLE" -Recurse -Filter touchHLE.data |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $data) { throw 'touchHLE.data not found; did the web build succeed?' }
Copy-Item $data.FullName $Out -Force
Copy-Item web\* $Out -Force
if ($TestApp) { Copy-Item $TestApp (Join-Path $Out 'test-app.ipa') -Force }

Get-ChildItem $Out | ForEach-Object { '{0,12:N0}  {1}' -f $_.Length, $_.Name }
