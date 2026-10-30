param([string]$Distribution, [switch]$Standalone)

$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
if (-not $Distribution) { $Distribution = Join-Path $projectRoot 'dist' }
$Distribution = (Resolve-Path -LiteralPath $Distribution).Path
Add-Type -AssemblyName System.IO.Compression.FileSystem
$bundle = [IO.Compression.ZipFile]::OpenRead((Join-Path $Distribution 'ets-launcher-windows-x64.zip'))
try {
    if ($bundle.Entries | Where-Object { $_.FullName.Replace('\', '/') -eq 'catalog/ets2.json' }) { throw 'Distribution ZIP still contains the replaced map catalog.' }
    if (-not ($bundle.Entries | Where-Object { $_.Name -eq 'quper-verification.md' })) { throw 'Distribution ZIP is missing the Quper documentation.' }
} finally { $bundle.Dispose() }
foreach ($requiredFile in @('ets-launcher.exe')) {
    if (-not (Test-Path -LiteralPath (Join-Path $Distribution $requiredFile) -PathType Leaf)) {
        throw "Distribution is missing $requiredFile"
    }
}

# Exercise the packaged GUI-subsystem executable without opening a window or
# accessing the website. The only modified game is this small, isolated fixture.
$testRoot = Join-Path $projectRoot ('artifacts\package-smoke-' + [Guid]::NewGuid().ToString('N'))
$application = Join-Path $testRoot 'Отдельный запуск'
New-Item -ItemType Directory -Path $application -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $Distribution 'ets-launcher.exe') -Destination $application
# Development GNU builds still need their toolchain DLLs. CI checks the MSVC
# build with -Standalone and copies exactly one executable, with no neighbors.
if (-not $Standalone) {
    foreach ($library in @('libgcc_s_seh-1.dll', 'libwinpthread-1.dll', 'WebView2Loader.dll')) {
        $path = Join-Path $Distribution $library
        if (Test-Path -LiteralPath $path) { Copy-Item -LiteralPath $path -Destination $application }
    }
}
$previousToolCache = $env:ETS_LAUNCHER_TOOL_CACHE
$env:ETS_LAUNCHER_TOOL_CACHE = Join-Path $testRoot 'Fresh tool cache'
try {
    $games = @(
        @{ Id = 'ets2'; Name = 'Euro Truck Simulator 2'; Executable = 'eurotrucks2.exe' },
        @{ Id = 'ats'; Name = 'American Truck Simulator'; Executable = 'amtrucks.exe' }
    )
    foreach ($game in $games) {
        $testGame = Join-Path $testRoot $game.Name
        $testBin = Join-Path $testGame 'bin\win_x64'
        New-Item -ItemType Directory -Path $testBin -Force | Out-Null
        [IO.File]::WriteAllText((Join-Path $testBin $game.Executable), 'temporary game executable')
        [IO.File]::WriteAllText((Join-Path $testGame 'dlc_mock.scs'), 'original map')
        $report = Join-Path $testGame 'result.txt'
        $fixture = Join-Path $projectRoot 'assets\installation-test.zip'
        $arguments = '--install-archive "{0}" "{1}" "{2}" {3}' -f $fixture, $testGame, $report, $game.Id
        $process = Start-Process -FilePath (Join-Path $application 'ets-launcher.exe') -ArgumentList $arguments -WorkingDirectory $application -WindowStyle Hidden -Wait -PassThru
        if ($process.ExitCode -ne 0) {
            if (Test-Path -LiteralPath $report) { Get-Content -LiteralPath $report | Write-Output }
            throw "Packaged installer exited with code $($process.ExitCode)"
        }
        if (([IO.File]::ReadAllText($report)) -notmatch 'Installed: 3') { throw 'Unexpected installation report.' }
        if ([IO.File]::ReadAllText((Join-Path $testGame 'dlc_mock.scs')) -ne 'new map data') { throw 'Map was not installed.' }
        if ([IO.File]::ReadAllText((Join-Path $testBin 'steam_api64.dll')) -ne 'test library payload') { throw 'Library was not installed.' }
        if ([IO.File]::ReadAllText((Join-Path $testBin $game.Executable)) -ne 'temporary game executable') { throw 'Game executable changed.' }
        $backups = @(Get-ChildItem -LiteralPath (Join-Path $testGame '.ets-launcher\backups') -Directory)
        if ($backups.Count -ne 1) { throw 'Expected one backup.' }
        if ([IO.File]::ReadAllText((Join-Path $backups[0].FullName 'original\dlc_mock.scs')) -ne 'original map') { throw 'Original map was not backed up.' }
        Write-Output ($game.Id + ' packaged installation passed: 3 files, backup preserved, executable unchanged.')
    }
    $helpers = @(Get-ChildItem -LiteralPath $env:ETS_LAUNCHER_TOOL_CACHE -Recurse -File -Filter '7z.exe')
    if ($helpers.Count -ne 1) { throw 'Embedded archiver was not prepared in the empty application cache.' }
    foreach ($assetName in @('7z.exe', '7z.dll', 'License.txt', 'LGPL-2.1.txt', 'ORIGIN.md')) {
        $cached = Join-Path $helpers[0].DirectoryName $assetName
        $original = Join-Path $projectRoot "assets\7zip\$assetName"
        if ((Get-FileHash -LiteralPath $cached -Algorithm SHA256).Hash -ne (Get-FileHash -LiteralPath $original -Algorithm SHA256).Hash) {
            throw "Embedded file does not match the official bundle: $assetName"
        }
    }
    if ($Standalone -and @(Get-ChildItem -LiteralPath $application -Force).Count -ne 1) {
        throw 'The application directory must contain only ets-launcher.exe.'
    }
    Write-Output 'Embedded archiver and licenses verified in a fresh cache; no adjacent archiver was needed.'
} finally {
    $env:ETS_LAUNCHER_TOOL_CACHE = $previousToolCache
}
