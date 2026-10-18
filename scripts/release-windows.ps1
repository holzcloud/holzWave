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
# wsh: the script builds wsh.exe for x86_64-pc-windows-msvc into app/src-tauri/binaries and passes the overlay
# app/src-tauri/tauri.wsh.conf.json (only release scripts pass it, so cargo test needs no binary), which installs wsh.exe
# beside holzwave-app.exe: the folder of the executable is the folder a pane's PATH gets. The two static Linux wsh builds
# (the resources that the app installs on a remote Linux host) are made by `sh scripts/wsh-linux.sh` on a Mac or a
# Linux host, not here: give the folder that holds wsh-linux-x86_64 and wsh-linux-aarch64 as -LinuxWsh. Without it the
# installer is built without the Linux resources and the script says so. Afterwards the script checks that wsh.exe is
# beside holzwave-app.exe and is the file it built.
#
# Written on a Mac without a Windows host: the parser and the pure checks are tried there; the build itself is the
# first thing row W12 of docs/test-plan.md runs. It works in Windows PowerShell 5.1 and in PowerShell 7.

param(
    [Parameter(Mandatory = $true)][string]$Version,
    [switch]$Upload,
    [string]$LinuxWsh
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

# wsh.exe is the sidecar, built here from this commit; the Linux builds come from -LinuxWsh.
$binaries = Join-Path $root 'app/src-tauri/binaries'
New-Item -ItemType Directory -Force $binaries | Out-Null
cargo build --locked --release -p wsh --target x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { Fail 'the wsh build failed' }
$sidecar = Join-Path $binaries 'wsh-x86_64-pc-windows-msvc.exe'
Copy-Item (Join-Path $root 'target/x86_64-pc-windows-msvc/release/wsh.exe') $sidecar -Force
$overlays = @('--config', 'src-tauri/tauri.wsh.conf.json')
if ($LinuxWsh) {
    foreach ($arch in 'x86_64', 'aarch64') {
        $src = Join-Path $LinuxWsh "wsh-linux-$arch"
        if (-not (Test-Path $src -PathType Leaf)) { Fail "no wsh-linux-$arch in $LinuxWsh (sh scripts/wsh-linux.sh makes it)" }
        Copy-Item $src (Join-Path $binaries "wsh-linux-$arch") -Force
    }
    Write-Host 'release-windows: the installer carries wsh.exe and the two Linux wsh builds'
} else {
    # A second overlay removes the resources of the first (a JSON merge: null deletes the key).
    $noLinux = Join-Path $root 'target/wsh-no-linux.conf.json'
    '{"bundle":{"resources":null}}' | Set-Content -Encoding ascii $noLinux
    $overlays += @('--config', $noLinux)
    Write-Host 'release-windows: no -LinuxWsh given: the installer carries wsh.exe but NOT the Linux wsh builds (the app cannot install wsh on a remote host from this installer)'
}
npm --prefix app run tauri -- build --bundles nsis @overlays
if ($LASTEXITCODE -ne 0) { Fail 'tauri build failed' }
git checkout -- app/src-tauri/Cargo.toml   # the Tauri CLI rewrites feature lists there; the commit stays as it is

$found = Get-ChildItem (Join-Path $root 'target/release/bundle/nsis') -Filter '*-setup.exe' |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $found) { Fail 'no *-setup.exe under target/release/bundle/nsis' }

# The package holds the wsh that was built: wsh.exe sits beside holzwave-app.exe and is the sidecar (the bundler copies the
# sidecar there at build time and puts the same file into the installer); with 7z at hand the installer listing says so too.
$sideBySide = Join-Path $root 'target/release/wsh.exe'
if (-not (Test-Path $sideBySide -PathType Leaf)) { Fail 'no wsh.exe beside holzwave-app.exe under target/release' }
if ((Get-FileHash -Algorithm SHA256 $sideBySide).Hash -ne (Get-FileHash -Algorithm SHA256 $sidecar).Hash) {
    Fail 'wsh.exe beside holzwave-app.exe is not the sidecar that was built'
}
$sevenZip = Get-Command 7z -ErrorAction SilentlyContinue
if ($sevenZip) {
    $listing = & $sevenZip.Source l $found.FullName
    if (-not ($listing -match ' wsh\.exe$')) { Fail 'the installer does not hold wsh.exe' }
    Write-Host 'release-windows: the installer holds wsh.exe, the file that was built'
} else {
    Write-Host 'release-windows: 7z is not on PATH: the installer content was not listed, only the build folder was checked'
}

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
