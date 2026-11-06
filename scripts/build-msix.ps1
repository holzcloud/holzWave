# Pack the Microsoft Store package (MSIX) of holzWave on Windows. Run it in PowerShell from the root of the checkout, after
# scripts\release-windows.ps1 has built the app for the same architecture (it leaves the build under target\release for x64
# and under target\aarch64-pc-windows-msvc\release for arm64):
#
#   .\scripts\build-msix.ps1                 pack the x64 package
#   .\scripts\build-msix.ps1 -Arch arm64     pack the ARM64 package
#   .\scripts\build-msix.ps1 -Certify        also run the Windows App Certification Kit on the package
#
# What it does: runs `node packaging/msix/build.mjs`, which stages holzwave-app.exe, wsh.exe, the logos and the manifest into
# target\msix\stage-<arch> (the manifest's version is the Store version mapped from tauri.conf.json), then packs that folder
# with makeappx.exe from the Windows SDK into target\msix\holzWave_<version>_<arch>.msix, and prints the path and the sha256.
# The identity values (package name, publisher, publisher display name) come only from packaging\msix\identity.json, which
# the owner makes from the Partner Center values (packaging\msix\identity.example.json shows the shape); the stager refuses a
# placeholder. The Store signs the package: no certificate is made or used here, so the .msix cannot be installed by
# double click, and it is for the Store upload only. The script never uploads anything.
#
# -Certify runs appcert.exe from the Windows App Certification Kit (part of the free Windows SDK) and keeps its report in
# target\msix. Needs Node (the stager) and the Windows SDK (makeappx.exe; for -Certify also the App Certification Kit).
#
# Written on a Mac without a Windows host and without PowerShell: this file was read, never run. makeappx, the certification
# kit and the package itself were never tried, and the appcert command line is from memory of its documentation. It works
# in Windows PowerShell 5.1 and in PowerShell 7.

param(
    [ValidateSet('x64', 'arm64')][string]$Arch = 'x64',
    [switch]$Certify
)

$ErrorActionPreference = 'Stop'

function Fail([string]$Message) {
    Write-Host "build-msix: $Message"
    exit 1
}

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) { Fail 'this script packs on Windows' }
if (-not (Get-Command node -ErrorAction SilentlyContinue)) { Fail 'node was not found (Node 26 or newer; the stager is a Node script)' }

# The build that scripts\release-windows.ps1 left for this architecture.
if ($Arch -eq 'arm64') { $releaseDir = Join-Path $root 'target/aarch64-pc-windows-msvc/release' } else { $releaseDir = Join-Path $root 'target/release' }
if (-not (Test-Path (Join-Path $releaseDir 'holzwave-app.exe') -PathType Leaf)) {
    Fail "no holzwave-app.exe in $releaseDir : run  .\scripts\release-windows.ps1 -Version <version>$(if ($Arch -eq 'arm64') { ' -Arch arm64' })  first"
}

# The Linux wsh builds that release-windows.ps1 copies there when it was given -LinuxWsh; without them the package has none.
$stagerArgs = @('packaging/msix/build.mjs', '--arch', $Arch, '--release-dir', $releaseDir, '--json')
$binaries = Join-Path $root 'app/src-tauri/binaries'
if ((Test-Path (Join-Path $binaries 'wsh-linux-x86_64') -PathType Leaf) -and (Test-Path (Join-Path $binaries 'wsh-linux-aarch64') -PathType Leaf)) {
    $stagerArgs += @('--linux-wsh', $binaries)
} else {
    Write-Host 'build-msix: the two Linux wsh builds are not in app\src-tauri\binaries: the package is staged without them'
}

$json = & node @stagerArgs
if ($LASTEXITCODE -ne 0) { Fail 'the stager refused (its message is above); the identity file is the usual cause' }
$plan = $json | ConvertFrom-Json
Write-Host "build-msix: staged $($plan.stageDir) (version $($plan.version), $($plan.arch))"

# makeappx.exe: the newest Windows SDK under Program Files (x86), x64 tools.
$kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
$makeappx = $null
if (Test-Path $kits) {
    $makeappx = Get-ChildItem $kits -Directory |
        Where-Object { $_.Name -match '^\d+\.\d+\.\d+\.\d+$' } |
        Sort-Object { [version]$_.Name } -Descending |
        ForEach-Object { Join-Path $_.FullName 'x64\makeappx.exe' } |
        Where-Object { Test-Path $_ -PathType Leaf } |
        Select-Object -First 1
}
if (-not $makeappx) { Fail 'makeappx.exe was not found: install the Windows SDK (free), the component "Windows SDK Signing Tools for Desktop Apps" or the full SDK' }

# The stager printed the command: makeappx pack /d <folder> /p <file>.msix /o
$packArgs = @($plan.command | Select-Object -Skip 1)
& $makeappx @packArgs
if ($LASTEXITCODE -ne 0) { Fail 'makeappx failed' }
if (-not (Test-Path $plan.msixPath -PathType Leaf)) { Fail "makeappx did not make $($plan.msixPath)" }
$sum = (Get-FileHash -Algorithm SHA256 $plan.msixPath).Hash.ToLower()
Write-Host "build-msix: packed $($plan.msixPath)"
Write-Host "build-msix: sha256 $sum"

if ($Certify) {
    $appcert = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\App Certification Kit\appcert.exe'
    if (-not (Test-Path $appcert -PathType Leaf)) { Fail 'appcert.exe was not found: install the Windows App Certification Kit (part of the free Windows SDK)' }
    $report = Join-Path (Split-Path -Parent $plan.msixPath) "certification-$($plan.version)-$($plan.arch).xml"
    & $appcert reset
    & $appcert test -appxpackagepath $plan.msixPath -reportoutputpath $report
    if ($LASTEXITCODE -ne 0) { Fail "the certification kit reported a problem or failed (exit code $LASTEXITCODE); the report, if any, is $report" }
    Write-Host "build-msix: certification report $report"
}
