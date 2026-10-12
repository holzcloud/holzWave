# Toolchain check and environment for local builds and tests of holzWave on Windows: the twin of scripts/env.sh.
# Source it with a dot, do not run it:
#
#   . .\scripts\env.ps1
#
# What it does, for this PowerShell session only (it never changes the machine, the registry or the user's PATH):
#
#   * finds Zig exactly 0.16.0 in $env:HOLZWAVE_ZIG_DIR or in $HOME\.holzwave\zig-0.16.0, puts it first on PATH and
#     prints `zig version`; it stops with a message when no Zig there is exactly 0.16.0. The pinned Ghostty commit
#     needs exactly 0.16.0 and Zig 0.17 breaks Ghostty's build.zig (Phase 2 decision D-02): a package manager's
#     "latest" is never the right Zig for this project;
#   * checks that Rust's host triple is x86_64-pc-windows-msvc, because Zig cannot build the MSVC library from another
#     host and a GNU toolchain cannot link it;
#   * checks that the Visual Studio C++ tools exist (vswhere.exe), the workload "Desktop development with C++";
#   * prints the versions of what it found (Zig, Rust, Cargo, Node, npm, git, Visual Studio).
#
#   .\scripts\env.ps1 -InstallZig
#
# The only part that downloads anything: it fetches the Zig 0.16.0 archive for this machine's architecture from
# ziglang.org, compares its sha256 with the value in ziglang.org's published index BEFORE anything is unpacked (a
# mismatch deletes the download and stops), unpacks it to $HOME\.holzwave\zig-0.16.0 and overwrites nothing that
# already exists. Afterwards source the script as above.
#
# Sourced, the script never calls `exit` (that would close the caller's window) and never changes strict mode or the
# error preference of the session; it only returns. Run, it ends with exit code 0 (everything found), 1 (something
# is missing or wrong) or 2 (not Windows). It works in Windows PowerShell 5.1 and in PowerShell 7.
#
# Written on a Mac without a Windows host: the parser and the pure parts are tested there (see
# docs/build-windows.md, "What was and was not verified"); the behaviour on Windows is row W1 of docs/test-plan.md.

param([switch]$InstallZig)

$HolzwaveZigVersion = '0.16.0'

function Write-HolzwaveLine([string]$Text) {
    Write-Host "env.ps1: $Text"
}

function Test-HolzwaveWindows {
    return ([System.Environment]::OSVersion.Platform -eq [System.PlatformID]::Win32NT)
}

# The home directory: $HOME is set by PowerShell on Windows; USERPROFILE is the same place.
function Get-HolzwaveHome {
    if ($HOME) { return $HOME }
    return $env:USERPROFILE
}

function Get-HolzwaveZigDefaultDir {
    return Join-Path (Join-Path (Get-HolzwaveHome) '.holzwave') ('zig-' + $HolzwaveZigVersion)
}

# The key of the machine in Zig's index (the machine's architecture, not the process's: a 32-bit shell on a 64-bit
# Windows must still get the 64-bit compiler). Returns $null for an architecture without a Zig build here.
function Get-HolzwaveZigKey {
    $arch = ''
    try { $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString() } catch { }
    if (-not $arch) {
        $arch = $env:PROCESSOR_ARCHITEW6432
        if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
    }
    switch -Regex ($arch) {
        '^(X64|AMD64)$' { return 'x86_64-windows' }
        '^ARM64$' { return 'aarch64-windows' }
        default { return $null }
    }
}

# Reads what `zig version` printed for a zig.exe at $Path. Returns the trimmed text, or $null when it did not run.
function Get-HolzwaveZigVersionOf([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return $null }
    try {
        $out = & $Path version 2>$null
        if ($LASTEXITCODE -ne 0) { return $null }
        return ([string]($out | Select-Object -First 1)).Trim()
    } catch {
        return $null
    }
}

