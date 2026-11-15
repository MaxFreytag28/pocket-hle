# Development helper: build touchHLE for the web (wasm32-unknown-emscripten).
#
# Needs the Emscripten SDK; point EMSDK at it or pass -Emsdk. Extra arguments
# are passed on to cargo.
param(
    [string]$Emsdk = $(if ($env:EMSDK) { $env:EMSDK } else { 'D:\Apps\emsdk' }),
    [string]$Log = 'build_web.log',
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$CargoArgs = @()
)
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

$env:Path = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' + [Environment]::GetEnvironmentVariable('Path', 'User')
$env:EMSDK = $Emsdk -replace '\\', '/'
$env:EMSDK_NODE = (Get-ChildItem "$Emsdk\node" -Directory | Select-Object -First 1).FullName + '\node.exe'
$env:EMSDK_PYTHON = (Get-ChildItem "$Emsdk\python" -Directory | Select-Object -First 1).FullName + '\python.exe'
$env:Path = "$Emsdk;$Emsdk\upstream\emscripten;$Emsdk\upstream\bin;" + $env:Path
$env:CMAKE_POLICY_VERSION_MINIMUM = '3.5'

# On Windows, Rust tooling looks for emcc.bat by default, but newer SDKs ship
# emcc.exe, so name the tools explicitly.
$em = "$Emsdk\upstream\emscripten"
$ext = if (Test-Path "$em\emcc.exe") { 'exe' } else { 'bat' }
$env:CC_wasm32_unknown_emscripten = "$em\emcc.$ext"
$env:CXX_wasm32_unknown_emscripten = "$em\em++.$ext"
$env:AR_wasm32_unknown_emscripten = "$em\emar.$ext"
$env:CARGO_TARGET_WASM32_UNKNOWN_EMSCRIPTEN_LINKER = "$em\emcc.$ext"

# The web build has no JIT (the interpreter is the only CPU backend) and uses
# Emscripten's own SDL2 and OpenAL rather than building them from source.
cargo build --release --target wasm32-unknown-emscripten --no-default-features @CargoArgs *> $Log
"web build exit=$LASTEXITCODE (log: $Log)"
exit $LASTEXITCODE
