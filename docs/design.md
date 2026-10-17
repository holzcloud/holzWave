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
| Chat text | Manrope | 13 px, line height 1.55 (code and fenced blocks stay mono) | 400 |

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
| Chat pane | a pane of its own kind (6.12): header `chat`, a bar with the provider chip, a transcript, a status line and a composer; flat and hairline, the only shadow is the provider sheet on a phone |
| AI providers dialog | the box of the Settings dialog (6.13): groups under `.label` legends, a list of providers as a radio group, a form with a write-only key field, a disclosure card for a gateway or a server of the user's own; on a phone it is the screen |
| Context card | a flat strip in the chat pane (6.14): a 2 px bar on its left, native checkboxes, one solid action, hairlines above and below, no shadow; it names the pane, the destination and every item that would go out |
| Approval card | a flat strip entry in the chat pane (6.15): a 2 px bar on its left, mono payload rows, one solid action, hairlines above and below, no shadow; it shows every field of a tool call, where the result goes and what untrusted text is in the turn |
| Chat list | a flat list inside the sidebar column (6.18): one row per stored chat with a mark (`●` on screen, `○`), a title and a mono line with the model and the age, the chat on screen with the rose wash and a 2 px rose bar, a delete button that asks twice (`×`, then `Delete?`); hairlines above and below, scrolls inside, no shadow |
| Reply action | a hairline mono button under a one-line code block of a finished reply (6.19): its word is its name, it puts the line at the prompt of the pane the chat acts for and never runs it; a block with characters that are not shown gets a warn line instead; no shadow |
| Diff box | a hairline box in mono inside an approval card (6.16): a head line with the kind, the counts and the sizes, then the changed lines with their line numbers and a `+` or `-` sign as the cue; no fill, wraps and scrolls inside, at most 16 rem high |

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
AI chat, AI chat states, AI sidebar, drag) and a platform switch. The PNGs are rendered from it.

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
| Sidebar (216 px, collapsible) | **Hosts** (state dot, protocol, reconnect counter), **Context** (Kubernetes context and namespace, Azure subscription, GAM profile), **History** (fuzzy search), **Widgets**. The column has a second form, the AI chat (280 px, 6.18), that takes its place |
| Status line (24 px) | tmux-style segments: workspace (solid), tabs and panes, Kubernetes context, Azure subscription, git branch, SSH count, sync state, renderer, encoding, version |

The AI chat is a pane (6.12). Its second form, in the sidebar's column, replaces the sidebar while it is shown (6.18): the
palette entry `AI chat in the sidebar` and `✦ AI` (when the tab has no chat pane) turn the shown column into the chat, and
`◂ Sidebar` turns it back. Every segment of the status line and every title-bar button opens the matching popover or
dialog.

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
| AI chat | ![](design/ai-chat-pane.png) ![](design/ai-chat-states.png) | the chat as a pane beside the shell (6.12): the bar with the provider chip and the place word, Markdown replies, a reply that streams with `Stop`; then the picker, a stopped reply, a failed turn and a long draft |
| AI chat: sharing context | ![](design/ai-consent-ask.png) ![](design/ai-consent-offer.png) | the consent card and the items that go out (6.14): the question before a pane is shared, the destination and its place, the findings of the redactor, the total and the best-effort sentence |
| AI chat: tool calls | ![](design/ai-approval.png) ![](design/ai-approval-states.png) | the approval card, the taint banner and the rows of tool results (6.15): every field of a request with its size, where the result goes, the untrusted text in the turn, `Allow once` and `Deny`; then a remote target, a request that can only be denied and the rows of a denied, a stopped and a finished call |
| AI chat: file changes | ![](design/ai-write.png) ![](design/ai-write-states.png) | the diff in the approval card, the Undo button and the states of a change (6.16): the kind, the counts and the sizes, the lines around each change with a `+` or `-` sign, the digest of the old text as the last field and the sentence that only the outcome is sent; then the rows of writes that ran, one that was taken back, one refused as changed and one whose undo was refused, and the card of a new file on a remote host |
| AI chat: running a command | ![](design/ai-command.png) ![](design/ai-command-states.png) | the command card, the end of the command next to the buttons, and the states of a finished command (6.17): the whole command in a payload box with no height limit, the banner with the sentence that a command will run, then the rows of a command that ran, one that failed, one that timed out and one that was denied, each with its report as text, and the card of a command on a remote host while it runs |
| AI providers | ![](design/ai-providers.png) | the dialog for providers, keys, the two privacy settings and the gateway disclosure (6.13) |
| AI chat (sidebar) | ![](design/ai-chat.png) | the column form (6.18), which replaces the sidebar: the head with `◂ Sidebar`, the title button and `＋ New chat`, the chat of 6.12 below it with the consent card of 6.14 |
| AI chat: conversations | ![](design/ai-sidebar-list.png) | the list of stored chats under the head (6.18): the chat on screen with the rose wash and a 2 px bar, the model and age of every row, a chat in a pane with no `×`, a delete that was armed (`Delete?`), the core's note about its folder and the sentence under the head |
| AI chat: approval in the column | ![](design/ai-sidebar-cards.png) | the approval card of 6.15 in the strip of the column (6.18): the fields stacked with the size at the right of the name, `✦ AI · 1` in the title bar, the place chip, the hash and the two buttons |
| AI chat: palette actions | ![](design/ai-actions.png) | the palette with the AI entries and the chord of the running platform (6.19): `Explain error` (`⇧⌘Y`) and `Command from description` (`⇧⌘G`) beside `AI chat`, `New AI chat`, `AI chat in the sidebar` and `AI providers…`, each with the section word `ai` |
| AI chat: explain an error | ![](design/ai-explain.png) | the chat after `Explain error` (6.19): the failed command in the pane at the left, the message written in the composer, the context card open on one item (the last command and its output, read), and the status line `Nothing is sent until you press Send.` |
| AI chat: command from a description | ![](design/ai-describe.png) | a reply to `Command from description` (6.19): the button `Insert at prompt` under a one-line block, the warn line under a block that holds characters that are not shown, the line at the prompt of pane 1 with the cursor after it, and the status line `Inserted at the prompt of pane 1. It was not run.` |
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

A chat pane on a phone is the screen, with a sheet from the bottom edge for the provider (6.12); the context card (6.14) and the approval card (6.15) sit inside the screen with the same 44 px targets, and on a phone the payload box of the approval card scrolls inside it. The diff box of a write card (6.16) is part of the card with the same 44 px targets, wraps its rows and scrolls inside its own height, and the `Undo` button of a finished write is 44 px too. On a phone the command card (6.17) grows and the strip scrolls, the command wraps and the buttons keep their 44 px targets. The providers dialog (6.13) on a phone is the screen too, and the sheet rules of the chat apply to its targets and its text. The AI form of the sidebar column (6.18) is the screen on a phone like the sidebar is, with the same 44 px targets and composer text of at least 16 px (the phone image is in 6.18).

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

### 6.11 Sidebar and widgets

`Cmd+B` (`Ctrl+Shift+B`), the title bar button `▥ Sidebar` or the palette shows a column 18 rem wide (216 px at the base
size) at the right of the stage, with a hairline on its left; the stage gets narrower and the grid is measured again. It is
hidden at first and remembers whether it was shown. Sections, each under a `.label` and separated by hairlines:

- **Hosts:** the saved connections. A state dot with a word: connected (filled), reconnecting (dotted ring, `⟳ 2/5`), failed
  (square, `failed`), not running (empty ring, the kind). A click connects.
- **History:** a search field and the eight best matches (the same search as 6.10). A click or `Enter` puts the command on the
  prompt of the focused pane and never runs it; `Escape` gives the keyboard back to the terminal.
- **Widgets:** panels from data, never code. `note` (text, saved while typing), `timer` (Pomodoro: `25:00`, a bar, the word
  `working`, `break`, `paused` or `ready`, Start, Pause, Reset), `list` (labelled commands, which go to the prompt, and
  links, which the system opens). Two exist from the start (Notes, Pomodoro); more are `widgets/<id>.json` files
  ([config.md](config.md)), and a file that is not valid is left out with its reason shown in the sidebar.

