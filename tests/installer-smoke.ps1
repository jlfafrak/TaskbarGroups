[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$wix = Join-Path $root 'target\wix5\package\tools\net6.0\any\wix.dll'
if (-not (Test-Path -LiteralPath $wix)) { throw 'Run installer\build-msi.ps1 first.' }
$id = [guid]::NewGuid().ToString('N')
$name = "TaskbarGroups-MSI-Test-$id"
$workspace = Join-Path $root "target\installer-test-$id"
$installPath = Join-Path $env:LOCALAPPDATA "Programs\$name"
$shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) "$name.lnk"
$upgradeId = [guid]::NewGuid().ToString()
$appId = [guid]::NewGuid().ToString()
$shortcutId = [guid]::NewGuid().ToString()
$installedVersion = $null
$v1 = Join-Path $workspace 'v1.msi'
$v2 = Join-Path $workspace 'v2.msi'

function Run-Msi([string[]] $MsiArgs, [string] $Log) {
    $arguments = $MsiArgs + @('/qn', '/norestart', '/l*v', ('"{0}"' -f $Log))
    return (Start-Process -FilePath msiexec.exe -ArgumentList $arguments -Wait -PassThru).ExitCode
}
function Assert([bool] $Condition, [string] $Message) {
    if (-not $Condition) { throw $Message }
}
function Build-TestPackage([string] $Version, [string] $Tag, [string] $MsiPath) {
    $dir = Join-Path $workspace $Tag
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $src = Join-Path $dir 'stub.rs'
    $exe = Join-Path $dir 'taskbar-groups.exe'
    $source = @'
fn main() {
    if std::env::args().any(|arg| arg == "--installer-shutdown") {
        let marker = std::env::current_exe().unwrap().with_file_name("shutdown.marker");
        if marker.with_file_name("busy.marker").exists() { std::process::exit(1); }
        std::fs::write(marker, "__TAG__").unwrap();
    }
    if std::env::args().any(|arg| arg == "--watch") {
        let marker = std::env::current_exe().unwrap().with_file_name("started.marker");
        std::fs::write(marker, "__TAG__").unwrap();
    }
}
'@.Replace('__TAG__', $Tag)
    [IO.File]::WriteAllText($src, $source)
    & rustc --edition 2024 $src -o $exe
    if ($LASTEXITCODE -ne 0) { throw 'Test payload compilation failed.' }
    & dotnet $wix build -arch x64 -d "ProductVersion=$Version" -d "AppExe=$exe" `
        -d "ProductName=$name" -d "InstallDirName=$name" -d "RegistryName=$name" `
        -d "UpgradeId=$upgradeId" -d "AppComponentId=$appId" -d "ShortcutComponentId=$shortcutId" `
        -pdbtype none (Join-Path $root 'installer\TaskbarGroups.wxs') -o $MsiPath
    if ($LASTEXITCODE -ne 0) { throw 'Test MSI build failed.' }
    return (Get-FileHash -Algorithm SHA256 -LiteralPath $exe).Hash
}

try {
    Assert (-not (Test-Path -LiteralPath $installPath)) 'Test install path already exists.'
    Assert (-not (Test-Path -LiteralPath $shortcut)) 'Test shortcut already exists.'
    New-Item -ItemType Directory -Force -Path $workspace | Out-Null
    $hash1 = Build-TestPackage '0.1.0' 'v1' $v1
    $hash2 = Build-TestPackage '0.2.0' 'v2' $v2
    $exit = Run-Msi @('/i', ('"{0}"' -f $v1)) (Join-Path $workspace 'install.log')
    Assert ($exit -eq 0) "Install failed: $exit"
    $installedVersion = $v1
    $installedExe = Join-Path $installPath 'taskbar-groups.exe'
    Assert ((Get-FileHash -Algorithm SHA256 -LiteralPath $installedExe).Hash -eq $hash1) 'Installed payload differs.'
    Assert (Test-Path -LiteralPath $shortcut) 'Start menu shortcut is missing.'
    $busy = Join-Path $installPath 'busy.marker'
    [IO.File]::WriteAllText($busy, 'busy')
    $exit = Run-Msi @('/i', ('"{0}"' -f $v2)) (Join-Path $workspace 'busy-upgrade.log')
    Assert ($exit -ne 0) 'Upgrade proceeded while the application was busy.'
    Assert ((Get-FileHash -Algorithm SHA256 -LiteralPath $installedExe).Hash -eq $hash1) 'Failed upgrade changed the installed executable.'
    Assert (Test-Path -LiteralPath $shortcut) 'Failed upgrade removed the Start menu shortcut.'
    Remove-Item -LiteralPath $busy
    $exit = Run-Msi @('/i', ('"{0}"' -f $v2)) (Join-Path $workspace 'upgrade.log')
    Assert ($exit -eq 0) "Upgrade failed: $exit"
    $installedVersion = $v2
    Assert ((Get-FileHash -Algorithm SHA256 -LiteralPath $installedExe).Hash -eq $hash2) 'Upgrade did not replace the payload.'
    $marker = Join-Path $installPath 'shutdown.marker'
    Assert ((Get-Content -LiteralPath $marker -Raw) -eq 'v1') 'Upgrade did not request the old app to close.'
    $started = Join-Path $installPath 'started.marker'
    for ($attempt = 0; $attempt -lt 30 -and -not (Test-Path -LiteralPath $started); $attempt++) { Start-Sleep -Milliseconds 100 }
    Assert ((Get-Content -LiteralPath $started -Raw) -eq 'v2') 'Upgrade did not restart the watcher.'
    $exit = Run-Msi @('/i', ('"{0}"' -f $v1)) (Join-Path $workspace 'downgrade.log')
    Assert ($exit -eq 1603 -or $exit -eq 1638) "Downgrade was not blocked: $exit"
    Assert ((Get-FileHash -Algorithm SHA256 -LiteralPath $installedExe).Hash -eq $hash2) 'Downgrade changed the payload.'
    $exit = Run-Msi @('/x', ('"{0}"' -f $v2)) (Join-Path $workspace 'uninstall.log')
    Assert ($exit -eq 0) "Uninstall failed: $exit"
    $installedVersion = $null
    Assert (-not (Test-Path -LiteralPath $installedExe)) 'Uninstall left the application executable.'
    Assert (-not (Test-Path -LiteralPath $shortcut)) 'Uninstall left the Start menu shortcut.'
    Assert ((Get-Content -LiteralPath $marker -Raw) -eq 'v2') 'Uninstall did not request the app to close.'
    Remove-Item -LiteralPath $marker, $started
    $exit = Run-Msi @('/i', ('"{0}"' -f $v1)) (Join-Path $workspace 'reinstall.log')
    Assert ($exit -eq 0) "Reinstall failed: $exit"
    $installedVersion = $v1
    Remove-Item -LiteralPath $installedExe
    $exit = Run-Msi @('/i', ('"{0}"' -f $v2)) (Join-Path $workspace 'missing-payload-upgrade.log')
    Assert ($exit -eq 0) "Upgrade with missing executable failed: $exit"
    $installedVersion = $v2
    Assert ((Get-FileHash -Algorithm SHA256 -LiteralPath $installedExe).Hash -eq $hash2) 'Upgrade did not restore the missing executable.'
    for ($attempt = 0; $attempt -lt 30 -and -not (Test-Path -LiteralPath $started); $attempt++) { Start-Sleep -Milliseconds 100 }
    Assert (Test-Path -LiteralPath $started) 'Watcher did not restart after recovering the executable.'
    Remove-Item -LiteralPath $installedExe
    $exit = Run-Msi @('/x', ('"{0}"' -f $v2)) (Join-Path $workspace 'final-uninstall.log')
    Assert ($exit -eq 0) "Uninstall with missing executable failed: $exit"
    $installedVersion = $null
    Write-Host 'MSI install, upgrade, downgrade protection, shutdown, and uninstall passed.'
} finally {
    if ($installedVersion) {
        $busyPath = Join-Path $installPath 'busy.marker'
        if (Test-Path -LiteralPath $busyPath) { Remove-Item -LiteralPath $busyPath -Force }
        $cleanupCode = Run-Msi @('/x', ('"{0}"' -f $installedVersion)) (Join-Path $workspace 'cleanup.log')
        if ($cleanupCode -ne 0 -and $cleanupCode -ne 1605) { Write-Warning "Test MSI cleanup failed: $cleanupCode. Logs: $workspace" }
    }
    $markerPath = Join-Path $installPath 'shutdown.marker'
    if (Test-Path -LiteralPath $markerPath) { Remove-Item -LiteralPath $markerPath -Force }
    $startedPath = Join-Path $installPath 'started.marker'
    if (Test-Path -LiteralPath $startedPath) { Remove-Item -LiteralPath $startedPath -Force }
    if (Test-Path -LiteralPath $installPath) { Remove-Item -LiteralPath $installPath -ErrorAction SilentlyContinue }
}
