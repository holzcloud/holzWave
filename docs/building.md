# Building holzWave from source

There is no installer yet. macOS on Apple Silicon is the only verified platform.

```sh
brew install rustup zig@0.16 node
export PATH="/opt/homebrew/opt/zig@0.16/bin:$PATH"   # Zig 0.16.0 exactly; see below
rustup default stable

git clone https://github.com/holzcloud/holzwave
cd holzwave
npm --prefix app ci && npm --prefix app run build    # the frontend, compiled by tsc
cargo run -p holzwave-app                            # builds libghostty-vt with Zig, then starts the app
```

**Why Zig 0.16.0 and not the newest?** `libghostty-vt` is built by Ghostty's own `build.zig`, which needs exactly
Zig 0.16. Zig 0.17 removed an API it uses and fails. The crate is pinned by git revision for the same reason.

Run the tests and the renderer benchmark locally:

```sh
cargo test --workspace
cargo run --release -p holzwave-spike -- throughput 256
```

Windows and Linux builds are written and cross-checked from a Mac and have not been run on their own platform yet (see [build-windows.md](build-windows.md) and [build-linux.md](build-linux.md)); iOS and Android are planned and not yet built. See [test-plan.md](test-plan.md) for the full test setup.

**Apple on-device AI (macOS).** `plugins/apple-ai` is a workspace crate with a first-party Swift package behind a small C ABI; it is not a Tauri plugin and no command reaches it from the page. `cargo test --workspace` builds it with SwiftPM on a Mac (Xcode or the Command Line Tools). The Foundation Models code is compiled only when the installed SDK has the framework (Xcode 26 or newer); with an older SDK the crate still builds and reports `sdk_lacks_framework`. With Xcode 27 run `rustup component add llvm-tools` once, because `swift-rs` uses `llvm-objcopy` there to export the C symbols. The framework is linked weakly (the Swift package disables its autolink entry and the build script passes `-weak_framework FoundationModels`), so a process that contains the bridge starts on macOS versions that do not have it; a crate that links the bridge must repeat that argument and the run path `/usr/lib/swift` from the build metadata, see `plugins/apple-ai/build.rs`. The core, the CLI and the socket crate never depend on the bridge, and Windows and Linux builds compile no Swift. `sh scripts/apple-ai-launch-check.sh` builds a small probe, signs it ad hoc, shows how it is linked and runs it; copy the file it prints to a Mac below macOS 26 to see it start there. `cargo run -p holzwave-apple-ai --example chat -- "your text"` asks the on-device model one question and shows the state of the model. The tests that talk to the real model are ignored by default and run with `HOLZWAVE_TEST_APPLE=1 cargo test -p holzwave-apple-ai --test live -- --ignored` on macOS 26 or later with Apple Intelligence switched on. The window of the model (about 4096 tokens) is read at run time, so a very large terminal selection is refused before it is sent. The app depends on the bridge on macOS only (`app/src-tauri/Cargo.toml`), so `cargo run -p holzwave-app` on a Mac needs a Swift 6 toolchain (Xcode, or the Command Line Tools of Xcode 16 or later) and the Xcode 26 SDK to include the model; with an older SDK the app still builds and the model reports `sdk_lacks_framework`. The app's build script repeats the weak link and the run path from the bridge's build metadata, so the app starts on macOS 13 to 25 too, and `bundle.macOS.minimumSystemVersion` in `app/src-tauri/tauri.conf.json` equals the floor of the Swift package (13.0, which the Homebrew cask also requires); a policy test keeps the two equal, and `app/src-tauri/tests/weak_link.rs` reads the load commands of the app's own test executable.

## Releases

Releases are built on this machine, never in CI. `sh scripts/release-macos.sh <version> --publish --casks` builds the app, packs the DMG, creates the GitHub release (a version with a hyphen is a
prerelease) and writes the casks into `Casks/` of this repository (`holzwave@preview` for every release, `holzwave` for releases
without a hyphen); commit and push them afterwards. The repository is its own Homebrew tap, like holzBar. `.\scripts\release-windows.ps1 -Version <version> -Upload` on Windows builds the
NSIS installer and attaches it to the same release. Both scripts refuse a dirty tree or a `main` that differs from
`origin/main`.

