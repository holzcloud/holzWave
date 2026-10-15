# Design

Status: **plan**. The foundations (section 1 to 5) are decided. The window direction (section 6) follows the
owner's brief: *the same on all five platforms, plain, terminal-nerd, a little of our colours, and modern.*

holzWave belongs to the holzcloud family: [holzBar](https://github.com/holzcloud/holzBar),
[holzkube-manager](https://github.com/holzcloud/holzkube-manager) and
[holzcloud-cms](https://github.com/holzcloud/holzcloud-cms). The family shares a dark ground, warm light,
Manrope for text, JetBrains Mono for anything a machine wrote, a 4 px grid, and the wooden plank. Each project has its
own accent. holzWave's is **rose on aubergine**.

## 1. Principles

1. **The terminal is the product.** Chrome is quiet; the content has the highest contrast on screen.
2. **Mono means literal.** Anything a machine wrote (output, paths, versions, status keys) is JetBrains Mono. Anything
   addressed to a person is Manrope.
3. **One accent.** Rose marks focus and action. It is never used for decoration next to a status colour.
4. **Warm, not cold.** Greys lean towards the aubergine; there is no neutral grey.
5. **The plank is fixed.** The wooden plank from the logo appears the same everywhere it appears.
6. **Reduced motion is a first-class mode**, and every state has a non-colour cue.

## 2. Colour

Taken from the logo (`assets/brand/holzWave.svg`) and the shared holzcloud tokens (status colours, ink).

| Token | Value | Use |
|---|---|---|
| `--hw-ground` | `#14091A` | window and terminal background |
| `--hw-ground-2` | `#1D0F26` | bars, headers |
| `--hw-pane` | `#1B0E20` | pane surface |
| `--hw-edge` | `#3A2144` | hairlines, dividers |
| `--hw-ink` | `#F6EFE6` | primary text |
| `--hw-ink-2` | `rgb(246 239 230 / .70)` | secondary text |
| `--hw-ink-3` | `#9C8EA0` | tertiary text, disabled |
| `--hw-rose` | `#FF8FB8` | focus, action, cursor, selection base |
| `--hw-rose-deep` | `#C23B72` | fills and large type on light grounds |
| `--hw-rose-wash` | `rgb(255 143 184 / .14)` | selected tab, hover |
| `--hw-paper` | `#FFFFFF` | the page of a PDF; a document keeps its own white on every theme |
| `--hw-cream` | `#FFE6F0` | the logo's light face; light-theme ground tint |
| `--hw-wood` | `#C98B4F` | the plank |
| `--hw-wood-hi` / `-lo` | `#E0A869` / `#9B6534` | plank light edge / grain |
| `--hw-ok` / `-warn` / `-danger` / `-info` | `#8CC271` / `#E8C15C` / `#EC8272` / `#8FB8D8` | status (same as holzcloud) |

**Contrast on `--hw-ground`** (WCAG ratio): ink 16.99, rose 9.11, ok 9.29, warn 11.27, danger 7.38, info 9.24,
ink-3 6.27. **`--hw-rose-deep` is 3.84: it is for fills and large type only, never body text on the dark ground.**
On cream, `rose-deep` is 4.55 and `ground` is 17.49.

### Terminal palette

| | normal | bright |
|---|---|---|
| black | `#1B0E20` | `#4A3652` |
| red | `#EC8272` | `#FFA396` |
| green | `#8CC271` | `#AEDD93` |
| yellow | `#E8C15C` | `#F5D883` |
| blue | `#6F9BD0` | `#93B8E6` |
| magenta | `#FF8FB8` | `#FFB3D1` |
| cyan | `#7FD1C7` | `#A4E6DD` |
| white | `#D9C9D6` | `#F6EFE6` |

Foreground `#F6EFE6`, background `#14091A`, cursor `--hw-rose`, selection `rgb(255 143 184 / .35)`. Every normal
colour meets 4.5:1 on the background except black, which exists for use as a *background* colour.

## 3. Typography

| Role | Face | Size | Weight |
|---|---|---|---|
| Chrome text (tabs, titles) | Manrope | 13 px | 500 / 600 |
| Labels (status bar, section tags) | JetBrains Mono, uppercase, tracked `.1em` | 11 px | 500 |
| Terminal | JetBrains Mono | 14 px (user setting) | 400 / 700 |
| Preview body | Manrope | 14 to 15 px, line height 1.6 | 400 |

Fonts are bundled with the app, never fetched at runtime. The terminal font is a setting; ligatures are off by
default.

## 4. Space, shape, depth

- 4 px grid; steps 4, 8, 12, 16, 24.
- Radius: 8 (controls), 16 (panes in card styles), 24 (overlays), 999 (pills).
- Hairlines are 1 px `--hw-edge`. Shadows only on floating layers (menus, palettes), never between panes.
- Pane padding 6 px around the grid (as built); the grid never sits flush against a pane edge.

## 5. Components

| Component | Rule |
|---|---|
| Window bar | traffic lights (macOS; the native title bar sits above the bar elsewhere, section 7), workspace chips, tabs, version label right-aligned in mono |
| Workspace chip / tab | text only; selected = rose wash and rose text, with an underline for non-colour cue |
| Pane header | state dot, title, context (path, host) in mono, right-aligned |
| Focus | 1 px rose ring plus a header cue; only shown when more than one pane exists |
| Divider | 1 px hairline, 8 px hit area, rose on hover, `col-resize` / `row-resize` |
| Selection | rose at 35 % over the cell background, keeps glyph contrast |
| Cursor | block, rose; hollow when the pane is unfocused; bar and underline per DECSCUSR |
| Status bar | mono labels, uppercase: version, renderer, pane count, workspace |
| Overlays | command palette, theme picker, connection manager: 24 px radius, the only elements with a shadow |
| Empty state | the logo's plank with one line of text; no illustrations |

**Motion:** 160 ms for hover and focus, 240 ms for layout changes, easing `cubic-bezier(.2,.7,.3,1)` (holzcloud).
No motion in the terminal grid itself. `prefers-reduced-motion` removes all transitions.

**Accessibility:** every colour state has a second cue (underline, dot shape, label). The focus ring is 2 px for
keyboard focus. Chrome text is at least 12 px. All chrome is operable from the keyboard.

**Accessibility preferences (DESK-24).** Motion, contrast and cursor blink are each `system` or an explicit choice in
the settings; an explicit choice beats the OS, in both directions, and a change of the OS preference is followed
while the app runs.

| Preference | OS signal the web view reports | Effect |
|---|---|---|
| Motion | `prefers-reduced-motion` | no chrome transitions (root class `motion-reduce`); the terminal grid never moves |
| Contrast | `prefers-contrast: more` and `forced-colors` | hairlines use `--hw-ink-3`, dim text uses `--hw-ink-2`, the focus line is 2 px, the shown status segment gets a 2 px outline (root class `contrast-high`); under forced colours the chrome uses system colours |
| Cursor blink | none of its own: it follows reduced motion | a cursor the program asked to blink does so with a 530 ms phase, unless the setting is `off`, or `system` with reduced motion; the phase restarts on output and on every key |
| UI scale | **none on macOS**; Windows: the Text size setting; Linux: GNOME's `text-scaling-factor` (read by the shell, plan 02.2-09) | the user's `ui_scale` (75 to 200 %) times the OS factor, for the chrome only, and the font size actions; see below |

The web view reports no OS text-size preference on macOS, so there the scalable UI font is the user's own setting
rather than an OS signal; the requirement DESK-24 says so in its wording, and nothing in the page pretends otherwise.
On Windows and on GNOME the shell reads the OS text size (the `TextScaleFactor` registry value through `reg.exe`, and
`text-scaling-factor` through `gsettings`), at start and when the window gains focus, and the page multiplies the user's
`ui_scale` by it for the chrome only (tabs, bar, status line, dialogs; 75 to 300 % in all). The terminal's own text
stays at `font_size`. Every other desktop, and every source that cannot be read, counts as 100 %. That this really
follows the OS on a real Windows or GNOME desktop is test-plan rows W12 and L5; nothing here claims it has been seen.

**Text on the accent fill.** `--hw-rose-deep` is a fill, and the text on it is `--hw-on-accent`: white or black,
whichever measures the higher WCAG ratio against the theme's own `rose_deep` (5.05:1 on all three built-in themes,
at least 4.58:1 on any colour). It replaces `--hw-ink`, which measured 3.84:1 to 4.42:1 there.

**Key bindings (DESK-25).** The core resolves the table (defaults, overrides, conflicts) and the page dispatches
shortcuts through the rows it sends; the editor records a chord with the core's canonical spelling and shows the
core's refusals. `Cmd+C` is a fixed shortcut: the core has no rebindable action for it.

## 6. Window direction: Terminal-modern

The owner's brief ruled out the glass, plank and paper looks: they depend on effects (blur, texture) that differ per
platform and do not scale to a phone. The direction is a refinement of candidate 3, *Haarlinie*, with the modern
parts kept.

![Desktop](design/6-desktop.png)
![Phone](design/7-phone.png)

The interactive source of every image in this section is [design/haarlinie-voll.html](design/haarlinie-voll.html): one
page with a view picker (daily, palette, new block, search, copy mode, connections, settings, shortcuts, previews, sync,
AI chat, drag) and a platform switch. The PNGs are rendered from it.

**Rules**

1. **One look everywhere.** The window chrome is drawn by holzWave, with no native title bar or tab control, so
   macOS, Windows, Linux, iOS and Android show the same thing. Only the OS window buttons and the system keyboard
   differ.
2. **Mono everywhere in the chrome.** Labels, tabs and the status line are JetBrains Mono; Manrope only appears in
   previews. Labels are uppercase and tracked, as in the holzcloud family.
3. **Flat and hairline.** No shadows between panes, no gradients, no blur. Panes are separated by a 1 px
   `--hw-edge` line. Only floating layers (command palette) have a shadow and a 8 px radius.
4. **Rose is rationed.** It is state, never decoration: it says where the user is and what the pointer is about to
   act on. It appears for the prompt chevron, the cursor, text selection, the pane focus line (the focused pane's top
   line), the active tab marker (wash, underline and the workspace diamond), the active status segment,
   divider hover, button hover wash (bar, dialog and search buttons), the selected palette row (wash and left bar),
   the focus ring of the palette and search inputs, the link underline (a link under the pointer with the modifier
   held), hint labels, the key binding capture row, and the accent colour of settings checkboxes and radios.
   Everything else is warm grey.
5. **tmux-style status line.** A bottom strip of segments (`workspace`, panes, renderer, encoding, version); the
   active workspace is a solid rose segment.
6. **Numbered tabs.** Tabs carry their number (`1 shell`) so `⌘1…9` is discoverable.
7. **Command palette first.** Actions are reachable by name with a fuzzy palette (`⌘K`); the chrome stays small.
8. **Responsive by layout, not by style.** On a phone the same `View` shows one pane per screen, the tab strip
   moves to the bottom, and an accessory row (`esc tab ctrl alt ← ↑ ↓ →`) sits above the system keyboard. Colours,
   type and spacing tokens are identical.

**Size tokens for touch:** minimum target 44 px on mobile and 28 px on desktop; the accessory row keys are 44 px high.

### 6.1 The window: three levels

holzWave has three nested levels, and each one has its own shape and place so that they cannot be confused.

| Level | Shape | Place | Holds |
|---|---|---|---|
| **Workspace** | round pill with a diamond (`◆` active, `◇` others), tab count as a small number; the active one is a solid `--hw-rose-deep` fill with `--hw-on-accent` text | title bar, left to right | its own tabs and layout |
| **Tab** | square, numbered (`1 shell`), underline and rose wash when shown, a close mark on hover | its own row under the title bar | one split tree of panes |
| **Pane** | tile with a 24 px header: state dot, type icon, title, context chip, context right-aligned, `⊞ ⤢ ✕` | the area under the tab row | one block: shell, SSH, preview, editor, monitor |

Anything that is not one of these three is **chrome around them**: the right title-bar group (sync, AI, sidebar, palette),
the sidebar and the status line.

![Shortcuts](design/shortcuts.png)

### 6.2 Anatomy

| Area | Content |
|---|---|
| Title bar (32 px) | window buttons (left on macOS, right on Windows and Linux), workspace pills, `＋`; right group: sync state `☁ synced`, `✦ AI`, `▥ Sidebar` toggle, `⌘K` |
| Tab row (28 px) | numbered tabs, a `⌁` mark on tabs with a remote connection, `＋`; a hint line right-aligned |
| Area | the split tree of the shown tab, 1 px hairlines between panes, no gaps; the focused pane has the rose line on its header |
| Sidebar (216 px, collapsible) | **Hosts** (state dot, protocol, reconnect counter), **Context** (Kubernetes context and namespace, Azure subscription, GAM profile), **History** (fuzzy search), **Widgets** |
| Status line (24 px) | tmux-style segments: workspace (solid), tabs and panes, Kubernetes context, Azure subscription, git branch, SSH count, sync state, renderer, encoding, version |

The sidebar and the AI chat share one column: opening the chat replaces the sidebar. Every segment of the status line
and every title-bar button opens the matching popover or dialog.

### 6.3 Views

Each view is a state of the same window. Overlays use the 24 px radius and the shadow; everything else stays flat.

| View | Image | What it shows |
|---|---|---|
| Daily | ![](design/6-desktop.png) | local shell, SSH pane that is reconnecting (`⟳ 2/5`) with a Kubernetes chip, SFTP editor, Markdown preview, system monitor, sidebar |
| Palette (`Cmd+K`) | ![](design/palette.png) | fuzzy list in groups (actions, connect, switch) with the chords on the right |
| New block | ![](design/new-block.png) | block picker with roadmap phases: shell, SSH, WSL, Docker, previews, editor, Kubernetes, Talos, Azure, GAM7, monitor, history |
| Search | ![](design/search.png) | scrollback search bar in the pane, hits marked, the current hit solid |
| Copy mode | ![](design/copy-mode.png) | keyboard selection, hint letters on hashes and paths, a solid status line with the keys |
| Connections | ![](design/connections.png) | list and form: host, user, key from the OS keychain, jump host, reconnect policy, startup command |
| Settings | ![](design/settings.png) | themes (built-in and custom), font, UI scale, accessibility as `System / On / Off` |
| Shortcuts | ![](design/shortcuts.png) | the key binding editor, see 6.4 |
| Previews | ![](design/previews.png) | file browser (local and SFTP) with image, PDF and video panes, see 6.5 |
| Sync | ![](design/sync.png) | Nextcloud endpoint, last sync, end-to-end encryption, conflicts with their backup |
| AI chat | ![](design/ai-chat.png) | replaces the sidebar; terminal content is sent only after a per-pane consent |
| Drag | ![](design/drag.png) | drop zones on the outer quarter of a pane, a tab drop zone, tear-off into a new window |

### 6.4 Shortcuts view

The editor lists every action grouped as in the palette (pane, tab, workspace, view and tools, line editing). A row is
the action's name, its action id in mono, and the chord of the **running platform**; a row without a chord shows `–`.
Selecting a row starts the capture (`Press a key …`); a chord that is taken shows the conflict with the action that owns
it and offers *Replace* or *Cancel*. The footer names the fixed shortcuts. The tables in [config.md](config.md) are the
defaults; the editor never shows another platform's keys, because custom bindings do not travel between platforms.

### 6.5 Previews

Built: the file browser (local and SFTP), Markdown, text with line numbers, images, video and audio. Not built yet: the PDF
pane and the editor (the picture shows them as designed). A file opens in the preview beside its browser; a second file
reuses that preview.

The system monitor is a pane too (`System monitor` in the palette; header `system`, context `local · 1 s`): CPU and memory as
eight-segment bars with their numbers beside them (the level is also said by the number turning amber, or red and
underlined, above 70 and 90 percent), the network rates, the uptime and the eight busiest processes (PID, name, CPU,
memory), one sample a second, paused while the window is hidden.

A preview is a pane like any other and uses the same header: image (size and weight as a chip), PDF (page and zoom),
video (a bar with time, volume and speed), Markdown (Manrope, the one place in the chrome that uses it) and the editor
(line numbers, syntax colours from the `--hw-*` tokens). Unsaved changes show as `● unsaved` in the preview's status line (amber, with the words as the second cue), and closing the pane asks first. `Cmd/Ctrl+S` saves, `Escape` gives the keyboard back to the terminal; on a Markdown page `e` opens its source. The file browser is a pane too: `Space` on a file opens its preview beside it,
and the path chip switches between local and an SSH host.

### 6.6 Mobile

On iOS and Android a pane fills the screen. Tabs and workspaces move into a sheet from the bottom: workspace pills on
top, the tabs of the active workspace as a list. The key row (`esc tab ctrl alt ↑ ↓`) sits above the system keyboard and
the bottom bar holds the tabs of the shown workspace. Tokens are the ones above; touch targets are at least 44 px.

![Phone](design/7-phone.png)

### 6.7 Platforms

The same window is drawn everywhere; only the OS window buttons move (traffic lights on the left on macOS, flat glyphs
on the right on Windows, round buttons on the right on GNOME). Shortcut labels follow the platform table in section 7.

| macOS | Windows | Linux |
|---|---|---|
| ![](design/platforms/macos.png) | ![](design/platforms/windows.png) | ![](design/platforms/linux.png) |

### 6.8 Connection states

A pane that runs a connection (SSH, WSL, Docker) keeps the pane header of every pane and says its state in the
context slot. The state is a shape and words, never colour alone:

| State | Dot | Header context | Status line |
|---|---|---|---|
| connected | as any pane (filled when focused, hollow otherwise) | the connection's name | `ssh N` |
| connecting | dotted ring | `Name · connecting` | `ssh N · M down` |
| reconnecting | dotted ring | `Name · ⟳ 2/5 in 3 s` | `ssh N · M down` |
| failed | hollow with a dash | `Name · failed, Enter retries` | `ssh N · M down` |

While a connection is down the pane screen says why on a line of its own, in the warn colour, and what is typed is kept
(up to 64 KiB) and sent after the connection is back. A failure that retrying cannot cure (a refused login, a changed host
key) leaves the pane open with the reason; Enter tries again. Every connection has an entry in the palette
(`Connect: Name (ssh)`) and in the connection manager (`Connections…`), which is the Connections view of 6.3.

### Candidates considered

The five earlier candidates stay as the record of the alternatives: [Glas](design/1-glas.png),
[Planke](design/2-planke.png), [Haarlinie](design/3-haarlinie.png), [Welle](design/4-welle.png),
[Papier](design/5-papier.png). The cream *Papier* look remains a candidate for a **light theme** later, because the
terminal palette and the tokens are shared by every chrome style.

## 7. Where Windows and Linux differ from macOS (Phase 2.2)

Every behaviour that differs from macOS on purpose is in this one table (ROADMAP success criterion 3). The look is the
same on every platform except where a row says otherwise. Windows and Linux were written and cross-checked from a Mac
and have **not** been run on their own platform: a row here says what was built, and the owner's run of
[test-plan.md](test-plan.md) Part 4 decides whether it works.

| Deviation | Reason | Changed by |
|---|---|---|
| **Native window frame** on Windows and Linux: the system's title bar with its buttons, and the page's bar directly below it. macOS keeps the overlay bar with the traffic lights | `titleBarStyle` and `hiddenTitle` are macOS-only options in Tauri, and a page-drawn bar needs drag, resize edges and window buttons that have a history of problems under Wayland; owner decision of 2026-10-12. Plan 02.2-06 pins it in a test | a page-drawn bar: Phase 10 |
| **`cmd.exe` does not report its directory**, so a split, a new tab and a restored pane start in the home directory | `cmd.exe` emits no OSC 7 and its `PROMPT` cannot percent-encode a path; the engine reads only OSC 7 (plans 02.2-04, 02.2-05) | none planned; PowerShell is the default shell when present |
| **PowerShell is wrapped by an injected prompt** (`-NoLogo -NoExit -EncodedCommand`) that announces the directory, so a split and a restore work on the default shell. There is no setting to switch it off in this phase. `$?` is not preserved for the original prompt, and the wrapper runs after the profile (assumed until W7) | the only way to get a directory out of PowerShell; a constant script, no user text in it (plan 02.2-04) | an off switch only if W7 shows a conflict with a prompt framework |
| **Kitty keyboard flags requested by a Windows program may not reach the engine** through ConPTY | ConPTY filters sequences in its non-passthrough mode and the pinned engine has no win32-input-mode; the Kitty path stays for macOS, Linux and SSH sessions (assumption A11, plan 02.2-04) | a sideloaded ConPTY with passthrough, if the run shows it matters |
| **No middle-click paste (X11 primary selection)** | the web view's clipboard API does not provide the primary selection | none planned |
| **Under Wayland there is no tear-off by the pointer and no window position restore** | Wayland gives a client no global coordinates; a pane or tab dragged out of the window is expected to cancel (row L3), the palette actions `Move pane to new window` and `Move tab to new window` work, and the compositor places every window (plan 02.2-03). Under XWayland (`GDK_BACKEND=x11`) the pointer numbers are expected to be usable: a way to test, not a default | none planned |
| **The UI text size follows the OS on Windows and GNOME only**; macOS, KDE and every other desktop use `ui_scale` alone | macOS has no such signal in its web view; the other desktops have no source this phase reads (plan 02.2-09, described above in section 5) | other desktops: not planned |
| **Fonts**: JetBrains Mono is bundled and first everywhere, so the cell size is the same; a glyph it lacks falls back through the OS: SF Mono and Menlo, then Cascadia Mono and Consolas, then DejaVu Sans Mono and Noto Sans Mono. **Emoji need a colour emoji font on Linux** (for example `fonts-noto-color-emoji`), else they are boxes | the fallback is the OS's (DirectWrite, fontconfig); nothing is bundled for it (plan 02.2-06) | a bundled emoji font: not planned |
| **Key tables are per platform** and keep plain `Ctrl+letter` for the shell: `Ctrl+Shift` chords, tab numbers on `Alt+Shift+digit`, no use of the Super key, no default for the six line-editing actions on Windows. `keybindings` overrides do not travel between platforms | `Ctrl+Alt` is AltGr on Windows, and the shell already binds the line-editing keys (plan 02.2-02; [config.md](config.md)) | settings sync (Phase 4) must translate them |
| **The clipboard shortcuts are fixed**: `Ctrl+Shift+C` and `Ctrl+Insert` copy, `Ctrl+Shift+V` and `Shift+Insert` paste, and `Ctrl+V` also pastes on Windows, as in Windows Terminal, so a program that wants that key itself (vim's visual block) does not receive it. Plain `Ctrl+C` always reaches the program. None of them is rebindable in this phase | there is no native Edit menu off macOS (plan 02.2-02) | rebinding: not planned |
| **The Option row of the settings dialog exists only on macOS**; `option_as_alt` has no meaning elsewhere. AltGr is typed as the character it makes and never as a chord | Option is a macOS key; Windows reports AltGr as `Ctrl` and `Alt` together (plan 02.2-02, plan 02.2-06) | none |
| **Reveal opens the containing folder on Linux** (`xdg-open`), where macOS and Windows select the file; links and files go to the platform's own opener (`open`, `rundll32 url.dll,FileProtocolHandler` and `explorer /select,`, `xdg-open`). A `\\server\share`, device or quoted path is refused on Windows before any file-system call | `xdg-open` has no select; a network path would contact the host (plan 02.2-03) | none |
| **The installers and packages are unsigned.** The Windows installer is NSIS only and per user; there is no MSI. SmartScreen warns on the first run | signing is Phase 10; WiX needs an optional Windows feature and cannot carry a pre-release version (plan 02.2-07; [build-windows.md](build-windows.md)) | signing: Phase 10 |
| **The AppImage's portability and the Flatpak's status are what the owner's run says.** The AppImage was built in a container and never started; the Flatpak is the last plan of the phase and ships only if it builds | no Linux desktop was available (plans 02.2-06, 02.2-08; [build-linux.md](build-linux.md)) | the owner's run of rows L6 and L10 |
| **In the Flatpak the shell runs on the host and its panes track the directory only when the shell announces it (OSC 7).** The app asks for `--talk-name=org.freedesktop.Flatpak`, which removes the sandbox for it; the process in the sandbox is `flatpak-spawn`, whose own directory means nothing, so it is not looked up. Whether the host shell gets a controlling terminal (job control, `sudo`) is an assumption until row L10. **Unverified; if the Flatpak does not ship it is not part of 0.0.1** | a terminal in a sandbox sees none of the user's tools (plan 02.2-08; [flatpak.md](flatpak.md)) | `host-spawn` if L10 shows no job control; or moving Flatpak out of 0.0.1 |
| **WebKitGTK may need a graphics workaround** (a blank or flickering window, mostly on NVIDIA). holzWave sets none by default | an unconditional override disables the fast path for everybody; owner decision of 2026-10-12: none until a failure is reproduced on hardware ([build-linux.md](build-linux.md)) | a follow-up plan if row L2 finds one |

### 6.9 Sync

The title bar's right group holds one button for the sync, a word with a cloud: `☁ synced` (green), `☁ syncing…` (blue),
`☁ sync error` (red, underlined) and `☁ sync off` (dim). The word is the cue, the colour the second one. A click opens a
popover under it (a floating layer, 24 rem wide), and so do the palette entries `Sync…` and `Sync now`.

- **Set up** (no account yet): a short sentence on what is kept and that it is encrypted here, four rows (folder address,
  user, app password, passphrase), a note that the passphrase cannot be recovered, and `Connect`. The secret fields are
  never filled in again. A refusal is said in the status line of the popover, in words.
- **Connected:** title `Nextcloud` with a chip (`connected`, `syncing`, `error`), the address and user, then the rows
  `Last sync` (`14:31 · 3 files`), `Encryption` (`end-to-end · passphrase`) and `Conflicts` (`none`, or the count and that
  the other version is kept), the list of what is synced, a line when changes from another computer wait for the next
  start, and the buttons `Close`, `Turn off` (asks with a second click) and `Sync now`.

### 6.10 History

![](design/history.png)

`Cmd+Shift+H` (`Ctrl+Shift+H`) or `Search the history` in the palette opens a floating dialog built like the palette: a
field, the matching commands below it, a foot with the keys. Matches are fuzzy; the matched letters are rose **and
underlined**. Beside a command: its directory (home as `~`), how long ago, `×n` when it was run more than once, and
`exit n` in words when it failed. `Enter` puts the command on the prompt of the focused pane and closes; it never
runs it. `Shift+Delete` forgets the chosen command, `Clear all` (asks with a second click) forgets everything, `Escape`
closes. The sidebar's History section (^R) comes with the sidebar and uses the same search.
