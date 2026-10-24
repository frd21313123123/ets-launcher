param([Parameter(Mandatory = $true)][string]$Executable)

$ErrorActionPreference = 'Stop'
$Executable = (Resolve-Path -LiteralPath $Executable).Path
# A runner can have VC++/GNU runtimes installed, so simply starting the EXE
# does not prove portability. Audit both normal and delay-loaded PE imports.
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$dumpbin = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -find 'VC\Tools\MSVC\**\bin\Hostx64\x64\dumpbin.exe' | Select-Object -Last 1
if ($LASTEXITCODE -ne 0 -or -not $dumpbin) { throw 'MSVC dumpbin.exe was not found.' }
$output = & $dumpbin /DEPENDENTS $Executable
if ($LASTEXITCODE -ne 0) { throw 'Could not inspect executable dependencies.' }
$dependencies = @($output | ForEach-Object {
    if ($_ -match '^\s*([A-Za-z0-9_.-]+\.dll)\s*$') { $Matches[1] }
} | Sort-Object -Unique)
if ($dependencies.Count -eq 0) { throw 'No PE dependencies were found; the audit did not run.' }
foreach ($dependency in $dependencies) {
    if ($dependency -match '^(vcruntime|msvcp|concrt|vcomp|libgcc|libstdc\+\+|libwinpthread|7z|WebView2Loader)') {
        throw "Standalone EXE depends on an external runtime: $dependency"
    }
    if ($dependency -notmatch '^api-ms-win-' -and -not (Test-Path -LiteralPath (Join-Path $env:SystemRoot "System32\$dependency") -PathType Leaf)) {
        throw "Standalone EXE depends on a non-system DLL: $dependency"
    }
}
Write-Output ('Standalone dependencies verified: ' + ($dependencies -join ', '))
