# holzWave as a Flatpak

**Status: unverified. Flatpak ships in 0.0.1 only if it builds on the owner's Linux machine and row L10 passes. Otherwise it is moved out of 0.0.1: revert the commits of plan 02.2-08, amend ROADMAP success criterion 2 and DESK-31, and add it to the deviations in `docs/design.md`.**

This is the last and the only conditional part of Phase 2.2. The deb, rpm and AppImage of [build-linux.md](build-linux.md)
do not depend on it. Everything here was written and checked from a Mac; nothing was run on a Linux desktop.

## What it is

A manifest, an AppStream file and a desktop file under `packaging/flatpak/`, named by the application id
`ch.holzcloud.holzwave` (the Tauri identifier), and the code path that makes a terminal inside a Flatpak useful: inside a
Flatpak the pane's shell is started **on the host**, through `flatpak-spawn --host`. It is a local build from this
repository, never a Flathub package; there is no signing, no repository and no automatic update.

| File | What |
|---|---|
| `packaging/flatpak/ch.holzcloud.holzwave.yml` | The manifest: GNOME 51 runtime and SDK (org.freedesktop 26.08), the Rust and Node 26 SDK extensions, Zig 0.16.0 as a build tool, and the build of this repository inside the SDK |
| `packaging/flatpak/ch.holzcloud.holzwave.metainfo.xml` | AppStream: name, licence `GPL-3.0-only`, the release, and a description that says plainly that the app can run commands on the host |
| `packaging/flatpak/ch.holzcloud.holzwave.desktop` | The desktop file (`Categories=System;TerminalEmulator;`) |
| `scripts/build-flatpak.sh` | `--check` (validate the three files), `--container-build` (build in Docker), no argument (build and install on a Linux machine) |

## Build and run (the owner's Linux machine)

1. Install `flatpak` and `flatpak-builder` with your package manager, and add Flathub once:
   `flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo`.
2. Install the runtime, the SDK and the two SDK extensions of the manifest (the script checks and prints this command if one is missing):
   `flatpak install --user flathub org.gnome.Platform//51 org.gnome.Sdk//51 org.freedesktop.Sdk.Extension.rust-stable//26.08 org.freedesktop.Sdk.Extension.node26//26.08`
