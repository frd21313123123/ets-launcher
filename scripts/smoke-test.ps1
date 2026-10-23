param([string]$Distribution)

$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
if (-not $Distribution) { $Distribution = Join-Path $projectRoot 'dist' }
$Distribution = (Resolve-Path -LiteralPath $Distribution).Path
foreach ($requiredFile in @('ets-launcher.exe', 'tools\7zip\7z.exe', 'tools\7zip\7z.dll', 'tools\7zip\License.txt')) {
    if (-not (Test-Path -LiteralPath (Join-Path $Distribution $requiredFile) -PathType Leaf)) {
        throw "Distribution is missing $requiredFile"
    }
}

# Exercise the packaged GUI-subsystem executable without opening a window or
# accessing the website. The only modified game is this small, isolated fixture.
$testRoot = Join-Path $projectRoot ('artifacts\package-smoke-' + [Guid]::NewGuid().ToString('N'))
$testGame = Join-Path $testRoot 'Euro Truck Simulator 2'
$testBin = Join-Path $testGame 'bin\win_x64'
New-Item -ItemType Directory -Path $testBin -Force | Out-Null
[IO.File]::WriteAllText((Join-Path $testBin 'eurotrucks2.exe'), 'temporary game executable')
[IO.File]::WriteAllText((Join-Path $testGame 'dlc_mock.scs'), 'original map')
$report = Join-Path $testRoot 'result.txt'
$fixture = Join-Path $projectRoot 'assets\installation-test.zip'
$arguments = '--install-archive "{0}" "{1}" "{2}"' -f $fixture, $testGame, $report
$process = Start-Process -FilePath (Join-Path $Distribution 'ets-launcher.exe') -ArgumentList $arguments -WorkingDirectory $projectRoot -WindowStyle Hidden -Wait -PassThru
if ($process.ExitCode -ne 0) {
    if (Test-Path -LiteralPath $report) { Get-Content -LiteralPath $report | Write-Output }
    throw "Packaged installer exited with code $($process.ExitCode)"
}
if (([IO.File]::ReadAllText($report)) -notmatch 'Installed: 3') { throw 'Unexpected installation report.' }
if ([IO.File]::ReadAllText((Join-Path $testGame 'dlc_mock.scs')) -ne 'new map data') { throw 'Map was not installed.' }
if ([IO.File]::ReadAllText((Join-Path $testBin 'steam_api64.dll')) -ne 'test library payload') { throw 'Library was not installed.' }
if ([IO.File]::ReadAllText((Join-Path $testBin 'eurotrucks2.exe')) -ne 'temporary game executable') { throw 'Game executable changed.' }
$backups = @(Get-ChildItem -LiteralPath (Join-Path $testGame '.ets-launcher\backups') -Directory)
if ($backups.Count -ne 1) { throw 'Expected one backup.' }
if ([IO.File]::ReadAllText((Join-Path $backups[0].FullName 'original\dlc_mock.scs')) -ne 'original map') { throw 'Original map was not backed up.' }
Write-Output 'Packaged installation passed: 3 files, original preserved, executable unchanged.'