The **Context** section (Kubernetes, Azure, GAM) is drawn with the integrations, not before.

The column has a second form, the AI chat (6.18). Only one form shows at a time; `Cmd+B` and `▥ Sidebar` show and hide the column in whichever form it is in.

### 6.12 AI chat

The chat is a pane of its own kind, like the file browser and the monitor: it fills a tile below a pane header, it is
opened next to the focused pane, and it is closed like any pane. Nothing in the chat is a floating layer except the
provider sheet on a narrow screen.

**Opening.** The title-bar button `✦ AI`, the palette entries `AI chat` and `New AI chat`, and the chord of `ai.open`
(`Cmd+Shift+A` on macOS, `Ctrl+Shift+A` on Windows and Linux) open a chat. `✦ AI` and `ai.open` follow one order: a chat
whose tool call waits for a decision and is not on screen is shown first (6.15); else the chat pane of the shown tab is
focused; else a shown sidebar column becomes the AI chat (6.18); else the focused pane is split to the right with a new
chat. `ai.new_chat` (no default chord) always starts a new conversation in a new pane. The button is pressed (rose wash and
rose border, as `▥ Sidebar`, with `aria-pressed`) while a chat is on screen, in a pane of the shown tab or in the column.
While calls wait for a decision the button shows the number, `✦ AI · 2` (6.15).
The palette entries `Explain error` and `Command from description` open the chat in the same order, write a message into
its composer and never send it (6.19).

**Header.** Type icon `✦`, title `chat`, and right-aligned in mono the chat's title (the first line of the first message,
at most 60 characters) or `new chat` before there is one.

**Parts.** Top to bottom, separated by hairlines: the bar, the provider picker (only while open), the transcript, the
status line and the composer.

- **Bar.** On the left the provider chip in mono, the profile name and the model (`Local · llama3.2`), then `→` and the
  host the request goes to. After it the place word as a small chip: `on device`, `this computer`, `local network` or
  `cloud`; `local network` and `cloud` are in the warn colour, and the word is the cue. The place word is shown only once
  a turn has told it, from the core's route, and is never guessed from a name, a preset or an address. At the right end
  the tokens of the last turn (`in 1.2k · out 340`). With one saved profile the chip is plain text; with two or more it
  is a button (`aria-haspopup`, `aria-expanded`); with none it reads `no provider set up` and is not a button. After the
  usage comes a button, `Providers…`, whose text is `Set up…` while the list of providers is known to be empty; it opens
  the dialog of 6.13. While a provider waits for its review (6.13) the text carries the count: `Providers… (1 to review)`.
  While `Local only` is on (6.13) the chip `local only` follows the place word: a button in the ok colour with a hairline
  that opens the dialog; it is not drawn while the switch is off.
- **Picker.** A radio group under the bar, flat and hairline (a bottom sheet at 640 px and narrower). One row per
  profile: `●` or `○`, the name, the model and the host. The chosen row has the rose wash and a 2 px rose bar on its left
  and `aria-checked`. The arrows move and choose; `Enter`, `Space` and `Escape` close it. With two or more profiles and
  no pin or choice nothing is sent: the first send opens the picker and says `Choose a provider first.`
- **Transcript.** `role="log"`, not live, focusable, and a text field of the page (it keeps its own keys). Each message
  has a label row (`YOU`, `ASSISTANT`, in the `.label` style) and its text in Manrope 13 px, line height 1.55. A user
  message has a 2 px hairline bar on its left. The assistant text is Markdown once the message is finished and plain
  text while it streams. A message that did not complete says `stopped`, `failed` or `interrupted` after the role (warn
  colour, underlined). While a reply streams the label says `ASSISTANT · thinking` (only reasoning so far) or
  `ASSISTANT · writing`. Reasoning text is not shown. A user message that carried context ends with one quiet line in
  mono, `Context sent: Screen of pane 1 (2.4 KiB) → this computer`, from the labels, sizes and place of the message
  itself, also after a reload; it never shows text. An assistant message ends with one row per tool call it made (6.15); the row of a write that
  ran carries an `Undo` button (6.16). A message of the role tool draws nothing of its own.
- **End row.** A turn that failed or was cut by a limit has one row with the mark `!`, the class in words and the core's
  one-line text (`! FAILED · overload  The provider is overloaded.`); a limit reads `! STOPPED · step limit`. The danger
  colour and the words are two cues.
- **Card strip.** Between the transcript and the status line, only while a card of 6.15 or the card of 6.14 is open; an
  approval card sits above the context card.
- **Status line.** One line, `role="status"`: `working`, `thinking`, `writing`, `waiting for your decision`, `stopping`, a refused send in the core's
  words, a hint of the page (dim, for example that sharing ended because the provider changed), and after a turn
  `reply finished`, `reply stopped` or `reply failed`. It clears when the user types.
- **Composer.** A multi-line field (1 to 8 rows, in the chat text face) and a `Send` button (solid `--hw-rose-deep` with `--hw-on-accent` text)
  that becomes `Stop` (hairline) while a turn runs. Below the field, left: the button `Context…` (6.14; it reads
  `Context · 1 item` while an offer is attached), then `≈ 12 tokens` (UTF-8 bytes divided by 3, rounded up, the rule of
  the core) and the keys `⏎ send · ⇧⏎ new line · Esc stop`; right: the button. Over 131072 bytes
  the count reads `too long · 131074 of 131072 bytes` (warn, underlined) and `Send` is disabled.

![AI chat pane](design/ai-chat-pane.png)
![AI chat states](design/ai-chat-states.png)

**States and their second cue.**

| State | Colour | Second cue |
|---|---|---|
| streaming reply | none | label `ASSISTANT · writing`, status `writing`, button `Stop` |
| stopped reply | warn | label word `stopped` (underlined), status `reply stopped` |
| failed turn | danger | end row with `!` and `FAILED · class`, status `reply failed` |
| cut by a limit | danger | end row with `!` and `STOPPED · step limit` |
| request leaves the computer | warn | the place word `local network` or `cloud` |
| draft too long | warn | `too long · N of 131072 bytes` underlined, `Send` disabled |
| chosen provider | rose wash | `●` instead of `○`, 2 px bar, `aria-checked` |

**Markdown.** The preview's allow-list. `code` and fenced blocks are mono. No image is ever loaded or shown. A link opens
in the system browser by a click only and shows its address as its tooltip. A link whose address carries a query string or
a user name is not a link: it is text followed by the address in brackets.
A one-line code block of a finished reply has the button `Insert at prompt` under it (6.19).

**Keys and focus.** Opening a chat focuses the composer, and so does the pane gaining the focus. `Enter` sends (not while
an input method composes), `Shift+Enter` makes a new line, and `Escape` stops a running reply and otherwise gives the
keyboard back to the terminal. A press of `Escape` in the message box stops the reply, and so ends every call that waits
for a decision; an approval card never takes the keyboard (6.15). The transcript can be focused; the arrows scroll it and
`Cmd+C` copies the selected text. `Escape` inside the context card closes the card and does nothing else. Closing the pane stops a running reply. Every
message names the pane the chat acts for (6.14), with context or without.

**Narrow screens (640 px and narrower).** The pane is the screen. `Send`, the chip, the picker rows and the composer are at
least 44 px high, and the composer text is at least 16 px. The picker is a sheet from the bottom edge (24 px radius on
top, a hairline) and carries the only shadow of the chat.

![AI chat on a phone](design/ai-chat-phone.png)

**Accessibility.** The chip's `aria-label` says where the request goes. The picker is a `radiogroup`. Nothing moves, so
`motion-reduce` has nothing to remove. Every hairline is `--hw-line`, so `contrast-high` holds. Under forced colours the
chosen row and the chip get a system-colour outline.

### 6.13 AI providers

The dialog in which the user adds the providers the chat talks to, keeps their keys and sets the two privacy switches.
It looks and keys like the Settings dialog and does nothing else: it contacts no server, lists no models and scans for
nothing.