# The pure part of the install check: the shasum and the URL from the published index must look the way they do on
# ziglang.org before the script trusts them. Returns a hashtable with Url and Sha, or throws a message.
function Resolve-HolzwaveZigEntry($Index, [string]$Key) {
    $version = $Index.PSObject.Properties[$HolzwaveZigVersion]
    if (-not $version) { throw "ziglang.org's index has no entry for Zig $HolzwaveZigVersion" }
    $entry = $version.Value.PSObject.Properties[$Key]
    if (-not $entry) { throw "ziglang.org's index has no Zig $HolzwaveZigVersion for $Key" }
    $url = [string]$entry.Value.tarball
    $sha = ([string]$entry.Value.shasum).ToLowerInvariant()
    $expected = "https://ziglang.org/download/$HolzwaveZigVersion/zig-$Key-$HolzwaveZigVersion.zip"
    if ($url -ne $expected) { throw "unexpected Zig archive address in the index: $url" }
    if ($sha -notmatch '^[0-9a-f]{64}$') { throw "unexpected Zig checksum in the index: $sha" }
    return @{ Url = $url; Sha = $sha }
}

# True when the file's sha256 equals $Expected (lower case hex).
function Test-HolzwaveSha256([string]$Path, [string]$Expected) {
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
    return ($actual -eq $Expected.ToLowerInvariant())
}

# Downloads, verifies and unpacks Zig. Returns $true when a usable Zig is at the default directory afterwards.
function Install-HolzwaveZig {
    $target = Get-HolzwaveZigDefaultDir
    if (Test-Path -LiteralPath $target) {
        Write-HolzwaveLine "$target exists already and is left alone (delete it yourself to install again)"
        return $true
    }
    $key = Get-HolzwaveZigKey
    if (-not $key) {
        Write-HolzwaveLine 'this machine architecture has no Zig build for holzWave; download Zig 0.16.0 yourself from https://ziglang.org/download/'
        return $false
    }
    $root = Split-Path -Parent $target
    $work = Join-Path $root ('download-' + [guid]::NewGuid().ToString('N'))
    $progress = $ProgressPreference
    try {
        # TLS 1.2 explicitly: Windows PowerShell 5.1 may default to older protocols.
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
        $ProgressPreference = 'SilentlyContinue'
        New-Item -ItemType Directory -Path $work -Force | Out-Null
        Write-HolzwaveLine 'reading the published index at https://ziglang.org/download/index.json'
        $index = (Invoke-WebRequest -UseBasicParsing -Uri 'https://ziglang.org/download/index.json').Content | ConvertFrom-Json
        $entry = Resolve-HolzwaveZigEntry $index $key
        $archive = Join-Path $work 'zig.zip'
        Write-HolzwaveLine "downloading $($entry.Url)"
        Invoke-WebRequest -UseBasicParsing -Uri $entry.Url -OutFile $archive
        if (-not (Test-HolzwaveSha256 $archive $entry.Sha)) {
            Write-HolzwaveLine "the sha256 of the download is not the one in the published index ($($entry.Sha)); nothing was unpacked"
            return $false
        }
        Write-HolzwaveLine 'sha256 matches the published index'
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $unpacked = Join-Path $work 'unpacked'
        [System.IO.Compression.ZipFile]::ExtractToDirectory($archive, $unpacked)
        $inner = Join-Path $unpacked ("zig-$key-$HolzwaveZigVersion")
        $version = Get-HolzwaveZigVersionOf (Join-Path $inner 'zig.exe')
        if ($version -ne $HolzwaveZigVersion) {
            Write-HolzwaveLine "the unpacked compiler says '$version', not $HolzwaveZigVersion; nothing was installed"
            return $false
        }
        Move-Item -LiteralPath $inner -Destination $target
        Write-HolzwaveLine "installed Zig $HolzwaveZigVersion to $target"
        return $true
    } catch {
        Write-HolzwaveLine "installing Zig failed: $($_.Exception.Message)"
        return $false
    } finally {
        $ProgressPreference = $progress
        if (Test-Path -LiteralPath $work) { Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue }
    }
}

# Puts $Dir first on this session's PATH unless it is the first entry already.
function Add-HolzwaveFirst([string]$Dir) {
    $first = ($env:PATH -split ';' | Select-Object -First 1)
    if ($first -ne $Dir) { $env:PATH = "$Dir;$env:PATH" }
}

