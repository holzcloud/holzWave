# Build the Windows installer locally and attach it to the GitHub release of the same version (the twin of
# scripts/release-macos.sh). Run it in PowerShell on the Windows machine, from the root of a clean checkout of main:
#
#   .\scripts\release-windows.ps1 -Version 0.0.1-alpha1            build, print the SHA-256
#   .\scripts\release-windows.ps1 -Version 0.0.1-alpha1 -Upload    also upload the installer and its checksum to the
#                                                                  release v0.0.1-alpha1 (it must exist: the macOS
#                                                                  script creates it)
#
# -Version must equal the version of app/src-tauri/tauri.conf.json. The installer is NSIS, per user, unsigned (no
# code signing yet, Phase 10), so Windows SmartScreen warns on the first start ("More info", "Run anyway"). Needs the
# toolchain of docs/build-windows.md (. .\scripts\env.ps1 checks it) and, for -Upload, the gh CLI logged in.
#
# Written on a Mac without a Windows host: the parser and the pure checks are tried there; the build itself is the
# first thing row W12 of docs/test-plan.md runs. It works in Windows PowerShell 5.1 and in PowerShell 7.

param(
    [Parameter(Mandatory = $true)][string]$Version,
    [switch]$Upload
)

$ErrorActionPreference = 'Stop'

function Fail([string]$Message) {
    Write-Host "release-windows: $Message"
    exit 1
}

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) { Fail 'this script builds on Windows' }
if ($Version -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$') { Fail "not a version: $Version" }

$conf = Get-Content (Join-Path $root 'app/src-tauri/tauri.conf.json') -Raw | ConvertFrom-Json
if ($conf.version -ne $Version) { Fail "version $Version is not the version of tauri.conf.json ($($conf.version))" }

if ((git rev-parse --abbrev-ref HEAD) -ne 'main') { Fail 'release from main only' }
if (git status --porcelain) { Fail 'the working tree is not clean' }
git fetch -q origin main
if ((git rev-parse HEAD) -ne (git rev-parse origin/main)) { Fail 'main is not the same commit as origin/main: pull first' }

. (Join-Path $PSScriptRoot 'env.ps1')   # puts Zig 0.16.0 on the PATH for this session, checks MSVC Rust and VS tools

npm --prefix app ci
if ($LASTEXITCODE -ne 0) { Fail 'npm ci failed' }
npm --prefix app run build
if ($LASTEXITCODE -ne 0) { Fail 'the page build failed' }
npm --prefix app run tauri -- build --bundles nsis
if ($LASTEXITCODE -ne 0) { Fail 'tauri build failed' }
git checkout -- app/src-tauri/Cargo.toml   # the Tauri CLI rewrites feature lists there; the commit stays as it is

$found = Get-ChildItem (Join-Path $root 'target/release/bundle/nsis') -Filter '*-setup.exe' |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $found) { Fail 'no *-setup.exe under target/release/bundle/nsis' }

$name = "holzWave_${Version}_x64-setup.exe"
$out = Join-Path $root 'target/release/bundle/release'
New-Item -ItemType Directory -Force $out | Out-Null
$exe = Join-Path $out $name
Copy-Item $found.FullName $exe -Force
$sum = (Get-FileHash -Algorithm SHA256 $exe).Hash.ToLower()
$sumFile = "$exe.sha256"
"$sum  $name" | Set-Content -Encoding ascii $sumFile
Write-Host "release-windows: built $exe"
Write-Host "release-windows: sha256 $sum"

if ($Upload) {
    $tag = "v$Version"
    gh release view $tag *> $null
    if ($LASTEXITCODE -ne 0) { Fail "the release $tag does not exist yet: run scripts/release-macos.sh first" }
    gh release upload $tag $exe $sumFile --clobber
    if ($LASTEXITCODE -ne 0) { Fail 'the upload failed' }
    Write-Host "release-windows: uploaded to $tag"
}