**Ways in.** The bar button `Providers…` of the chat (the text is `Set up…` while the list of providers is known to be
empty), the palette entry `AI providers…` (it has no chord) and the button `AI providers` of the Settings dialog, beside
`Key bindings`. One dialog at a time; a second ask while it is open changes nothing. When it closes the keyboard goes back
to where it came from: the composer of the chat, else the terminal.

**Box.** The box of the Settings dialog (a floating layer: hairline, 8 px radius, the one shadow), 40 rem wide, scrolling
inside. From the top: the title `AI providers`; the status line (`role="status"`, one line: `Saved.`, `Removed.`,
`Allowed.`, `Saving…`, or a refusal in words in the warn colour); the notes of the core about its provider file, only
when it sent any; three groups separated by hairlines, each a fieldset under a `.label` legend; then the button `Close`.
While it is open it takes the keys: nothing reaches the terminal, `Tab` stays inside, the arrows move inside a radio
group, `Enter` in a text field saves and `Escape` closes.

**Privacy.** A checkbox `Local only` with the hint "Refuse every request whose destination is not this computer. A server
on your local network counts as not this computer." and a radio group `Models on this computer` with `Ask first` and
`Do not ask`, hint "Whether a model on this computer needs your consent before it sees terminal text. A provider that is
not on this computer always asks." Both controls show only what the core answered: a refused value goes back to the last
good one, and a change made by hand in the settings file shows without a request.

**Providers.** A radio group with one row per provider: a native radio, the name, `Type · model` dim, the host in mono and
the key word. The row of the edited provider has the rose wash and a 2 px rose bar on its left. A row whose address awaits
review also says `review` in the warn colour with a `!` before it. `Add provider` sits below the list; with none, the
list says "No provider yet."

**Provider.** The legend is `Provider`, or `New provider` while adding. In this order:

- `Name`.
- `Type`, a radio group of the eleven types in the order of the core. The type of a saved provider cannot change, and the
  line says so: "The type of a saved provider cannot change. Add a new provider for another type."
- `API shape`, a radio group shown only for a type that speaks more than one (Vercel two, Cloudflare four, Custom five).
- `Address`, a text field. The hint reads "Leave empty to use the address of this type." or, for a type with no default,
  "Required: the address of your gateway or server." The core's refusal appears under the field in the danger colour,
  underlined and with a `!` mark; the field is `aria-invalid`. The page checks nothing itself: the address goes as typed.
- A checkbox `Plain http on my local network` ("Only for a server on your own network. Never for the internet.").
- `Model`, a text field. "Type the model name the provider uses. holzWave does not ask the provider for a list."
- `API key`, a password field that is never filled in. Beside it the key word, below it the advice for that state, and
  `Remove key` when a key state other than `no key` exists. A saved key is replaced by entering a new one.
- `Gateway token`, the same, only for the Cloudflare type.
- The disclosure card, when the core asks for one (below).
- The buttons `Save` (solid `--hw-rose-deep`, `--hw-on-accent` text) and `Remove provider` (hairline); the first click on
  `Remove provider` turns it into `Click again to remove`, and any other action disarms it.

![AI providers](design/ai-providers.png)

**Keys.** A key is typed once and goes to the keychain in one call; it is never shown, echoed, kept or put into a
provider. The field has no show toggle. The state of a key is one word, with the colour as the second cue:

| State | Word | Colour and cue | Advice |
|---|---|---|---|
| none saved | `no key` | warn when the type needs a key, else plain | |
| saved | `key saved` | ok | The key is stored in the system keychain. It is never shown again; enter a new one to replace it. |
| missing | `key missing` | warn, underlined | The keychain has no key for this provider. Enter it again. |
| other address | `key for another address` | warn, underlined | The address changed after the key was saved, so the key is not used. Enter the key again. |
| damaged | `key damaged` | warn, underlined | The saved key record is damaged. Enter the key again. |
| unreadable | `keychain unreadable` | warn, underlined | The keychain could not be read. Try again later. |
| no keychain | `no keychain` | warn, underlined | This device has no system keychain, so a key cannot be saved. A server that needs no key still works. |

**Disclosure card.** Shown only when the core's view of a provider carries a disclosure. A hairline box with a 2 px warn
bar on its left, the mark `!`, a title (`Gateway: {host}` or `Your own server: {host}`), the body and the button
`Allow {host}` (hairline). Gateway: "A gateway relays your prompts to other providers and may keep a log of them.
holzWave sends nothing to {host} until you allow it." Own server: "holzWave cannot tell who runs {host} or what it keeps.
It sees every prompt you send to it. holzWave sends nothing until you allow it." For the OpenRouter type one more
sentence: "holzWave asks OpenRouter not to use providers that keep your data. OpenRouter can still log what it receives."
`Save` never allows anything: only the button `Allow {host}` does, once per address, with the saved fields and not the
unsaved edits. A changed host asks again. The chat does not send with a provider that waits for this review; it opens
the dialog on that provider and keeps the draft.

**Nothing is probed.** No test connection, no model list, no scan for local servers; saving contacts nothing. A place
word never appears in this dialog: where a request goes is told by the chat after a turn.

**States and their second cue.**

| State | Colour | Second cue |
|---|---|---|
| edited provider | rose wash | `●` radio, 2 px bar on the left |
| review pending | warn | the word `review` underlined with a `!` before it, the card in the form |
| refused field | danger | words under the field, underlined, `!` mark, `aria-invalid` |
| key needs attention | warn | the key word, underlined |
| armed removal | danger border | the text `Click again to remove` |

**Narrow screens (640 px and narrower).** The dialog is the screen: full width and height, no radius, no shadow. The
fields stack, and the rows, options, inputs and buttons are at least 44 px high; input text is at least 16 px. The rules
of the chat sheet (6.12) apply.

![AI providers on a phone](design/ai-providers-phone.png)

**Accessibility.** Every control has a visible label; a field with a refusal is `aria-invalid` and `aria-describedby` its
error; the status line is `role="status"`; the list and the option groups are radio groups. Nothing moves, so
`motion-reduce` has nothing to remove. Every hairline is `--hw-line`, so `contrast-high` holds. Under forced colours the
chosen row and the card get a system-colour outline.

### 6.14 Sharing context

The card in which the user shares a pane with the chat, sees what would go out and sends it. Terminal and file text
reaches a provider only through this card: the user presses `Context…`, shares the pane, ticks what to attach, sees every
item as a label, a size and a list of what was hidden, and sends. The page never holds the text; it sends ids and the core
sends the text it approved.

**The pane.** The card is about the pane the chat acts for: the last pane the user focused that shows a terminal or a
preview (a chat, a file browser or a monitor does not count). Its number and name are in the card title (`PANE 1 · shell`,
the name cut at 40 characters). With none, `Context…` is `aria-disabled` and a press says `Focus a terminal or preview
pane first.` in the status line. When the user focuses another such pane the card closes and an attached offer is dropped
(the first pane stays shared). Every message the chat sends names this pane, with context or without.

**Opening.** `Context…` is a hairline mono button at the left of the composer foot, the size of `Providers…`: 28 px high
on desktop, 44 px at 640 px and narrower, with `aria-expanded` and `aria-controls` the card. A press opens the card and
only then is the core asked (it looks up a host name when it classifies a destination), so a mount, a focus or a redraw
asks nothing. While an offer is attached the button reads `Context · 1 item` (`Context · 2 items`, `Context · none` when
every item is unticked), so what will go out is visible next to `Send`. The palette action `Explain error` (6.19) opens the card for the pane the chat acts for with
one item chosen and previewed and shares nothing by itself.

**Modes.** Each mode has words, so colour is never the only cue.