function Add-HolzwaveIfMissing([string]$Dir) {
    if (-not (Test-Path -LiteralPath $Dir -PathType Container)) { return }
    foreach ($entry in ($env:PATH -split ';')) {
        if ($entry.TrimEnd('\') -ieq $Dir.TrimEnd('\')) { return }
    }
    $env:PATH = "$Dir;$env:PATH"
}

# Finds Zig 0.16.0: the directory named by HOLZWAVE_ZIG_DIR first, then the default one. Returns the directory, or
# $null after saying what it found instead.
function Find-HolzwaveZig {
    $candidates = @()
    if ($env:HOLZWAVE_ZIG_DIR) { $candidates += $env:HOLZWAVE_ZIG_DIR }
    $candidates += (Get-HolzwaveZigDefaultDir)
    foreach ($dir in $candidates) {
        $exe = Join-Path $dir 'zig.exe'
        $version = Get-HolzwaveZigVersionOf $exe
        if ($version -eq $HolzwaveZigVersion) { return $dir }
        if ($version) {
            Write-HolzwaveLine "$exe is Zig $version, not $HolzwaveZigVersion (exactly 0.16.0 is needed; Zig 0.17 breaks Ghostty's build)"
        } else {
            Write-HolzwaveLine "no zig.exe in $dir"
        }
    }
    return $null
}

# The first line of what a command prints, or $null when it is not there or fails.
function Get-HolzwaveFirstLine([string]$Command, [string[]]$Arguments) {
    if (-not (Get-Command $Command -ErrorAction SilentlyContinue)) { return $null }
    try {
        $out = & $Command @Arguments 2>$null
        if ($LASTEXITCODE -ne 0) { return $null }
        return ([string]($out | Select-Object -First 1)).Trim()
    } catch {
        return $null
    }
}

# The host line of `rustc -vV`, as the triple.
function Get-HolzwaveRustHost {
    if (-not (Get-Command rustc -ErrorAction SilentlyContinue)) { return $null }
    try {
        foreach ($line in (& rustc -vV 2>$null)) {
            if ($line -match '^host:\s*(\S+)\s*$') { return $Matches[1] }
        }
    } catch { }
    return $null
}

# Visual Studio (or Build Tools) with the C++ x64 tools: the installation path and version, or $null.
function Find-HolzwaveVisualStudio {
    $programFiles = ${env:ProgramFiles(x86)}
    if (-not $programFiles) { $programFiles = $env:ProgramFiles }
    if (-not $programFiles) { return $null }
    $vswhere = Join-Path $programFiles 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path -LiteralPath $vswhere -PathType Leaf)) { return $null }
    try {
        $path = & $vswhere -products '*' -latest -requires 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64' -property installationPath 2>$null | Select-Object -First 1
        if (-not $path) { return $null }
        $version = & $vswhere -products '*' -latest -requires 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64' -property catalog_productDisplayVersion 2>$null | Select-Object -First 1
        return @{ Path = [string]$path; Version = [string]$version }
    } catch {
        return $null
    }
}

