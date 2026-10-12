# Building holzWave on Windows

For the owner's own Windows machines. Everything here is built and run locally: no step uses GitHub to build, creates a
tag or a release, or needs a CI file (Phase 2 D-04). The result is a per-user NSIS installer that is **not signed**
(signing is Phase 10).

> [!IMPORTANT]
> **Honest status.** This guide was written on a Mac. No Windows host was available, so no step below was run on
> Windows. What was checked on the Mac is listed at the end ([What was and was not verified](#what-was-and-was-not-verified));
> the rest is the owner's first run, and rows W1 to W13 of [test-plan.md](test-plan.md) (Part 4) are where a step that
> does not work as written gets noted. Where a step says "unverified", that is meant literally.

## Rules of this build

- **Zig is exactly 0.16.0.** The pinned Ghostty commit builds the terminal engine with its own `build.zig`, which needs
  Zig 0.16.0; Zig 0.17 removed an API it uses and fails (Phase 2 decision D-02). `winget`, Scoop, Chocolatey and every
  other package manager hand out the newest Zig, so none of them is used: `scripts/env.ps1` installs and checks the exact
  version.
- **Everything else is the newest version**: Rust stable, Node, npm, git, Visual Studio Build Tools.
- **Build only locally**, on your own machine.
- x64 Windows only. ARM64 is not a 0.0.1 target.

## What you need

| What | Notes |
|---|---|
| Windows 10 1809 or newer, or Windows 11, x64 | |
| Visual Studio Build Tools, workload **Desktop development with C++** | The MSVC build of the terminal engine needs the MSVC compiler, its C and C++ headers and the Windows SDK. Zig cannot get these from another host: it fails on a Mac for exactly that reason, so this step exists only on Windows. Download "Build Tools for Visual Studio" from <https://visualstudio.microsoft.com/downloads/> and tick that workload (the Windows SDK is part of it). A full Visual Studio with the same workload works too. |
| Rust stable with the **MSVC** host | From <https://rustup.rs>; the host triple must be `x86_64-pc-windows-msvc` (the default on Windows). A GNU toolchain cannot link the engine. `rustup update` keeps it current. |
| Node 26 or newer, with npm | <https://nodejs.org>. Used for the page (TypeScript) and the Tauri CLI. |
| git | <https://git-scm.com>. The engine's build clones Ghostty at the pinned commit the first time. |
| Zig **0.16.0** | Installed by `scripts\env.ps1 -InstallZig` below, never by a package manager. |
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
.\scripts\env.ps1 -InstallZig      # once: downloads Zig 0.16.0, verifies it, unpacks it to $HOME\.holzwave\zig-0.16.0
. .\scripts\env.ps1                # every new window: note the dot and the space in front
```

If PowerShell says that running scripts is disabled (the default policy of Windows PowerShell 5.1), allow local scripts
for this window only, then run the command again:

```powershell
Set-ExecutionPolicy -Scope Process -ExecutionPolicy RemoteSigned
```

What `env.ps1` does, and does not do:

- `-InstallZig` fetches the 0.16.0 archive for your machine from ziglang.org, compares its sha256 with the value in the
  published index at <https://ziglang.org/download/index.json> **before anything is unpacked**, and stops on a mismatch.
  It overwrites nothing: if `$HOME\.holzwave\zig-0.16.0` exists, it says so and leaves it.
- Sourced (`. .\scripts\env.ps1`), it finds Zig in `$env:HOLZWAVE_ZIG_DIR` or in `$HOME\.holzwave\zig-0.16.0`, accepts
  only exactly `0.16.0`, puts it first on `PATH` of this window and prints `zig version`. It checks that Rust's host is
  `x86_64-pc-windows-msvc` and that `vswhere.exe` finds the Visual Studio C++ tools, and prints what it found: Zig,
  Rust, Cargo, Node, npm, git and Visual Studio. It changes nothing but this window's `PATH`, and it never closes your
  window.
- It ends with `env.ps1: ready` when the three hard requirements (Zig, Rust host, C++ tools) are met. Each missing one
  gets one line that says what to install. If a Zig other than 0.16.0 is on your `PATH`, it does no harm: the checked one
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

The installer lands under `target\release\bundle\nsis\`, a name like `holzWave_0.0.1-alpha1_x64-setup.exe` (the bundler
chooses the name; look in the folder). The `target` folder is at the root of the checkout, not inside `app`.

- **Per user, no administrator.** The installer installs for the current user without an administrator prompt, by
  default under `%LOCALAPPDATA%\holzWave` (the bundler's choice: unverified here, W12 shows it). An upgrade needs no
  administrator either.
- **No MSI is built.** The bundler downloads the WiX toolset for an MSI at build time and WiX needs an optional Windows
  feature; an MSI also cannot carry a version like `0.0.1-alpha1`. NSIS only.
- **WebView2.** The installer uses the default WebView2 mode: when the runtime is missing it downloads the Evergreen
  bootstrapper, so that case needs the network at install time. (An installer that carries the runtime would add about
  127 MB; not chosen.)
- **The bundler downloads the NSIS tools on the first build.** That is a network step of `tauri build`, not of the
  guide's own scripts.

### 7. Install, start, uninstall

1. Run the installer. It is **unsigned**, so SmartScreen shows "Windows protected your PC": choose **More info**, then
   **Run anyway**. This is expected for every unsigned installer. Signing is Phase 10.
2. Start holzWave from the Start menu. The app creates `%APPDATA%\ch.holzcloud.holzwave\` (settings, layout, session,
   themes) on first use; the files are described in [config.md](config.md).
3. Uninstall from **Settings, Apps** (or with the `uninstall.exe` in the install folder). The program files should be
   gone afterwards. Whether the uninstaller offers to remove the settings folder is for W12 to note; if it does not,
   delete `%APPDATA%\ch.holzcloud.holzwave\` yourself.

### Publishing the installer's checksum

An unsigned file cannot be told apart from a tampered one by Windows, so a checksum next to the file is the only check a
user has. When an installer is ever shared:

```powershell
Get-FileHash -Algorithm SHA256 .\target\release\bundle\nsis\holzWave_0.0.1-alpha1_x64-setup.exe
```

Publish the hash beside the file (on the release page when there is one). No release is made by this guide.

## Reporting a result

Open Part 4 of [test-plan.md](test-plan.md). For every row W1 to W13, write `pass`, or `fail` with a line of what you
saw, in its Result column, and also note any step of **this guide** that failed with its exact message (W1 covers the
guide from the top). Rows you could not run stay empty. A failing row becomes a gap plan; DESK-30 is marked done only when
the rows pass.

## What was and was not verified

Verified on a Mac, with a tool that exists there:

| What | How |
|---|---|
| The installer configuration: NSIS only, per user, no signing key of any kind, no WiX section, `icons/icon.ico` listed and present, no install hook, base file unchanged | `app/src-tauri/tests/bundle.rs` reads `tauri.windows.conf.json` and the base file |
| `scripts/env.ps1` parses with the PowerShell parser of PowerShell 7.6.6; sourced on a Mac it returns with a message and does not close the shell; it leaves no function or variable behind | run with `pwsh` |
| The Zig install path: the published index is read (the entry of 0.16.0 for both Windows architectures), a foreign address or a malformed checksum in the index is refused, a download whose sha256 differs is deleted and nothing is unpacked, the archive's real checksum equals the index and its layout is the one the script expects, a Zig of another version is refused | run with `pwsh` on the Mac against the real ziglang.org download, and with a tampered checksum |
| The core and the app compile for `x86_64-pc-windows-gnu`, including every test target | `sh scripts/cross-check.sh all` |

**Not verified, because no Windows host was available:** that `env.ps1` finds `vswhere.exe` and the C++ tools, reads
`rustc -vV` and runs `zig.exe` on Windows; that Windows PowerShell 5.1 runs it (written to its limits, not tried); the
MSVC build of the terminal engine; that the guide's commands run in this order from a clean user account; the installer,
its name, its install folder, SmartScreen and the uninstaller; the WebView2 bootstrapper; the ConPTY tests; and every
row of Part 4. The two `winget` ids and the long-path advice are from memory.