| Mode | Title | Bar | Second cue and content |
|---|---|---|---|
| `ask` | `Consent needed` | warn | the mark `!`; "Terminal content is only sent when you share it." (for a preview pane: "The file this pane shows is only sent when you share it."), the question "Share pane 1 with Local · llama3.2? It goes to localhost:11434 (this computer).", "You still see every item before a message is sent."; buttons `Share pane 1` (solid) and `No` |
| `pick` | `Shared · pane 1 · shell`, or `Pane 1 · shell` when asking is off | hairline | one checkbox per kind the core lists (`Selection`, `Visible screen`, `Last command and its output`, `File as saved`, `Preview as saved`), "Nothing is attached yet.", buttons `Preview` (solid, disabled until a kind is ticked), `Stop sharing` (only when shared) and `Close`; with asking off the line "Asking is off for models on this computer. You still see every item." |
| `offer` | `Goes out with your next message` | hairline | the destination at the right (host in mono and the place chip), one row per item with a ticked checkbox, the label, the size and under it the findings in words; "Nothing there: Selection." for kinds that read empty; the total, the persistence line, the validity line, the best-effort sentence; buttons `Read again` and `Discard` |
| `blocked` | `Cannot share` | danger | the mark `!`, the reason in words (underlined), `Providers…` for `local_only` and `destination`, `Close` |
| `busy` | `Asking…` or `Reading…` | hairline | `aria-busy`, no buttons |

![Sharing a pane](design/ai-consent-ask.png)
![What goes out](design/ai-consent-offer.png)

**Card.** A flat strip between the transcript and the status line, a hairline above and below, a 2 px bar on its left
(warn while consent is needed, danger when it cannot go on, hairline otherwise), a `.label` title, body text like the chat
text (Manrope 13 px), labels and numbers in mono, no shadow, at most half the height of the pane and scrolling inside.

**What the user sees.** Every item is a label, a size and findings, never its text: an excerpt would need a way to read the
text, and the page has none. The last command of a pane that has just started can be the shell's start-up text, and
nothing on the card can tell; untick it when in doubt. The last command appears in the pick list only when the engine can
bound command output; the card offers exactly the kinds the core lists. The redaction report is shown per item, next to
the sentence "Best effort, not a guarantee: holzWave hides the secrets it recognises and cannot promise that none are
left.", and no text of the page says that secrets are never sent. The same content cannot be sent twice in one chat; the
page says so when the core refuses it.

**Words of the findings.** One line per rule the redactor reports, in the core's order; a rule the page has no words for reads `other change`, so nothing is dropped. `characters` rules show their count in brackets, `times` rules show `×N` from two, and the cut shows no count.

| Rule | Words | Count | Tone |
|---|---|---|---|
| `hidden_text` | hidden text removed | chars | warn |
| `escape_sequence` | terminal escape sequences removed | times | plain |
| `control_chars` | control characters removed | chars | plain |
| `bidi_control` | text direction controls removed | chars | warn |
| `zero_width` | invisible characters removed | chars | warn |
| `tag_chars` | hidden tag characters removed | chars | warn |
| `private_key` | private key hidden | times | plain |
| `anthropic_key` | Anthropic key hidden | times | plain |
| `openai_key` | OpenAI key hidden | times | plain |
| `google_api_key` | Google API key hidden | times | plain |
| `github_token` | GitHub token hidden | times | plain |
| `aws_access_key` | AWS access key hidden | times | plain |
| `slack_token` | Slack token hidden | times | plain |
| `jwt` | JSON web token hidden | times | plain |
| `authorization` | authorization header hidden | times | plain |
| `bearer_token` | bearer token hidden | times | plain |
| `cookie` | cookie hidden | times | plain |
| `url_credentials` | password in an address hidden | times | plain |
| `secret_assignment` | secret assignment hidden | times | plain |
| `high_entropy` | random-looking string hidden | times | plain |
| `marker_escaped` | look-alike markers defused | times | plain |
| `truncated` | cut to fit the size limit | none | plain |

**Words of the refusals.** One sentence for each of the thirteen reasons of the core; the page holds them as a table that a golden file written by the core's tests keeps equal to the core's names. A reason the page does not know shows the core's own text. The same content sent twice in one chat reads: "This content was already sent in this chat. Change it, or ask again after it changed."

| Reason | Sentence |
|---|---|
| `not_shared` | This pane is not shared with the chat yet. Share it first. |
| `stale_offer` | The attached context is no longer valid. Read it again. |
| `bad_pick` | The chosen items do not match what was read. Read the context again. |
| `bad_kinds` | That choice of content cannot be read. Tick one to five kinds. |
| `unsupported_kind` | This pane cannot offer that kind of content. |
| `nothing_to_share` | There is nothing in this pane that can be shared. |
| `pane_gone` | The pane is closed or no longer shows what was shared. |
| `denied` | This place may not be shared. |
| `not_text` | This file is not text, so it cannot be shared. |
| `unreadable` | The pane or file could not be read. Try again. |
| `busy` | The pane did not answer within two seconds. Try again. |
| `destination` | The address of this provider cannot be checked or is not allowed. Nothing was sent. |
| `local_only` | Local only is on, and this provider is not on this computer. |

**Places.** The place of a destination is one of the core's four names, written `on device`, `this computer`, `local network` or `cloud`, and `unknown place` for anything else; `local network` and `cloud` are in the warn colour and underlined.

**After a send.** The offer is spent and the card closes; the sent message ends with the quiet line `Context sent:` and the
labels, sizes and place. The share stays in the core, so the next `Context…` opens the pick list at once.

**When sharing ends.** The page drops what it knows of the share, and any attached offer, when the provider, the model or
the saved address changes (the picker, or the providers dialog), when the shared pane or the chat closes, with `Stop
sharing`, and when a privacy setting changes. The status line says so (`Sharing ended because the provider changed. Share
again to attach context.`) and the next `Context…` asks the core again. An offer lives five minutes; an older one is never
sent and the status line says `The attached context expired. Add it again.` The page never reads the pane again by itself.

**Local only.** While the switch is on, the place chip of the destination in the card and the chip `local only` in the chat
bar say so; a destination that leaves the computer is refused and the card says why, with `Providers…`.

**Keys and focus.** Opening moves the focus to the first control of the card and closing returns it to `Context…`. `Escape`
inside the card closes it and is not passed on; `Space` ticks; `Enter` in the composer sends with the attached offer; every
control carries `data-field`, so a typed character reaches no terminal.

**Narrow screens (640 px and narrower).** `Context…`, every button, every checkbox row and every item row are at least
44 px high, the card is at most 60 % of the viewport height and scrolls inside, and the composer text stays at least 16 px.

![Sharing context on a phone](design/ai-consent-phone.png)

**Accessibility.** The card is a `group` named `Share context`, each checkbox has its visible label, the mode word is text,
the status line is the live region, every hairline is `--hw-line` so `contrast-high` holds, and nothing moves. Under forced
colours the card and the place chip get a system-colour outline.

### 6.15 Approving a tool call

The card in which the user sees the exact request of a tool call and allows it once or denies it, and the rows in which
the transcript shows what a tool call did. The core enforces the rule: a call cannot run without a decision that repeats
the hash of the card, and the card is made from the same list of fields as the hash. The page only shows that list and
relays a person's press. It does not summarise, shorten, reorder, merge, translate or classify a field, it keeps nothing
for later, and it sends the chat, the call, the verdict and the hash of the card, never a path, a command or a text.

**Place.** A flat strip entry in the card strip (6.12), between the transcript and the status line and above the context
card of 6.14. One card per waiting call, in the order the assistant asked, a hairline above and below, a 2 px bar on its
left (warn while a decision is needed, danger when the call can only be denied, hairline while the verdict travels and
after it). Body text is in the chat text face (Manrope 13 px); field names, values, sizes and the hash are in mono. No
shadow. The strip is at most half the height of the pane and scrolls inside.

**Head.** A `.label` title `Approval needed` with the mark `!`, then the words of the tool (`Read a file`, `List a
folder`, `Search files`, `Read a pane`, `Write a file`, `Edit a file`, `Run a command`), and at the right, in mono, the pane and the target label of the core (`pane 1 ·
This computer`). When the target is a remote host the chip `remote host` in the warn colour follows; the word is the cue.
Under the head one line says what the effect is: `Reads only. Nothing is changed.` or, for a write, `Changes one file. Check the changes below.` or, for a command, `Runs one command in a separate process with your account's rights. Check the command below.` A write card shows the diff box of 6.16 between this line and the payload.

