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

Windows, Linux, iOS and Android are planned and not yet built. See [test-plan.md](test-plan.md) for the full test setup.

## Releases

Releases are built on this machine, never in CI. `sh scripts/release-macos.sh <version> --publish --casks` builds the app, packs the DMG, creates the GitHub release (a version with a hyphen is a
prerelease) and writes the casks into `Casks/` of this repository (`holzwave@preview` for every release, `holzwave` for releases
without a hyphen); commit and push them afterwards. The repository is its own Homebrew tap, like holzBar. `.\scripts\release-windows.ps1 -Version <version> -Upload` on Windows builds the
NSIS installer and attaches it to the same release. Both scripts refuse a dirty tree or a `main` that differs from
`origin/main`.