# The whole check. Returns the exit code: 0 everything the build needs was found, 1 something is missing, 2 not Windows.
function Invoke-HolzwaveEnv([bool]$Install) {
    if (-not (Test-HolzwaveWindows)) {
        Write-HolzwaveLine 'this script is for Windows; on macOS and Linux use scripts/env.sh'
        return 2
    }
    if ($Install) {
        if (-not (Install-HolzwaveZig)) { return 1 }
    }

    $ok = $true

    Add-HolzwaveIfMissing (Join-Path (Get-HolzwaveHome) '.cargo\bin')

    $zigDir = Find-HolzwaveZig
    if ($zigDir) {
        Add-HolzwaveFirst $zigDir
        Write-HolzwaveLine "zig version $(Get-HolzwaveZigVersionOf (Join-Path $zigDir 'zig.exe')) ($zigDir)"
    } else {
        Write-HolzwaveLine 'no Zig 0.16.0 found: run  .\scripts\env.ps1 -InstallZig  once, or set HOLZWAVE_ZIG_DIR to the folder of a Zig 0.16.0 zig.exe'
        $ok = $false
    }

    $rustHost = Get-HolzwaveRustHost
    if (-not $rustHost) {
        Write-HolzwaveLine 'rustc was not found: install Rust with rustup (https://rustup.rs) and choose the MSVC host, then open a new window'
        $ok = $false
    } elseif ($rustHost -ne 'x86_64-pc-windows-msvc') {
        Write-HolzwaveLine "the Rust host is $rustHost, holzWave needs x86_64-pc-windows-msvc: run  rustup default stable-x86_64-pc-windows-msvc"
        $ok = $false
    } else {
        Write-HolzwaveLine "$(Get-HolzwaveFirstLine rustc @('--version')) (host $rustHost)"
        $cargo = Get-HolzwaveFirstLine cargo @('--version')
        if ($cargo) { Write-HolzwaveLine $cargo }
    }

    $vs = Find-HolzwaveVisualStudio
    if ($vs) {
        Write-HolzwaveLine "Visual Studio C++ tools $($vs.Version) ($($vs.Path))"
    } else {
        Write-HolzwaveLine 'the Visual Studio C++ tools were not found: install "Build Tools for Visual Studio" with the workload "Desktop development with C++" (the MSVC build of the terminal engine needs it)'
        $ok = $false
    }

    # Not part of the build's hard requirements here: a missing one is said, not a failure of the check.
    $node = Get-HolzwaveFirstLine node @('--version')
    if ($node) { Write-HolzwaveLine "node $node" } else { Write-HolzwaveLine 'node was not found (Node 26 or newer is needed for the page and the tests)' }
    $npm = Get-HolzwaveFirstLine npm @('--version')
    if ($npm) { Write-HolzwaveLine "npm $npm" }
    $git = Get-HolzwaveFirstLine git @('--version')
    if ($git) { Write-HolzwaveLine $git } else { Write-HolzwaveLine 'git was not found' }

    if ($ok) { return 0 }
    return 1
}

$HolzwaveSourced = ($MyInvocation.InvocationName -eq '.')
$HolzwaveResult = Invoke-HolzwaveEnv $InstallZig.IsPresent
if ($HolzwaveResult -eq 0) {
    if ($HolzwaveSourced) {
        Write-HolzwaveLine 'ready'
    } else {
        Write-HolzwaveLine 'everything was found; source the script with a dot to use it in a session:  . .\scripts\env.ps1'
    }
} elseif ($InstallZig.IsPresent -and -not $HolzwaveSourced -and $HolzwaveResult -eq 1) {
    Write-HolzwaveLine 'next: source the script with a dot to check the rest:  . .\scripts\env.ps1'
}

# Nothing of the script stays in the caller's session except PATH: its functions and variables are removed again.
$HolzwaveExit = [int]$HolzwaveResult
$HolzwaveRun = -not $HolzwaveSourced
Remove-Item -Path Function:\Write-HolzwaveLine, Function:\Test-HolzwaveWindows, Function:\Get-HolzwaveHome,
    Function:\Get-HolzwaveZigDefaultDir, Function:\Get-HolzwaveZigKey, Function:\Get-HolzwaveZigVersionOf,
    Function:\Resolve-HolzwaveZigEntry, Function:\Test-HolzwaveSha256, Function:\Install-HolzwaveZig,
    Function:\Add-HolzwaveFirst, Function:\Add-HolzwaveIfMissing, Function:\Find-HolzwaveZig,
    Function:\Get-HolzwaveFirstLine, Function:\Get-HolzwaveRustHost, Function:\Find-HolzwaveVisualStudio,
    Function:\Invoke-HolzwaveEnv -ErrorAction SilentlyContinue
Remove-Variable -Name HolzwaveZigVersion, HolzwaveSourced, HolzwaveResult, InstallZig -ErrorAction SilentlyContinue
if ($HolzwaveRun) { exit $HolzwaveExit }
Remove-Variable -Name HolzwaveExit, HolzwaveRun -ErrorAction SilentlyContinue