**Payload.** A hairline box in mono with one row for every field the core sent, in the core's order: the name (dim), the
value (ink, wrapped, never cut) and the size (dim, at the right, `27 B`). The box is at most 16 rem high, scrolls inside
and takes the focus with `Tab`, so it can be scrolled by keyboard. A command card is the one place where the payload box has no height limit and no scroll of its own (6.17): the card grows and the strip scrolls, so the end of the command is never hidden. Control, bidirectional, zero-width and tag characters
are written as escapes (`\u{202e}`); nothing is dropped, and a long value scrolls and is never cut in the middle.

**What cannot be shown.** A request with more than 16 rows, a value over 393216 characters, more than 524288 characters
in all, a tool or an effect this page does not know, or a field of a wrong shape is not shown as a card that can be
allowed: it has the danger bar, the title `Cannot show in full` or `Cannot show this request`, a sentence that says why,
and only `Deny`. A card without a usable hash has no button and says that stopping the reply ends it.

**Destination.** A line under the payload: `The result is sent to llama3.2 at localhost:11434 (this computer).` (for a command: `The command's output is sent to llama3.2 at localhost:11434 (this computer).`) The place
is a chip, in the warn colour and underlined when the place leaves the computer, and the place word is the core's (6.12).
When the route is not known, for example after a reload, the line reads `The result is sent to the model of this chat.` (for a command: `The command's output is sent to the model of this chat.`)

**Banner.** When the turn holds text the user did not write, a block with a 2 px warn bar, the mark `!` and the label
`Untrusted text in this turn` says so, with the core's labels (`output of list_dir`, `attached context`) in brackets and
the sentence that this text can try to steer the assistant. The banner of a tainted command adds `A command will run. Read all of it before you allow it.` The banner follows the core's taint flag or its list of
labels, whichever says so. It does not claim that anything is safe.

**Hash and buttons.** The line `hash 9f3a41c2d7be` (the first 12 characters, dim, mono), then `Allow once` (solid
`--hw-rose-deep`, `--hw-on-accent` text) and `Deny` (hairline), in this order in the document, then the foot `Allows this
one call only. Nothing is kept for later. Esc denies.` There is no `Always allow`, no allow for the session and no setting
that could carry one.

**Modes.** Each mode has words, so colour is never the only cue.

| Mode | Bar | Second cue and content |
|---|---|---|
| `ask` | warn | the mark `!`, both buttons |
| `sending` | hairline | title unchanged, buttons disabled, `aria-busy`, the line `Sending…` |
| `sent` | hairline | the line `Allowed · running…` or `Denied`, no buttons, until the core's result removes the card |
| `ended` | hairline | the line `That call is no longer waiting.`, no buttons |
| `cannot` | danger | the mark `!`, the title `Cannot show in full` or `Cannot show this request`, only `Deny` |

A refusal that keeps the card puts the core's text above the buttons in the error tone (danger, underlined). A changed
request says `The request changed. Nothing ran. Read the card again.` and the card is drawn again from the core's state.

**Keys and focus.**

| Key or press | Effect |
|---|---|
| a card appears | nothing: it never takes the focus; the status line says `waiting for your decision` |
| `Tab` | reaches the diff box of a write card (6.16), the payload box, `Allow once` and `Deny` in this order; from the message box `Shift+Tab` reaches `Deny` first |
| `Space`, `Enter` on a button | decides |
| `Escape` with the focus inside a card | denies that call and returns the focus to the message box |
| `Escape` in the message box | stops the reply, which ends every waiting call |
| a press on `Allow once` within half a second of the card appearing | ignored (a click aimed at something that moved cannot allow a call); `Deny` always works |
| a click or key press made by a script | ignored |

No key allows a call without the focus on its button.

**Tool rows.** Under the assistant text that asked for it, one row per tool call: a mark (`✓` ran, `!` error, `…`
waiting, `–` no result), the words of the tool, the core's one-line summary in mono and, at the right, the state word
(`ran`, `error`, `waiting`, `no result`); the word is the cue. A toggle `Show result (1.2 KiB)` opens the result in a mono
box under the label `Result · untrusted text`; it is text only (no Markdown, no link, no image), at most 20000
characters, and a longer result ends with `[cut for display]`. Line feeds and tabs of the result stay, so a result of
several lines (the report of a command, the lines of a file) is drawn in lines; every other hidden character is written as an
escape. A message of the role tool draws nothing of its own.

**Waiting.** While calls wait, the status line of the chat says `waiting for your decision` and the title-bar button reads
`✦ AI · 2` (the number in the warn colour; the number is the cue) with the accessible name `AI chat, 2 calls wait for your
decision`.

**Words.** Every sentence of the card is fixed, except what the core says: the page holds the words of the tools, of the
effect and of the states as tables, and the sentences below as constants. The names of the refusals, the tools, the effects
and the two verdicts are held to the core by a golden file that the core's tests write; a name the core adds or renames fails
there and in the page's tests in the same change.

| Tool | Words |
|---|---|
| `read_file` | Read a file |
| `list_dir` | List a folder |
| `grep` | Search files |
| `read_pane` | Read a pane |
| `write_file` | Write a file |
| `edit_file` | Edit a file |
| `run_command` | Run a command |

| Effect | Words |
|---|---|
| `read` | Reads only. Nothing is changed. |
| `write` | Changes one file. Check the changes below. |
| `exec` | Runs one command in a separate process with your account's rights. Check the command below. |

| Refusal of a decision | The card |
|---|---|
| `unknown_call`, `not_pending`, `already_used`, `cancelled`, `duplicate_call`, `too_many`, `not_approved` | `That call is no longer waiting.` (a quiet line, no buttons, the same note in the status line) and one request for the state of the chat |
| `hash_mismatch`, `payload_changed`, `taint_changed`, `scope_mismatch` | stays, with `The request changed. Nothing ran. Read the card again.`, and one request for the state of the chat |
| any other reason, an unknown one, `invalid_decision` | stays, with the core's own text above the buttons; an answer that is not the core's refusal reads `The decision could not be sent.` |

A refusal is never retried: the next press is a new decision of the person.

| Sentence | Where |
|---|---|
| Approval needed | title of a card that waits |
| Cannot show in full | title of a request over the caps |
| Cannot show this request | title of a request that cannot be shown for any other reason |
| Allow once, Deny | the two buttons |
| Allows this one call only. Nothing is kept for later. Esc denies. | foot of a card that waits |
| Sending… | while the verdict travels |
| Allowed · running…, Denied | after the verdict, until the core's result removes the card |
| That call is no longer waiting. | an ended card |
| remote host | chip of a remote target |
| Untrusted text in this turn | title of the banner |
| This turn contains text you did not write (output of list_dir). It can try to steer the assistant. Check the target and every field before you allow. | the banner; the brackets name the core's labels and are left out when there are none |
| The result is sent to llama3.2 at localhost:11434 (this computer). | the destination; the host and the place are left out when the route does not tell them |
| The result is sent to the model of this chat. | the destination when the route is not known |
| This request is too large to show in full. It can only be denied. | a request over the caps |
| This page does not know this kind of request, so it cannot show it. It can only be denied. | an unknown tool or effect |
| This request could not be read in full. It can only be denied. | a field of a wrong shape |
| This request cannot be shown or answered here. Stop the reply to end it. | a request with no usable hash |
| 3 more calls wait for a decision. | beyond the 16 cards that are drawn |
| Result · untrusted text | label of an opened result |
| Show result (1.2 KiB), Hide result | the toggle of a result |
| [cut for display] | end of a result of more than 20000 characters |
| ran, error, waiting, no result | state words of a tool row, with the marks `✓`, `!`, `…` and `–` |
| waiting for your decision | status line while calls wait |

