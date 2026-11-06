# Building holzWave on Windows

For the owner's own Windows machines. Everything here is built and run locally: no step uses GitHub to build, creates a
tag or a release, or needs a CI file (Phase 2 D-04). The result is a per-user NSIS installer that is **not signed**: the
author has no code-signing certificate (the reason is in [signing.md](signing.md)).

> [!IMPORTANT]
> **Status.** This guide was written on a Mac and has since been followed on Windows by the author, who installed and ran
> the preview installer there. What was checked on the Mac is listed at the end
> ([What was and was not verified](#what-was-and-was-not-verified)); rows W1 to W14 of [test-plan.md](test-plan.md)
> (Part 4) are where a step that does not work as written gets noted.

## Rules of this build

- **Zig is exactly 0.17.0.** The pinned Ghostty commit builds the terminal engine with its own `build.zig`, and its
  `build.zig.zon` says `minimum_zig_version = "0.17.0"` (the commit before the pin needed 0.16.0, so the version moved
  with the pin). `winget`, Scoop, Chocolatey and every other package manager hand out the newest Zig, which will not stay
  0.17.0, so none of them is used: `scripts/env.ps1` installs and checks the exact version.
- **Everything else is the newest version**: Rust stable, Node, npm, git, Visual Studio Build Tools.
- **Build only locally**, on your own machine.
- x64 is the build that has been run. `scripts\release-windows.ps1 -Arch arm64` can build a Windows ARM64 installer
  (see [The ARM64 build](#the-arm64-build-never-run)); that build was never run and its result is untried.

## What you need

| What | Notes |
|---|---|
| Windows 10 1809 or newer, or Windows 11, x64 | |
| Visual Studio Build Tools, workload **Desktop development with C++** | The MSVC build of the terminal engine needs the MSVC compiler, its C and C++ headers and the Windows SDK. Zig cannot get these from another host: it fails on a Mac for exactly that reason, so this step exists only on Windows. Download "Build Tools for Visual Studio" from <https://visualstudio.microsoft.com/downloads/> and tick that workload (the Windows SDK is part of it). A full Visual Studio with the same workload works too. |
| Rust stable with the **MSVC** host | From <https://rustup.rs>; the host triple must be `x86_64-pc-windows-msvc` (the default on Windows). A GNU toolchain cannot link the engine. `rustup update` keeps it current. |
| Node 26 or newer, with npm | <https://nodejs.org>. Used for the page (TypeScript) and the Tauri CLI. |
| git | <https://git-scm.com>. The engine's build clones Ghostty at the pinned commit the first time. |
| Zig **0.17.0** | Installed by `scripts\env.ps1 -InstallZig` below, never by a package manager. |
| For the ARM64 build only: the Visual Studio component **MSVC ARM64/ARM64EC build tools** and the Rust target `aarch64-pc-windows-msvc` | Tick the individual component with the id `Microsoft.VisualStudio.Component.VC.Tools.ARM64` in the Visual Studio Installer (the id is the authority, the display name changes between releases), then `rustup target add aarch64-pc-windows-msvc`. The build host stays an x64 machine with the MSVC host triple; `scripts\env.ps1` checks the component when `HOLZWAVE_WINDOWS_ARCH` is `arm64`. Building on an ARM64 Windows host was not considered. |
| WebView2 Evergreen runtime | Already on current Windows 10 and Windows 11. The installer downloads its bootstrapper when the runtime is missing, which needs the network at install time. |
| Network on the first build | Cloning Ghostty and fetching its Zig packages (some come from codeberg.org). Later builds reuse what is cached. |

The package ids `Rustlang.Rustup`, `OpenJS.NodeJS` and `Git.Git` should work with `winget install` (unverified; the
websites above are the authority). Open a new terminal after installing so `PATH` is current.

**Use a short folder for the checkout**, for example `C:\src\holzWave`. Cargo's nested build folders plus Ghostty's own
file names can exceed the 260 character path limit (the engine's build script clones with `core.longpaths=true` for this
reason). Whether a long folder works here was not tried.

## Steps

All commands are for PowerShell 7 (`pwsh`) or Windows PowerShell 5.1, started in an ordinary (not administrator)
window, from the root of the checkout.

### 1. Get the code

```powershell
git clone https://github.com/holzcloud/holzWave C:\src\holzWave
cd C:\src\holzWave
```

(Cloning is a download; nothing builds on GitHub.) If you work on a branch of the phase, `git checkout` it now.

### 2. Check and set up the environment

```powershell
.\scripts\env.ps1 -InstallZig      # once: downloads Zig 0.17.0, verifies it, unpacks it to $HOME\.holzwave\zig-0.17.0
. .\scripts\env.ps1                # every new window: note the dot and the space in front
```

If PowerShell says that running scripts is disabled (the default policy of Windows PowerShell 5.1), allow local scripts
for this window only, then run the command again:

```powershell
Set-ExecutionPolicy -Scope Process -ExecutionPolicy RemoteSigned
```

What `env.ps1` does, and does not do:

- `-InstallZig` fetches the 0.17.0 archive for your machine from ziglang.org, compares its sha256 with the value in the
  published index at <https://ziglang.org/download/index.json> **before anything is unpacked**, and stops on a mismatch.
  It overwrites nothing: if `$HOME\.holzwave\zig-0.17.0` exists, it says so and leaves it.
- Sourced (`. .\scripts\env.ps1`), it finds Zig in `$env:HOLZWAVE_ZIG_DIR` or in `$HOME\.holzwave\zig-0.17.0`, accepts
  only exactly `0.17.0`, puts it first on `PATH` of this window and prints `zig version`. It checks that Rust's host is
  `x86_64-pc-windows-msvc` and that `vswhere.exe` finds the Visual Studio C++ tools, and prints what it found: Zig,
  Rust, Cargo, Node, npm, git and Visual Studio. It changes nothing but this window's `PATH`, and it never closes your
  window.
- It ends with `env.ps1: ready` when the three hard requirements (Zig, Rust host, C++ tools) are met. Each missing one
  gets one line that says what to install. If a Zig other than 0.17.0 is on your `PATH`, it does no harm: the checked one
  is put in front of it.

### 3. Install the page's tools and build the page

```powershell
npm --prefix app ci                # the Tauri CLI, TypeScript and the browser-test driver; nothing else
npm --prefix app run build         # compiles the page into app\dist (the app build needs it)
```

### 4. Run the tests

```powershell
cargo test --workspace
npm --prefix app test
```

The first `cargo` command builds the terminal engine with the MSVC toolchain through Zig. **This is the long step**
(minutes on the first run; the number was not measured here), and it is the one that cannot be tried on a Mac.

What the output means on Windows:

- A test file that shows `running 0 tests` is not a failure. The tests that start `/bin/sh` or use Unix PTYs are
  compiled only on macOS and Linux (`#![cfg(unix)]`); on Windows their binaries are empty by design.
- The Windows-only ConPTY tests live in `core/tests/conpty.rs`. Run them on their own with their messages:

  ```powershell
  cargo test -p holzwave-core --test conpty -- --nocapture
  ```

  A case that cannot run says why (for example `skipped: no PowerShell on this machine`) and still counts as passed:
  read that output, not only the summary line. Each case maps to a row of the test plan (W1, W2, W3, W5, W7).
- A real failure prints the test's name and what it was waiting for. Copy it into the Result column of the row.

### 5. Run the app

```powershell
cargo run -p holzwave-app
```

A window opens with one pane and your default shell (PowerShell 7 if installed, else Windows PowerShell, else `cmd`;
the order and the `shell` setting are in [config.md](config.md)).

### 6. Build the installer

```powershell
npm --prefix app run tauri -- build --bundles nsis
```

`tauri.windows.conf.json` already sets the targets to NSIS and the install mode to per user, so the `--bundles nsis`
only repeats it. The command runs the page build itself and compiles the app in release mode (another long build).

The installer lands under `target\release\bundle\nsis\`, a name like `holzWave_0.0.1-beta2_x64-setup.exe` (the bundler
chooses the name; look in the folder). The `target` folder is at the root of the checkout, not inside `app`.

- **Per user, no administrator.** The installer installs for the current user without an administrator prompt, by
  default under `%LOCALAPPDATA%\holzWave` (the bundler's choice: unverified here, W12 shows it). An upgrade needs no
  administrator either.
- **No MSI is built.** The bundler downloads the WiX toolset for an MSI at build time and WiX needs an optional Windows
  feature; an MSI also cannot carry a version like `0.0.1-beta2`. NSIS only.
- **WebView2.** The installer uses the default WebView2 mode: when the runtime is missing it downloads the Evergreen
  bootstrapper, so that case needs the network at install time. (An installer that carries the runtime would add about
  127 MB; not chosen.)
- **The bundler downloads the NSIS tools on the first build.** That is a network step of `tauri build`, not of the
  guide's own scripts.

### 7. Install, start, uninstall

1. Run the installer. It is **unsigned**, so SmartScreen shows "Windows protected your PC": choose **More info**, then
   **Run anyway**. This is expected for every unsigned installer. There is no signed installer to compare it with; why it is
   unsigned is in [signing.md](signing.md).
2. Start holzWave from the Start menu. The app creates `%APPDATA%\ch.holzcloud.holzwave\` (settings, layout, session,
   themes) on first use; the files are described in [config.md](config.md).
3. Uninstall from **Settings, Apps** (or with the `uninstall.exe` in the install folder). The program files should be
   gone afterwards. Whether the uninstaller offers to remove the settings folder is for W12 to note; if it does not,
   delete `%APPDATA%\ch.holzcloud.holzwave\` yourself.

### Publishing the installer's checksum

An unsigned file cannot be told apart from a tampered one by Windows, so a checksum next to the file is the only check a
user has. When an installer is ever shared:

```powershell
Get-FileHash -Algorithm SHA256 .\target\release\bundle\nsis\holzWave_0.0.1-beta2_x64-setup.exe
```

Publish the hash beside the file (on the release page when there is one). It says the download is whole, not who made it.
No release is made by this guide.

## The ARM64 build (never run)

```powershell
.\scripts\release-windows.ps1 -Version 0.0.1-beta2 -Arch arm64
```

`-Arch` takes `x64` (the default; the script does what it always did) or `arm64`. With `arm64` the script sets
`HOLZWAVE_WINDOWS_ARCH=arm64` for the environment check, requires the Rust target `aarch64-pc-windows-msvc` (it stops with the
`rustup target add` line when it is missing), builds `wsh.exe` for that target, runs the Tauri build with
`--target aarch64-pc-windows-msvc` and names the result `holzWave_<version>_arm64-setup.exe` with its `.sha256` file, under
`target\release\bundle\release`. The terminal engine is built through Zig for `aarch64-windows-msvc` (the engine's build
script maps the Rust target to it), and the Zig 0.17.0 `aarch64-windows` archive is already in the table of `env.ps1`, so
nothing new is downloaded. The Linux `wsh` resources that the installer carries are the same on both architectures.

The app is a native ARM64 program. The NSIS installer program itself is an x86 program, which Windows on ARM runs under
emulation. **Not run:** the ARM64 build on any machine, the installer, the app on ARM64 hardware (none was available), the
ConPTY tests there. A release note or a listing must call it untried; the x64 installer remains the one that has been run.

## The Microsoft Store package (built-only, never run)

A package for the Microsoft Store is prepared next to the unsigned installer. **Nothing of it has been built or run on any
Windows machine** (not on x64 and not on ARM64), and the Store channel is on hold: the identity values below do not exist yet.

- **What it is.** An MSIX package that the Store signs. The author has no code-signing certificate and none is needed for the
  Store, so the `.msix` made here is not signed and cannot be installed by double click; it is only for the Store upload. The
  GitHub installer stays unsigned, as step 7 says.
- **Identity values come from the owner's Partner Center.** The package name, the publisher string and the publisher display
  name are shown on the product's identity page once the Store account and the product exist. They are never invented in this
  repository: `packaging/msix/identity.example.json` shows the shape with placeholders, the real file
  `packaging/msix/identity.json` is untracked, and the stager refuses a placeholder or an empty value. A wrong value makes a
  different package family.
- **Version.** A Store version has four numbers, the first not 0 and the fourth reserved for the Store, and it can never go
  down after an upload. `packaging/msix/version.mjs` maps the project version to it: `major.minor.patch` with an optional
  `-alphaN`, `-betaN` or `-rcN` becomes `(major+1).minor.(patch*100 + stage).0`, with stage N for alpha, 20 + N for beta, 60 + N
  for rc and 99 for a final version (`0.0.1-beta2` is `1.0.122.0`, `0.0.1` is `1.0.199.0`). A version that fits no rule is
  refused, never rounded. Unit tests pin the table and the order.
- **Steps (Windows).** After `scripts\release-windows.ps1` has built the app: `.\scripts\build-msix.ps1` (or `-Arch arm64`).
  It runs `node packaging/msix/build.mjs`, which stages `holzwave-app.exe`, `wsh.exe`, the Linux `wsh` resources when they are
  there, the app's own icons as logos and the generated `AppxManifest.xml` into `target\msix`, then packs the folder with
  `makeappx.exe` and prints the path and the sha256. `-Certify` also runs the Windows App Certification Kit. `makeappx` and the
  kit come with the free Windows SDK. The script uploads nothing.
- **What the manifest declares.** `runFullTrust` (a terminal starts the person's own shell, so the app runs with the person's
  rights) and `internetClient`, and nothing else: no file associations, no protocol handlers, no startup tasks.
- **The stager runs on any host** (Node only), so the version mapping, the manifest, the staged folder and the refusals are
  tested on a Mac. The manifest is checked by the stager's own reader, not by Microsoft's schema tools.

**Not run, and open:**

- The stager's output through `makeappx`, the certification kit and the Store; whether the Store accepts the package. The
  target-version values in the manifest (`MinVersion` 10.0.17763.0, `MaxVersionTested` 10.0.26100.0) and the plain logo files
  (the Store may ask for more sizes) are unchecked until the kit has run.
- The `appcert` command line in `build-msix.ps1` is written from the documentation, not tried; `build-msix.ps1` itself was
  read, not run (no PowerShell on the Mac).
- **WebView2 on a clean machine.** Whether a package without the bundled bootstrapper finds the Evergreen runtime on a clean
  Windows 10 machine was not tried.
- **The app's per-user files under MSIX.** Packaged desktop apps get package virtualisation of some writes under `AppData`;
  this was not relied on and not tried. Whether settings, saved connections and keychain entries behave the same in an MSIX
  install is for the first install on Windows to show, and [config.md](config.md) describes the unpackaged app only.

## The preview installer built from a Mac (GNU ABI, not the MSVC build)

For a test round before a Windows build host is set up, the owner builds a Windows installer **on the Mac**:

```sh
brew install mingw-w64                       # once: the GNU-ABI linker and resource compiler (the only system install)
rustup target add x86_64-pc-windows-gnu      # once
sh scripts/release-windows-cross.sh <version>       # clean tree; the version of tauri.conf.json
```

The result is `target/release-cross/holzWave_<version>_x64-setup.exe` and its `.sha256` file. The script only builds and
inspects; it never publishes. To attach the files to the release that `scripts/release-macos.sh --publish` created:
`gh release upload v<version> target/release-cross/holzWave_<version>_x64-setup.exe*`.

> [!WARNING]
> **What this installer is not.** It is built for the **GNU ABI** (`x86_64-pc-windows-gnu`, linked with mingw-w64), not
> the MSVC ABI that steps 1 to 7 above build. The author has run this GNU-ABI holzWave on Windows; a failure that shows up
> only with this installer may still come from the ABI and not from the code. It is unsigned (SmartScreen warns, see
> step 7) and per user. The MSVC build on a Windows host is the better build and replaces this one; use it as soon as a
> Windows machine builds.

How the script gets there, and what each step is for (measured on an Apple Silicon Mac, details in
`.planning/phases/02.2-windows-linux/02.2-RESEARCH-10.md`):

- **Rust and Zig** cross-compile for `x86_64-pc-windows-gnu`; Zig builds the terminal engine for `x86_64-windows-gnu`
  (it cannot build for MSVC on a Mac). The GNU linker drops a weak stack-probe symbol of Zig's runtime and fails with
  "relocation truncated to fit ... `___chkstk_ms`"; the script links this target with `-Wl,--no-gc-sections` (set only
  for this target through `CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS`), which costs a few MB of executable size.
- **NSIS** is not installed on the Mac. The Tauri bundler needs a `makensis` on `PATH` there; the script puts a one-line
  `makensis` in front of it that runs NSIS 3.10 in a Docker container (image `holzwave-nsis`: Ubuntu with the `nsis` and
  `7zip` packages), with the repository and the bundler's cache mounted at the same paths. The same container reads the
  finished installer back with 7-Zip.
- **WebView2.** The GNU ABI loads `WebView2Loader.dll` as a DLL (the MSVC build links it in); the bundler installs it
  beside the executable. The installer still downloads the WebView2 runtime when it is missing, as in step 6.
- **No console window.** `app/src-tauri/src/main.rs` makes release builds a GUI program (a pinned test); the first build
  of this plan showed that without it the executable is a console program.
- **Windows 10 or newer.** The executable imports the Universal C Runtime (`api-ms-win-crt-*`) of the mingw-w64 build;
  no mingw runtime DLL is needed (the script fails if one is imported).

Checked on the Mac by the script (and nowhere else): the executable is a 64-bit GUI PE file that imports no mingw
runtime DLL; the installer is an NSIS installer that holds `holzwave-app.exe` and `WebView2Loader.dll`, both read back out
of it and compared with the built files. **Not checked:** that the installer runs, that the application starts, that
WebView2 accepts the GNU-linked loader, that ConPTY works from this executable, SmartScreen, the uninstaller. Row W14 of
[test-plan.md](test-plan.md) is where the owner tests this exact file, and the other rows then apply to what was installed from it.

## Reporting a result

Open Part 4 of [test-plan.md](test-plan.md). For every row W1 to W14, write `pass`, or `fail` with a line of what you
saw, in its Result column, and also note any step of **this guide** that failed with its exact message (W1 covers the
guide from the top). Rows you could not run stay empty. A failing row becomes a gap plan; DESK-30 is marked done only when
the rows pass.

## What was and was not verified

Verified on a Mac, with a tool that exists there:

| What | How |
|---|---|
| The installer configuration: NSIS only, per user, no signing key of any kind, no WiX section, `icons/icon.ico` listed and present, no install hook, base file unchanged | `app/src-tauri/tests/bundle.rs` reads `tauri.windows.conf.json` and the base file |
| `scripts/env.ps1` parses with the PowerShell parser of PowerShell 7.6.6; sourced on a Mac it returns with a message and does not close the shell; it leaves no function or variable behind | run with `pwsh` |
| The Zig install path: the published index is read (the entry of 0.17.0 for both Windows architectures), a foreign address or a malformed checksum in the index is refused, a download whose sha256 differs is deleted and nothing is unpacked, the archive's real checksum equals the index and its layout is the one the script expects, a Zig of another version is refused | run with `pwsh` on the Mac against the real ziglang.org download, and with a tampered checksum |
| The core and the app compile for `x86_64-pc-windows-gnu`, including every test target | `sh scripts/cross-check.sh all` |
| The preview installer: the GNU-ABI release build links, the executable is a 64-bit GUI PE file that imports no mingw runtime DLL, the NSIS installer holds the executable and `WebView2Loader.dll`, both read back out of it | `sh scripts/release-windows-cross.sh <version>` on a Mac (mingw-w64 from Homebrew, NSIS in Docker) |

**Store package (written and tested on the Mac, never built):** the version mapping, the manifest generator and the stager have
unit tests that run on any host; the manifest was also parsed once by Python's XML parser. `scripts/build-msix.ps1` was read
only. Not checked: `makeappx`, the certification kit, an install of the package, WebView2 on a clean machine, the per-user
files under MSIX, the Store's acceptance, and any Store identity (none exists yet).

**ARM64 (read, not run):** `-Arch arm64` in `scripts/release-windows.ps1` and the ARM64 check in `scripts/env.ps1` were
written without a Windows host and without `pwsh`, so they were only read. That the engine's build script maps
`aarch64-pc-windows-msvc` to Zig's `aarch64-windows-msvc` target was read in the pinned source of the engine's build script.
Not checked: the ARM64 build, the ARM64 installer and the app on ARM64 hardware.

**Not covered by the Mac checks:** that `env.ps1` finds `vswhere.exe` and the C++ tools, reads `rustc -vV` and runs
`zig.exe` on Windows; Windows PowerShell 5.1 running it; the MSVC build of the terminal engine; the guide's commands in
this order from a clean user account; the installer, its install folder, SmartScreen, the uninstaller, the WebView2
bootstrapper and the ConPTY tests. The author has installed and run the preview installer on a real Windows machine
(rows W14 and W1 to W13 are the record); the two `winget` ids and the long-path advice are still from memory.

**Not covered since the move to Zig 0.17.0 (2026-11-01):** that record and the `pwsh` row above were made with Zig 0.16.0 and
were not repeated. No Windows host and no `pwsh` were available for the move. What was checked: the index entry of 0.17.0
for `x86_64-windows` and `aarch64-windows`, the sha256 of both downloaded `.zip` files against it, and the folder name inside
(`zig-<key>-0.17.0`, which `env.ps1` expects). Not checked: `env.ps1 -InstallZig` and the sourced `env.ps1` on Windows, and the
MSVC build of the terminal engine with Zig 0.17.0. The GNU-ABI cross build (`scripts/cross-check.sh`) is the only Windows build
that was run with Zig 0.17.0, see the result in [test-plan.md](test-plan.md) and the plan summary.
