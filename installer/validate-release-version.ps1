[CmdletBinding()]
param(
    [Parameter(Mandatory)][string] $Tag,
    [Parameter(Mandatory)][string] $Version
)

$ErrorActionPreference = 'Stop'
if ($Version -cnotmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') {
    throw "Package version '$Version' must contain three canonical numeric components."
}
$parts = $Version.Split('.')
$limits = @(255, 255, 65535)
for ($i = 0; $i -lt $limits.Count; $i++) {
    $number = [uint32] 0
    if (-not [uint32]::TryParse($parts[$i], [ref] $number) -or $number -gt $limits[$i]) {
        throw "Package version '$Version' exceeds Windows Installer's version limits."
    }
}
if ($Tag -cne "v$Version") {
    throw "Release tag '$Tag' must exactly match package version v$Version."
}
