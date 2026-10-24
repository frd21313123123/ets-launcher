param([string]$Target = $env:CARGO_BUILD_TARGET)

$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$buildArguments = @('build', '--release', '--locked')
if ($Target) { $buildArguments += @('--target', $Target) }
& powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'cargo.ps1') @buildArguments
if ($LASTEXITCODE -ne 0) { throw 'Build failed.' }
$distribution = Join-Path $projectRoot 'dist'
New-Item -ItemType Directory -Path $distribution -Force | Out-Null
$releaseDirectory = if ($Target) { Join-Path $projectRoot "target\$Target\release" } else { Join-Path $projectRoot 'target\release' }
Copy-Item -LiteralPath (Join-Path $releaseDirectory 'ets-launcher.exe') -Destination $distribution
Copy-Item -LiteralPath (Join-Path $projectRoot 'README.md') -Destination $distribution
Copy-Item -LiteralPath (Join-Path $projectRoot 'LICENSE') -Destination $distribution
Copy-Item -LiteralPath (Join-Path $projectRoot 'assets\branding\icon.ico') -Destination (Join-Path $distribution 'ets-launcher.ico')
$archiveTools = Join-Path $distribution 'tools\7zip'
New-Item -ItemType Directory -Path $archiveTools -Force | Out-Null
foreach ($assetName in @('7z.exe', '7z.dll', 'License.txt', 'LGPL-2.1.txt', 'ORIGIN.md')) {
    Copy-Item -LiteralPath (Join-Path $projectRoot "assets\7zip\$assetName") -Destination $archiveTools
}
# Include GNU runtime DLLs when using the project's local GNU toolchain.
$runtimeBin = Join-Path $projectRoot '.tools\rustup\toolchains\stable-x86_64-pc-windows-gnu\lib\rustlib\x86_64-pc-windows-gnu\bin'
foreach ($library in @('libgcc_s_seh-1.dll', 'libwinpthread-1.dll')) {
    $libraryPath = Join-Path $runtimeBin $library
    if ($Target -notlike '*msvc' -and (Test-Path -LiteralPath $libraryPath)) { Copy-Item -LiteralPath $libraryPath -Destination $distribution }
}
Write-Output "Ready: $distribution\ets-launcher.exe"