**`wsh` in the packages.** Every release script builds `wsh` for its target and hands the Tauri bundler the overlay
`app/src-tauri/tauri.wsh.conf.json` with `--config`: `bundle.externalBin` names the sidecar `binaries/wsh`, and the two static
Linux builds are `bundle.resources` (`wsh/linux-x86_64` and `wsh/linux-aarch64` in the resource folder of the app, which the
app installs on a remote Linux host). It is an overlay and not part of `tauri.conf.json` because `tauri-build` requires a named
sidecar file at compile time: the default configuration names none, so `cargo test` and `cargo run` need no binary. The bundler
installs the sidecar beside the app executable, which is the folder that the app puts on a pane's `PATH` when `wsh` is in it:
`Contents/MacOS/wsh` in the macOS app, `wsh.exe` in the install folder on Windows, `/usr/bin/wsh` in the deb and the rpm and
`usr/bin/wsh` in the AppImage. The Flatpak does not carry it, see [flatpak.md](flatpak.md). `sh scripts/wsh-sidecar.sh <rust target
triple>` builds the sidecar for one target into `app/src-tauri/binaries/` (an ignored folder of build products, never committed).
`sh scripts/wsh-linux.sh` builds the two static Linux binaries (`x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`)
on any host with no Linux cross compiler: `wsh` has no C dependency, so the linker is `zig cc` (Zig 0.16 is a build requirement
already) or, where that does not link, Rust's own `rust-lld`; run `rustup target add x86_64-unknown-linux-musl
aarch64-unknown-linux-musl` once. It checks with `file` that each is an ELF of the right machine and statically linked.
Each release script then inspects the package it built: `scripts/bundle-check-macos.sh` (the file is beside the executable that
`CFBundleExecutable` names, it is arm64, its signature is valid, `wsh help` runs, the Linux resources are static ELF files, nothing is
world-writable, and the files are byte-identical to the ones built), `scripts/release-windows-cross.sh` (`7z` lists `wsh.exe`
and the resources in the installer and `wsh.exe` is identical to the built one), `scripts/linux-container-bundle.sh` (the deb and
the rpm list `/usr/bin/wsh` and the resources, and `wsh` is identical to the built one; the AppImage is a note, as before) and
`scripts/release-windows.ps1` (`wsh.exe` beside `holzwave-app.exe` is the built one; the Linux builds come from `-LinuxWsh`, a
folder made by `sh scripts/wsh-linux.sh` on a Mac or Linux host, and without it the installer has no Linux resources and says so).
The macOS bundle, the GNU-ABI installer and the arm64 deb, rpm and AppImage of the container script were built and inspected on a Mac
with these checks; `scripts/release-windows.ps1` (the MSVC installer) and the amd64 Linux build were not run since `wsh` was added to
them, and no installed package has been run on a machine of the owner yet.

**Preview packages for Windows and Linux, built on this Mac** (owner decision of 2026-10-14; they only build and checksum,
they never publish):

```sh
sh scripts/release-windows-cross.sh <version>      # holzWave_<version>_x64-setup.exe, GNU ABI, unsigned, per user
sh scripts/release-linux-container.sh <version>    # holzWave_<version>_amd64.deb, _x86_64.rpm, _x86_64.AppImage, unsigned
gh release upload v<version> target/release-cross/holzWave_<version>_*     # by hand, to the release macOS created
```

Both leave the files and their `.sha256` files in `target/release-cross/`, need a clean working tree (not necessarily
`main`) and Docker. The Windows installer is a GNU-ABI build (`brew install mingw-w64` once), not the MSVC build of a
Windows host, and has never run on Windows; the Linux packages come from an emulated amd64 container (Ubuntu 24.04) and
have never run on a Linux desktop. Rows W14 and L11 of [test-plan.md](test-plan.md) are their first run; the limits are
in [build-windows.md](build-windows.md) and [build-linux.md](build-linux.md).

