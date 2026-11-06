# holzWave as a Flatpak

> **Status: not submitted to Flathub, never started on a machine.** The Flatpak was built once in a container on arm64 (the
> architecture of the Docker VM of a Mac). Nobody has started it: every desktop is untried (GNOME, KDE Plasma, Xfce,
> Cinnamon, MATE, Sway, Hyprland and i3), the x86_64 build was not run, and Flathub has not seen any of it. Everything below
> about how it behaves is what the code and the files say, not what a run showed. It is a beta. If the Flatpak cannot be
> accepted, it is moved out of 0.0.1 (see [If it does not ship](#if-it-does-not-ship)); the deb, rpm and AppImage of
> [build-linux.md](build-linux.md) do not depend on it.

Everything here was written and checked from a Mac. The rows that need a Linux desktop are in Part 4 (row L10) and Part 12
(rows FPK1 to FPK7) of [test-plan.md](test-plan.md), and their Result cells are empty.

## What it is

A manifest, an AppStream file and a desktop file under `packaging/flatpak/`, named by the application id
`ch.holzcloud.holzwave` (the Tauri identifier), and the code that decides where a pane starts inside a Flatpak. **A pane
starts in the sandbox by default.** The person can switch on `Host shell` in Settings, and then new panes start on the
host through `flatpak-spawn --host`. The Flatpak ships the permission that this needs, which is the reason for most of the
text on this page.

These files are references for the owner to read, not a submission. Flathub does not accept a manifest or a pull request
written by an AI agent, and holzWave is built with AI help (see [flathub-submission.md](flathub-submission.md)): the owner
writes the manifest and the pull request that go to Flathub by hand. There is no signing, no repository and no automatic
update here. The Linux signatures described in [signing.md](signing.md) cover the deb, the rpm and the AppImage only.

| File | What |
|---|---|
| `packaging/flatpak/ch.holzcloud.holzwave.yml` | The manifest: GNOME 51 runtime and SDK (org.freedesktop 26.08), the Rust and Node 26 SDK extensions, Zig 0.17.1 as a build tool, the nine permissions with their reasons in comments, and the build of this repository inside the SDK with no network in any build step |
| `packaging/flatpak/ch.holzcloud.holzwave.metainfo.xml` | AppStream: name, developer `holzcloud.ch`, links, licence `GPL-3.0-only`, branding colours, release notes, and a description that says it is a beta, which permissions it asks for and what is untested. It has no screenshots, because none exist |
| `packaging/flatpak/ch.holzcloud.holzwave.desktop` | The desktop file (`Categories=System;TerminalEmulator;`) |
| `packaging/flatpak/generated/` | Tool output, never edited by hand: the Zig packages of the terminal engine, the Cargo crates and the npm packages as hashed sources, the Ghostty commit, the one cargo command of the build, and the pin of the tool that made them |
| `scripts/build-flatpak.sh` | `--check` (validate the three files), `--container-build` (build in Docker), `--engine-offline`, `--app-cli-free`, no argument (build and install on a Linux machine) |
| `scripts/flatpak-generate-sources.sh` | Regenerates the Cargo and npm source files in a throw-away container, with `--check` |
| `scripts/flatpak-zig-sources.sh` | Regenerates the Ghostty pin and the Zig package list |

**Where your data is.** Inside a Flatpak the app data directory of [config.md](config.md) is `$XDG_DATA_HOME/ch.holzcloud.holzwave/`,
with the XDG variables of the sandbox. Flatpak points them below `~/.var/app/ch.holzcloud.holzwave/`, so the settings, the saved
connections, the history and the file of the `Host shell` choice are there and not in the `~/.local/share` of the host. Secrets
are in neither place: they are in the system keychain. `flatpak uninstall --delete-data ch.holzcloud.holzwave` removes the folder.
That is what Flatpak does for any app; it was not looked at in a running holzWave.

## What the Flatpak does not carry: `wsh`

The deb, the rpm, the AppImage, the macOS app and the Windows installer install the `wsh` command beside the app. The
Flatpak does not: the manifest installs only `holzwave-app`.

- **In a sandbox pane** there is no `wsh`, because nothing installs it. Whether a copy placed in the app's own folder would
  work from a sandbox pane was not tried.
- **In a host pane** a copy installed by the Flatpak would be unreachable anyway, because a shell on the host cannot see
  `/app/bin`. If you want `wsh` on the host of a Flatpak install, put a copy there yourself; the static Linux build that
  the packages of the other formats carry (`sh scripts/wsh-linux.sh` makes it) runs on any Linux host.

## Build and run (the owner's Linux machine)

1. Install `flatpak` and `flatpak-builder` with your package manager, and add Flathub once:
   `flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo`.
2. Install the runtime, the SDK and the two SDK extensions of the manifest (the script checks and prints this command if one is missing):
   `flatpak install --user flathub org.gnome.Platform//51 org.gnome.Sdk//51 org.freedesktop.Sdk.Extension.rust-stable//26.08 org.freedesktop.Sdk.Extension.node26//26.08`
3. `sh scripts/build-flatpak.sh` builds and installs the Flatpak for your user. It writes only below `target/` and into your
   Flatpak installation.
4. `flatpak run ch.holzcloud.holzwave`, then work through row L10 and rows FPK1 to FPK7 of [test-plan.md](test-plan.md).

The first build takes a while (a release build of the whole app plus the Ghostty engine).

**No build step has network access.** flatpak-builder downloads every source the manifest declares before it starts the
build, and each module then builds in a sandbox with no network. Every source is pinned by content: two Zig archives (a
sha256 each), the Ghostty checkout (a git source at one commit), and three generated files with the Zig packages, the Cargo
crates and the npm packages, each with a hash. Nothing in the build step downloads: the container run reads the build log
and prints `flatpak-container-build: offline` only when it finds no download line from cargo or npm in it. That proof rests
on flatpak-builder's default (a module has no network unless it asks for it, and none does) plus the empty log; it was not
shown with a separate probe from inside the sandbox.

**Where the generated files come from.** `scripts/flatpak-zig-sources.sh` and `scripts/flatpak-generate-sources.sh` write
them, from the lock files, in a throw-away Docker container, with the generator tool of the Flatpak project checked out at
the commit that `generated/generator-pin.txt` names and its Python packages installed with their hashes. Change a lock file,
run `sh scripts/flatpak-generate-sources.sh`, and commit the result; `sh scripts/flatpak-generate-sources.sh --check`
regenerates into a scratch folder and compares byte for byte, and tests compare the files with the lock files. Running them
needs the network (that is the tool, not the Flatpak build). Whether Flathub accepts generated files made by a script that
was written with AI help is a question for the maintainers, not settled ([flathub-submission.md](flathub-submission.md)).

**The source of the module.** In the manifest the `holzwave` module reads the working tree (`type: dir`), so that the recipe
builds what is on disk. That is a local recipe. For a submission the owner replaces it with a `git` source of this
repository whose `tag` and `commit` name a release; none is written, because none exists yet.

**The owner writes the Flathub manifest by hand.** The manifest in the repository is a reference that shows what the build
needs. [flathub-submission.md](flathub-submission.md) is the owner's checklist for the submission, and it contains no text
for the pull request.

## What it costs in sandboxing

Every permission of the manifest, what it is for and what it costs. The manifest has a comment above each line that says the
same, and the AppStream description lists them too; a test compares this table with the list in the manifest.

| Permission | What it is for | What it costs |
|---|---|---|
| `--socket=wayland` | Showing the window on a Wayland session | As any graphical app |
| `--socket=fallback-x11` | Showing the window on an X11 session, or when Wayland is not there | As any graphical app |
| `--share=ipc` | Shared memory with the X server (X11 only) | As any graphical app |
| `--share=network` | The SSH connections you start, the sync you set up, the AI chat when you send a message, a model server on this computer | The sandbox no longer limits what the app contacts: Flatpak has no rule per host or port. The app's own rules do (see [What connects, and when](#what-connects-and-when)) |
| `--device=dri` | GPU rendering for the web view | Access to the graphics devices, as any graphical app |
| `--filesystem=home` | `~`, the directories of panes and "reveal in the file manager" mean what they mean on the host, also in a sandbox pane | The app and a sandbox pane can read and write your home folder. The host permission below already allows everything this one does |
| `--talk-name=org.freedesktop.Flatpak` | Running a command on the host (`flatpak-spawn --host`) for a host pane | **Removes the sandbox for this app**, see below |
| `--socket=ssh-auth` | The built-in SSH client signs in with the keys in your SSH agent, which it finds through `SSH_AUTH_SOCK`; the socket forwards the host's agent into the sandbox | The app can ask your agent to sign. The host permission already removes the sandbox, so this adds none |
| `--talk-name=org.freedesktop.secrets` | The keychain: passwords, passphrases and provider keys are kept in the Secret Service on the session bus, never in a file. The Secret portal does not cover this name | The app can talk to your keychain service. The host permission already removes the sandbox, so this adds none |

**`--talk-name=org.freedesktop.Flatpak` removes the sandbox for this app.** It lets the app run any command on the host with
your rights. A terminal cannot be very useful without it: inside the sandbox a shell sees only the runtime's `/bin/sh` and
none of your tools. It is shipped, but it is used only when you ask for it:

- **The setting `Host shell` is off by default.** Panes start in the sandbox. You switch it on in Settings (the group
  exists only inside a Flatpak). With it on, new panes start on the host; panes that are already open keep running where they
  are.
- **The setting is never synced.** The choice is kept in a file of this computer, so another computer does not inherit a
  way out of the sandbox.
- **The setting cannot grant the permission.** The permission is fixed in the manifest when the Flatpak is built, so the
  setting can only use it or say that it is missing. A person can take it away with a Flatpak tool, for example
  `flatpak override --user --no-talk-name=org.freedesktop.Flatpak ch.holzcloud.holzwave` (not run here); the setting then
  reads `not available`, and panes stay in the sandbox.
- **While the permission is present, the sandbox does not protect the rest of your system from this app, whatever the
  setting says.** The default is a behaviour of the app, not a guarantee of the sandbox.
- **The integrations (the Kubernetes, Talos, Azure and GAM7 blocks) do not run in a Flatpak** whatever the setting says, so
  no tool of theirs is started on the host.

The only host command holzWave starts is your login shell (or the program of the `shell` setting). The manifest, the AppStream
description and this page say so. Terminals that are already on Flathub (Konsole, WezTerm, BlackBox, Ptyxis and Contour, read
on 2026-11-05) ask for the same permission; none of them offers it as a setting.

If the reviewers of Flathub do not accept this permission, [flathub-submission.md](flathub-submission.md) lists the
fallbacks, and [If it does not ship](#if-it-does-not-ship) says what moves out of the first release if none works.

## What connects, and when

The app has the network (`--share=network`). holzWave has no telemetry, no account, no update check and no crash report,
in a Flatpak as everywhere else, and `core/tests/project_policy.rs` pins the dependency and source rules behind that.
The permission only lets the connections below happen. Unverified in a Flatpak, like the rest of this page.

**At launch.** On a fresh install nothing connects when the app starts, and nothing connects while it sits idle. Two
things can connect early once you have set them up:

- the sync: its first run is about ten seconds after the start, then it runs every five minutes, to the folder address
  you saved. It does nothing while sync is off;
- a Files or Monitor block on a saved SSH connection that was open when you quit: a restart opens the same views, and a
  block connects when it is drawn.

**Afterwards, only what you start.** What connects, when, and to where:

- **A saved SSH connection:** when you open it as a pane, a Files block or a Monitor block, and again after a lost
  connection, as often as the connection's `reconnect_attempts` says. It goes to the host and port of the connection,
  and of its `jump` host.
- **The sync:** after you set it up (see above) and when you press Sync now. It goes to the address you saved.
- **The AI chat:** only when a message is sent, or when `wsh ai` sends one (after you answer its card). Opening the
  chat, the providers dialog or the picker connects to nothing. It goes to the address of the provider you set up. With
  `Local only` on, the app refuses a provider that is not on this computer, before any connection. Terminal text leaves
  a pane only after you share it.
- **A link you click:** it is handed to the opener of the desktop; that is not a connection of holzWave.
- **The shells in the panes:** a pane in the sandbox uses the network of the app, a pane on the host uses the host's network
  like any terminal. What you run in a pane is not a connection of holzWave.

**Who limits it.** The sandbox no longer does: Flatpak has no rule per host. The app's own rules do (saved addresses
only, `ai_local_only`, consent per pane, the HTTPS rules of the AI client, no socket before a send), and tests of the
app hold them. The Flatpak does not.

**Taking the permission away.**
`flatpak override --user --unshare=network ch.holzcloud.holzwave` removes it: then SSH, the sync, the AI chat and a
model server on this computer cannot connect, and the rest of the app works.
`flatpak override --user --reset ch.holzcloud.holzwave` undoes it.
`flatpak info --show-permissions ch.holzcloud.holzwave` shows what the install has.

## How the shell is started

Inside a Flatpak (`/.flatpak-info` exists or `FLATPAK_ID` is set) a local pane starts in one of two ways, by the setting
`Host shell`. A pane of a saved SSH connection starts no local shell and is not affected.

**Sandbox (the default).** The shell is found as on any Linux: the `shell` setting if it names a program the sandbox can
start, else `$SHELL`, else `/bin/sh`, started the way every Linux pane starts its shell. It runs inside the sandbox, with the runtime's tools and none
of yours. The directory of the pane is read from the process, as on any Linux, so split in the same directory, new tab in
the same directory and the restore of a session work as they do outside a Flatpak. Each new pane shows one notice line before
the shell's own output, so a person who wonders why their tools are missing sees why:

- with the setting off: `holzWave: this pane runs inside the Flatpak sandbox. To use the tools on this computer, turn on Host shell in Settings.`
- with the permission removed: `holzWave: this pane runs inside the Flatpak sandbox. This Flatpak has no permission to run commands on the host; see the Flatpak page of the documentation.`

**Host (`Host shell` on).** A pane runs

```
flatpak-spawn --host --watch-bus [--directory=<dir>] --env=TERM=... [--env=...] -- <program> <arguments>
```

- Without the `shell` setting the program is `sh -c <one constant script>` on the host: it asks `getent passwd` for your
  shell, falls back to `/bin/sh`, and starts it as a login shell (`-l`), so the shell has the login environment of the host.
- With the `shell` setting that program is started (with `-l`). The setting is **not** looked up in the sandbox: a program of
  the host that the sandbox cannot see is not a defect there.
- The command line holds only constants, one validated directory in one `--directory=` argument, the variables a pane
  gets (`TERM`, `COLORTERM`, `TERM_PROGRAM`, `TERM_PROGRAM_VERSION` and, when the app has no locale, `LC_CTYPE`) and, after
  `--`, the program and its arguments as separate arguments. A directory that looks like an option, has spaces or an equals
  sign, stays inside its one argument; a directory with a control character is refused and the pane starts in your home
  folder. `--watch-bus` ends the host process when the app dies.
- The directory of a host pane is read only through OSC 7 (see [Known limits](#known-limits)).
- **Whether a host shell has job control (`Ctrl+C`, `Ctrl+Z`, `fg`) and works with `sudo` is an assumption** until the owner
  runs rows L10 and FPK4 of [test-plan.md](test-plan.md). The shell is started by the Flatpak portal on the host and receives the
  pane's terminal as its input and output; whether it also becomes the terminal's session leader was not verifiable here.

Nothing changes outside a Flatpak: on macOS, Windows and a normal Linux session no pane is wrapped and no `Host shell` group is drawn.

## Known limits

- **The directory of a host pane is tracked only when the host shell announces it (OSC 7).** In a sandbox pane holzWave reads
  the directory of the shell's process, as on any Linux. For a host pane the process in the sandbox is `flatpak-spawn`, whose
  directory means nothing, so it is not looked up. Split in the same directory, new tab in the same directory and the restore
  of a session then work only with a shell that announces its directory (fish does; for bash,
  `PROMPT_COMMAND='printf "\e]7;file://%s%s\a" "$HOSTNAME" "$PWD"'` is the usual line, which does not encode spaces or other
  special characters in a path, so such a directory is not tracked; zsh has an equivalent in a `precmd` function). Unverified in a Flatpak.
- **A start directory that no longer exists on the host** is passed to `flatpak-spawn` as it is; outside a Flatpak the shell would
  start in the home folder instead. What the host does then is not known here.
- **PTY semantics of `flatpak-spawn --host` are an assumption** (see above). If rows L10 and FPK4 show no job control, the
  owner decides between `host-spawn` (a small helper binary that creates the terminal on the host: a new dependency question)
  and leaving the Flatpak out of the first release.
- **The SSH agent.** `--socket=ssh-auth` forwards the host's agent into the sandbox. Whether a connection signs in with an agent
  key from inside the sandbox, and what a person sees when no agent runs, was not tried (row FPK5).
- **The keychain.** The passwords, passphrases and provider keys of SSH, sync and the AI chat live in the system keychain
  (the Secret Service on Linux). The manifest asks for its session bus name (`--talk-name=org.freedesktop.secrets`) because the
  Secret portal does not cover it; that this is enough, and that a keychain prompt appears on each desktop, was not tried
  (row FPK6). If it does not work, saving a password, a passphrase or a key fails, and the app does not fall back to a file.
- **A model server on this computer.** Flatpak describes the permission as access to the network. That it also reaches a
  server on this computer's own loopback address (Ollama, LM Studio, llama.cpp) is expected and not verified; row L10
  checks it.
- WebKitGTK starts its own helper processes; whether it runs the same inside the sandbox (its own sandbox for them) is shown by
  the first run.
- x86_64 and aarch64 for Flathub, which builds both itself. Here only the aarch64 build ran, in a container. The manifest lists
  both Zig archives, and the shared sources carry no architecture limit.
- The generated sources include registry-hosted native npm packages (the TypeScript compiler, esbuild and the canvas
  library ship binaries for every platform; the build unpacks only the one of its own machine). Whether Flathub accepts that is not known.

## What was checked from the Mac, and what only the owner's machine shows

Checked (Docker Desktop, Ubuntu 26.04, arm64):

- `sh scripts/build-flatpak.sh --check`: `flatpak-builder --show-manifest` reads the manifest without warning,
  `appstreamcli validate --no-net` and `desktop-file-validate` pass.
- `sh scripts/build-flatpak.sh --container-build`: **the manifest built** in a privileged Docker container with Zig 0.17.1 and
  no network in any build step (the run printed `flatpak-container-build: offline`). It installed the GNOME 51 runtime, the SDK
  and the two SDK extensions from Flathub, built the release app from 765 hashed crates, 115 npm packages and the Zig packages,
  removed Zig from the result, composed AppStream and exported the files, and `ldd` found every library of the binary in the
  runtime. That build is for **aarch64** (the architecture of the Docker VM on an Apple Silicon Mac), so it used the aarch64 Zig
  archive: the x86_64 archive and checksum of the manifest were not exercised, and the app was not started (no display, and
  no host to spawn on). The container build was not repeated after the permissions of this page were changed; the change
  touches the permission list and comments, not the build.
- The wrapping function and its refusals (`core/tests/platform.rs`), that a pane in host mode starts `flatpak-spawn` with the
  documented line, does not set a process directory and takes its directory from OSC 7 only (`core/src/pane.rs`,
  `core/tests/pane.rs`, `core/tests/flatpak_host.rs` with a stand-in for `flatpak-spawn`), that the setting decides where a
  pane starts, that its file is never synced, and how the permission is read from `/.flatpak-info` (with sample text, not a real one).
- The facts of the three files (`app/src-tauri/tests/flatpak.rs`): the id, the exact list of nine permissions and their
  disclosure in the manifest, the AppStream text and this page, the pinned build sources, no key, no foreign address, and what
  this page says about the network.

Only the owner's machine shows: that the manifest builds there (the x86_64 Zig archive and checksum, the owner's Flatpak
and runtime versions), that the app starts under X11 and Wayland on each desktop, the icon and the name in the menu and on the
task bar, that links and files open through the portals, and everything about the shells: that a pane starts in the sandbox
by default, that the setting turns host panes on, that it reads `not available` without the permission, a host shell's
terminal, `sudo` and job control, the host's tools and home, and that closing a pane leaves no host process of ours. It also
shows that the SSH agent and the keychain work through the sandbox, that nothing connects when the app starts on a fresh
install, that an SSH host and a model server on this computer are reachable, and what happens when the network is taken away.
Those are rows L10 and FPK1 to FPK7.

## If it does not ship

If the build fails on the owner's machine, a row of the Flatpak fails in a way that makes it unusable, or Flathub does not
accept the app in any of the forms of [flathub-submission.md](flathub-submission.md), the owner decides to move the Flatpak
out of 0.0.1. What moves out:

1. The README and [distribution.md](distribution.md) stop offering a Flatpak and say it is not part of 0.0.1. The deb, the rpm
   and the AppImage carry the Linux release.
2. The Flatpak row of the platform differences in [design.md](design.md) (section 7) is rewritten to say that, not deleted, and
   the `Host shell` group stays in the code, because it does nothing outside a Flatpak.
3. The files under `packaging/flatpak/`, the script and this page stay as references with their status lines changed to
   "not part of 0.0.1"; nothing is deleted from the history.

Before that, the fallbacks of the checklist are tried in order: the permission shipped with the setting (this page), no
permission with an override command that the person runs themselves, and the setting only in a build that is not on Flathub.
