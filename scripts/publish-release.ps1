param(
    [Parameter(Mandatory = $true)][string]$Archive,
    [string]$Repository = $env:GITHUB_REPOSITORY,
    [string]$Commit = $env:GITHUB_SHA,
    [string]$BuildNumber = $env:GITHUB_RUN_NUMBER
)

$ErrorActionPreference = 'Stop'
if ($Repository -notmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$') { throw 'Invalid GitHub repository.' }
if ($Commit -notmatch '^[a-fA-F0-9]{40,64}$') { throw 'Invalid source commit.' }
if (-not $env:GH_TOKEN) { throw 'GH_TOKEN is required for publishing a build.' }
Get-Command gh -ErrorAction Stop | Out-Null
$Archive = (Resolve-Path -LiteralPath $Archive).Path
$checksumPath = $Archive + '.sha256'
$checksum = (Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash.ToLowerInvariant()
('{0}  {1}' -f $checksum, [IO.Path]::GetFileName($Archive)) | Set-Content -LiteralPath $checksumPath -Encoding utf8NoBOM
$tag = 'build-' + $Commit.Substring(0, 12)
$notesPath = Join-Path (Split-Path -Parent $Archive) 'release-notes.md'
@"
Automated Windows x64 build from commit $Commit.

Download **ets-launcher-windows-x64.zip**, unzip the entire archive and run **ets-launcher.exe**.
Keep the **tools/7zip** directory next to the executable.

Formatting, Clippy, all tests and the packaged installation smoke test passed.
"@ | Set-Content -LiteralPath $notesPath -Encoding utf8NoBOM

# Each commit has its own release; rerunning the same commit updates only its assets.
& gh release view $tag --repo $Repository --json url --jq .url 2>$null | Out-Null
if ($LASTEXITCODE -eq 0) {
    & gh release upload $tag $Archive $checksumPath --repo $Repository --clobber | Out-Null
} else {
    & gh release create $tag $Archive $checksumPath --repo $Repository --target $Commit --prerelease --title "Windows build #$BuildNumber" --notes-file $notesPath | Out-Null
}
if ($LASTEXITCODE -ne 0) { throw 'Failed to publish the Windows ZIP.' }
$releaseUrl = & gh release view $tag --repo $Repository --json url --jq .url
if ($LASTEXITCODE -ne 0) { throw 'Failed to verify the published release.' }
Write-Output $releaseUrl
if ($env:GITHUB_STEP_SUMMARY) {
    @"
## Windows x64 build
[Download ETS Launcher]($releaseUrl)

Unzip the entire archive and run **ets-launcher.exe**.
The ZIP includes 7-Zip, licenses and SHA256 checksums.
"@ | Add-Content -LiteralPath $env:GITHUB_STEP_SUMMARY -Encoding utf8NoBOM
}
