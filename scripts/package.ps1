param([string]$Target = $env:CARGO_BUILD_TARGET)

$ErrorActionPreference = 'Stop'
function Copy-PackageBinary([string]$Source, [string]$Directory) {
    $destination = Join-Path $Directory (Split-Path -Leaf $Source)
    # An open launcher can keep unchanged runtime DLLs mapped. They already
    # contain the exact packaged bytes and do not need to be overwritten.
    if ((Test-Path -LiteralPath $destination -PathType Leaf) -and
        (Get-FileHash -LiteralPath $Source -Algorithm SHA256).Hash -eq
        (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash) { return }
    Copy-Item -LiteralPath $Source -Destination $destination
}
$projectRoot = Split-Path -Parent $PSScriptRoot
$buildArguments = @('build', '--release', '--locked')
if ($Target) { $buildArguments += @('--target', $Target) }
& powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'cargo.ps1') @buildArguments
if ($LASTEXITCODE -ne 0) { throw 'Build failed.' }
$distribution = Join-Path $projectRoot 'dist'
New-Item -ItemType Directory -Path $distribution -Force | Out-Null
$releaseDirectory = if ($Target) { Join-Path $projectRoot "target\$Target\release" } else { Join-Path $projectRoot 'target\release' }
Copy-PackageBinary (Join-Path $releaseDirectory 'ets-launcher.exe') $distribution
Copy-Item -LiteralPath (Join-Path $projectRoot 'README.md') -Destination $distribution
Copy-Item -LiteralPath (Join-Path $projectRoot 'LICENSE') -Destination $distribution
Copy-Item -LiteralPath (Join-Path $projectRoot 'assets\licenses\map-manager.txt') -Destination (Join-Path $distribution 'Map-Manager-LICENSES.txt')
# MSVC links WebView2Loader statically. GNU development builds need its DLL.
if ($Target -notlike '*msvc') {
    $webViewLoader = Get-ChildItem -Path (Join-Path $releaseDirectory 'build\webview2-com-sys-*\out\x64\WebView2Loader.dll') -File | Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if (-not $webViewLoader) { throw 'WebView2 loader was not built.' }
    Copy-PackageBinary $webViewLoader.FullName $distribution
}
$mapDocs=Join-Path $distribution 'docs'
New-Item -ItemType Directory -Path $mapDocs -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $projectRoot 'docs\quper-verification.md') -Destination $mapDocs
if (Test-Path -LiteralPath (Join-Path $mapDocs 'maps-verification.md')) { Remove-Item -LiteralPath (Join-Path $mapDocs 'maps-verification.md') }
Copy-Item -LiteralPath (Join-Path $projectRoot 'assets\branding\icon.ico') -Destination (Join-Path $distribution 'ets-launcher.ico')
# The 7-Zip binaries and licenses are embedded in ets-launcher.exe.
# Include GNU runtime DLLs when using the project's local GNU toolchain.
$runtimeBin = Join-Path $projectRoot '.tools\rustup\toolchains\stable-x86_64-pc-windows-gnu\lib\rustlib\x86_64-pc-windows-gnu\bin'
foreach ($library in @('libgcc_s_seh-1.dll', 'libwinpthread-1.dll')) {
    $libraryPath = Join-Path $runtimeBin $library
    if ($Target -notlike '*msvc' -and (Test-Path -LiteralPath $libraryPath)) { Copy-PackageBinary $libraryPath $distribution }
}
$bundleFiles = @('ets-launcher.exe', 'README.md', 'LICENSE', 'Map-Manager-LICENSES.txt', 'ets-launcher.ico', 'docs')
if ($Target -notlike '*msvc') {
    $bundleFiles += @('libgcc_s_seh-1.dll', 'libwinpthread-1.dll', 'WebView2Loader.dll') | Where-Object { Test-Path -LiteralPath (Join-Path $distribution $_) }
}
$bundlePaths = @($bundleFiles | ForEach-Object { Join-Path $distribution $_ })
Compress-Archive -LiteralPath $bundlePaths -DestinationPath (Join-Path $distribution 'ets-launcher-windows-x64.zip') -Force
Write-Output "Ready: $distribution\ets-launcher.exe"
