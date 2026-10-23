$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$env:CARGO_HOME = Join-Path $projectRoot '.tools\cargo'
$env:RUSTUP_HOME = Join-Path $projectRoot '.tools\rustup'
$cargoBin = Join-Path $env:CARGO_HOME 'bin'
$env:Path = "$cargoBin;$env:Path"
if (-not (Test-Path -LiteralPath (Join-Path $cargoBin 'rustup.exe'))) {
    New-Item -ItemType Directory -Path (Join-Path $projectRoot '.tools') -Force | Out-Null
    $installer = Join-Path $projectRoot '.tools\rustup-init.exe'
    Invoke-WebRequest 'https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-gnu/rustup-init.exe' -OutFile $installer
    & $installer -y --no-modify-path --profile minimal --default-host x86_64-pc-windows-gnu --default-toolchain stable
    if ($LASTEXITCODE -ne 0) { throw 'Rust installation failed.' }
}
& (Join-Path $cargoBin 'rustup.exe') component add rustfmt clippy llvm-tools-preview
if ($LASTEXITCODE -ne 0) { throw 'Rust components installation failed.' }
& (Join-Path $cargoBin 'rustc.exe') --version
& (Join-Path $cargoBin 'cargo.exe') --version
