[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$toolRoot = Join-Path $projectRoot 'target\wix5'
$wixDll = Join-Path $toolRoot 'package\tools\net6.0\any\wix.dll'
$wixArchive = Join-Path $toolRoot 'wix.5.0.2.zip'
$expectedSha512 = 'CFE7EED5D79371A57E4D10A8E3EE87AC9E3C49D9E3FB27908E3312B53237196ECDC4510F6A49D809DC6B7F4A84BE0757AE361D8CB9E464A0AC352A934CEA3638'

if (-not (Get-Command dotnet -ErrorAction SilentlyContinue)) { throw 'Install a .NET runtime (6 or newer) to run WiX.' }
if (-not (Test-Path -LiteralPath $wixDll)) {
    New-Item -ItemType Directory -Force -Path $toolRoot | Out-Null
    Invoke-WebRequest -Uri 'https://api.nuget.org/v3-flatcontainer/wix/5.0.2/wix.5.0.2.nupkg' -OutFile $wixArchive
    if ((Get-FileHash -Algorithm SHA512 -LiteralPath $wixArchive).Hash -ne $expectedSha512) { throw 'WiX download checksum does not match.' }
    Expand-Archive -LiteralPath $wixArchive -DestinationPath (Join-Path $toolRoot 'package') -Force
}

Push-Location $projectRoot
try {
    $metadata = cargo metadata --no-deps --format-version 1 | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Could not read Cargo metadata.' }
    $package = $metadata.packages | Where-Object { $_.name -eq 'taskbar-groups' } | Select-Object -First 1
    $version = $package.version
    if ($version -notmatch '^(\d+)\.(\d+)\.(\d+)$' -or [int]$Matches[1] -gt 255 -or [int]$Matches[2] -gt 255 -or [int]$Matches[3] -gt 65535) {
        throw "Version '$version' must have three numeric parts within Windows Installer limits."
    }
    $hostLine = rustc -vV | Where-Object { $_ -like 'host: *' }
    if ($hostLine -ne 'host: x86_64-pc-windows-msvc') { throw 'Building this MSI requires the x86_64-pc-windows-msvc Rust toolchain.' }
    $buildRoot = Join-Path $projectRoot 'target\msi-build'
    cargo build --release --locked --target-dir $buildRoot
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }
    $exe = Join-Path $buildRoot 'release\taskbar-groups.exe'
    $outputDir = Join-Path $projectRoot 'target\installer'
    New-Item -ItemType Directory -Force -Path $outputDir | Out-Null
    $output = Join-Path $outputDir "TaskbarGroups-$version-x64.msi"
    & dotnet $wixDll build -arch x64 -d "ProductVersion=$version" -d "AppExe=$exe" -pdbtype none (Join-Path $PSScriptRoot 'TaskbarGroups.wxs') -o $output
    if ($LASTEXITCODE -ne 0) { throw 'MSI build failed.' }
    Write-Host "Built $output"
} finally {
    Pop-Location
}