![Approving a tool call](design/ai-approval.png)
![Tool call states](design/ai-approval-states.png)

**Narrow screens (640 px and narrower).** `Allow once`, `Deny` and the result toggle are at least 44 px high, the strip is
at most 60 % of the viewport height and scrolls inside, and the payload box scrolls inside.

![Approving a tool call on a phone](design/ai-approval-phone.png)

**Accessibility.** The card is a `group` named by its title, `aria-busy` while sending, the status line is the live region,
every hairline is `--hw-line` so `contrast-high` holds, and nothing moves. Under forced colours the card, the remote chip
and the place chip get a system-colour outline.

### 6.16 Changing a file

The card in which the user sees exactly what a write tool will do to a file before it does it, and the button with which
the user takes a finished write back. The core builds the diff from the very text whose digest is part of the card's hash,
checks its own script (the unchanged and removed lines joined are the old text, the unchanged and added lines joined are
the new text) and makes no card when the check fails; the table looks at the file again when the call runs and refuses it
when the text, the size or the change time differ from what the card showed. The page only draws the rows the core sent. It
computes no diff, no count, no digest and no sentence from a file's text, and it does not summarise, reorder, merge, shorten
or classify a row.

**Place.** The diff is a box inside the approval card of 6.15, between the effect line and the payload box; the order in the
card is head, effect line, diff, payload, destination, banner, hash and buttons. The card keeps every rule of 6.15: a flat
strip entry, a 2 px bar on its left, mono payload rows, one solid action.

**Heading and effect.** The tool words are `Write a file` and `Edit a file`; the effect line reads `Changes one file. Check
the changes below.` The head is unchanged: `pane 1 · This computer`, and the chip `remote host` for a remote target. The
payload keeps every field the core sent, in the core's order, and the digest of the old text is the last row (`base`,
`sha256:` and 64 hexadecimal digits, or `none` for a file that does not exist yet), so the hash covers what the diff shows.

**Diff box.** A hairline box in mono with no fill. Its head line says the kind (`Replaces the whole file`, `Creates a new
file`, `Changes parts of the file`), then the counts from the core (`3 lines added, 1 removed`) and, at the right, the sizes
(`1.2 KiB → 1.3 KiB`, and `new · 80 B` for a new file). Under it are the rows. A row has the old line number and the new
line number (dim, right aligned, empty where the core sent none), the sign and the text. The sign is `+` for an added line
and `-` for a removed line, in the `--hw-ok` and `--hw-danger` colours, so the sign is the second cue; an unchanged line has
no sign. The text is ink, wraps and is never cut. Hidden characters are written as escapes (`\u{202e}`) and a carriage return
as `\r`, never dropped. A gap row says `⋯ 40 unchanged lines` in the dim colour and stands for a run of unchanged lines the
core left out (three unchanged lines are kept around every change); a note row says the core's sentence in the dim colour
(`No newline at end of file`). The box is at most 16 rem high, scrolls inside and takes the focus with `Tab` (the 2 px rose
ring), so it can be scrolled by keyboard.

**Destination and banner.** For a write the destination line reads `Only the outcome (saved or not) is sent to llama3.2 at
localhost:11434 (this computer).`, with the place chip as for a read; the model never receives the text of the file. The
banner of a tainted turn keeps the sentences of 6.15 and adds `A file will be changed. Check the changes below before you
allow it.`

**What cannot be shown.** A write card without a usable diff, a diff the page cannot show in full (over 4000 rows or 524288
characters of row text), an unknown row operation, or counts that disagree with the rows can only be denied, with the title
and the danger bar of 6.15. The caps of the payload are raised for writes to 393216 characters in one value and 524288 in
all, which is the escaped size of the largest `write_file`; they are held to the core by a golden. The core refuses a change
it cannot show in full before a card exists, so the page rule is a second wall.

**Tool row of a write.** After a write that ran, its row shows the mark, the tool words, the core's summary, the state word
`ran` and, at the right, the button `Undo`. A press asks the core to put the old text back, which it does only while the
file still holds exactly the text the assistant wrote. After a successful undo the state word reads `ran · undone` and the
button is gone; a refusal puts the core's sentence under the row in the error tone (danger, underlined). Undo asks for no
second click, because the core refuses unless the file is as the assistant left it. A file the assistant created cannot be
undone (nothing is ever removed), and a change that is no longer known (after a restart, or when the chat was deleted)
cannot be undone either; for those two reasons the button is gone as well. The result box shows the core's sentence under
`Result · untrusted text` as for every tool.

**Keys and focus.**

| Key or press | Effect |
|---|---|
| a card appears | nothing: it never takes the focus |
| `Tab` | reaches the diff box, the payload box, `Allow once` and `Deny` in this order; from the message box `Shift+Tab` reaches `Deny` first |
| `Escape` with the focus in the diff box | denies that call, as everywhere in the card |
| a press on `Undo` | one request with the chat and the call; a script's click does nothing |

**Words.** Fixed sentences of the page, held to the core's kinds and operations by tables.

| Tool | Words |
|---|---|
| `write_file` | Write a file |
| `edit_file` | Edit a file |

| Sentence | Where |
|---|---|
| Changes one file. Check the changes below. | the effect line of a write |
| Replaces the whole file | kind `replace` |
| Creates a new file | kind `create` |
| Changes parts of the file | kind `edit` |
| 3 lines added, 1 removed | the counts, from the core; `1 line added` in the singular |
| 1.2 KiB → 1.3 KiB, new · 80 B | the sizes |
| ⋯ 40 unchanged lines | a gap row |
| Only the outcome (saved or not) is sent to llama3.2 at localhost:11434 (this computer). | the destination of a write |
| Only the outcome (saved or not) is sent to the model of this chat. | the destination when the route is not known |
| A file will be changed. Check the changes below before you allow it. | the banner of a tainted turn, after the sentences of 6.15 |
| Undo | the button on the row of a write that ran |
| Undoing… | the button while the press travels (it is disabled) |
| ran · undone | the state word of a row whose write was taken back; the button is gone |
| The undo could not be sent. | an answer to the press that is not the core's refusal |

| Reason of a refused undo | Sentence under the row (the core's own) | Button |
|---|---|---|
| `unknown_change` | That change can no longer be undone here. | gone |
| `created` | The assistant created this file. Undo cannot remove it. | gone |
| `file_changed` | The file changed after the assistant saved it. Nothing was restored. | stays |
| `denied` | That file is in a protected place. Nothing was restored. | stays |
| `unreadable` | The file could not be read or saved. Nothing was restored. | stays |

A refusal is never retried: the next press is a new request of the person. The sentences are the core's and the page holds the
names of the reasons only, held to the core by a golden.

![Changing a file](design/ai-write.png)
![File change states](design/ai-write-states.png)

**Narrow screens (640 px and narrower).** The diff box is part of the card with the same 44 px targets, its text wraps and it
scrolls inside its own height. `Undo` is at least 44 px high and wide.

![Changing a file on a phone](design/ai-write-phone.png)

**Accessibility.** The diff box is a group named by its head line. The sign is a character, not a colour, and the line
numbers are text. Every hairline is `--hw-line`, so `contrast-high` holds, and nothing moves. Under forced colours the box
and the `Undo` button get a system-colour outline and the signs stay characters.

### 6.17 Running a command

The card in which the user sees exactly what a command will do, where and for how long before it runs, and the rows in
which the transcript tells how it ended. The core enforces the rule: the host runs only the command and the working
directory of the approval ticket, through a fixed interpreter and in a separate process with a scrubbed environment, no
input, bounded output and a timeout, and a stop ends the command and everything it started. The page only draws what the
core sent and relays a person's press. It does not summarise, shorten, reorder, merge, translate or classify a command,
and it sends the chat, the call, the verdict and the hash of the card, never the command.

**Place.** Nothing new is added to the card. A command card is the approval card of 6.15 with the document order head,
effect line, payload, destination, banner, hash and buttons, and it keeps every rule of that card: a flat strip entry, a 2
px bar on its left, mono payload rows and one solid action.

**Heading and effect.** The tool words are `Run a command`; the effect line reads `Runs one command in a separate process
with your account's rights. Check the command below.` The head is unchanged: `pane 1 · This computer`, and the chip
`remote host` for a command on a saved connection.

**Payload.** One row for every field the core sent, in the core's order: `tool`, `pane`, `target`, `remote`, `command`,
`cwd` (only when the call named one), `timeout_secs` and `resolved_cwd`. The command is the core's escaped text (`\n`,
`\t` and `\u{202e}` are visible), wrapped and never cut. On a command card the payload box has no height limit and no
scroll of its own: the card grows and the strip scrolls, so the end of the command is the last thing above the
destination line, the banner, the hash and the buttons. The box keeps its place in the keyboard order, and a command
card is the one place where it has no height limit (6.15 limits it everywhere else).

