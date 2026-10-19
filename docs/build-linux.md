# Building holzWave on Linux

For the owner's own Linux machines (a Debian family and a Fedora family distribution, X11 and Wayland). Everything here
is built and run locally: no step uses GitHub to build, creates a tag or a release, or needs a CI file (Phase 2 D-04).
The result is a `.deb`, an `.rpm` and an AppImage; the packages are not signed (signing is Phase 10).

> [!IMPORTANT]
> **Status.** The Linux half of the code and the packaging were run in a Linux container on a Mac (Ubuntu 26.04,
> arm64): the whole core test suite and the app crate's tests pass there, and a deb, an rpm and an AppImage were built
> and the first two inspected. The author has since installed and run the packages on real Linux machines. Rows L1 to
> L11 of [test-plan.md](test-plan.md) (Part 4) are where a package name or a command that differs on a real machine is
> noted. See [What was and was not verified](#what-was-and-was-not-verified).

## Rules of this build

- **Zig is exactly 0.16.0.** The pinned Ghostty commit builds the terminal engine with its own `build.zig`, which needs
  Zig 0.16.0; Zig 0.17 removed an API it uses and fails (Phase 2 decision D-02). A distribution's `zig` package, Snap,
  Homebrew on Linux and `zigup` hand out the newest Zig, so none of them is used: download the exact version from
  ziglang.org as shown below.
- **Everything else is the newest version**: Rust stable (from rustup, not the distribution's older `rustc`), Node,
  npm, git, the system libraries.
- **Build only locally**, on your own machine.
- x86_64 for 0.0.1. The container route below is arm64 on an Apple Silicon Mac, a stand-in for structure and tests; the
  preview packages of the next-but-one section are built in an emulated amd64 container.

## Packages

Names as the package managers know them. Everything marked "checked" was looked up in a container on 2026-10-12
(`apt-cache policy` on Ubuntu 26.04, `dnf repoquery` on Fedora 44); neither is the exact distribution release of your
machine, and no package was installed on a real desktop.

### Debian family (Debian, Ubuntu, Mint, Pop!_OS)

```sh
sudo apt update
sudo apt install build-essential pkg-config git curl ca-certificates xz-utils file \
    libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev
```

All of these names exist on Ubuntu 26.04 (checked). The container built the release app, the deb and the rpm with
this set plus a few packaging tools (`dpkg-dev`, `rpm`, `cpio`, `patchelf`, `squashfs-tools`, `desktop-file-utils`,
`xdg-utils`, which the container needed only to inspect the packages).

The Tauri prerequisites page also lists `wget`, `libssl-dev`, `libxdo-dev` and `libayatana-appindicator3-dev`. The
container build did not need any of them, because holzWave has no tray icon and no system library for TLS. **The first
build on a machine confirms it**: if the build asks for one of them, install it and note it in row L1.

To run an AppImage you need FUSE 2: `libfuse2t64` on Ubuntu 24.04 and newer and on current Debian, `libfuse2` on older
releases (both names checked as far as Ubuntu 26.04 goes). Without it, run the AppImage with
`--appimage-extract-and-run`.

### Fedora family (Fedora)

```sh
sudo dnf install gcc gcc-c++ make pkgconf-pkg-config git curl xz file \
    webkit2gtk4.1-devel gtk3-devel librsvg2-devel openssl-devel
```

Checked on Fedora 44: `webkit2gtk4.1-devel`, `gtk3-devel`, `gcc`, `gcc-c++`, `make`, `pkgconf-pkg-config`, `git`, `curl`,
`xz`, `file`, `librsvg2-devel`, `openssl-devel`, `fuse-libs` (FUSE 2 for the AppImage), `patchelf`, `desktop-file-utils`,
`xdg-utils`, `squashfs-tools`. `openssl-devel` is the Tauri prerequisite list's, not something the Debian container
needed. `dnf group install c-development` brings the compilers too. `wget` is provided by `wget2-wget` there. **Not
verified:** that this set builds holzWave on Fedora: the build itself was only run on Ubuntu.

### Other distributions

Arch, openSUSE and the rest are not covered. The libraries are WebKitGTK **4.1** (the `webkit2gtk-4.1` API, not 4.0)
and GTK 3.

## Tools

### Rust, Node, git

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh     # Rust stable; accept the default profile
```

Open a new terminal afterwards (or `. "$HOME/.cargo/env"`). Node 26 or newer with npm comes from
<https://nodejs.org> (distribution packages are usually older). git comes from the package list above.

### Zig 0.16.0 (exactly)

`scripts/env.sh` looks for Zig in `$HOLZWAVE_ZIG_DIR`, then in `$HOME/.holzwave/zig-0.16.0` (the folder
`scripts/env.ps1` uses on Windows), then in Homebrew's keg-only `zig@0.16`. Put the archive there:

```sh
cd "$(mktemp -d)"
arch=$(uname -m)                         # x86_64 or aarch64
curl --proto '=https' --tlsv1.2 -fLO "https://ziglang.org/download/0.16.0/zig-$arch-linux-0.16.0.tar.xz"
# The checksum comes from ziglang.org's published index. With jq:
curl --proto '=https' --tlsv1.2 -fsSL https://ziglang.org/download/index.json | jq -r ".[\"0.16.0\"][\"$arch-linux\"].shasum"
sha256sum "zig-$arch-linux-0.16.0.tar.xz"      # the two lines must be equal; stop if they are not
mkdir -p "$HOME/.holzwave/zig-0.16.0"
tar -xJf "zig-$arch-linux-0.16.0.tar.xz" -C "$HOME/.holzwave/zig-0.16.0" --strip-components=1
```

For orientation, the index of 2026-10-12 gave `70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00` for
`x86_64-linux` and `ea4b09bfb22ec6f6c6ceac57ab63efb6b46e17ab08d21f69f3a48b38e1534f17` for `aarch64-linux`. The index
is the authority; do not trust a copy of it, including this one, if they differ.

## Steps

From a normal (not root) terminal.

```sh
git clone https://github.com/holzcloud/holzWave
cd holzWave
. scripts/env.sh                      # Rust and Zig on PATH; sourcing prints nothing
zig version                           # must print exactly 0.16.0; if not, fix this before anything else
npm --prefix app ci                   # the Tauri CLI, TypeScript and the browser-test driver; nothing else
npm --prefix app run build            # compiles the page into app/dist (the app build needs it)
cargo test --workspace                # the first run builds the terminal engine with Zig: minutes
npm --prefix app test
cargo run -p holzwave-app             # the app itself
```

`scripts/env.sh` is meant to be sourced (with the dot). It never exits your shell and sets nothing but `PATH`.

Open the window and go through the rows of [test-plan.md](test-plan.md) Part 2 and Part 4 (L1, L2).

### X11 and Wayland

GTK picks the session's own display protocol; `echo "$XDG_SESSION_TYPE"` says which one you are in (`x11` or
`wayland`). Run the same build in both sessions: log out, choose the other session on the login screen (GNOME offers
"GNOME on Xorg" or "GNOME"; KDE offers "Plasma (X11)" or "Plasma (Wayland)", depending on the release).

To test the XWayland path from inside a Wayland session, start it with `GDK_BACKEND=x11 cargo run -p holzwave-app`.
That is a **way to test**, not a default: single-user reports say that forcing X11 breaks input methods for some people,
and holzWave sets no `GDK_BACKEND` itself.

Some things differ under Wayland on purpose (no tear-off by dragging, no window position restore): the list is in
[design.md](design.md) section 7, "Deviations from macOS".

## Packages

```sh
npm --prefix app run tauri -- build --bundles deb,rpm,appimage
```

`tauri.linux.conf.json` already sets the three targets, so the `--bundles` only repeats it. The build runs the page
build itself and compiles the app in release mode (another long build). The packages land in the `target` folder at the
root of the checkout, not in `app`:

| Format | Folder | Name on x86_64 (the container made the arm64 ones) |
|---|---|---|
| deb | `target/release/bundle/deb/` | `holzWave_0.0.1-alpha1_amd64.deb` (arm64: `..._arm64.deb`) |
| rpm | `target/release/bundle/rpm/` | `holzWave-0.0.1-alpha1-1.x86_64.rpm` (arm64: `...aarch64.rpm`) |
| AppImage | `target/release/bundle/appimage/` | `holzWave_0.0.1-alpha1_amd64.AppImage` (the arm64 one was `..._aarch64.AppImage`) |

Look in the folder: the bundler chooses the names. The package's `Depends` (deb) and `Requires` (rpm) are
WebKitGTK 4.1 and GTK 3; nothing else, and no package runs an install script (the container build checked both, and
`app/src-tauri/tests/bundle.rs` pins that no script is configured).

What the bundler downloads at build time: the AppImage tools (linuxdeploy and its plugins). That is a network step of
`tauri build`. If the AppImage step fails with a FUSE message, build again with
`APPIMAGE_EXTRACT_AND_RUN=1 npm --prefix app run tauri -- build --bundles appimage` (that is what the container run did).
Tauri users report that newer distributions can also fail in linuxdeploy's `strip` step; `NO_STRIP=true` in front of the
command is the reported workaround (unverified here).

### Install and remove

```sh
sudo apt install ./target/release/bundle/deb/holzWave_0.0.1-alpha1_amd64.deb      # Debian family; resolves WebKitGTK
sudo dnf install ./target/release/bundle/rpm/holzWave-0.0.1-alpha1-1.x86_64.rpm   # Fedora family
chmod +x ./target/release/bundle/appimage/holzWave_0.0.1-alpha1_amd64.AppImage    # AppImage: nothing is installed
```

The installed program is `holzwave-app`, the menu entry is "holzWave" under System or Terminal, and the desktop file is
`/usr/share/applications/holzWave.desktop`. The package name is not the product name: read it first with
`dpkg-deb -f <file>.deb Package` or `rpm -qp --qf '%{NAME}\n' <file>.rpm`, then remove with `sudo apt remove <name>` or
`sudo dnf remove <name>`. (The deb's name was `holz-wave` in the container.) Settings stay in
`~/.local/share/ch.holzcloud.holzwave/` (or `$XDG_DATA_HOME/ch.holzcloud.holzwave/`); remove it by hand.

Row L7 checks the menu entry, its icon, the taskbar grouping and the Wayland app id after an install. The desktop file
names `StartupWMClass=ch.holzcloud.holzwave`; if the taskbar shows two icons, the class the window really gets differs
and that line is the one-line fix.

## Without a Linux machine: the container route

For people with a Mac (or any machine with Docker) and no Linux at hand:

```sh
sh scripts/linux-container-test.sh all        # core and app crate tests in an Ubuntu container (core | app | all)
sh scripts/linux-container-bundle.sh          # builds the deb and rpm, inspects them, tries the AppImage
```

The packages of the second script are copied to `target/linux-bundle/`. On an Apple Silicon Mac the container is arm64:
a stand-in for the x86_64 structure, the dependencies and the tests, **not** for graphics, input methods or the CPU. The
release build of the engine fetches Zig packages from the network the first time (some from codeberg.org); the bundle
script's header says what to do when that host is down. Both scripts print `ok` as their last line and exit non-zero
otherwise; a missing Docker is a failed gate, not a skip.

## The preview packages built from a Mac (x86_64, emulated)

Until a Linux machine builds the packages itself, the owner builds x86_64 packages **on the Mac**, in an amd64 Docker
container:

```sh
sh scripts/release-linux-container.sh 0.0.1-alpha1     # clean tree; the version of tauri.conf.json
```

The result, in `target/release-cross/` (next to the Windows installer), is `holzWave_<version>_amd64.deb`,
`holzWave_<version>_x86_64.rpm` and `holzWave_<version>_x86_64.AppImage`, each with a `.sha256` file. The script only
builds and inspects; it never publishes. To attach the files to the release that `scripts/release-macos.sh --publish`
created: `gh release upload v<version> target/release-cross/holzWave_<version>_*`.

> [!WARNING]
> **What these packages are not.** They were built in a container whose amd64 code is **emulated** on an Apple Silicon Mac
> (Docker Desktop, Rosetta): the code is real x86_64 code, and the author has run the packages on Linux desktops
> (row L11 of [test-plan.md](test-plan.md)). They are unsigned. They need **glibc 2.39 or newer** and WebKitGTK
> 4.1 (the base image is Ubuntu 24.04): Ubuntu 24.04 or newer, Debian 13 or newer, Fedora 40 or newer and the like; an
> older distribution cannot install them and must build from source as described above.

What the script does, and why (details in `.planning/phases/02.2-windows-linux/02.2-RESEARCH-10.md`):

- It runs `scripts/linux-container-bundle.sh` with `HOLZWAVE_LINUX_ARCH=amd64` and `HOLZWAVE_BUNDLE_APPIMAGE=required`,
  which builds the images `holzwave-linux-test-amd64` and `holzwave-linux-bundle-amd64` (own volumes, own output folder
  `target/linux-bundle-amd64/`; the arm64 ones are untouched), builds the release app, makes the three packages and
  inspects the deb and rpm (dependencies, files, icons, desktop file, no install script).
- **The base is Ubuntu 24.04, not 26.04.** The glibc 2.43 of Ubuntu 26.04 fails to create files under Rosetta (GNU
  `tar`: "Cannot open: Function not implemented"), so the amd64 image cannot even unpack Zig; 24.04 works, and its older
  glibc lets the packages run on more distributions.
- **The AppImage tool is unpacked once.** Rosetta cannot start the downloaded AppImage output plugin of linuxdeploy
  ("Exec format error": its type-2 runtime is a static-pie file), but it runs the plugin's contents. The image unpacks
  the plugin (the same unpinned "continuous" download the Tauri bundler makes) and the build puts a one-line stand-in
  for the downloaded file in the bundler's cache.
- The script then reads each package back: the deb is `amd64`, the rpm `x86_64`, the AppImage an x86-64 ELF file whose
  `AppRun`, program and bundled WebKitGTK are there (the AppImage is opened without being run).

The first run builds the images and compiles everything in the emulated container (minutes for the images and the
compile of the application, measured in the research document); later runs reuse the docker volumes.

## Graphics: a blank, white or flickering window

holzWave shows its page through WebKitGTK. On some setups, most often NVIDIA GPUs, WebKitGTK and the graphics driver
disagree. The symptoms the Tauri documentation names:

- The window opens but stays blank or white.
- The window flickers, especially while resizing.
- The app dies on resize with no useful error output.
- The console shows `AcceleratedSurfaceDMABuf was unable to construct a complete framebuffer`.
- The console shows `Gdk-Message: Error 71 (Protocol error) dispatching to Wayland display.`

Try these **in this order**; the earlier ones keep hardware acceleration. Stop at the first one that helps, and note
which one it was in row L2:

1. **Kernel mode setting.** NVIDIA drivers older than 545 often need `nvidia_drm.modeset=1` as a kernel parameter
   (boot loader configuration; this is a change to your system, so it is yours to make). Check what is set with
   `cat /sys/module/nvidia_drm/parameters/modeset` (`Y` is on).
2. `__NV_DISABLE_EXPLICIT_SYNC=1`. Often fixes the Wayland "Error 71" crash without a performance cost.
3. `WEBKIT_DISABLE_DMABUF_RENDERER=1`. Fixes the DMABUF framebuffer error and the "Error 71" crash, at the cost of the
   faster rendering path.
4. `WEBKIT_DISABLE_COMPOSITING_MODE=1`. The last resort for the silent crash on resize; it disables accelerated
   compositing entirely.

Set one for a single run:

```sh
WEBKIT_DISABLE_DMABUF_RENDERER=1 cargo run -p holzwave-app          # from the checkout
WEBKIT_DISABLE_DMABUF_RENDERER=1 holzwave-app                        # an installed deb or rpm
WEBKIT_DISABLE_DMABUF_RENDERER=1 ./holzWave_0.0.1-alpha1_amd64.AppImage
```

Set one for the menu entry of an installed package: copy `/usr/share/applications/holzWave.desktop` to
`~/.local/share/applications/holzWave.desktop` and change its `Exec=` line to
`Exec=env WEBKIT_DISABLE_DMABUF_RENDERER=1 holzwave-app` (several variables: `Exec=env A=1 B=1 holzwave-app`). Log out and
in, or run `update-desktop-database ~/.local/share/applications`, if the menu does not pick it up.

**holzWave sets none of these by default.** Owner decision of 2026-10-12: no override until a failure is reproduced on
real hardware, because each one disables a faster path for everybody, including people on working setups, and the Tauri
documentation itself says to ship an unconditional override only when the app is verified to be affected. If L2 finds a
setup that needs one, the fix is a follow-up plan (a conditional default, or a line in the desktop file), not a guess
made now.

**WebGL on WebKitGTK.** A WebGL2 context can silently sit on a software rasterizer, and WebKitGTK reports the renderer
as `Apple GPU` on every Linux machine, so the page cannot tell. holzWave falls back to a Canvas 2D renderer when WebGL2
is not available; the status line names the renderer in use. If typing or scrolling feels slow with WebGL2 shown, note
it in row L3 (the speed, the GPU and the driver) rather than trusting the label.

## Flatpak

**Status: unverified.** A Flatpak is the last and conditional part of this phase: it ships with 0.0.1 only if it builds on
your machine and row L10 passes. `sh scripts/build-flatpak.sh` builds and installs it (it needs `flatpak`,
`flatpak-builder`, the GNOME 51 runtime and SDK and two SDK extensions, and the network), and
[flatpak.md](flatpak.md) says what it costs in sandboxing (the app can run commands on the host and use the network),
what is known to differ, and what to do if it does not build. This guide's own build does not make one.

## Reporting a result

Part 4 of [test-plan.md](test-plan.md): write `pass`, or `fail` with a line of what you saw, in the Result column of
each row L1 to L11, and note any step of **this guide** that failed with its exact message (L1 covers the guide from
the top). A failing row becomes a gap plan; DESK-31 is marked done only when the rows pass.

## What was and was not verified

Verified in a Linux container on a Mac (Ubuntu 26.04.1, arm64, rustc 1.99, Zig 0.16.0):

| What | How |
|---|---|
| The core tests and the app crate's tests pass on Linux | `sh scripts/linux-container-test.sh all` |
| `scripts/env.sh` sourced by dash, bash and zsh finds `$HOME/.holzwave/zig-0.16.0` and `$HOLZWAVE_ZIG_DIR`, never exits, and leaves the macOS `PATH` exactly as it was | run in the container and on the Mac |
| The release build, a deb, an rpm and an AppImage; the deb and rpm have the right dependencies, files, icons, categories, keywords and no install script | `sh scripts/linux-container-bundle.sh` |
| The same for x86_64: the release build, deb, rpm and AppImage in an amd64 container (Ubuntu 24.04, emulated); each is read back as x86_64 and the AppImage is opened without being run | `sh scripts/release-linux-container.sh <version>` |
| The Debian family package names exist on Ubuntu 26.04; the Fedora family names exist on Fedora 44 | `apt-cache policy`, `dnf repoquery` in containers |
| The commands that unpack Zig have the shape of the container image's own (checksum from the published index, `tar --strip-components=1`) | the image build of `scripts/linux-container-test.sh` |
| The graphics section's symptoms, variables and their order | the Tauri documentation page "Linux Graphics Issues" (<https://v2.tauri.app/develop/debug/linux-graphics/>), read on 2026-10-12 |

**Not covered by the container checks:** every graphics statement as seen on a screen; X11 and Wayland runs; GNOME and
KDE; installs on a real Debian family and Fedora family machine; any run on real x86_64 hardware; that the AppImage starts;
the `NO_STRIP` and FUSE remedies; `update-desktop-database` and the `Exec=env ...` override. The author has run the
packages on real Linux machines (the rpm and the AppImage on Fedora KDE); the rows of Part 4 are the record.
