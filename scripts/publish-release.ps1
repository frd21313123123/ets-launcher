param(
    [Parameter(Mandatory = $true)][string]$Executable,
    [string]$Repository = $env:GITHUB_REPOSITORY,
    [string]$Commit = $env:GITHUB_SHA,
    [string]$BuildNumber = $env:GITHUB_RUN_NUMBER
)

$ErrorActionPreference = 'Stop'
if ($Repository -notmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$') { throw 'Invalid GitHub repository.' }
if ($Commit -notmatch '^[a-fA-F0-9]{40,64}$') { throw 'Invalid source commit.' }
if (-not $env:GH_TOKEN) { throw 'GH_TOKEN is required for publishing a build.' }
Get-Command gh -ErrorAction Stop | Out-Null
$Executable = (Resolve-Path -LiteralPath $Executable).Path
if ([IO.Path]::GetFileName($Executable) -ne 'ets-launcher.exe') { throw 'Expected ets-launcher.exe.' }
$checksumPath = $Executable + '.sha256'
$checksum = (Get-FileHash -LiteralPath $Executable -Algorithm SHA256).Hash.ToLowerInvariant()
('{0}  {1}' -f $checksum, [IO.Path]::GetFileName($Executable)) | Set-Content -LiteralPath $checksumPath -Encoding utf8NoBOM
$tag = 'build-' + $Commit.Substring(0, 12)
$notesPath = Join-Path (Split-Path -Parent $Executable) 'release-notes.md'
@"
Automated Windows x64 build from commit $Commit.

Download and run **ets-launcher.exe**. This is the complete Windows x64 application.
No ZIP extraction, Rust, Visual C++ Redistributable, torrent client or separately installed archiver is required.
The executable contains 7-Zip and its licenses; they are prepared automatically in the user's local application cache when needed.

Formatting, Clippy, all tests, installation from an isolated EXE with an empty cache, and the system-DLL dependency check passed.
"@ | Set-Content -LiteralPath $notesPath -Encoding utf8NoBOM

# Each commit has its own release; rerunning the same commit updates only its assets.
& gh release view $tag --repo $Repository --json url --jq .url 2>$null | Out-Null
if ($LASTEXITCODE -eq 0) {
    & gh release upload $tag $Executable $checksumPath --repo $Repository --clobber | Out-Null
} else {
    & gh release create $tag $Executable $checksumPath --repo $Repository --target $Commit --prerelease --title "Windows build #$BuildNumber" --notes-file $notesPath | Out-Null
}
if ($LASTEXITCODE -ne 0) { throw 'Failed to publish the Windows executable.' }
$releaseUrl = & gh release view $tag --repo $Repository --json url --jq .url
if ($LASTEXITCODE -ne 0) { throw 'Failed to verify the published release.' }
$downloadUrl = 'https://github.com/{0}/releases/download/{1}/ets-launcher.exe' -f $Repository, $tag
Write-Output $releaseUrl
if ($env:GITHUB_STEP_SUMMARY) {
    @"
## Windows x64 build
[Download ets-launcher.exe]($downloadUrl) | [Release and SHA256]($releaseUrl)

Run the downloaded **ets-launcher.exe**. This single file contains the application, archiver and licenses.
"@ | Add-Content -LiteralPath $env:GITHUB_STEP_SUMMARY -Encoding utf8NoBOM
}