**Destination.** The line reads `The command's output is sent to llama3.2 at localhost:11434 (this computer).`, with the
place chip as for a read; when the route is not known it reads `The command's output is sent to the model of this chat.`

**Banner.** The banner of a tainted command keeps the sentences of 6.15 and adds `A command will run. Read all of it
before you allow it.`

**Modes.** Unchanged. After `Allow once` the card says `Allowed · running…` until the result arrives. A stop (the `Stop`
button, or `Escape` in the message box) ends the command and everything it started, and the row then reads `error` with the
core's sentence for a stopped call.

**Tool row of a command.** The mark, `Run a command`, the core's one-line summary and the state word. A command that exits
with status 0 is `ran`; any other end (a non-zero status, a time out, a lost connection, a stop, a denial) is `error` with
the mark `!`, so colour is never the only cue. The result box shows the core's report under `Result · untrusted text`:
the first line is `Exit status: N.` or a `Stopped: ...` line, then the sections `Standard output:` and `Standard error:`
or `No output.`, with `[earlier output omitted]` where the start of a stream was cut. The model hears the same text.

**Keys and focus.** Those of 6.15: a card never takes the focus, `Tab` reaches the payload box, `Allow once` and `Deny` in
this order, `Escape` with the focus inside a card denies that call, and a press on `Allow once` within half a second of the
card appearing is ignored.

**Words.** Fixed sentences of the page, held to the core's tool and effect tables by a golden.

| Tool | Words |
|---|---|
| `run_command` | Run a command |

| Effect | Words |
|---|---|
| `exec` | Runs one command in a separate process with your account's rights. Check the command below. |

| Sentence | Where |
|---|---|
| The command's output is sent to llama3.2 at localhost:11434 (this computer). | the destination of a command |
| The command's output is sent to the model of this chat. | the destination when the route is not known |
| A command will run. Read all of it before you allow it. | the banner of a tainted turn, after the sentences of 6.15 |
| Exit status: 0. | first line of the result of a command that ended by itself |
| Stopped: the command ran longer than 60 seconds and was ended. | first line of the result of a command that timed out |
| Stopped: the command was ended before it finished. | first line of the result of a command that was stopped |
| ran, error | the state words of a finished command |

![Running a command](design/ai-command.png)
![Command states](design/ai-command-states.png)

**Narrow screens (640 px and narrower).** `Allow once`, `Deny` and the result toggle are at least 44 px high and wide, the
strip is at most 60 % of the viewport height and scrolls inside, and the command wraps, so the payload box never scrolls
sideways.

![Running a command on a phone](design/ai-command-phone.png)

**Accessibility.** As in 6.15: the card is a group named by its title, every hairline is `--hw-line` so `contrast-high`
holds, and nothing moves. A failed command is told by the mark `!` and the word `error` as well as by colour.

### 6.18 AI chat in the sidebar

The column at the right of the stage (6.11) has two forms that take turns: the sidebar (hosts, history and widgets, 18 rem
wide) and the AI chat (23.333 rem, 280 px at the base size, with a hairline on its left like the sidebar). Only one shows at
a time. `▥ Sidebar` and `Cmd+B` show and hide the column in whichever form it is in. The form is not remembered across
launches: the column opens as the sidebar, so a launch never puts a transcript on the screen or asks the core for a chat
unasked. The title bar needs no new control.

![AI chat in the sidebar](design/ai-chat.png)

The chat in the column is the module of the chat pane (6.12): the same bar, transcript, strip of cards, status line and
composer, fed by the same events. It is not a block, so it is not in the layout and not restored at launch (its file is,
and the list shows it).

**Entering.** The palette entry `AI chat in the sidebar` (no chord) shows the column in the AI form and gives the composer
the keyboard; with the form already showing it only gives the keyboard. `✦ AI` and the chord of `ai.open` follow the order
of 6.12: a chat whose tool call waits and is not on screen, else the chat pane of the shown tab, else a shown column
becomes the AI chat, else a new chat opens as a pane. `ai.new_chat` always opens a new conversation in a new pane. The
button is pressed while a chat is on screen in either form.

**Leaving.** `◂ Sidebar` in the head returns the column to the sidebar form and the keyboard to the terminal. The chat keeps
running and keeps its transcript. Hiding the column while the focus is inside it gives the keyboard to the terminal.

**Head.** A `.label` row, `✦ AI CHAT` at the left and `◂ Sidebar` (dim, no border) at the right, a hairline below. Under it
the chat bar row: the title button (the chat's title in mono on one line with an ellipsis, a caret `▾` that turns `▴` while
the list is open, `new chat` before the first message) and `＋ New chat` (hairline). The chat's own bar (provider chip,
place chip, `Providers…`, usage) follows as in 6.12.

| Control | Rule | Second cue |
|---|---|---|
| `◂ Sidebar` | dim text, no border; returns the column to the sidebar | the arrow and the word |
| Title button | opens and closes the list; `aria-expanded`, `aria-controls` the list | the caret `▾` and `▴` |
| `＋ New chat` | hairline; makes an id and shows an empty chat; asks the core for no file | the words |
| Row on screen | rose wash, a 2 px rose bar on its left, `aria-current` | the mark `●` against `○` |
| Row of a chat in a pane | says `in pane 3` instead of the age; no `×` | the words |
| `×` | 28 px high; the first press arms it | it turns into `Delete?`, underlined, in the danger colour |

**Chat list.** The list opens under the head inside the column: flat, a hairline above and below, at most 40 % of the height
of the column, scrolling inside. The list is read from the core when it opens and nowhere else. There is one row per chat
that has a file, newest first, and always a row for the chat on screen (first when it has no file yet, titled `new chat`
with the line `nothing sent yet`). A row has the mark `●` (on screen) or `○`, the title (one line, ellipsis; `untitled
chat` when empty) and under it, in mono and dim, the model the chat is pinned to and how long ago it last changed
(`llama3.2 · 2 h ago`). Titles are the first line of what someone typed, so they are text and nothing else. The notes that
the core sends about the folder are shown as dim lines above the rows (at most five). At most 500 rows are drawn.

![Conversations](design/ai-sidebar-list.png)

**One chat has one place.** A chat that is open in a pane of this window says `in pane 3` instead of the age. Choosing it
focuses that pane (or, when the pane is in another tab, says `That chat is open in another tab.`), and it has no delete
button, so a chat is never in the column and in a pane at once. A switch carries nothing: the strip, the attached offer and
the draft belong to the chat they were made for, and a chat that is left and idle lets go of its page session, and with it a
share it held. A chat that is replying keeps streaming and is whole when the user returns.

