param([Parameter(ValueFromRemainingArguments = $true)][string[]]$CargoArguments)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$localCargo = Join-Path $projectRoot '.tools\cargo\bin\cargo.exe'
if (Test-Path -LiteralPath $localCargo) {
    $env:CARGO_HOME = Join-Path $projectRoot '.tools\cargo'
    $env:RUSTUP_HOME = Join-Path $projectRoot '.tools\rustup'
    $env:Path = "$(Split-Path -Parent $localCargo);$env:Path"
    $gnuTools = Join-Path $env:RUSTUP_HOME 'toolchains\stable-x86_64-pc-windows-gnu\lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained'
    if (Test-Path -LiteralPath $gnuTools) { $env:Path = "$gnuTools;$env:Path" }
    $llvmBin = Split-Path -Parent $gnuTools
    $llvmArchiveTool = Join-Path $llvmBin 'llvm-ar.exe'
    if (Test-Path -LiteralPath $llvmArchiveTool) {
        # llvm-ar is a multicall binary: selecting this documented LLVM driver
        # name runs its dlltool implementation, without an external assembler.
        $nativeTools = Join-Path $projectRoot '.tools\native'
        New-Item -ItemType Directory -Path $nativeTools -Force | Out-Null
        $dlltool = Join-Path $nativeTools 'llvm-dlltool.exe'
        if (-not (Test-Path -LiteralPath $dlltool)) { Copy-Item -LiteralPath $llvmArchiveTool -Destination $dlltool }
        $env:Path = "$llvmBin;$env:Path"
        # The encoded form keeps a path containing spaces in a single argument.
        $env:CARGO_ENCODED_RUSTFLAGS = @("-Cdlltool=$dlltool", '-Clinker=rust-lld.exe', '-Clinker-flavor=ld.lld', '-Clink-self-contained=yes') -join [char]31
    }
    $cargoExecutable = $localCargo
} else {
    $cargoExecutable = (Get-Command cargo -ErrorAction Stop).Source
}
Push-Location $projectRoot
try {
    & $cargoExecutable @CargoArguments
    exit $LASTEXITCODE
} finally { Pop-Location }