3. `sh scripts/build-flatpak.sh` builds and installs the Flatpak for your user. It writes only below `target/` and into your
   Flatpak installation. The build downloads: the crates and npm packages of the lock files, the Zig archive (pinned by sha256
   in the manifest) and the Zig packages that the Ghostty engine fetches during its build. That is why it is a local build
   with the network allowed in the build step (`--share=network` as a build argument; the running app has its own
   network permission, see [What it costs in sandboxing](#what-it-costs-in-sandboxing)).
4. `flatpak run ch.holzcloud.holzwave`, then work through row L10 of [test-plan.md](test-plan.md).

The first build takes a while (a release build of the whole app plus the Ghostty engine).

## What it costs in sandboxing

- **`--talk-name=org.freedesktop.Flatpak` removes the sandbox for this app.** It lets the app run any command on the host
  with your rights. A terminal cannot be useful without it: inside the sandbox a shell sees only the runtime's `/bin/sh` and
  none of your tools. The only host command holzWave starts is your login shell (or the program of the `shell` setting).
  The manifest, the AppStream description and this page say so; Flathub submission is not planned, and Flathub would
  not accept this permission without a long justification.
- **`--filesystem=home`** gives the app your home folder, so `~`, the directories of panes and "reveal in the file manager"
  mean what they mean on the host. The sandbox is already open through the permission above, so this costs nothing more.
- **`--share=network`** gives the app the network. Flatpak has no narrower permission (its subsystems are `network` and
  `ipc`, with no rule per host or port), so this is the smallest one. It is the network of the app and not of the
  shells, which run on the host and have the host's network anyway. The SSH connections you start, the sync you set up
  and the AI chat when you send a message need it. Nothing connects when the app starts on a fresh install. The sandbox
  no longer limits what the app contacts; the app's own rules do (saved addresses only, Local only, consent per pane).
  The permission above already allows any host command with your rights, so the app's process gains no capability that
  a host command would not have; Flatpak's documentation adds that network access also reaches host services on
  abstract Unix sockets.
- Wayland, fallback X11, `--device=dri` and `--share=ipc` as every graphical app.

If the owner does not accept the host-command permission, Flatpak is moved out of 0.0.1 (see the status line).

## How the shell is started

Inside a Flatpak (`/.flatpak-info` exists or `FLATPAK_ID` is set) a pane runs

```
flatpak-spawn --host --watch-bus [--directory=<dir>] --env=TERM=... [--env=...] -- <program> <arguments>
```

- Without the `shell` setting the program is `sh -c <one constant script>` on the host: it asks `getent passwd` for your
  shell, falls back to `/bin/sh`, and starts it as a login shell (`-l`), like every Unix pane.
- With the `shell` setting that program is started (with `-l`). The setting is **not** looked up in the sandbox: a program of
  the host that the sandbox cannot see is not a defect there.
- The command line holds only constants, one validated directory in one `--directory=` argument, the variables a pane
  gets (`TERM`, `COLORTERM`, `TERM_PROGRAM`, `TERM_PROGRAM_VERSION` and, when the app has no locale, `LC_CTYPE`) and, after
  `--`, the program and its arguments as separate arguments. A directory that looks like an option, has spaces or an equals
  sign, stays inside its one argument; a directory with a control character is refused and the pane starts in your home
  folder. `--watch-bus` ends the host process when the app dies.
- Nothing changes outside a Flatpak: on macOS, Windows and a normal Linux session no pane is wrapped.

## Known limits

- **The directory of a pane is tracked only when the host shell announces it (OSC 7).** Outside a Flatpak holzWave reads the
  directory of the shell's process; in a Flatpak the process in the sandbox is `flatpak-spawn`, whose directory means
  nothing, so it is not looked up. Split in the same directory, new tab in the same directory and the restore of a session
  then work only with a shell that announces its directory (fish does; for bash, `PROMPT_COMMAND='printf "\e]7;file://%s%s\a" "$HOSTNAME" "$PWD"'`
  is the usual line, which does not encode spaces or other special characters in a path, so such a directory is not
  tracked; zsh has an equivalent in a `precmd` function). Unverified in a Flatpak.
- **A start directory that no longer exists on the host** is passed to `flatpak-spawn` as it is; outside a Flatpak the shell would
  start in the home folder instead. What the host does then is not known here (row L10 note).
- **PTY semantics of `flatpak-spawn --host` are an assumption until row L10 passes.** The shell is started by the Flatpak
  portal on the host and receives the pane's terminal as its input and output; whether it also becomes the terminal's session
  leader, so that job control (`Ctrl+C`, `Ctrl+Z`, `fg`) and `sudo` work, was not verifiable. If L10 shows no job control, the
  owner decides between `host-spawn` (a small helper binary that creates the terminal on the host: a new dependency question)
  and moving Flatpak out of 0.0.1.
- WebKitGTK starts its own helper processes; whether it runs the same inside the sandbox (its own sandbox for them) is shown by
  the first run.
- x86_64 only for 0.0.1. The manifest also lists the aarch64 Zig archive, which lets the same manifest build on an ARM
  machine (the container build on an Apple Silicon Mac uses it); that is not a target.

## What was checked from the Mac, and what only the owner's machine shows

Checked (Docker Desktop, Ubuntu 26.04, arm64):

- `sh scripts/build-flatpak.sh --check`: `flatpak-builder --show-manifest` reads the manifest without warning,
  `appstreamcli validate --no-net` and `desktop-file-validate` pass.
- `sh scripts/build-flatpak.sh --container-build`: **the manifest builds** in a privileged Docker container (GNOME 51 and the
  two SDK extensions installed from Flathub, Zig 0.16.0 and the Ghostty packages downloaded, the release build of the app,
  Zig removed from the result again, AppStream composed, files exported, `ldd` finds every library of the binary in the
  runtime). That build is for **aarch64** (the architecture of the Docker VM on an Apple Silicon Mac), so it used the
  aarch64 Zig archive: the x86_64 archive and checksum of the manifest were not exercised, and the app was not started
  (no display, and no host to spawn on).
- The wrapping function and its refusals (`core/tests/platform.rs`), that a pane in host-spawn mode starts `flatpak-spawn` with the
  documented line, does not set a process directory and takes its directory from OSC 7 only (`core/src/pane.rs`,
  `core/tests/pane.rs`, `core/tests/flatpak_host.rs` with a stand-in for `flatpak-spawn`), and that the flags follow the
  environment (`app/src-tauri/src/state.rs`).
- The facts of the three files (`app/src-tauri/tests/flatpak.rs`): the id, the permission and its disclosure, the pinned Zig archive, no key, no foreign address.

Only the owner's machine shows: that the manifest builds there (the x86_64 Zig archive and checksum, the owner's Flatpak and
runtime versions), that the app starts under X11 and Wayland, the icon and the name in the menu and on the task bar, that links and
files open through the portals, and everything about the host shell: its terminal, `sudo`, job control, the host's tools
and home, and that closing a pane leaves no host process of ours (row L10).

## If it does not ship

If the build fails or row L10 fails and the owner decides to move Flatpak out of 0.0.1:

1. Revert the commits of plan 02.2-08 (the code path, the packaging files and the script with its tests and this page are separate commits so that this is clean; the reverts also remove this page).
2. Amend ROADMAP success criterion 2 of Phase 2.2 from "Flatpak, AppImage und deb/rpm bauen lokal" to "AppImage und deb/rpm bauen lokal; Flatpak ist aus 0.0.1 herausgenommen" and reword DESK-31 the same way (deb, rpm and AppImage in 0.0.1; Flatpak later).
3. Add the Flatpak to the deviations in [design.md](design.md) section 7 as moved out of 0.0.1 (the row about the Flatpak there is then rewritten, not deleted).