**Delete is deliberate.** Every saved row that no pane holds has a delete button `×` at its right (the chat on screen too, once it has a file; the chat with no file has none). The first press turns it into
`Delete?` (the word is the cue); the second press on the same button deletes the chat with its file and stops a running
reply; there is no undo. Any other press, `Escape`, closing the list or reading it again disarms it. Deleting the chat on
screen shows a new empty chat. A sentence about the list (a refusal in the core's words, `Chat deleted.`) is said in a
one-line status line under the head, so it is seen with the list closed.

**The chat.** The module of 6.12 fills the rest of the column: bar, transcript, strip of cards, status line, composer. It
acts for the last terminal or preview pane the user focused (6.14), with the focused pane as the only fallback; every send
names that pane and a turn is stopped when that pane closes. Closing any other pane does nothing to it.

**Cards.** The consent card (6.14), the approval card with the diff and the command (6.15 to 6.17) and the tool rows are the
same components in the strip. In the column the payload rows stack (the name above its value, the size at the right of the
name), the diff and the command wrap and never need a horizontal scroll, and the strip is at most half the height of the
column and scrolls inside.

![Approval in the column](design/ai-sidebar-cards.png)

**A decision is brought to the person.** When a tool call waits in the column's chat and the column is hidden or shows the
sidebar, `✦ AI · n` shows the column in the AI form on that chat. A card never takes the keyboard by itself.

**Keys and focus.** Opening the AI form focuses the composer. `Escape` in the composer stops a running reply or gives the
keyboard to the terminal (6.12); `Escape` in the list closes it (first it disarms a `Delete?`); the arrows move between the
rows, `Home` and `End` go to the first and the last. Every control carries `data-field`, so the page's own keys stay with it.

**Narrow screens (640 px and narrower).** The column is the screen, as the sidebar is. `◂ Sidebar`, the title button,
`＋ New chat`, the row buttons and `×` are at least 44 px high (`×` also 44 px wide), composer text is at least 16 px, and the
provider picker stays the bottom sheet of 6.12.

![On a phone](design/ai-sidebar-phone.png)

**Accessibility.** While the AI form shows, the column is an `aside` named `AI chat`. The title button has `aria-expanded`
and `aria-controls` the list; the list is a `list` named `Chats` and the chat on screen has `aria-current`; the status line
of the list is `role="status"`; `×` is named `Delete` and described by its row. Hairlines are `--hw-line` so `contrast-high`
holds, nothing moves, and under forced colours the chat on screen and the focused controls get a system-colour outline.
Rose marks only the chat on screen and the focus cues.

| Sentence | Where |
|---|---|
| new chat | the title of a chat with no message, in the head and in its row |
| nothing sent yet | the line of the row of a chat with no file |
| untitled chat | the title of a stored chat whose title is empty |
| Delete? | the armed delete button |
| Chat deleted. | the status line after a delete |
| That chat is open in another tab. | the status line after a press on a row whose pane is in another tab |
| The list was not understood. | the status line when the core's list cannot be read |

### 6.19 Palette and inline actions

The chat has two entries in the palette and one button in its transcript. They prepare and the person decides: nothing
here sends a message, shares a pane, reads the text of a pane or runs a command.

| Palette entry | Chord on macOS | Chord on Windows and Linux | What it prepares |
|---|---|---|---|
| `Explain error` | `Cmd+Shift+Y` | `Ctrl+Shift+Y` | a message in the composer and the context card of 6.14 for the pane the chat acts for, with one item chosen |
| `Command from description` | `Cmd+Shift+G` | `Ctrl+Shift+G` | the start of a message that asks for one shell command, with no context |

Both are listed with `AI chat`, `New AI chat`, `AI chat in the sidebar` and `AI providers…` under the section word `ai`,
with the chord of the running platform at the right.

![AI actions in the palette](design/ai-actions.png) Both can be changed or unbound in the key binding editor (6.4). `Y` is
for why and `G` for generate; `Ctrl+Shift+E` stays unused because the input method of GNOME takes it. They add no control
to the title bar, the pane header or the status line: nothing in the page tells which command of a pane failed, so there is
nothing to attach a control to, and nothing is drawn for what is not built.

**Where they open.** Both follow the order of 6.12 for `✦ AI`: a chat whose tool call waits and is not on screen, else the
chat pane of the shown tab, else the shown column, which becomes the AI chat (6.18), else the focused pane is split to the
right with a new chat. The message goes into the composer of the chat that came up and into no other, the composer gets the
keyboard with the caret at the end, and a second run of an action leaves one copy of the text and no second chat. A draft
the person has already typed is kept as it is: the prepared text goes in only when the composer is empty.

**Explain error.** It acts for the pane the chat acts for (6.14) and needs that pane to be a terminal pane; otherwise the
notice `Focus a terminal pane to explain its error.` is shown and nothing opens. The composer receives `Explain the error
in the attached terminal output: what went wrong, the most likely cause, and the command or change that fixes it.` The
context card (6.14) opens for that pane with one item chosen, in this order: the selection when the pane shows one; else the
last command and its output when the core lists that kind; else the visible screen. The last command appears only when the
engine can bound command output, so the second choice is what happens when it cannot. The card, its words and its rules are
those of 6.14: the pane is still shared first (the card asks), the item is previewed before anything can leave, the redaction
findings and the destination are shown, and the person can tick another item or none. The status line says `Nothing is sent
until you press Send.`

![Explain error](design/ai-explain.png)

**Command from description.** The composer receives `Write one shell command for this task. Reply with the command alone in
one code block and, after it, one sentence saying what it does. Task: ` with the caret after the colon and the space. No
context card opens and nothing from the terminal goes with the message; what leaves is the person's finished text. The status
line says `Describe the task after "Task:", then press Send.`

**Insert at prompt.** Under a code block of a finished assistant reply that is one line (after one trailing line break is
dropped and the ends are trimmed), has at most 1024 characters and holds no hidden character, a hairline button `Insert at
prompt` (mono, uppercase, a `.label`) stands left under the block. A click puts exactly that line at the prompt of the pane
the chat acts for, at the cursor, through the paste path of the terminal, with no line break, and does not run it; the
terminal pane then has the keyboard and the status line says `Inserted at the prompt of pane 1. It was not run.` It inserts
nothing and says why when that pane is not a terminal pane of the shown tab (`There is no terminal pane to insert into.`),
when it shows a full-screen program (`That pane shows a full-screen program, so nothing was inserted.`), or when the terminal
refuses the text (`The terminal did not take the text, so nothing was inserted.`); a refused paste is never repeated. The
click must be a real press: an event that a script made is ignored. The line is checked again at the click, and the page
sends no key press at all, so the person reads the line at the prompt and presses `Enter` themselves.

| State of a code block | What is drawn | Second cue |
|---|---|---|
| one clean line | the button `Insert at prompt` | the word |
| several lines, a tab, an empty block, a line over 1024 characters | nothing | none; the person selects and copies what they want |
| one line that holds a hidden character | no button; under the block the line `! Not insertable: it holds characters that are not shown.` in the warn colour, underlined | the mark `!` and the words |

![Command from description](design/ai-describe.png)

A hidden character is a control character (an escape sequence, a carriage return, a C1 control, DEL), a bidirectional
control, a zero-width, tag or private-use character, a line or paragraph separator, or a space that is not the plain space.
A reply gets at most 16 buttons and notes.

**Narrow screens (640 px and narrower).** `Insert at prompt` is at least 44 px high, and the prepared message in the composer
is at least 16 px like all composer text; the context card keeps its own 44 px rules. On a narrow screen the click also shows
the terminal pane (the layout answer of focusing it), where the inserted line is.

![On a phone](design/ai-actions-phone.png)

**Accessibility.** The button's text is its name and the block above it is read first; the warn line is text with the mark
and words; the status line is the live region; hairlines are `--hw-line` so `contrast-high` holds; nothing moves; under
forced colours the button and its focus ring use system colours; the keyboard focus ring is 2 px.

| Sentence | Where |
|---|---|
| Nothing is sent until you press Send. | the status line after `Explain error` |
| Describe the task after "Task:", then press Send. | the status line after `Command from description` |
| Focus a terminal pane to explain its error. | the notice when the pane the chat acts for is not a terminal |
| Inserted at the prompt of pane N. It was not run. | the status line after a click on `Insert at prompt` |
| There is no terminal pane to insert into. | the status line when the pane is no terminal pane of the shown tab |
| That pane shows a full-screen program, so nothing was inserted. | the status line when the pane is on the alternate screen |
| The terminal did not take the text, so nothing was inserted. | the status line when the paste was refused or empty |
