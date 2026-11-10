$ErrorActionPreference = 'Stop'
$validator = Join-Path $PSScriptRoot '..\installer\validate-release-version.ps1'

function Assert-Rejected([string] $Tag, [string] $Version) {
    try { & $validator -Tag $Tag -Version $Version } catch { return }
    throw "Expected tag '$Tag' with version '$Version' to be rejected."
}

& $validator -Tag 'v0.2.0' -Version '0.2.0'
Assert-Rejected 'v0.2.1' '0.2.0'
Assert-Rejected '0.2.0' '0.2.0'
Assert-Rejected 'v0.2.0-rc.1' '0.2.0'
Assert-Rejected 'V0.2.0' '0.2.0'
Assert-Rejected 'v01.2.0' '01.2.0'
Assert-Rejected 'v256.0.0' '256.0.0'
Write-Host 'Release version validation passed.'
