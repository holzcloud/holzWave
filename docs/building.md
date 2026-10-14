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

## Releases

Releases are built on this machine, never in CI. `sh scripts/release-macos.sh <version> --publish --casks` builds the app, packs the DMG, creates the GitHub release (a version with a hyphen is a
prerelease) and writes the casks into `Casks/` of this repository (`holzwave@preview` for every release, `holzwave` for releases
without a hyphen); commit and push them afterwards. The repository is its own Homebrew tap, like holzBar. `.\scripts\release-windows.ps1 -Version <version> -Upload` on Windows builds the
NSIS installer and attaches it to the same release. Both scripts refuse a dirty tree or a `main` that differs from
`origin/main`.

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

