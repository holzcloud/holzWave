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
| Window bar | traffic lights (macOS), or the three window buttons the system uses on Windows and Linux (6.33; with the setting `System` the system's own title bar sits above the bar instead, section 7), workspace chips, tabs, version label right-aligned in mono |
| Workspace chip / tab | text only and, while a pane they hold needs you, one mark per kind (6.23); selected = rose wash and rose text, with an underline for non-colour cue |
| Pane header | state dot, title, context (path, host) in mono, right-aligned |
| Focus | 1 px rose ring plus a header cue; only shown when more than one pane exists |
| Divider | 1 px hairline, 8 px hit area, rose on hover, `col-resize` / `row-resize` |
| Selection | rose at 35 % over the cell background, keeps glyph contrast |
| Cursor | block, rose; hollow when the pane is unfocused; bar and underline per DECSCUSR |
| Status bar | mono labels, uppercase: version, renderer, encoding and pane count, plus the copy-mode word, a notice and the target of a link under the pointer while they last; no workspace segment (the workspaces are the pills of the title bar) |
| Overlays | command palette, theme picker, connection manager, and the request card of `wsh ai` (6.20): 24 px radius, the only elements with a shadow; the request card has no backdrop and never takes the keyboard |
| Empty state | the logo's plank with one line of text; no illustrations |
| Chat pane | a pane of its own kind (6.12): header `chat`, a bar with the provider chip, a transcript, a status line and a composer; flat and hairline, the only shadow is the provider sheet on a phone |
| AI providers dialog | the box of the Settings dialog (6.13): groups under `.label` legends, a list of providers as a radio group, a form with a write-only key field, a disclosure card for a gateway or a server of the user's own; on a phone it is the screen |
| Context card | a flat strip in the chat pane (6.14): a 2 px bar on its left, native checkboxes, one solid action, hairlines above and below, no shadow; it names the pane, the destination and every item that would go out |
| Approval card | a flat strip entry in the chat pane (6.15): a 2 px bar on its left, mono payload rows, one solid action, hairlines above and below, no shadow; it shows every field of a tool call, where the result goes and what untrusted text is in the turn |
| Chat list | a flat list inside the sidebar column (6.18): one row per stored chat with a mark (`●` on screen, `○`), a title and a mono line with the model and the age, the chat on screen with the rose wash and a 2 px rose bar, a delete button that asks twice (`×`, then `Delete?`); hairlines above and below, scrolls inside, no shadow |
| Reply action | a hairline mono button under a one-line code block of a finished reply (6.19): its word is its name, it puts the line at the prompt of the pane the chat acts for and never runs it; a block with characters that are not shown gets a warn line instead; no shadow |
| Diff box | a hairline box in mono inside an approval card (6.16): a head line with the kind, the counts and the sizes, then the changed lines with their line numbers and a `+` or `-` sign as the cue; no fill, wraps and scrolls inside, at most 16 rem high |
| Shell request card | a floating layer without a backdrop (6.20): the prompt of a `wsh ai` request in full, the piped text by counts and findings, the destination and whether it leaves this computer, `Don't send` and `Send once`; it never takes the keyboard, and only a real press after half a second can send |
| On-device row | a read-only group row in the providers dialog (6.21): the name `Apple on-device`, a state word, one sentence of advice and, when the model cannot answer, a hairline `Check again`; in the chat picker it is an ordinary row with no host; shown only on a Mac, flat and hairline, no shadow |
| Sign-in box and web dialogs | the box of the Settings dialog centred on the ground before sign-in (6.28), and the account and people dialogs and the layer `Confirm it is you` (6.29): hairline, 8 px radius, the one shadow, labels above fields, one solid button, errors with `!` and an underline; the browser build only |
| Key row | the six keys `esc tab ctrl alt ↑ ↓` in one strip above the on-screen keyboard (6.6): 44 px high and wide, hairline keys with a 6 px gap; `ctrl` and `alt` are sticky, told apart by shape (idle, armed with an underline, locked with a filled square and an underline), never by colour alone; not drawn with a hardware keyboard |
| Selection handles and menu | two 44 px handles drawn as a small rose teardrop on a 2 px stem at both ends of a selection on a touch layout, and a floating layer with `Copy` and `Paste` (6.6): 8 px radius, the one shadow rule, 44 px buttons |
| Local network notice and row | a text-only floating layer on the first start of a Mac (6.24): the reason, `Not now` and `Allow`; and a read-only group row `Local network` in the Settings dialog with the state in words, one sentence of advice and the buttons `Open System Settings`, `Allow` and `Check again`; macOS 15 or later only, flat and hairline, the one shadow is the layer's |

**Motion:** 160 ms for hover and focus, 240 ms for layout changes, easing `cubic-bezier(.2,.7,.3,1)` (holzcloud).
No motion in the terminal grid itself. `prefers-reduced-motion` removes all transitions.

**Accessibility:** every colour state has a second cue (underline, dot shape, label). The focus ring is 2 px for
keyboard focus. Chrome text is at least 12 px. All chrome is operable from the keyboard.

**Accessibility preferences (DESK-24).** Motion, contrast and cursor blink are each `system` or an explicit choice in
the settings; an explicit choice beats the OS, in both directions, and a change of the OS preference is followed
while the app runs.

| Preference | OS signal the web view reports | Effect |
|---|---|---|
| Motion | `prefers-reduced-motion` | no chrome transitions (root class `motion-reduce`); the terminal grid never moves, and an animated picture in a pane does not play (6.31) |
| Contrast | `prefers-contrast: more` and `forced-colors` | hairlines use `--hw-ink-3`, dim text uses `--hw-ink-2`, the focus line is 2 px (root class `contrast-high`); under forced colours the chrome uses system colours |
| Cursor blink | none of its own: it follows reduced motion | a cursor the program asked to blink does so with a 530 ms phase, unless the setting is `off`, or `system` with reduced motion; the phase restarts on output and on every key |
| UI scale | **none on macOS**; Windows: the Text size setting; Linux: GNOME's `text-scaling-factor` (read by the shell, plan 02.2-09) | the user's `ui_scale` (75 to 200 %) times the OS factor, for the chrome only, and the font size actions; see below |

The web view reports no OS text-size preference on macOS, so there the scalable UI font is the user's own setting
rather than an OS signal; the requirement DESK-24 says so in its wording, and nothing in the page pretends otherwise.
On Windows and on GNOME the shell reads the OS text size (the `TextScaleFactor` registry value through `reg.exe`, and
`text-scaling-factor` through `gsettings`), at start and when the window gains focus, and the page multiplies the user's
`ui_scale` by it for the chrome only (tabs, bar, status line, dialogs; 75 to 300 % in all). The terminal's own text
stays at `font_size` (except in the phone and tablet layouts of 6.6, where a pinch changes it). Every other desktop, and every source that cannot be read, counts as 100 %. That this really
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
AI chat, AI chat states, AI sidebar, attention marks, drag) and a platform switch. The PNGs are rendered from it.

**Rules**

1. **One look everywhere.** The window chrome is drawn by holzWave, with no native tab control and, by default, no native title bar (on Windows and Linux the page's bar is the window bar, 6.33), so
   macOS, Windows, Linux, Android and the browser show the same thing. Only the OS window buttons and the system keyboard
   differ.
2. **Mono everywhere in the chrome.** Labels, tabs and the status line are JetBrains Mono; Manrope only appears in
   previews. Labels are uppercase and tracked, as in the holzcloud family.
3. **Flat and hairline.** No shadows between panes, no gradients, no blur. Panes are separated by a 1 px
   `--hw-edge` line. Only floating layers (command palette) have a shadow and a 8 px radius.
4. **Rose is rationed.** It is state, never decoration: it says where the user is and what the pointer is about to
   act on. It appears for the prompt chevron, the cursor, text selection, the pane focus line (the focused pane's top
   line), the active tab marker (wash, underline and the workspace diamond),
   divider hover, button hover wash (bar, dialog and search buttons), the selected palette row (wash and left bar),
   the focus ring of the palette and search inputs, the link underline (a link under the pointer with the modifier
   held, a printed address and a link the program marked alike), the `Open` button of the link dialog (6.31), hint labels, the key binding capture row, and the accent colour of settings checkboxes and radios.
   Everything else is warm grey.
5. **tmux-style status line.** A bottom strip of segments (panes, renderer, encoding, version), with the mode word,
   the notice and the target of a link under the pointer (6.31) taking a segment while they last, and the Kubernetes context and the Azure subscription when the core has
   one (6.11). It does not repeat the workspaces: they are the pills of the title bar.
6. **Numbered tabs.** Tabs carry their number (`1 shell`) so `⌘1…9` is discoverable.
7. **Command palette first.** Actions are reachable by name with a fuzzy palette (`⌘K`); the chrome stays small.
8. **Responsive by layout, not by style.** On a phone the same `View` shows one pane per screen, the tab strip
   moves to the bottom, and an accessory row (`esc tab ctrl alt ← ↑ ↓ →`) sits above the system keyboard. Colours,
   type and spacing tokens are identical.

**Size tokens for touch:** minimum target 44 px on mobile and 28 px on desktop; the accessory row keys are 44 px high. The buttons of the title bar are the exception: they keep the size of the other bar buttons at every width, also in a window of 640 px and narrower.

### 6.1 The window: three levels

holzWave has three nested levels, and each one has its own shape and place so that they cannot be confused.

| Level | Shape | Place | Holds |
|---|---|---|---|
| **Workspace** | round pill with a diamond (`◆` active, `◇` others), tab count as a small number; the active one is a solid `--hw-rose-deep` fill with `--hw-on-accent` text | title bar, left to right | its own tabs and layout |
| **Tab** | square, numbered (`1 shell`), underline and rose wash when shown, a close mark on hover | its own row under the title bar | one split tree of panes |
| **Pane** | tile with a 24 px header: state dot, type icon, title, context chip, context right-aligned, `⊞ ⤢ ✕` | the area under the tab row | one block: shell, SSH, server session, preview, editor, monitor |

Anything that is not one of these three is **chrome around them**: the right title-bar group (sync, AI, sidebar, palette),
the sidebar and the status line.

A workspace is shown once, as a pill in the title bar; the status line does not repeat it. The attention marks of 6.23 sit inside
the pill and the tab and change neither shape: the pill stays round with its diamond and the tab stays square and numbered.

**The keyboard moves between two regions: the terminal and the bar.** The bar is the whole of the title bar and the tab row, the window
buttons of 6.33 included. `F6` takes the keyboard from the terminal to the first control of the bar and `Shift+F6` to the last (the
actions `region.focus_next` and `region.focus_prev`, with `F6` and `Shift+F6` as the default on every platform); from the bar, either of
them gives the keyboard back to the terminal, and so does `Escape`. The palette entry `Focus the bar` (`bar.focus`, no chord) goes to the
bar from anywhere and is the way when a program on the alternate screen (htop, mc) has the terminal input, because a function key alone is
that program's while it runs: the page leaves `F6` and `Shift+F6` to it and sends them on, and takes them again in the bar and in every
other place. The entries are not offered where no bar is drawn (the phone layout has its own strips, 6.6). Who has the keys:

| The keyboard is | `Tab`, `Shift+Tab` | `Enter`, `Space` | arrows | `Escape` | `F6`, `Shift+F6` | every other key |
|---|---|---|---|---|---|---|
| in the terminal input | the terminal's `Tab` (nothing moves) | sent to the terminal | sent to the terminal | sent to the terminal | go to the bar (sent to a program on the alternate screen) | sent to the terminal |
| on a control of the bar | the next and the previous control of the bar, in the order of the page; past the last or before the first the keyboard goes to the terminal | press the control (a tab, a workspace or a plus button gives the keyboard back to the terminal after it acts, as after a click; the window buttons keep it) | the control's own (the page sends nothing) | give the keyboard back to the terminal | give the keyboard back to the terminal | sent to the terminal, as before |
| on another control of the page (a button of the sidebar, a link, a tabbed or scrollable box) | the browser's own: the next stop | the control's own | the control's own | the feature's own (a dialog closes, a list closes) | go to the bar | sent to the terminal, as before |
| in a text field of the page | the field's own | the field's own | the field's own | the field's own | go to the bar | the field's own |

A chord with Ctrl, Alt or Cmd is a shortcut in every row and is resolved before the table is read. The rows about a control apply when the control has the focus for the keyboard (`:focus-visible`): a button that only a click left focused leaves the keys to the terminal, as before. The page walks the bar itself on `Tab`
(it does not leave `Tab` to the engine), because WebKit on macOS makes buttons stops of the tab order only when the system's full keyboard
access is on, and the bar must not depend on that setting. Every control of the bar keeps the 2 px focus ring, and the bar is a group
named `Window bar` for a screen reader. The bar stays drawn as it was: nothing new is drawn for the keyboard.

![Shortcuts](design/shortcuts.png)

### 6.2 Anatomy

| Area | Content |
|---|---|
| Title bar (32 px) | window buttons (left on macOS; on Windows and Linux on the side the system uses, 6.33; none on Android and in the browser), workspace pills, `＋`; right group: sync state `☁ synced`, `✦ AI`, `▥ Sidebar` toggle, `⌘K` |
| Tab row (28 px) | numbered tabs, a `⌁` mark on tabs with a remote connection, the attention marks of 6.23 (`? ▲ ! ✓`) after the name of a tab that holds a pane out of view that needs you, `＋`; a hint line right-aligned |
| Area | the split tree of the shown tab, 1 px hairlines between panes, no gaps; the focused pane has the rose line on its header |
| Sidebar (216 px, collapsible) | **Hosts** (state dot, protocol, reconnect counter), **Context** (Kubernetes context and namespace, Azure subscription, GAM profile), **History** (fuzzy search), **Widgets**. The column has a second form, the AI chat (280 px, 6.18), that takes its place |
| Status line (24 px) | tmux-style segments (the workspaces are not repeated here, the pills of the title bar are their place): tabs and panes, Kubernetes context and Azure subscription (buttons, 6.11), git branch, SSH count, sync state, renderer, encoding, version |

The AI chat is a pane (6.12). Its second form, in the sidebar's column, replaces the sidebar while it is shown (6.18): the
palette entry `AI chat in the sidebar` and `✦ AI` (when the tab has no chat pane) turn the shown column into the chat, and
`◂ Sidebar` turns it back. Every segment of the status line and every title-bar button opens the matching popover or
dialog.

### 6.3 Views

Each view is a state of the same window. Overlays use the 24 px radius and the shadow; everything else stays flat.

| View | Image | What it shows |
|---|---|---|
| Daily | ![](design/6-desktop.png) | local shell with the chip of a Kubernetes context (6.11), SSH pane that is reconnecting (`⟳ 2/5`), SFTP editor, Markdown preview, system monitor, sidebar |
| Palette (`Cmd+K`) | ![](design/palette.png) | fuzzy list in groups (actions, connect, switch) with the chords on the right |
| New block | ![](design/new-block.png) | block picker with roadmap phases: shell, SSH, WSL, Docker, previews, editor, Kubernetes, Talos, Azure, GAM7, monitor, history; Kubernetes, Talos, Azure and GAM7 are not opened from it yet: they open from the command palette and from the sidebar's Context section (6.25 says why) |
| Search | ![](design/search.png) | scrollback search bar in the pane, hits marked, the current hit solid |
| Copy mode | ![](design/copy-mode.png) | keyboard selection, hint letters on hashes and paths, a solid status line with the keys |
| Connections | ![](design/connections.png) | list and form: host, user, login (`Key file`, `Password`, `Agent` or `Code or password prompt`), secret from the OS keychain, jump host, reconnect policy, startup command, use wsh on this host; above them, when a server is known, its row and its sessions (6.34) |
| Login prompt | ![](design/login-prompt.png) | the question of a server that asks for a one-time code or a password prompt (6.22): the title and host of your own saved connection, the fixed sentence that the text comes from the server, its words in a bordered block with one field per question, `Cancel` and `Send` |
| Settings | ![](design/settings.png) | themes (built-in and custom), font, UI scale, accessibility as `System / On / Off` |
| Shortcuts | ![](design/shortcuts.png) | the key binding editor, see 6.4 |
| Previews | ![](design/previews.png) | file browser (local and SFTP) with image, PDF and video panes, see 6.5 |
| Sync | ![](design/sync.png) | Nextcloud endpoint, last sync, end-to-end encryption, conflicts with their backup |
| Sync target | ![](design/sync-transport.png) | the set-up popover with the choice `WebDAV` or `holzWave server` (6.9): set up with WebDAV, set up with the server and not signed in (`Sign in…`), the server with its sync target off (`Sync is off on this server.`, `Connect` disabled), and connected to the server |
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
| wsh ai request | ![](design/ai-wsh.png) | a program in a terminal asked the model a question (6.20): the card at the top right with the prompt, the piped text by counts and findings, the destination with the place chip and the buttons; nothing has left this computer yet |
| wsh ai states | ![](design/ai-wsh-states.png) | the card in four states (6.20): asking for a cloud provider, asking for a model on this computer, after `Send once`, and after a refusal from holzWave with a second request waiting |
| On-device model | ![](design/ai-device.png) | the chat on a Mac with Apple Intelligence (6.21): the picker with the on-device row chosen, the bar naming `Apple on-device · system` with no arrow and no host, and the place chip `on device` |
| On-device states | ![](design/ai-device-states.png) | the group `On device` of the providers dialog in three states and a chat whose pinned model is gone (6.21): the reason in words, underlined and marked, the advice, `Check again`, and the sentence that keeps the draft and uses no other provider |
| Monitor over SSH | ![](design/monitor-ssh.png) | the system monitor of a Linux host beside its shell (6.5): header `system · talos-1 · 1 s`, the same bars, rates, uptime and eight busiest processes, read from the host |
| Monitor states | ![](design/monitor-ssh-states.png) | the line above the bars in four states (6.5): connecting, no answer with the numbers kept dimmed, not a Linux host, and a host key that is not known yet; each warn line has words, an underline and the mark `!` |
| Attention marks | ![](design/attention.png) | the marks of 6.23 on the tab row and the title-bar pills: `?` waits for you, `▲` rang the bell, `!` command failed, `✓` command finished, each in its status colour and never rose; the tab `2 holzkube` holds all four side by side in that order, the pill `infra` shows `! ✓` (the kinds of its tabs, once each), the shown tab and the shown workspace carry none; a floating card lists the four kinds with their words and sources |
| Local network notice | ![](design/local-network.png) | the notice of 6.24 over the daily window, on a Mac with macOS 15 or later: `Allow local network access?`, the lead, the three reasons, the note that macOS asks next and that panes count as holzWave, `Not now` (hairline) and `Allow` (solid, focused) |
| Local network states | ![](design/local-network-states.png) | the group `Local network` of the Settings dialog in four states (6.24): `allowed`, `denied` (underlined, `!`), `not asked yet` and `not known` (`?`), each with its advice and the buttons it offers; `Open System Settings` is always there |
| Host shell | ![](design/host-shell.png) | the Settings dialog on the Linux window inside a Flatpak (6.32): the group `Host shell` after Accessibility and before the buttons, one row `Run panes on the host` with the dim meta `Flatpak`, the state word `off` at the right and one sentence of advice; absent everywhere outside a Flatpak |
| Host shell states | ![](design/host-shell-states.png) | the group `Host shell` in its three states (6.32): `off`, `on` (warn, underlined, `!`, the box ticked) and `not available` (`?`, the box dashed and disabled), each with its advice; and the line a new local pane shows while the switch is off or not available |
| Window frame | ![](design/window-frame.png) | the window buttons of Windows and Linux (6.33): the bar in each style and on each side (Windows flat on the right, GNOME round, stock GNOME with one close button, a layout with the buttons on the left, every other desktop flat) and the states of a button, each with a second cue: rest, hover, close under the pointer on Windows, keyboard focus, maximised, close under the pointer on Linux |
| Window frame settings | ![](design/window-frame-settings.png) | the group `Window frame` of the Settings dialog on Windows and on Linux (6.33): the row `Frame`, on Linux also the row `Buttons`, the advice under each, and the status line `Applies after the next start.` |
| Integration block | ![](design/integration-block.png) | a Kubernetes pods block beside its shell (6.25): header `pods`, context `Kubernetes · prod · payments · 10 s` marked as production, the bar with the context and namespace chips and `↻ Refresh`, a table with the status in words, a selected row with its `Logs` button, and the line `Updated 4 s ago` |
| Integration states | ![](design/integration-states.png) | the block in four states (6.25): `Reading …`, the tool not found with the places searched, a path field, `Set path` and `Check again`, an error line with the last rows kept dimmed, and a table cut with its note |
| Context section | ![](design/integration-context.png) | the sidebar's Context section (6.11): one row for each of Kubernetes, Talos, Azure and GAM with the mark, the selection as text, `production` (warn, underlined, `!`) on the Kubernetes row and its `Shell` button, the listbox open under it with the current value marked, the line `New blocks start with prod-eu.`, the status line segments `⎈ prod-eu/payments` `! PRODUCTION` and `az holz-prod`, and a shell pane whose header has the chip `⎈ prod-eu` `! production`; beside it the section in two states, a tool not found with `Set path` and Azure `signed out` |
| Integration card | ![](design/integration-confirm.png) | the card for `Kubernetes · Delete pod` in a production context, armed (6.26): head `! Confirm needed`, the effect line, the target rows, the argument list spelled element by element, the guard line `Production context: Production EU.` (warn, underlined, `!`), the hash, the danger bar and the button `Press again to allow on production`; the pods block beside it has `Logs`, `Exec…` and `Delete…` on the selected row |
| Integration card states | ![](design/integration-confirm-states.png) | four cards (6.26): a Local verb with the `changes` sentence, an Exec verb on a context that is not production, `Allowed · running…`, and `That action is no longer waiting.`; under the first one the header of a held terminal with the hollow dot with a dash and `exited 1` (warn, underlined, `!`) |
| Integration profiles | ![](design/integration-profiles.png) | the dialog of 6.27 over the daily window: the group `Profiles` with a production profile chosen (`production`, warn, underlined, `!`), the form of that profile (name, tool radios, context and namespace, `Production context` checked with its sentence), `Secret variables` with one write-only row (`saved`, `Set`, `Clear`), `Environment` with a refusal of the core on a row (danger, underlined, `!`), the sync note, `Delete` and `Save` |
| Web: sign in | ![](design/web-signin.png) | the page of the browser build before a person is signed in (6.28): the wordmark, the name of the server, the passkey button, one button for each single sign-on provider, `or`, the password form; the code step after a wrong code with the one sentence for every failure |
| Web: first administrator | ![](design/web-first-admin.png) | `Set up this server` (6.28): the masked setup token, the username, the display name, the two sign-in methods and the refusal of a weak password |
| Web: invitation | ![](design/web-invite.png) | `You were invited` (6.28): the authenticator step with the key in groups of four, the link and `Code`, and the recovery step whose `Continue` waits for `I have saved this code` |
| Web: unlock | ![](design/web-unlock.png) | `Your saved passwords are locked` (6.28) over the window: the passphrase with its failure sentence, and the recovery code, each with `Not now` and `Unlock` |
| Web: account | ![](design/web-account.png) | the account dialog (6.29) with its six groups and the layer `Confirm it is you` above it |
| Web: people | ![](design/web-people.png) | the people dialog (6.29): the table of accounts with state marks, the menu of one row and the invitation link shown once |
| Web: window | ![](design/web-window.png) | the window as the browser shows it (6.30): no window buttons, the marks of the status line, the upload target, the `Download` row menu and the Notifications setting |
| Server sessions | ![](design/server-sessions.png) | the Connections view with the server row above the saved connections (6.34): signed in, `Sign out` and `New session`, the help line, two sessions with their size, start, attached count, `held` or `free`, `Attach` and `End`; behind it a local pane and a remote pane whose header says `WRITING`; with the Browser platform the row is `This server` |
| Server pane states | ![](design/server-pane-states.png) | the writer label in a pane header (6.34): `WRITING` with a filled square, `WATCHING` with a hollow one and `FREE` with a dashed one, `Take over` after the last two; the seven states of the pane; the server row in its states |
| Device sign-in | ![](design/device-authorize.png) | the step in the app and the page `Authorize device` (6.35), with the states of the second screen and the three results of the page |
| Device lists | ![](design/device-lists.png) | the Devices group of the Account dialog, the list of devices of an administrator with the column `Owner`, and the group `Device access` with its two switches, one of them turned off by the configuration (6.35) |
| Links | ![](design/links.png) | links in the output of a shell pane (6.31): two at rest with a dotted underline, one under the pointer with a solid underline and its target in the `LINK` segment of the status line, with the notice beside it that says why a click could not open it, one with the open gesture held in rose, one in text the program underlined itself, and an address with a scheme that is not allowed, which stays plain text |
| Link dialog | ![](design/link-ask.png) | the question `Open this link?` (6.31) for a link whose text names one site and whose target is another: the text and the target in mono, `Cancel` focused, `Open` solid rose |
| Images | ![](design/images.png) | pictures in a shell pane (6.31): one in each of the three layers, a picture clipped at the edge of the pane body, a picture made of placeholder cells, and the cursor and a selection drawn over a picture |
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
underlined, above 70 and 90 percent), the network rates (the download rate in blue, the upload rate in the secondary ink,
each with its arrow `↓` or `↑`; rose marks focus and action only), the uptime and the eight busiest processes (PID, name, CPU,
memory), one sample a second, paused while the window is hidden.

On another computer. Every saved SSH connection also has the palette entry `System monitor: Name (ssh)` (WSL and Docker
connections have none). The pane is the same pane: header `system`, context `Name · 1 s` where the monitor of this computer
says `local · 1 s`, the same bars, rates, uptime and eight busiest processes, one sample a second, paused while the window
is hidden. The numbers are those of the other computer, read from its `/proc`, so only Linux hosts are supported; a host
that is not Linux says so and shows nothing, and nothing is guessed for it. holzWave runs one fixed, read-only command
through the saved connection (no terminal, no forwarding, nothing from the page, the pane or the connection put into it) and
keeps only numbers and process names; command lines and environments are never read. Panes of the same host share one
sample. holzWave never learns a host key this way: a host whose key has not been seen yet is refused until the connection
has been opened once. While the pane waits or the host does not answer it says why on a line of its own above the bars, in
words: `Connecting to Name …` (plain, until the first answer); `! No answer from Name: reason` (warn colour, underlined, with
the mark `!`; the last numbers stay, dimmed, and come back by themselves when the host answers again);
`! Not supported: Name does not run Linux (it says Darwin).` (the same three cues; the bars and the table stay empty; the
host is asked again after half a minute). On a narrow window the line wraps; the pane has no control of its own, so there is
no phone drawing. Not drawn: a choice of interval, a process kill, history graphs.

![Monitor over SSH](design/monitor-ssh.png)

![Monitor states](design/monitor-ssh-states.png)

The line is a `role="status"` region; its state and tone are `data-state` and `data-tone`, and dimmed numbers are
`data-stale` on the monitor. Hairlines and dim text use `--hw-line` and `--hw-dim`, so `contrast-high` holds; under forced
colours the warn line gets a system-colour outline; there is no motion. The words by state are `Connecting to Name …`,
`No answer from Name: reason` (without a reason: `No answer from Name.`),
`Not supported: Name does not run Linux (it says System).` and, when the host named no system,
`Not supported: Name did not answer like a Linux computer.`. Reasons are the core's plain texts, for example
`The host key is not known yet. Connect to this host once, then try again.`, `the connection was lost` and
`the host did not answer in time`.

A preview is a pane like any other and uses the same header: image (size and weight as a chip), PDF (page and zoom),
video (a bar with time, volume and speed), Markdown (Manrope, the one place in the chrome that uses it) and the editor
(line numbers, syntax colours from the `--hw-*` tokens). Unsaved changes show as `● unsaved` in the preview's status line (amber, with the words as the second cue), and closing the pane asks first. `Cmd/Ctrl+S` saves, `Escape` gives the keyboard back to the terminal; on a Markdown page `e` opens its source. The file browser is a pane too: `Space` on a file opens its preview beside it,
and the path chip switches between local and an SSH host.

### 6.6 Mobile

On Android a pane fills the screen. Tabs and workspaces move into a sheet from the bottom: workspace pills on
top, the tabs of the active workspace as a list. The key row (`esc tab ctrl alt ↑ ↓`) sits above the system keyboard and
the bottom bar holds the tabs of the shown workspace. Tokens are the ones above; touch targets are at least 44 px.

![Phone](design/7-phone.png)

A chat pane on a phone is the screen, with a sheet from the bottom edge for the provider (6.12); the context card (6.14) and the approval card (6.15) sit inside the screen with the same 44 px targets, and on a phone the payload box of the approval card scrolls inside it. The diff box of a write card (6.16) is part of the card with the same 44 px targets, wraps its rows and scrolls inside its own height, and the `Undo` button of a finished write is 44 px too. On a phone the command card (6.17) grows and the strip scrolls, the command wraps and the buttons keep their 44 px targets. The providers dialog (6.13) on a phone is the screen too, and the sheet rules of the chat apply to its targets and its text. The AI form of the sidebar column (6.18) is the screen on a phone like the sidebar is, with the same 44 px targets and composer text of at least 16 px (the phone image is in 6.18). The request card of `wsh ai` (6.20) is a bottom sheet over the full width with 44 px buttons and text of at least 16 px.

**Layout classes.** Android has two classes, and the page picks one from the window width and the short side of the screen, never from a user agent: the tablet layout when the window is at least 840 px wide and the short side of the screen is at least 480 px, the phone layout otherwise. The short side keeps a landscape phone, whose keyboard leaves little height, on the phone layout; a 7 inch tablet held upright and a window narrower than 840 px in split screen get the phone layout too. The class is decided again when the width changes and never when the on-screen keyboard changes the height, so the layout does not jump while a person types. Desktop and browser windows never take either class: the rule applies on Android only (the page reads the platform name `android`), so a narrow desktop window is unchanged.

**Phone layout.** One pane fills the screen. The top strip keeps the workspace diamond, the workspace name and the name of the tab or pane as in the picture above, then one empty place for a status chip, then the buttons `✦` and `⌘`; the empty place is kept so that nothing moves when a chip appears. A tab with more than one pane shows `2/3` after its name (the second of three panes), and the sheet lists the panes of the shown tab under the tabs: one row for each pane, 44 px high, with the state dot (filled when the pane has the focus, hollow otherwise, as in 6.8), the pane's name and its context in mono, dim; the pane with the focus also has the rose wash and a 2 px bar on its left, so the focus is not told by colour alone. Tapping a row focuses that pane and closes the sheet. The panes that are not shown keep running, so a person does not lose a build by looking at another pane.

**System bars.** The window is drawn edge to edge, so the page keeps its own bars clear of the system's. The top strip is as high as its 44 px target plus the height of the status bar (and a display cutout), with its background drawn behind the system bar so that no gap shows; the bottom bar is as high as its 44 px target plus the height of the navigation bar, whether that is three buttons or the gesture line, and the sheet from the bottom edge pads its content by the same height. The room is one value for each edge, `--hw-inset-top`, `-right`, `-bottom` and `-left`: the larger of the WebView's own `env(safe-area-inset-*)` and `--hw-sys-*`, the height that the Android shell measured and wrote into the root element in CSS pixels. The second source exists because an old WebView (91, on Android 12 images) reports no inset at all; where both report, they agree. Without a system bar all four are 0, so the desktop, the browser and the pictures are unchanged. The tablet layout does not use the values yet.

**Key row.** The six keys `esc tab ctrl alt ↑ ↓`, each at least 44 px high and wide with 6 px between them, because a terminal needs keys that a system keyboard does not have. The label of a key is its name in mono, lower case. `esc`, `tab`, `↑` and `↓` are plain keys. `ctrl` and `alt` are sticky and have three states that differ by shape, so `contrast-high` and `forced-colors` keep them apart: **idle** (as the plain keys); **armed** after one tap (the next key is sent with the modifier, then the key is idle again): a 2 px underline under the label and the rose wash; **locked** after a second tap within 350 ms (it stays on until it is tapped again): a filled square before the label, the 2 px underline and the rose wash. The row sits directly above the on-screen keyboard and follows it when it opens and closes (on the tablet the status line is the one strip between them: the row sits directly above the status line); with the keyboard closed it sits above the bottom bar while a terminal pane has the focus. It is not drawn when a hardware keyboard is attached (below), nor while the focus is in a pane that is not a terminal, such as a chat.

![Key row on a phone: idle, ctrl armed, ctrl locked, and a hardware keyboard attached with no key row](design/phone-keys.png)

**Selection.** A long press of about half a second on the text of a pane selects the word under the finger (the core's word selection) and shows two handles and a menu. A handle is a 44 px touch target drawn as a small rose teardrop at the start or the end of the selection, on a 2 px stem that reaches the text, so the end of the selection is also told by a line; dragging a handle moves that end. The menu is a floating layer, so it has the one shadow of section 4, with an 8 px radius and two 44 px buttons, `Copy` and `Paste`. It sits above the selection, or below it when there is no room above. `Copy` puts the text on the clipboard and clears the selection; `Paste` goes through the same paste confirmation as on the desktop, so a program cannot receive text that the person did not see. A tap anywhere else clears the selection and the handles. A mouse-style selection by dragging does not exist on a touch layout, because a drag already scrolls.

**Gestures.** One finger dragging up or down scrolls the history of the pane; a program that tracks the mouse gets the same wheel input as on the desktop, and the core decides. A drag that begins on a handle moves the handle and never scrolls. Two fingers pinching change the UI scale of this device in steps of 5 within the existing range of 75 to 200 percent, and a label `UI scale 125 %` shows in the top strip while the fingers move and stays 800 ms after they lift; it takes the place of the status chip and is text, so it does not rest on colour. The shared `ui_scale` of the preferences travels with `settings.json` to every computer, so a pinch on a phone must not resize a desktop: the value of a pinch is kept on this device only, is never synced, and replaces the shared value on this device while it exists; changing the UI scale in the preferences on this device removes it. Section 5 says that `ui_scale` scales the chrome only, so in the phone and tablet layouts the terminal's cell size follows `font_size` times the effective UI scale divided by 100, and a pinch changes the text that a person sees. The desktop and the browser are not changed by this rule.

![Selection with two handles and the Copy and Paste menu, a pinch with its label, and the panes of a tab in the sheet](design/phone-select.png)

**Tablet.** The window is the desktop window of 6.1 to 6.3: the two-row bar with workspace pills and numbered tabs, tiled panes, dividers and the sidebar, with these differences. Every control of the bar and of a pane header is at least 44 px high and wide, and the close mark of a tab shows on the shown tab as its own 44 px target, because there is no pointer to hover with. A divider keeps its 1 px line and has a touch area 24 px wide. The key row sits at the bottom of the window, directly above the status line, when there is no hardware keyboard. The palette and the sheets are as on the desktop. There is no tear-off (`Move pane to new window` and `Move tab to new window` are absent, and a pane cannot be dragged out of the window) because the minimum Android version offers no second window, and the bar has no window buttons and no drag region, since the system owns the window.

![The tablet layout: 44 px bar, two panes with a divider, the key row above the status line at the bottom](design/tablet.png)

**Hardware keyboard.** With a hardware keyboard attached the key row is not drawn, the chords of the Linux table apply with the labels `Ctrl` and `Alt` (section 7), and nothing else in the layout changes.

**Lock chip.** The empty place of the phone's top strip (and the same chip in the status line on the tablet) holds a chip that is shown only on Android and only when the store of secrets exists, so that nothing moves when it appears. It has three states, told apart by a glyph and a word, never by colour alone: `Locked` is a closed padlock and the label `LOCKED` on a rose outline; `Unlocked` is an open padlock and the label `OPEN` on a hairline outline, dim; `Not protected` is a padlock with a diagonal bar and the label `NO LOCK` on a hairline outline with a dashed border, so that `contrast-high` and `forced-colors` keep it apart from the other two. Tapping `Locked` opens the unlock sheet, tapping `Unlocked` locks at once, and tapping `Not protected` opens the notice below. The chip is a 44 px target and its label is the `.label` style.

**Unlock sheet.** A bottom sheet in the shape of the sheet of 6.12, with the title `Secrets are locked`, the line `Unlock with your fingerprint, face or screen lock to use saved passwords, keys and the AI key.`, a solid rose button `Unlock` (44 px) and a text button `Not now`. The button starts the system's own prompt; the app never shows a password field or asks for a password itself, so nothing here can be mistaken for the system prompt, which Android draws. When the prompt is cancelled the sheet stays and the quiet line `Not unlocked` appears under the buttons. The first action that needs a secret while the store is locked opens the sheet, never the start of the app by itself, and that action runs again after a successful unlock.

**Locked answers.** Where an action cannot run because the secrets are locked, its own place says so in the quiet style of a dim line, `Locked` and then the action: a new connection reads `Locked: unlock to connect` (as the first line of the pane that was to open) and an AI send reads `Locked: unlock to send` (as the status line of the chat). Sessions that are already open are untouched and show nothing. The rule of 5 minutes is stated once, in the preferences row below, and not at each place.

**Sync while locked.** The title-bar button of 6.9 reads `☁ sync paused` in the dim colour, and the status line of its popover reads `Paused: secrets are locked` with a pause mark `‖` before it. It is not an error: it has the hairline and the dim colour of the other quiet lines, never the red and the underline of `☁ sync error`. After an unlock it changes to the normal state by itself and a run starts at once. It sits in the places the sync already has; no new place is drawn. While the vault is locked, the popover's `Turn off` is disabled, so that no sync secret is left behind in the vault (turning the sync off deletes them, and a locked vault cannot be written): the button is dim with a dashed border, so that it is not told by colour alone, and its reason is the line above the buttons, `Paused: secrets are locked`, which names it as its description; there is no new word. `Close` and `Sync now` stay available, and after an unlock `Turn off` works as before and still asks with a second click.

**No screen lock.** A device with no screen lock gets a plain notice in the notice style: `This device has no screen lock. holzWave still keeps your secrets encrypted with a key of this device, but anyone who can open the phone can use them. Set a screen lock in the system settings to protect them.` The chip shows `Not protected`. The notice has a text button `Dismiss`, is shown once at each start of the app, and the chip keeps saying it after it is dismissed. When a screen lock has been set in the meantime, the notice keeps its words and offers the button `Use protected mode` beside `Dismiss`; it asks the system once (the unlock prompt) and then switches, and until the person presses it the app does not ask again.

**Preferences row.** The Settings dialog gets one group `Security`, after Accessibility and before the buttons, on Android only. It holds one row: the label `Protect secrets with the screen lock` and a switch, on by default, that carries the word `ON` or `OFF` inside it so that its state is not told by position or colour alone, with the help text `Asked once when you open holzWave and again after 5 minutes in the background. Live connections keep running while locked.` Turning the switch off opens a dialog in the box of the Settings dialog with the text `Anyone who can open this phone can then use your saved passwords and keys.`, the line `This applies to this device only`, and the buttons `Turn off` and `Keep on`; the switch is a fact of this device and is never synced. The switch has two states that differ by shape and by word: `ON` has the knob at the right, a filled knob, a rose outline and the rose wash; `OFF` has the knob at the left, a hollow knob and a dim hairline outline with no wash.

**Vault lost.** When the system has deleted the key (the screen lock was removed or the key was invalidated), a dialog in the same box says `Saved secrets can no longer be read` and `The key that protected them is gone from this device. Your passwords, keys and the AI key have to be entered again.`, with the buttons `Start fresh` and `Not now`, and the line `Nothing else is lost: layouts, history and settings stay.` Until a person chooses, the chip shows `Locked` and the locked answers apply.

![Lock chip in its three states, the unlock sheet, the no-lock notice, the preferences row with its dialog, the switch on and off, the sync popover with its disabled Turn off while locked, and the vault-lost dialog over the paused sync line](design/phone-lock.png)

**File hand-over.** Android only: two palette entries and two rows of the sheet, `Copy a file into the shell folder` and `Copy a file out of the shell folder`. Into shows the system's file dialog and then a line in the notice style, `Copied report.pdf (1.2 MB) to ~/handover/report.pdf`. Out first asks for the path inside the shell folder in a one-line box of the palette's own shape, a prompt and one text field whose label is `File to copy out, from the shell folder` (Enter sends, Escape leaves, an empty answer sends nothing), then shows the system dialog to name the target, then says `Copied report.pdf (1.2 MB) out`. A file larger than 256 MiB is refused before anything is copied, with `report.pdf is 612 MB; the limit is 256 MB`. A name that is already taken gets a number (`report-2.pdf`) and the line says `Copied report.pdf (1.2 MB) to ~/handover/report-2.pdf`; a copy never overwrites. A copy that does not happen for any other reason says one plain line in the same style as the refusal, `Could not copy the file`, with the `!` mark and the underline besides the warn colour; a dialog that is left says nothing. A line stays for about eight seconds or until it is tapped. The shell can read only its own folder, files are handed over one at a time, and there is no folder mirror and no all-files permission.

**Notification.** The notification that Android requires while shells or connections run reads `holzWave is running` with the text `3 shells, 1 connection` (the counts, with the singular at 1; `No shells or connections` is never shown, because the service stops then) and one action `Stop all`, which ends every shell and connection and removes the notification. It is on a low-importance channel named `Running sessions`, with no sound and no badge. Android draws it, so the spec fixes the words only.

![File hand-over: the sheet with its two rows, the palette with its two entries, the confirmation and refusal lines, and the notification](design/phone-handover.png)

### 6.7 Platforms

The same window is drawn everywhere; only the window buttons move (traffic lights on the left on macOS; on Windows and Linux the page draws them, flat glyphs
on the right on Windows and on every Linux desktop that is not GNOME, round buttons on GNOME on the side its button layout names, which may be the left, 6.33), and in a browser there are none (6.30). Shortcut labels follow the platform table in section 7.

| macOS | Windows | Linux | Browser |
|---|---|---|---|
| ![](design/platforms/macos.png) | ![](design/platforms/windows.png) | ![](design/platforms/linux.png) | ![](design/platforms/web.png) |

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
(`Connect: Name (ssh)`) and in the connection manager (`Connections…`), which is the Connections view of 6.3. The connection manager also lists the server (6.34), above the saved connections: a server is a source of sessions and not a saved connection, so it has no palette entry, and the pane of one of its sessions says its state in the same context slot, with the words of that section.

The Login choice of an SSH connection has four radio options, in this order: `Key file`, `Password`, `Agent` and `Code or
password prompt`. `Agent` means that the person's ssh-agent signs the login: the form shows no passphrase field and no
removal row, the key row is labelled `Public key (optional)` and names a public key file that picks one of the agent's keys,
and the hint under the form says "The ssh-agent signs the login. Nothing is stored. The public key file picks one of its
keys." `Code or password prompt` means that the server asks the questions when the person connects (a one-time code, a
password prompt of PAM, a token's challenge) and the person answers them in the dialog of 6.22: the form shows no key row, no
passphrase field and no removal row, and the hint says "The server asks for your answer when you connect. Nothing is
stored." The choice is always explicit, never a fallback from a key file or a password. The selected option has a filled
mark and the others a hollow one, so the choice does not rest on colour.

The Connections form of an SSH connection ends with one checkbox row, `Use wsh on this host`, unchecked by default, and
under it a dim note in the form's hint style: "A program on this host can then open files and rename this pane in
holzWave through a private tunnel. It can never use the AI or approve anything. The host must allow Unix socket
forwarding." The checkbox is the same row as `Remove the saved password` (a box and its words, no colour cue). A host
that is not ticked gets no tunnel, no wrapped shell and no socket. WSL and Docker connections do not show the row.

The palette entry `Install wsh on this host` acts on the connection of the pane that has the focus: it uploads the
bundled static `wsh` to the host's `~/.holzwave/bin`, runs `wsh version` there to check it, and says the result as one line
on the pane, in the warn colour like the other connection lines (`wsh 0.0.1 was installed on Name`, `wsh 0.0.1 is already
installed on Name`, or why not: a host that is not Linux, a processor other than `x86_64` or `aarch64`, a build of holzWave
without the bundled files, a server that refuses the upload). It runs only when the person chooses it, never when a
connection opens and never on a timer, and it works for a pane that runs a saved SSH connection only; for any other pane
it says so on the pane. The connection must also have `Use wsh on this host` ticked for the tunnel to start. There is no
new drawing: the line is the pane's existing notice line.

### Candidates considered

The five earlier candidates stay as the record of the alternatives: [Glas](design/1-glas.png),
[Planke](design/2-planke.png), [Haarlinie](design/3-haarlinie.png), [Welle](design/4-welle.png),
[Papier](design/5-papier.png). The cream *Papier* look remains a candidate for a **light theme** later, because the
terminal palette and the tokens are shared by every chrome style.

## 7. Where Windows, Linux and the browser differ from macOS

Every behaviour that differs from macOS on purpose is in this one table (ROADMAP success criterion 3). The look is the
same on every platform except where a row says otherwise. Windows and Linux were written and cross-checked from a Mac
and have since been run by the author on Windows and Linux machines: a row here says what was built, and
[test-plan.md](test-plan.md) Part 4 is the record of the run. The last rows are about the browser build, where the desktop
window runs in a web page (6.30); they say what the page is specified to do.

| Deviation | Reason | Changed by |
|---|---|---|
| **Own window frame** on Windows and Linux: the system's title bar is gone and the page's bar is the window bar, with the three window buttons drawn in the style of the system (6.33). macOS keeps the overlay bar with the traffic lights. The choice is made per device by the setting `Frame` (`Auto`, `Own`, `System`) | the owner reversed the decision of 2026-10-12 (keep the system's title bar) so that the window has one look and the bar is the only strip above the panes; the system's title bar stays available as the setting `System`, because dragging, resizing and the buttons of a page-drawn bar were never seen on a real Windows or Linux window and have a history of problems under Wayland | the setting `Frame`: `System` brings the system's title bar back |
| **No Windows 11 snap layout flyout** over the Maximize button | the flyout needs a native surface that a web view does not give; dragging a window to an edge and the Windows key with the arrow keys still snap it | none planned |
| **The own frame has never been seen on a real Windows or Linux window by anyone.** Its drag by the free space of the bar, the double click that maximises, the resize edges, the three buttons and their drawn states, the geometry of a tear-off window without a frame and the pointer drop target across two such windows are covered by unit tests, a browser test with a mock window in WebKit and Chromium and compile checks for Windows and Linux, and by nothing else. On Linux the reading of the desktop's button layout through `gsettings` was never run against a real `gsettings`, it is unknown whether `gsettings` answers inside a Flatpak, and its 300 ms deadline is a guess nothing measured | the author works on a Mac, and the owner chose automatic tests only for this change | owner rows for real windows, if the owner adds them later |
| **Under Wayland `Auto` is the own frame, and that was not tried.** The app cannot find out while it runs whether such a window can be moved or resized, and a frame cannot change on an open window, so the choice is made before the window exists; if the window cannot be moved or resized, the person is stuck with it until the setting is changed (the dialog opens from the palette, which the keyboard reaches; the file can be edited with the window closed) | the owner decided that `Auto` is the own frame everywhere, against the advice of the research | the setting `Frame` set to `System` in the Settings dialog (open it from the palette), or `window_frame.json` edited by hand, then a restart (the way back is in [config.md](config.md)) |
| **`F6` and `Shift+F6` move the keyboard between the terminal and the bar** (6.1) on every platform, and a program on the alternate screen keeps them. On a Mac laptop the key is `fn+F6` unless the system's function keys are standard; the palette entry `Focus the bar` is the way that needs no function key. In a browser `F6` is also the key a browser uses for its address bar: the page takes it while it has the keyboard, and whether every browser leaves the key to the page was not tried. **This was tested in WebKit and Chromium with Playwright's keyboard and a stub window, and never on a real keyboard, with a real screen reader, or in the app's own web view on Windows or Linux**; the walk through the bar, the buttons pressed by `Enter` and `Space`, the return by `F6` and `Escape` and the window buttons of a Windows window are covered by the browser test `regions.test.mjs` and the rest by unit tests | the bar was out of the keyboard's reach (a bar button that had the focus sent `Tab` and `Enter` to the terminal); a function key is the one key that types nothing and that no default chord of any platform uses | the chords in the settings, or the palette entry |
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
| **The Windows installer and the macOS app are unsigned; the Linux packages can carry minisign signatures.** The Windows installer is NSIS only and per user; there is no MSI. SmartScreen warns on the first run. The macOS app is signed ad hoc and the cask removes the quarantine attribute | WiX needs an optional Windows feature and cannot carry a pre-release version (plan 02.2-07; [build-windows.md](build-windows.md)); there is no certificate path for the author on Windows; there is no Apple developer account ([signing.md](signing.md)) | signing on Windows and macOS: not planned |
| **The AppImage's portability and the Flatpak's status are what the owner's run says.** The AppImage was built in a container and has been started on Fedora KDE. The Flatpak builds in a container; it has not been started on any desktop, so a run of it is not run until a row of [test-plan.md](test-plan.md) holds a result | the packages are built in a container ([build-linux.md](build-linux.md)) | the owner's run of rows L6 and L10 and the Linux desktop rows |
| **In the Flatpak a local pane starts in the sandbox until the person turns on `Host shell` (6.32); it is off by default.** With it on, a new pane starts on the host through `flatpak-spawn --host`, which removes the sandbox for that pane, and the pane tracks its directory only when the shell announces it (OSC 7), because the process in the sandbox is `flatpak-spawn` and its own directory means nothing. A sandbox pane's directory is read from the process, as everywhere else. The Kubernetes, Talos, Azure CLI and GAM7 integrations stay unavailable in a Flatpak whatever the switch says. Whether a host shell gets a controlling terminal (job control, `sudo`) is an assumption until row L10. **All of it is unverified: no Flatpak has been started on a desktop, and if the Flatpak does not ship it is not part of 0.0.1** | a terminal in a sandbox sees none of the user's tools, and a switch that is off by default asks the person before the sandbox is left; the app ships the permission to run commands on the host, which the person can revoke ([flatpak.md](flatpak.md)) | `host-spawn` if L10 shows no job control; or moving Flatpak out of 0.0.1 |
| **WebKitGTK may need a graphics workaround** (a blank or flickering window, mostly on NVIDIA). holzWave sets none by default | an unconditional override disables the fast path for everybody; owner decision of 2026-10-12: none until a failure is reproduced on hardware ([build-linux.md](build-linux.md)) | a follow-up plan if row L2 finds one |
| **The on-device model row and the group `On device` exist only on macOS 26 or later with Apple Intelligence**; nothing of them is drawn on Windows and Linux | Apple's model exists only there and the bridge to it is not compiled elsewhere; the chat picker and the providers dialog look the same on every platform otherwise (6.21) | none planned |
| **The notice and the group `Local network` exist only on macOS 15 or later**; nothing of them is drawn on Windows and Linux, and `local_network.json` is never written there | the privacy setting `Local Network` is a macOS feature and the probe behind it is not compiled elsewhere (6.24) | none planned |
| **The group `Host shell` exists only inside a Flatpak**; nothing of it is drawn on macOS, Windows, a Linux session that is not a Flatpak, a phone or the browser build, and the file of the choice is never written there | the sandbox exists only in a Flatpak, and the choice is a fact of this computer that is never synced (6.32) | none planned |
| **Android: the Linux chord table and the labels `Ctrl` and `Alt`, for a hardware keyboard.** With one attached the key row is not drawn; without one the key row (6.6) supplies `esc`, `tab`, `ctrl`, `alt` and the arrows that an on-screen keyboard lacks | a phone has no Command key, and the system keyboard is the system's, so its look is not drawn here | none planned |
| **Android: no window tear-off, no window buttons and no drag region.** `Move pane to new window` and `Move tab to new window` are absent, and the bar starts at the left edge | the minimum Android version has no second window of the app, and the system draws and moves the window | none planned |
| **Android: no `wsh`, no Docker or WSL connections, and none of the Kubernetes, Talos, Azure CLI and GAM7 integrations.** Nothing of them is drawn | the programs they call are not on a phone, and a helper that opens files and renames panes needs a shell of the desktop kind | none planned |
| **Android: the biometric prompt and the notification are drawn by Android.** The app fixes its own words and shapes around them (6.6); the prompt and the notification differ by device and Android version | the system owns both, and an app that drew its own would look like a fake prompt | none planned |
| **Android: what the phone and tablet layouts show has been run on an arm64 emulator only; nothing here has been seen on a device.** Soft and hardware keyboards, touch feel and real screen sizes are not run | the author has no Android device, so only what an emulator shows has been looked at | a run on a device, when there is one |
| **Browser: no window buttons and no title-bar drag region.** The browser draws its own bar above the page, and the page's bar starts at the left edge with the first workspace pill | a page cannot draw or move the window it is shown in (6.30) | none planned |
| **Browser: its own chord table, and the keyboard lock in full screen.** The defaults leave out the chords a browser keeps for itself (`Ctrl+W`, `Ctrl+T`, `Ctrl+N`, `Ctrl+Tab`, the same with `Shift`, `Ctrl+PageUp`, `Ctrl+PageDown`); labels use `Ctrl` and `Alt`. The browser table is the Linux table except that `pane.close`, `tab.new` and `workspace.new` sit on `Alt+Shift+W`, `Alt+Shift+T` and `Alt+Shift+N`, because the browser keeps `Ctrl+Shift+W`, `Ctrl+Shift+T` and `Ctrl+Shift+N` (close window, reopen tab, private window). In full screen a Chromium browser is asked to pass those chords and `Escape` to the page (`navigator.keyboard.lock()`), after the person chose `Toggle full screen` in the palette (6.30); other browsers keep their chords. Every chord can be changed in the settings | the browser takes those chords before the page sees them; a keyboard lock exists only in Chromium and only in full screen | a browser that offers a lock outside full screen |
| **Browser: the clipboard is the browser's.** Copy and paste use the asynchronous clipboard API on a user gesture and the `paste` event where the API refuses; a terminal program can write the clipboard (OSC 52) only after a gesture and the existing setting, and can never read it | a browser lets a page read or write the clipboard only on a gesture, and a program in a pane must not be able to read it | none planned |
| **Browser: no tear-off.** One window `main` is mirrored by every browser tab of the person; `Move pane to new window` and `Move tab to new window` are absent | the person's windows live on the server and every tab shows the same set | none planned |
| **Browser: no text-scale signal from the system.** The UI scale is the `ui_scale` setting alone, as on macOS | a web page has no OS text size to read; the browser's zoom is a separate setting | none planned |
| **Browser: no local-network group and no notice.** Nothing of 6.24 is drawn | the privacy setting belongs to the app, and the browser build runs on a server, not on the person's computer | none planned |
| **Browser: the server is `This server`, with no sign-in.** The Connections view lists the server the page was loaded from as `This server` with the state `signed in` and without `Sign in…` and `Sign out`; the choice of the sync target (6.9) is not drawn (6.34) | the browser's own sign-in is the credential, and a server does not sync to a server | none planned |
| **Browser: no `Host shell` group.** Nothing of 6.32 is drawn | the panes run on the server, and a Flatpak is a way to install a desktop app | none planned |
| **Browser: `Reveal in folder` and open-in-folder answer `Not available in the browser.`** and are absent from menus and the palette | the files are on the server, not in the person's file manager | the `Download` action replaces them |
| **Browser: no `file` links.** A link the program marked with a `file` address, and a printed file path, are plain text: no underline, no click, hint mode skips them | a folder cannot be shown from a page, and the file is on the server, not on the person's computer; the other schemes behave as on the desktop (6.31) | the `Download` action |
| **Browser: files are moved by upload and download.** A file dropped on a shell pane is uploaded to the upload folder and its server path is typed into the pane; `Download` in the row menu and the palette fetches a file through a short-lived link | the browser has no path to a file on the server and the server has none to the person's disk | none planned |

### 6.9 Sync

The title bar's right group holds one button for the sync, a word with a cloud: `☁ synced` (green), `☁ syncing…` (blue),
`☁ sync error` (red, underlined) and `☁ sync off` (dim). The word is the cue, the colour the second one. A click opens a
popover under it (a floating layer, 24 rem wide), and so do the palette entries `Sync…` and `Sync now`.

- **Set up** (no account yet): a short sentence on what is kept and that it is encrypted here, a choice of the target
  (below), four rows (folder address, user, app password, passphrase), a note that the passphrase cannot be recovered, and
  `Connect`. The secret fields are never filled in again. A refusal is said in the status line of the popover, in words.
- **The choice of the target.** Above the rows, a radio group named `Transport` with two options, `WebDAV` and `holzWave server`, exactly one
  active (the chosen one has the filled mark, the other the hollow one, as in the login choice of 6.8). The passphrase row stays and is
  shared by both. With `holzWave server` the rows for the folder address, the user and the app password are replaced by `Server address`
  (the address from the sign-in of 6.35, or a dim `–` while there is none) and a state line: `not signed in` with the link `Sign in…`, or
  `signed in`. While the person is not signed in, `Connect` is disabled (a dashed border and the dim colour; the state line is its
  description, as in 6.6). When the server has the sync target off, the state line says `Sync is off on this server.` (warn, underlined,
  `!`) and `Connect` is disabled in the same way. A dim sentence under the choice says what a change of target does: `Changing the target
  keeps your passphrase. Files are not copied from one target to the other; the next run reads the new one.` The choice is not drawn in the
  browser window: a server does not sync to a server. The pictures are in the Sync target view.

![Sync target](design/sync-transport.png)

- **Connected:** title `Nextcloud` (`holzWave server` when that is the target) with a chip (`connected`, `syncing`, `error`), the address and user, then the rows
  `Last sync` (`14:31 · 3 files`), `Encryption` (`end-to-end · passphrase`) and `Conflicts` (`none`, or the count and that
  the other version is kept), the list of what is synced, a line when changes from another computer wait for the next
  start, and the buttons `Close`, `Turn off` (asks with a second click) and `Sync now`. When another computer removed
  files in the last run (a theme or a widget it deleted), one line sits under the list of what is synced, in the dim
  colour of the other notes of the popover: `Removed on another computer: 2. Copies are kept in sync-conflicts.` It is
  shown only while the count is above zero, is the count of the last run and not a running total, and its cue is its
  words.
  With the server as the target the line under the title reads `holz.example.net · signed in`. A run that found the sign-in ended
  (the server refused the token of this device) is not an error: the chip says `not signed in`, the line under the title says
  `holz.example.net · not signed in`, the state line with the link `Sign in…` of the set-up sits under it, there is no error text, and
  the button of the title bar takes the word, the colour and the underline of `☁ sync error` until the next run succeeds. The sentence
  about changing the target is drawn in the set-up only.

### 6.10 History

![](design/history.png)

`Cmd+Shift+H` (`Ctrl+Shift+H`) or `Search the history` in the palette opens a floating dialog built like the palette: a
field, the matching commands below it, a foot with the keys. Matches are fuzzy; the matched letters are rose **and
underlined**. Beside a command: its directory (home as `~`), how long ago, `×n` when it was run more than once, and
`exit n` in words when it failed. `Enter` puts the command on the prompt of the focused pane and closes; it never
runs it. `Shift+Delete` forgets the chosen command, `Clear all` (asks with a second click) forgets everything, `Escape`
closes. The list is the history of the host the focused pane runs on: commands typed in a local pane are found from local
panes, commands typed on a saved connection from panes that run that connection. `^R` at a shell prompt opens this dialog when the user installed the opt-in binding (`wsh init <shell> --ctrl-r`): it
opens for the pane that has the focus and a pick goes to that pane's prompt without running it. Plain `Ctrl+R` stays with
the shell, as section 7 requires, unless the binding is installed. The sidebar's History section (6.11) uses the same search.

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

- **Context:** the tools the person already uses on the command line, between Hosts and History (the phone has no sidebar and
  no section). The head has the label `Context` and, at the right, the hairline button `Profiles…` (it opens the dialog of 6.27; it is not
  drawn at 640 px and narrower) and the button `↻` (`Read the context again`). There is one
  row for each of `Kubernetes`, `Talos`, `Azure` and `GAM`, in that order, and a row is always drawn, so there is never an empty
  heading. A row has two lines. The first is the **mark** (`⎈` Kubernetes, `◇` Talos, and the text marks `az` and `gam`), the
  tool's name in the label style and, at the right, its **words**. The second is the **selection** as mono text: Kubernetes
  the context and the namespace (`prod-eu · payments`), Talos the context and the node, Azure the subscription, GAM the
  section, and `—` when nothing is selected. Every value is a button of the row; the namespace of a Kubernetes row and the
  node of a Talos row are second, smaller buttons of the same line. The values come from the core, which resolves them for the focused pane: the
  pane's own selection when it has one, else the pick of the tool, else what the tool itself is set to. The page formats them
  and decides nothing.
  - **The words.** The page adds only these, and each has a second cue besides its colour: `production` (warn, underlined, with
    the mark `!`; the flag is the core's, never a guess from a name), `not found` (warn, underlined, `!`) with the button
    `Set path` on a line of its own under the selection, `signed out` (warn, underlined, `!`) for Azure, and `stale` with the value dimmed, which says the value is the
    one the tool had when it was last read; `↻` in the head reads again. A tool that cannot be used shows the core's reason in
    one line under its row (warn, underlined, `!`) beside `Set path`.
  - **The list.** A press on a value opens a listbox under the row with the entries of the matching list (contexts,
    namespaces, nodes, subscriptions, sections), the one of an integration block (6.25): the first cells of a row are its
    text, the current value has the wash, a 2 px bar and `●` against `○`, `Up`, `Down`, `Home` and `End` move, `Enter` or
    `Space` chooses, `Escape` closes it and gives the keyboard back to the terminal. A list that cannot be read shows the
    core's sentence in its place. The page shows no value the core did not send.
  - **The reach of a choice.** A choice says what it changed in the line under the section (`role="status"`). When the
    focused pane is an integration block of that tool, or a terminal that was opened for that tool from a row (a shell), the
    choice sets the selection of **that pane** and the line says `Pane 3 now uses prod-eu.` With any other pane focused it
    sets the **pick** of the tool, which new blocks start from, and the line says `New blocks start with prod-eu.` A choice
    writes nothing to the tool's own files and changes no pane but the one named; making a choice the tool's own setting is the
    last entry of the list, `Make default…`, which asks for the card of 6.26 and changes nothing until it is allowed (6.25). A choice the core refuses is its sentence in that line
    (warn, underlined, `!`) and changes nothing.
  - **Shell.** A Kubernetes row has the hairline button `Shell` on a line of its own under the selection. It opens a terminal in a new tab
    with the person's shell and the context of the row (the context and the namespace the row shows, which the core sets for
    that terminal only). While a shell opened from a row is the focused pane, a line under the row says `A shell's own startup
    file can replace this.`, because a startup file of the shell runs after holzWave set the context.
  - **Not found.** `Set path` opens the path field of the block (6.25) under the row: the field `Absolute path of the program`,
    `Set path` and `Check again`; the core checks the path and its sentence, when it refuses one, is the line under the section.
  - **Asking.** The page asks the core for the rows when it starts, when the core says that a selection, the tools or a held
    pane changed, when the focus or the layout changes, on `↻` and every 30 seconds while the window is visible; a window that
    is hidden asks nothing, and nothing asks from a draw. If the core cannot be reached the last rows stay, dimmed.

The **status line** shows the same fact in two segments, drawn only when the core has a value: `⎈ prod-eu/payments` for
Kubernetes and `az holz-prod` for Azure. The value is the one the section shows for the focused pane, so the two cannot
disagree; right after the focus moves, and before the core has answered for the new pane, the segments are dimmed with the
word `stale`. A production selection adds the word `PRODUCTION` to the segment (warn, underlined, with the mark `!`). A segment
is a button (`aria-label` `Kubernetes context prod-eu, namespace payments, production`) that shows the sidebar, brings the
Context section into view and gives the keyboard to its first value; it is as high as the status line and keeps the look of the
other segments (a hairline at its right). A name is machine text, not a label: the context, the namespace and the subscription
are shown with the case the core sent, in mono, and only the word `PRODUCTION` is in capitals.

The **pane header** of a terminal that holzWave gave a context (a shell opened from a row) has a chip between the title and the
context: `⎈ prod-eu`, mono, hairline, round like the other chips, with `production` after it (warn, underlined, `!`) when the
core says so. Its `title` says `Set by holzWave for this pane`. A pane the core did not list has no chip, and a pane whose
header already names its context (an integration block, 6.25) shows it there and has no second one.

![Context section](design/integration-context.png)

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
  no pin or choice nothing is sent: the first send opens the picker and says `Choose a provider first.` On a Mac whose
  on-device model can answer, the list ends with one more row, `Apple on-device` and `system`, with no host (6.21); when
  it cannot answer the row is not drawn and the reason is in the providers dialog.
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
  `reply finished`, `reply stopped` or `reply failed`. A chat that is pinned to the on-device model while it cannot answer
  says `Apple on-device is not available. Choose another provider, or see the provider settings for the reason.` as an error
  line, like every refusal of a send (6.21). It clears when the user types.
- **Composer.** A multi-line field (1 to 8 rows, in the chat text face) and a `Send` button (solid `--hw-rose-deep` with `--hw-on-accent` text)
  that becomes `Stop` (hairline) while a turn runs. Below the field, left: the toggle `Auto` (6.14; it reads
  `● Auto · pane 1` while it is on and `○ Auto` while it is off), then `≈ 12 tokens` (UTF-8 bytes divided by 3, rounded up, the rule of
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
| Auto on, off | rose wash and border while on | `● Auto · pane 1` against `○ Auto`, `aria-pressed` |

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
when it sent any; three groups separated by hairlines, each a fieldset under a `.label` legend, and on a Mac a fourth,
`On device`, between Privacy and Providers (6.21); then the button `Close`. The order is title, status line, notes,
Privacy, On device (macOS only), Providers, Provider, buttons. The group `On device` is read-only: it says whether the
system model can answer and why not, and it has no switch.
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
fields stack, and the rows, options, inputs and buttons are at least 44 px high; input text is at least 16 px. The label of
a checkbox (`Local only`, `Plain http on my local network`) is its target and is as high as the rows: at least 44 px here and
at least 28 px on a desktop. The rules of the chat sheet (6.12) apply.

![AI providers on a phone](design/ai-providers-phone.png)

**Accessibility.** Every control has a visible label; a field with a refusal is `aria-invalid` and `aria-describedby` its
error; the status line is `role="status"`; the list and the option groups are radio groups. Nothing moves, so
`motion-reduce` has nothing to remove. Every hairline is `--hw-line`, so `contrast-high` holds. Under forced colours the
chosen row and the card get a system-colour outline.

### 6.14 Sharing context

The card in which the user shares a pane with the chat, and the toggle `Auto` that sends it. Terminal and file text
reaches a provider only through the consent given in this card, and goes with a message only when the pane is shared or
the core says that sharing is not required (a model on this computer with `Models on this computer › Do not ask`). The
user sees every sent item as a label, a size and a list of what was hidden: as the card before a hand-made offer is sent,
and as the quiet line under the message. The page never holds the text; it sends ids and the core sends the text it approved. A request that a program in a pane makes with `wsh ai` asks in the card of 6.20, with the same words for findings and places.

**The pane.** The chat acts for the last pane the user focused that shows a terminal or a preview (a chat, a file browser
or a monitor does not count). Its number is on the toggle and its number and name are in the card title (`PANE 1 · shell`,
the name cut at 40 characters). With none, the toggle reads `● Auto · no pane`, the message goes without context and the
palette action `Explain error` says `Focus a terminal or preview pane first.` in the status line. When the user focuses
another such pane the card closes and an attached offer is dropped (the first pane stays shared). Every message the chat
sends names this pane, with context or without.

**Auto.** `Auto` is a hairline mono pill at the left of the composer foot, the size of `Providers…`: 28 px high on desktop,
44 px at 640 px and narrower. It is a toggle per chat, on by default; a new chat in the same window starts with the choice
made last and a reload starts it on. The page stores nothing about it. Its text is a filled dot, the word and the pane
(`● Auto · pane 2`, `● Auto · no pane`) or a hollow dot and the word (`○ Auto`); it carries `aria-pressed`, and while it is
on it has a rose wash and a rose border. The dot shape and the words are the second cue; the accessible name is the words
without the dot. A press asks the core nothing and never opens the card, so a mount, a focus or a redraw asks nothing.

With Auto on, a send asks the core for the state of the pane the message names. When the pane is shared, or the core says
that sharing is not required, the page reads every kind the core lists for the pane (selection, visible screen, last
command and output, file, preview) into one offer and sends the message with it; no card appears, and the quiet line
under the message lists what went out. When sharing is needed and not given, the card of mode `ask` opens with the Auto
sentence, the message is not sent, the draft stays and the status line says `Share this pane with the chat to send it
along, or switch Auto off.` `Share pane N` shares the pane, closes the card and the status line says `Pane shared. Press
Send again to send it along.`; the page does not send by itself. `No` closes the card and the draft stays. The card asks
once for each pane and provider; afterwards every send attaches the pane without a new card. Content equal to what the chat
already carried is sent without it and the status line says `This pane shows nothing new since it was last sent, so this
message went without it.` Any other refusal of the core stops the send, keeps the draft and shows the refusal sentence
(6.14 words of the refusals); a pane with nothing to share sends a plain message. Switching Auto off closes a card that
Auto opened. With Auto off nothing is attached.

A card that the person opened by hand (the palette action `Explain error`, 6.19) decides what goes with its message: its
offer is sent as it is, an offer with every item unticked sends a plain message, and Auto adds nothing to it. The pick
list of that card is the only place of `Stop sharing`. The palette action opens the card for the pane the chat acts for
with one item chosen and previewed and shares nothing by itself.

**Modes.** Each mode has words, so colour is never the only cue.

| Mode | Title | Bar | Second cue and content |
|---|---|---|---|
| `ask` | `Consent needed` | warn | the mark `!`; "Terminal content is only sent when you share it." (for a preview pane: "The file this pane shows is only sent when you share it."), the question "Share pane 1 with Local · llama3.2? It goes to localhost:11434 (this computer).", "You still see every item before a message is sent." (when Auto opened the card: "With Auto on, this pane goes with every message you send, and the items are listed under each message. Switch Auto off to stop."); buttons `Share pane 1` (solid) and `No` |
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

**After a send.** The offer is spent and a card closes; the sent message ends with the quiet line `Context sent:` and the
labels, sizes and place, drawn from what the core recorded, never from the page. The share stays in the core, so the next
send with Auto attaches the pane without asking.

**When sharing ends.** The page drops what it knows of the share, and any attached offer, when the provider, the model or
the saved address changes (the picker, or the providers dialog), when the shared pane or the chat closes, with `Stop
sharing`, and when a privacy setting changes. The status line says so (`Sharing ended because the provider changed. Share
again to attach context.`) and the next send with Auto asks the core again. An offer lives five minutes; an older one is never
sent and the status line says `The attached context expired. Add it again.` The page reads a pane only when a message is
sent with Auto on or when a card is opened by hand.

**Local only.** While the switch is on, the place chip of the destination in the card and the chip `local only` in the chat
bar say so; a destination that leaves the computer is refused and the card says why, with `Providers…`.

**Keys and focus.** Opening a card by hand moves the focus to the first control of the card; a card that a send opened leaves the focus in the composer. Closing a card returns the focus to the toggle `Auto` when it was in the card. `Escape`
inside the card closes it and is not passed on; `Space` ticks; `Enter` in the composer sends with the attached offer; every
control carries `data-field`, so a typed character reaches no terminal.

**Narrow screens (640 px and narrower).** `Auto`, every button, every checkbox row and every item row are at least
44 px high, every button is also at least 44 px wide, the card is at most 60 % of the viewport height and scrolls inside, and
the composer text stays at least 16 px.

![Sharing context on a phone](design/ai-consent-phone.png)

**Accessibility.** The card is a `group` named `Share context`, each checkbox has its visible label, the mode word is text,
the status line is the live region, every hairline is `--hw-line` so `contrast-high` holds, and nothing moves. Under forced
colours the card, the place chip and the pressed toggle get a system-colour outline.

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
| `×` | 28 px high and wide; the first press arms it | it turns into `Delete?`, underlined, in the danger colour |

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

### 6.20 Asking from the shell (wsh ai)

A program in a pane can ask the model a question with `wsh ai`, and text piped into it goes along. That is not consent:
every process in a pane holds the same token, so a process can ask and can never answer. Nothing leaves this computer until
the person answers in the window. A request shows as a card in the window of the pane that asked, and only a press on that
card releases it. The request has no tools: the model gets a question and answers in words, and the answer is printed in
the pane that asked. holzWave keeps no copy of the request.

**When a card appears.** Every request asks, unless the destination is on the device or on this computer and the setting
`ai_local_context` is `auto` (6.13); then the answer streams at once and no card is drawn. A destination on the local
network or on the internet always asks, whatever the setting. `Local only` (6.13) refuses a destination that leaves this
computer before any card is shown, and the program is told why. The provider is the one of the last chat turn of this run,
else the only provider that is set up; the command line has no flag to choose one, and holzWave never guesses, so with
several providers and no chat yet the program is told to send a message in a chat first.

**Place and form.** A floating layer without a backdrop, 24 rem wide, at the top right of the window of the pane that asked,
under the title bar and over the stage; at 640 px and narrower a bottom sheet over the full width. It has the look of the
dialogs: `--hw-ground-2`, a hairline `--hw-line`, an 8 px radius and the shadow of the floating layers (the page's only
shadow, so the card takes it from the dialogs' class and adds none). It never takes the keyboard: typing in the terminal goes
on while a card waits, and the terminal stays usable around it.

**Parts, top to bottom.**

| Part | Words |
|---|---|
| Head | a `.label` `wsh ai · pane 3`, and `1 of 2` at the right when more than one request waits |
| Title | `Send this to the model?`; when the destination leaves this computer it starts with the mark `!` in the warn colour |
| Lead | `A program in pane 3 asked this. Nothing has left this computer yet.` |
| `Prompt` | the cleaned prompt in full, in a box that scrolls after about six lines, hidden characters written as `\u{...}` escapes; below it the findings of its cleaning in words; the words `none: the piped text is the question` when it is empty |
| `Piped text` | the counts (`142 lines · 6.1 KiB, 5.8 KiB after cleaning`), the findings of the gate in the words of 6.14 and the sentence of 6.14 that it is best effort and no guarantee; the word `none` when nothing was piped in. The text itself is never shown: an excerpt would put the text into the page |
| `To` | the profile and the model (`anthropic · claude-sonnet`), the host in mono and the place chip: `on device` or `this computer` in ink, `local network` or `cloud` followed by `· leaves this computer` in the warn colour, underlined; the words are the cue and the colour the second |
| Foot | `The answer is printed in pane 3. holzWave does not keep this request. Esc refuses.` |
| Fault line | empty, unless an answer was refused; then the core's sentence, marked `!` |
| Buttons | `Don't send` (left) and `Send once` (right, the solid one) |

The card shows what is sent: the prompt after cleaning is the text that goes out, the destination is the one the core
worked out, and every card carries a seal of what it shows. The press names the request and the seal and nothing else, so a
request that changed since it was shown is refused (`This request changed since it was shown; look at it again.`) and the
card is read again.

![A wsh ai request](design/ai-wsh.png)

**Behaviour.**

- Neither button has the focus when a card appears and nothing moves the focus.
- `Send once` is ignored for the first 500 ms after the card appears and while a press is on its way; `Don't send` is never
  ignored. Only a press that a person made counts: a click or a key that a script made is ignored.
- `Escape` with the focus inside the card refuses.
- While a card waits the palette has the entry `Answer the wsh ai request`; it moves the keyboard to `Don't send`. The entry
  is gone when the last card goes.
- With several waiting requests the oldest is shown and the others are counted in the head. At most four wait; a fifth is
  refused in the program with a sentence. An unanswered request ends after 120 seconds and nothing is sent; a refusal or a
  timeout makes the same pane wait ten seconds before it may ask again.
- After `Send once` the foot reads `Sending…` and both buttons are disabled until the card closes.
- A card closes when the request is answered, when the program that asked left, when the wait ran out, when the pane closed
  and when the app is closing. An answered card leaves nothing behind; the others leave one sentence in the status line.
- A reload brings the cards back: a page that loads asks for the requests that wait for its window.

**States.** Each has words as well as colour.

| State | What is drawn | Second cue |
|---|---|---|
| waiting, cloud | the warn chip `cloud · leaves this computer` | the words and the underline |
| waiting, this computer | the chip `this computer` in ink | the words |
| sending | the foot `Sending…`, both buttons disabled | the word |
| refused | the fault line with the core's sentence | the mark `!` and the underline |
| several waiting | `1 of 2` in the head | the count |

![The states of the card](design/ai-wsh-states.png) The picture draws the four cards a little narrower than the 24 rem of the app so that they fit side by side.

**Sentences.**

| Sentence | Where |
|---|---|
| Send this to the model? | the title |
| A program in pane N asked this. Nothing has left this computer yet. | the lead |
| none: the piped text is the question | the prompt row of a request with no words of its own |
| The answer is printed in pane N. holzWave does not keep this request. Esc refuses. | the foot |
| Sending… | the foot after `Send once` |
| That request is not waiting any more. | the status line when an answer finds nothing waiting |
| The answer could not be sent. | the fault line when the refusal is not the core's |
| The wsh ai request from pane N ended before it was answered. | the status line when the program left |
| The wsh ai request from pane N timed out; nothing was sent. | the status line when the wait ran out |
| The wsh ai request from pane N was withdrawn: the pane closed. | the status line when the pane closed |

**Not built.** A choice of provider on the card, an always-allow or remember-this-pane choice, an excerpt of the piped text,
a sound or a desktop notification, a badge in the title bar (the mark on the tab and the pill of the pane that asked is built, 6.23), a button that shows the pane, and a kept chat or history of
requests. `Send once` sends this one request.

**Narrow screens (640 px and narrower).** The card is a bottom sheet over the full width; both buttons are at least 44 px
high and the text is at least 16 px.

![On a phone](design/ai-wsh-phone.png)

**Accessibility.** The card is an `alertdialog` named by its title, the prompt and the findings are text, the place chip says
where the request goes in words, hairlines are `--hw-line` so `contrast-high` holds (`--hw-ink-3` on the card), nothing moves,
and under forced colours the card and its focus ring use system colours; the keyboard focus ring is 2 px.

### 6.21 On-device model

On a Mac, the system's on-device language model can answer a chat. It is one provider with the fixed id `apple` and the
fixed model `system`; there is nothing to set up: no key, no address, no model name and no type. Its name in every place is
`Apple on-device`. It exists only on macOS 26 or later with Apple Intelligence switched on. Everywhere else it is not
drawn, and on Windows and Linux neither the row nor the group exists at all (section 7).

**Where it appears.**

1. The provider picker of the chat (6.12) has one more row, last, `Apple on-device` and `system`, with no host. It is there
   only when the shell reports the model as available; it is absent, not disabled, when it is not.
2. The providers dialog (6.13) has the group `On device` between Privacy and Providers on a Mac, always, so the reason is
   visible when the row is absent from the picker.
3. The bar of the chat names it like any provider, `Apple on-device · system`, with no arrow and no host. After a turn the
   place chip says `on device`. That word is the core's own and is shown only after a turn, never guessed.

**The group `On device`.** One read-only row. It is not a radio, because there is nothing to choose or edit: the name
`Apple on-device`, the dim meta `system model`, then the state. When the model is available the state is the word
`available` in the ok colour and the line below it is `The model runs on this Mac.` When it is not, the state is the reason
in words in the warn colour, underlined, with the mark `!` before it; one sentence of advice stands below it, and a hairline
button `Check again` reads the list once more. There is no polling: the button is the only way to look again, and the status
line of the dialog says `Checked.` when it has. The words come from two tables of the page, one keyed by the state word and
one by the advice word; the page never shows a word of the shell as it is, and a word that is in no table reads as `other`
(reason) and `unknown` (advice).

| State word | Reason | Advice word | Advice | Tone and second cue |
|---|---|---|---|---|
| `available` | available | `none` | The model runs on this Mac. | ok; the word |
| `apple_intelligence_not_enabled` | Apple Intelligence is switched off. | `enable_apple_intelligence` | Turn it on in System Settings, then choose Check again. | warn; words, underline, `!` |
| `device_not_eligible` | This Mac cannot run Apple Intelligence. | `not_supported` | Choose another provider. | warn; words, underline, `!` |
| `model_not_ready` | The model is not ready yet. | `wait_for_model` | It may still be downloading. Try again in a few minutes. | warn; words, underline, `!` |
| `os_too_old` | This needs macOS 26 or later. | `update_macos` | Update macOS, then choose Check again. | warn; words, underline, `!` |
| `sdk_lacks_framework` | This build of holzWave was made without Apple's model framework. | `not_in_this_build` | Use a build made with Xcode 26 or later. | warn; words, underline, `!` |
| `other` | The on-device model is not available. | `unknown` | The reason is not known. Check again later. | warn; words, underline, `!` |

The shell registers the provider only for the exact state word `available`. A word with another case, a trailing blank or
a name nobody knows never registers it and never makes the row selectable. The state is asked only when a person acts: when
the list of providers is read, when a send or a context call names the model, and when a `wsh ai` request needs a provider.
There is no timer and nothing is asked at launch. A model that was available and then is not refuses the next turn by itself.

**Asks first.** The model runs on this computer, so it is covered by the choice `Models on this computer` of 6.13: it asks
before it sees terminal text unless the person switched that off. It declares no tools, so a chat with it can only talk.

**No fallback.** When a chat is pinned to the model and the model is not available, a send does nothing but say `Apple
on-device is not available. Choose another provider, or see the provider settings for the reason.` in the status line,
open the picker when another provider exists, and keep the draft. No other provider is used and no id is replaced. A send
that lost the race with a state change is shown as the core's refusal and goes nowhere else.

**Narrow screens (640 px and narrower).** The picker row and the group follow the sheet and 44 px rules of 6.12 and 6.13.
There is no phone drawing, because the model exists only on macOS.

**Accessibility.** The group is a `role="group"` labelled by its legend. The state word carries `data-state` and
`data-tone`. `Check again` has a visible label and the 2 px focus ring; when the state turns available while the button has
the focus, the keyboard moves to the row of the group. Hairlines use `--hw-line`, so `contrast-high` holds; under forced
colours the row gets a system-colour outline. Nothing moves.

![On-device model](design/ai-device.png)
![On-device model states](design/ai-device-states.png)

**Not drawn.** A model list, a download button, a switch for Apple Intelligence and a link into System Settings: holzWave
cannot switch the model on, and says where the person can.

### 6.22 Login prompt

A server that logs a person in with a question (a one-time code, a password prompt of PAM, the challenge of a hardware
token) is reached through the `Code or password prompt` login of 6.8. When such a server asks, the page shows this dialog.
It is the page's own dialog (the floating layer of 6.3, the title style of the Settings dialog, the buttons of every
dialog); nothing in it is new except the bordered block that holds the server's words.

**When it appears.** Only when a person opened a pane or a file browser on the connection. A monitor, a command of the
assistant or any other background use of the connection never raises it: they say that the connection asks for a login code
and that a pane must be opened on it first. One login serves the pane, the files and the commands of a connection, so a
person answers once.

**What it holds, top to bottom.**

1. The title `Login prompt from NAME`, and under it the line `Host: HOST`. Both come from the person's own saved connection,
   looked up by its id in the core. The server never writes either.
2. The fixed sentence `This text comes from the server, not from holzWave.` in the dim note style. It is always there.
3. A hairline block with the legend `The server says`. In it, in this order: the server's name for the round in bold, its
   instructions as plain text with the line breaks the core kept, and one row per question: the question's text as the label
   and a field. A question whose server flag says that the answer is not echoed is a masked field (`type="password"`) with
   the word `hidden` after it; one that echoes is a plain field with the word `shown`. The words are the second cue; the
   field's look does not depend on colour. At most five questions are shown, and every text is already cleaned and bounded by
   the core (at most 60, 300 and 200 characters; control, direction and zero-width characters removed).
4. The line `Your answer goes to this login only. Nothing is stored.`
5. The buttons `Cancel` and `Send` (the solid one), at the right.

**The server's text is untrusted.** It is set as text, never as markup, and it is never a title. holzWave asks for no other
secret in this dialog: a text that says it is a holzWave question is the server's, and the fixed sentence above it says so.
The dialog never remembers an answer, offers no `remember` choice and never fills a field in.

**Keys.** The first field has the focus when the dialog opens. `Tab` and `Shift+Tab` move through the fields and the
buttons and stay in the dialog. `Enter` in the last field sends, in an earlier field it moves on to the next. `Escape`
cancels, and so does `Cancel`. The dialog has `role="dialog"`, `aria-modal`, a label that names the title and fields whose
labels are tied to them. When the dialog closes, the focus returns to where it was.

**How it ends.** `Send` gives the answers, in the order of the questions, to the core and the dialog goes. `Cancel` and
`Escape` end the login and the pane says so and waits for `Enter`. When no answer comes in two minutes the core ends the
login the same way and tells every window, and the dialog goes without being asked. A second question that arrives while one
is open waits and is shown after the first has ended. The fields are emptied and removed whenever the dialog goes.

**Accessibility.** Hairlines use `--hw-line` and the dim text `--hw-ink-2` under `contrast-high`; the 2 px focus ring holds
for every field and button; nothing moves under `motion-reduce`; under forced colours the block and the fields keep their
borders. Every size is in `rem`.

![Login prompt](design/login-prompt.png)

**Not drawn.** A `remember this code` choice, a countdown, a second dialog for a second question at once, and any way to
answer a question without the dialog.

### 6.23 Attention marks

When a pane that is out of view needs the user, the tab and the workspace pill that hold it carry a small mark, so the
person does not open every tab to find the AI question, the finished command or the program that rang the bell. The mark
sits **inside** the pill and the tab; it adds no fourth level and changes neither shape: the workspace is still a round
pill with a diamond, the tab is still square and numbered, a pane is still a tile.

**When a mark shows.** Only for a pane that is out of view: a pane whose tab is not the shown tab of the shown workspace.
Every pane of the shown tab is in view, an unfocused split included, so no pane-level mark exists. The pill of the shown
workspace carries no mark (its tabs do), and the shown tab carries none. The focus of the operating-system window is not
considered: a bell in a pane that is in view marks nothing, also while the app is in the background.

**The four kinds.** One glyph of the bundled mono face per kind, in a status token and never in rose (rose is the colour of
the shown tab in the same row and would read as "shown"). The shape and the words are the second cue; colour is the third.

| Kind | Glyph | Token | Words | Source |
|---|---|---|---|---|
| ask | `?` | `--hw-info` | waits for you | an approval card, a consent card in mode ask, a `wsh ai` request that waits |
| bell | `▲` | `--hw-warn` | rang the bell | BEL in the output of the pane |
| failed | `!` | `--hw-danger` | command failed | an OSC 133 `D` mark with a status other than 0 (the exclamation mark is the design's error mark, 6.17) |
| done | `✓` | `--hw-ok` | command finished | an OSC 133 `D` mark with status 0 or no status |

There is no `x` shape: the close mark `×` sits next to the mark.

**Where the kinds come from.** The bell is BEL in the output of the pane. A command that ended is the `D` mark of OSC 133
that the shell hooks write when a command that ran ends (an empty line writes none, and a shell without the hooks writes
no mark, so no mark appears); the mark carries the exit status, 0 or none is a finished command, any other number a failed
one, and a status that is not a whole number counts as none. There is no minimum duration. The question mark comes from
what the window already knows: a tool call that waits for approval in a chat pane, a consent card in mode ask, a `wsh ai`
request that waits (it marks the pane that asked). The page closes a consent card when the terminal it asks about leaves the
view, so in practice that card does not wait out of view; the approval card and the request do. Only the pane and the status
number cross into the page, never the command or its output.

**Several kinds at once.** A tab or a pill shows **every kind that is present, side by side, one symbol per kind**. There is
no count and no "most urgent wins". The fixed order from left to right is ask, bell, failed, done, so the place of a symbol
is itself a cue. A tab that holds a waiting question and a finished command shows `? ✓`; one that holds all four shows
`? ▲ ! ✓`. The title and the accessible name of the tab or the pill list every kind present in words, in the same order.
A pill shows the kinds of all the tabs of its workspace together, once each.

**Width.** The marks never shrink and never wrap: each is one glyph of the bar's own size with a small gap, so all four
take 3.4 rem with their padding. The tab gives way to them the way it already gives way to the host label, by shortening its
own name (the tab keeps its minimum of 3 rem for the name, and its own minimum grows by the width of the marks, so the
marks and the close mark are never clipped); the pill grows by the same 3.4 rem and keeps its 22 px height. The item
stays 28 px high on a desktop (44 px on a phone), the mark is inside the item, so the target does not change. With all
four kinds present nothing overlaps the name or the close mark.

**Where it sits.** After the name and before the close mark `×`, in the pill and in the tab. Bold mono text of the bar's own
size, no box, no fill, no animation. It is hidden when empty.

**Title and accessible name.** The tab's title reads `LABEL · WORDS` (`2 holzkube · rang the bell`), its accessible name
`LABEL, WORDS`; several kinds are joined with a comma in the fixed order (`waits for you, rang the bell`). Without a mark
both are the label alone.

**Not a control.** The mark has no setting and no keyboard stop: a press on it does what a press on its tab (or pill) does,
and the tab is the stop. A switch to turn marks off is not built.

**What clears it.** Latched against level: a bell, a finished command and a failed command stay on the pane until its tab is
shown, or the pane exits or leaves the window; showing the tab clears them. The question mark is not latched: it exists
exactly while a card waits and the pane is out of view, so it returns when the person shows the pane and leaves again before
answering, and it goes for good when the card is answered or withdrawn.

**Floods.** A program can ring the bell or end a command as fast as it can write. A run of bells is folded in the engine,
each pane lets at most one bell and one command end through per 500 ms, and the page keeps one set of kinds per pane, so a
flood leaves one mark.

**Accessibility.** The shape and the words are the cues; the colour is a status token of the theme, which measures 7.38:1 to
11.27:1 on the ground (section 2), so `contrast-high` needs no rule of its own. The mark never moves, so `motion-reduce` has
nothing to remove. Under `forced-colors` the browser gives it the system text colour and the glyph shape stays the cue.
The bar buttons stay 28 px high because the mark is inside the item.

**On a phone.** (Design only; the app has no phone layout yet.) The mark follows the name in the bottom tab strip and in
the rows of the sheet, in 44 px rows, and the `Sheet` opener shows the kinds of anything out of view, once each.

![Attention marks](design/attention.png)
![On a phone](design/attention-phone.png)

**Not built.** A sound, an operating-system notification, notifications from OSC 9 and OSC 777, a switch to turn marks off,
a count, a mark on a pane header, the chat in the sidebar column (it has no pane; the `✦ AI` button of the title bar covers
it), and a mark for a finished reply of the assistant (whether it is a question cannot be known, and marking every reply
would make the mark mean nothing).

### 6.24 Local network on macOS

On a Mac with macOS 15 or later, the system keeps a privacy setting `Local Network` for every app: a program that connects to
another computer on the local network (an SSH host, an AI server such as Ollama or LM Studio on another computer, a
Nextcloud on the LAN) is blocked until the person allows it. holzWave tells the person why before macOS asks, and shows the
state in the Settings. On macOS 13 and 14 the system has no such setting and never asks, so nothing here is drawn. On Windows
and Linux neither the notice nor the group exists at all (section 7). There is no phone drawing, because the feature exists
only on a Mac.

**The rule.**

1. The notice is shown **once per computer**, at the first start, in the first window only (the window labelled `main`; a
   torn-off window never shows it). The choice is kept in `local_network.json`, a file of this computer that is never synced:
   another Mac must ask for itself.
2. `Allow` is the only thing that makes holzWave touch the local network, and so the only thing that makes macOS show its own
   question. Nothing reads or probes the network before it was pressed: until then the state is `not asked yet`.
3. `Not now`, Escape and a click outside end the notice for good on this computer. Nothing asks again by itself. The way back
   is the button `Allow` of the group below, which stays while the state is `not asked yet`.
4. A denial is shown only by a read of the state (the dialog opening, the window gaining focus, `Check again`), never right
   after `Allow`: macOS may refuse an operation before the person has answered its question, so the line after `Allow` says
   that the question is open and asks the person to read again. There is no timer.

**The notice.** A floating layer with the box of the Settings dialog: a hairline, the 8 px radius, the one shadow of a floating
layer, 30 rem wide, the title style of the other dialogs. Text only, top to bottom:

- Title `Allow local network access?`
- Lead `holzWave reaches computers on your local network:` and a bulleted list: `SSH to machines on your network.`,
  `Local AI servers on other computers, such as Ollama or LM Studio.`, `Sync with a Nextcloud on your network.`
- Note `macOS asks for your permission next. Programs you run in a pane, such as ssh, count as holzWave. You can change this
  any time in Settings.` A server on this Mac is loopback and needs no permission, which is why the list says "other computers".
- The buttons `Not now` (hairline) and `Allow` (the solid one, focused first), at the right. A status line above the buttons
  stays empty unless `Allow` could not be stored; it then says the reason in the warn colour and the notice stays.

Tab moves between the two buttons, Enter presses the focused one, Space too, Escape is `Not now`. A Cmd chord is swallowed and
no key reaches the terminal while the notice is open. While `Allow` waits for the answer (up to about 3 s) the two buttons ignore
presses. When the notice closes the keyboard goes back to the terminal.

**The group `Local network`.** In the Settings dialog, after Accessibility and before the buttons, on macOS only (and removed
when the shell says the system has no such setting). A `fieldset` with `role="group"` labelled by its legend, in the manner
of the group of 6.21: one row with the name `Access`, the dim meta `Privacy & Security`, the state at the right, one sentence
of advice below it and the buttons. The state is words first; a state that needs attention is also underlined or marked.

| State word | Shown as | Tone and second cue | Advice |
|---|---|---|---|
| `allowed` | allowed | ok; the word | holzWave can reach computers on your local network. |
| `denied` | denied | warn; the word, an underline and the mark `!` | macOS blocks holzWave from your local network. Turn holzWave on in System Settings, then choose Check again. |
| `not_asked` | not asked yet | plain; the word | macOS has not been asked. Choose Allow and macOS shows its own question. |
| `unknown` | not known | plain; the word and the mark `?` | holzWave could not tell. macOS may still be waiting for your answer. Choose Check again. |

The buttons: `Open System Settings` always (it opens the Privacy & Security pane of System Settings through the system's own
opener, with no text of the page); `Allow` only while the state is `not asked yet`; `Check again` while it is `denied` or
`not known`. A line of the group (`role="status"`) says `Checking…` while a read runs, `Checked.` after `Check again`, and after
`Allow` `Asked. Answer the macOS question, then choose Check again.`; a refusal of a button is said there in words. The page
shows no state word until the first answer. The state is read when the Settings dialog opens, when the window gains focus while
the dialog is open (at most once a second) and after a button; never by a timer. When the pressed button goes away (`Allow`
becomes `Check again`, or `Check again` goes when the state turns allowed) the keyboard moves to the row. The words come from a
table of the page keyed by the shell's state word; a word that is in no table reads as `unknown`.

**Limits a reader must know.** Reading the state is itself a local network operation, so the page reads only after `Allow`
(a state read before that answers `not asked yet` without touching the network). The system can refuse an operation before the
person answered its question, so `denied` is reported only by a read. An ad hoc signed build may be asked about again after an
update, and the question then comes without the notice; the text of the question is the one of the app's `Info.plist`. A
program started from Terminal is exempt from the setting, which is why no test run from a terminal can see it.

**Accessibility.** Hairlines use `--hw-line`, so `contrast-high` holds; the 2 px focus ring holds for every button and for the
row; under forced colours the row gets a system-colour outline and the box keeps its border; nothing moves, so `motion-reduce`
has nothing to remove. Every size is in `rem`; the buttons are at least 28 px high.

![Local network notice](design/local-network.png)

![Local network states](design/local-network-states.png)

**Not drawn.** A switch that grants or revokes the permission (holzWave cannot change it; it says where the person can), a list
of hosts, a badge, a notification, and a notice again at a later start.

### 6.25 Integration blocks

A tool the person already uses on the command line (`kubectl`, `talosctl`, `az`, `gam`) can be read in a pane of its own
kind. holzWave does not reimplement the tool: the core runs it with an argument list that it builds itself, cleans what it
printed and sends a **view model**, a table, a key-value list or a text. The pane draws that model and nothing else. It is a
pane like the monitor (6.5): the same header strip, flat and hairline, mono, no shadow. This section covers the block; the
confirmation card of a verb that changes something, the Context section of the sidebar and the profiles dialog are drawn in
the sections that build them.

**How a block is reached.** The New block picker of 6.3 is drawn but not built, so the command palette and the sidebar's
Context section (6.11) are the ways in. The palette has one entry for each view that each tool lists, titled `<tool>: <view>`
with the labels the core sends (`Kubernetes: Pods`, `Talos: Machine status`, `Azure: Subscriptions`, `GAM7: Users`), and one for
each terminal that needs no argument (`Kubernetes: Shell with the current context`, `Talos: Dashboard`, `Talos: Kernel
messages`, `Azure: Sign in`). A view opens as a block in a new tab. A terminal that only reads opens at once in a new tab; one
that is not read-only asks first through the card of 6.26. A verb that needs a value (`Logs`, `Exec in pod`, `Delete pod`) or
that changes something has no palette entry: it is a button of a row or of the Context section, so that its value and its
card come with it. A tool that is not installed keeps its entries, and the block they open says so and offers the path field.
The entries come from the core's list of tools, so a tool added later brings its own and nothing is drawn for it here. Two
actions, `integration.context` (`Show the Context section`) and `integration.profiles` (`Integration profiles…`), are in the
palette and in the key binding editor with no default chord, so a person can give them one. A phone has neither the palette
nor these entries.

**The pane.** Header `pods` (the id of the shown view, with underscores as spaces, in the label style: `contexts`, `nodes`,
`machine status`), context in mono `Kubernetes · prod · payments · 10 s`: the tool, the selection of this pane and, for a
view that refreshes by itself, the interval. Before the core has answered, the context is the tool's name alone. The view is
changed from the core (a palette entry or a row action of a later section), never by the block itself.

**Production.** When the core says that the selection is a production one, the context text of the header is in the warn
colour, underlined and has the mark `!` before it, and the bar shows the word `production` (warn, underlined, `!`) beside the
context chip. The flag is the core's; the page never decides it from a name.

**The bar.** One row under the header, hairline below it. From the left: one chip for each selection the tool has, showing
the current value (`context prod-eu`, `namespace payments`; Kubernetes has a context and a namespace, Talos a context and a
node, Azure a subscription, GAM a section), then the production word when it applies, then at the right `↻ Refresh`. A chip
is a button with `aria-haspopup="listbox"`. A press opens a listbox under the chip with the entries of the matching list
(`contexts`, `namespaces`, `nodes`, `subscriptions`, `sections`): the first cells of a row are its text, the current value has
the wash and a 2 px bar, and the mark `●` against `○`. Choosing an entry sets the selection **of this pane only** and reads
the view again; the list closes and the focus returns to the chip. A list that cannot be read shows the core's sentence in
place of the entries. The page never shows a value that the core did not send.

**The body** is one of three shapes, every string in it set as text:

- A **table**: a header row in the label style, one row for each entry separated by hairlines, the cells in mono, a cell
  cut at 240 characters with an ellipsis (the whole text is its `title`). A row with the tone `dim` is quiet. A row with the
  tone `warn` or `danger` gets the mark `!` before its first cell, and its cells are in the status colour; the status word
  in the cell is the second cue, and `danger` is also underlined. The selected row has the rose wash and a 2 px rose bar.
  Row actions are buttons at the end of the row, hairline, the word is the core's label (`Logs`). An action that changes
  something or opens a session (`Exec…`, `Delete…`, `Use context…`) has `…` after its label and the `title` `Asks for
  confirmation`: it opens the card of 6.26 and nothing runs before that card is allowed. A table is `role="table"` and the
  rows `role="row"`.
- A **key-value list**: the label in the dim colour, the value in mono, one pair for each line, the same tones.
- A **text**: preformatted, wrapping, mono, scrolling inside the pane.

**States, by words.** Each has a word and a second cue besides colour, and lives on the `role="status"` line under the body,
or in the body where it replaces the rows.

| State | Words | Second cue |
|---|---|---|
| waiting for the first answer | `Reading …` | the word; no rows |
| tool not found | the core's sentence (`kubectl was not found on this computer. Install it or set where it is.`), the places that were searched, a field for the path, `Set path` and `Check again` | warn, underline, the mark `!` |
| tool unusable | the core's reason (`kubectl cannot be used: writable by everyone.`) | warn, underline, `!` |
| signed out, or any other state of the account | drawn as the model the core sends, with its own rows; the page does not interpret the word | the rows |
| a read failed | `! ` and the core's one sentence; the last rows stay, dimmed, until a good answer | warn, underline, `!`, dimming |
| the list was cut | the core's note under the table (`Showing the first 2000 of 2500 rows.`) | the words |
| nothing to show | `Nothing to show.` | the words |
| a good answer | `Updated just now`, `Updated 12 s ago`, `Updated 3 min ago`; the age advances once a second | the words |

The path field is a text field of the page: it keeps its own keys and `Escape` gives the keyboard back to the terminal. A
path the core refuses is shown as the core's sentence on the status line. The field sends the path to the core and to nothing
else.

**Refresh.** A view that the core says refreshes by itself (the Kubernetes pods, every 10 s) is read again on that interval
**only while the window is visible and the block is shown**, never with two reads on the way at once, and not after the pane
is closed. Every other view reads when it opens, when the selection changes, on `↻ Refresh` and on the key `r`. A refresh
asks the core; the block builds nothing. A message from the core that a selection or the tools changed (another window
chose, a program was set) carries no data and only makes the block ask again.

**Keys.** While the pane has the focus: `Up`, `Down`, `Home` and `End` move the selected row, `r` refreshes. In the listbox:
`Up`, `Down`, `Home` and `End` move, `Enter` or `Space` chooses, `Escape` closes it and gives the keyboard back to the terminal. `Escape` in the path field returns the focus to the
terminal. All other keys stay with the terminal and the shortcuts.

**Accessibility.** The status line is a `role="status"` region; the chips and buttons have their words as their names. A
state and a tone are `data-state` and `data-tone`, a dimmed table is `data-stale`. Hairlines use `--hw-line` and dim text
`--hw-dim`, so `contrast-high` holds; the 2 px focus ring holds for every chip, button and the field; under `forced-colors`
the warn marks and the selected row get a system-colour outline; nothing moves, so `motion-reduce` has nothing to remove.
Every size is in `rem`; chips and buttons are at least 28 px high on a desktop and 44 px at 640 px and narrower.

**Text from tools is untrusted.** A pod, a context or a domain is named by other people. Every string in the block is the
core's cleaned text and is set as text, never as markup: a name that looks like an element is shown as those characters.
The page sends the core facts only: the tool, the view, the pane, the argument values of a row's own action and a value that
was chosen from a list the core sent.

![Integration block](design/integration-block.png)

![Integration states](design/integration-states.png)

**Make default.** The list under a chip of a tool that has a Local verb for that kind of value (the verb whose first
argument is of that kind: Kubernetes context and namespace, Talos context, Azure subscription; the GAM section has none) ends
with the entry `Make default…`, after a hairline and without a `●`. It asks for the confirmation card of 6.26 for that verb with the value
the pane has now, so the tool's own setting becomes what this pane reads. It is not drawn when nothing is selected, and
choosing it changes nothing until the card is allowed. The Context section of the sidebar (6.11) has the same entry.

**Not drawn.** A profiles dialog, a choice of interval, a column sort, a filter field and a log viewer (a log opens in a
terminal pane).

### 6.26 Approving an integration action

The card in which the person sees what an integration will run before it changes something or opens a session with their
credentials, and allows it once or denies it. It is of the family of the approval card (6.15) and the shell request card
(6.20): flat, hairline, a 2 px bar, one solid action, and it never takes the keyboard. The core enforces the rule: it
prepares the card from the request, spells every argument, names the target and the guard, and binds all of it to a hash; only
a confirm that repeats the card's ticket and hash runs anything, the core resolves the request again at that moment, and a
ticket is good once. The page only shows what the core sent and relays a person's press. It does not summarise, shorten,
reorder, merge, translate or classify a field, it keeps nothing for later, and the only things it sends are the ticket and the
hash it was given. It builds no argument, name, program or hash.

**When a card appears.** For every action that is not a plain read: a **Local** verb (changes the tool's own configuration:
`Use context…`, `Make default…`), a **Write** verb (changes a cluster or an account: `Delete pod…`) and an **Exec** verb (opens
a session with your credentials: `Exec…`). A verb that only reads has no card. Nothing runs, and no pane opens, until the card
is allowed.

**Place and form.** A floating layer without a backdrop, 24 rem wide, at the top right of the window under the title bar, in
whichever pane the action started and whichever tab is shown, so a tab switch cannot hide it; at 640 px and narrower a bottom sheet
over the full width. It has the look of the dialogs (`--hw-ground-2`, a hairline `--hw-line`, the radius and the shadow of the
floating layers, which is the page's only shadow). A bar of 2 px runs along its left edge. Typing in the terminal goes on while a card waits. A second card that arrives
while one is shown waits behind it and is counted in the head (`1 of 2`); the core holds at most eight.

**Parts, top to bottom.**

| Part | Words |
|---|---|
| Head | a `.label` `Confirm needed` with the mark `!`, and `1 of 2` at the right when more than one card waits |
| Title | the tool and the verb as the core's tool list names them: `Kubernetes · Delete pod` (the core's ids when the list does not know them) |
| Effect line | one sentence for the effect of the verb (see below) |
| Target | one row for each pair of the core's target in the core's order: the name (dim, mono) and the value (ink, mono, wrapped, never cut): `context prod-eu`, `namespace payments`, `pod api-0` |
| Arguments | a hairline box in mono with one row for the program (`program kubectl`) and one for each argument (`arg 1 delete` to `arg 9 api-0`), in the order the core spelled them, each element on its own row; at most 20 rem high, it scrolls inside and takes the focus with `Tab` |
| Guard | only for a production context: `Production context: Production EU.` in the warn colour, underlined, with the mark `!` and the word `production`; the name is the core's |
| Hash | `hash 9f3a41c2d7be`: the first 12 characters, dim, mono |
| Line | a live region (`role="status"`) for the sentence of the mode (armed, sending, done, ended, changed) |
| Fault line | empty, unless the core refused the press; then the core's sentence, danger, underlined, marked `!`, above the buttons |
| Buttons | `Deny` (left, hairline) and `Allow once` (right, solid `--hw-rose-deep`, `--hw-on-accent` text) |
| Foot | `Allows this one action only. Nothing is kept for later. Esc denies.` |

Control, bidirectional, zero-width and tag characters in a name were written as escapes (`\u{202e}`) by the core and are shown
as they came; nothing is dropped, and a long value wraps.

**Effect lines.**

| Effect | Words |
|---|---|
| `write` | Changes a cluster or an account. Check the target below. |
| `local` | Changes the tool's own configuration. It writes: the core's `changes` sentence (`the kubeconfig files named by KUBECONFIG, by default ~/.kube/config`). The `changes` pair is shown here and not a second time among the target rows. |
| `exec` | Opens a session with your credentials. Check the command below. |

**Production.** When the core's guard names a production context the guard line is drawn and the card needs **two presses**. The
first press on `Allow once` (after the half second) arms the card and sends nothing: the bar turns danger, the button reads
`Press again to allow on production`, and the line says `Armed. The next press allows this on production. Nothing has run
yet.` so that assistive technology announces it. The second press sends the confirm, and only when half a second has passed since the first (a double click arms the card and
cannot allow it). `Deny`, `Escape` or the card ending
disarms it, and nothing is remembered: the next card, also for the same action, starts unarmed. A card that stays armed is not
timed out by the page; it ends with its ticket after 120 seconds.

**Modes.** Each mode has words, so colour is never the only cue. `data-mode` holds the mode.

| Mode | Bar | Second cue and content |
|---|---|---|
| `ask` | warn | the mark `!` in the head, both buttons |
| `armed` | danger | the guard line, the button `Press again to allow on production`, the line `Armed. The next press allows this on production. Nothing has run yet.` |
| `sending` | hairline | the line `Allowed · running…`, both buttons disabled, `aria-busy` |
| `done` | hairline | the line `Done: <the core's one-line note>` for a verb that ran, `Opened pane 5` for an Exec verb (the layout of the core is applied and the new pane appears), no buttons; the card leaves after a few seconds and the status line keeps the sentence |
| `ended` | hairline | the line `That action is no longer waiting.` for a ticket the core does not know, that expired or that was answered, no buttons |
| `changed` | warn | the line `The request changed. Nothing ran. Read the card again.`; the page asks the core for a new card for the same action and draws it; the half second and the two presses start again |
| `cannot` | danger | the mark `!`, the title `Cannot show this request`, the sentence `This request could not be read in full. It can only be denied.`, only `Deny` |

A refusal of any other kind (a tool that cannot be started, a failed run) keeps the card with the core's sentence in the fault
line; the card is unarmed again and a new decision is needed. A card the page cannot read (no ticket, no 64-character hash, a field list
of the wrong shape, more than 64 fields, a value over 8192 characters, or members that do not agree with the fields the hash
covers) is `cannot`.

**Keys and focus.**

| Key or press | Effect |
|---|---|
| a card appears | nothing: it never takes the focus; the status line says `waiting for your decision` |
| `Tab` | reaches the argument box, `Allow once` and `Deny` in this order |
| `Space`, `Enter` on a button | decides |
| `Escape` with the focus inside the card | denies (the ticket is cancelled) and returns the focus to the terminal pane that had it; `Escape` elsewhere does nothing to the card |
| a press on `Allow once` within half a second of the card appearing, also of a card drawn again after a changed request, and a second press on an armed card within half a second of the first | ignored (a click aimed at what moved cannot allow an action) |
| a click or key press made by a script | ignored; `Deny` works at any time for a real press |
| the palette entry `Answer the integration request` | moves the keyboard to `Deny`; the entry is there only while a card waits |

No key allows an action without the focus on its button.

**A held terminal.** A terminal that an Exec verb opened and whose program ended stays open with the program's last output.
Its header says so: the dot is hollow with a dash, and the context slot reads `exited 1` (the status the core gives; `exited`
when it gives none) in the warn colour, underlined, with the mark `!`. The terminal line itself comes from the core. A pane the core does not list as held has the header it always had.

**Sentences.**

| Sentence | Where |
|---|---|
| Confirm needed | the head |
| Kubernetes · Delete pod | the title |
| Changes a cluster or an account. Check the target below. | the effect line of a Write verb |
| Changes the tool's own configuration. It writes: … | the effect line of a Local verb |
| Opens a session with your credentials. Check the command below. | the effect line of an Exec verb |
| Production context: Production EU. | the guard line |
| Allow once, Press again to allow on production, Deny | the buttons |
| Allows this one action only. Nothing is kept for later. Esc denies. | the foot |
| Armed. The next press allows this on production. Nothing has run yet. | the line of an armed card |
| Allowed · running… | after the press, until the core answers |
| Done: pod "api-0" deleted | a verb that ran, with the core's note |
| Opened pane 5 | an Exec verb |
| That action is no longer waiting. | an ended card |
| The request changed. Nothing ran. Read the card again. | a request that changed since the card was drawn |
| Cannot show this request / This request could not be read in full. It can only be denied. | a card the page cannot read |
| Denied. Nothing ran. | the status line after `Deny` |
| exited 1 | the context slot of a held terminal |

![Integration card](design/integration-confirm.png)

![Integration card states](design/integration-confirm-states.png) The picture draws the four cards a little narrower than the 24 rem of the app so that they fit side by side.

**Not built.** An `Always allow`, an allow for the session or for a pane, a setting that carries an approval, a remembered
choice, a timer that arms or disarms a card, a sound or a notification, and a card for a verb that only reads.

**Narrow screens (640 px and narrower).** The card is a bottom sheet over the full width; both buttons are at least 44 px high and
the text is at least 16 px.

**Accessibility.** The card is an `alertdialog` named by its title and described by the effect line, the line is the live
region, `aria-busy` while sending, hairlines are `--hw-line` so `contrast-high` holds (`--hw-ink-3` on the card), nothing
moves so `motion-reduce` has nothing to remove, the focus ring is 2 px, and under forced colours the card, the guard line
and the focus ring use system colours.

### 6.27 Integration profiles

The dialog in which a person keeps what holzWave remembers about one connection of an integration tool (6.25): which tool, which
context, namespace, node, subscription or section it pins, whether it is a **production** context, whether it keeps a
configuration folder of its own, the plain environment variables the tool is started with and the *names* of the secret
variables whose values live in the keychain. It looks and keys like the AI providers dialog (6.13) and does nothing else: it
runs no tool, reads no cluster and tests no connection. The core owns every rule: it validates the profile, refuses what it
does not allow and says why in a sentence, and the dialog shows that sentence. The page checks only how long a field is, and it
builds no name, argument list, path or id that the person did not type, except the id of a new profile (below).

**What a profile is, and where it goes.** The profile file holds names and flags and is synced between the person's computers.
Secret values are kept in the keychain and travel only inside the encrypted secrets of the sync. A path of this computer (the
program of a tool, a configuration folder) is never part of a profile. The dialog says so in one note under the groups.

**Way in.** The hairline button `Profiles…` in the head of the sidebar's Context section (6.11), left of `↻`. It is not drawn
at 640 px and narrower, and a phone has no Context section, so the dialog has no way in on a phone yet; the palette
entry comes with the palette (it has no chord). One dialog at a time; a second ask while it is open changes nothing. `Escape`,
`Close` and a press on the backdrop close it, and the keyboard goes back to the terminal.

**Box.** The box of the Settings dialog and the providers dialog (a floating layer: hairline, 8 px radius, the one shadow), 40 rem
wide, scrolling inside. From the top: the title `Integration profiles`; the status line (`role="status"`, one line); the notes of
the core about its profile file, only when it sent any, as dim lines (a file that did not load is named there, with the reason);
four groups, each a fieldset under a `.label` legend: `Profiles`, `Profile`, `Secret variables`, `Environment`; the note about
syncing; the buttons `Delete` and `Save`; then `Close`. While it is open it takes the keys: nothing reaches the terminal, `Tab`
stays inside, the arrows move inside a radio group, `Enter` in a text field saves, and `Escape` closes.

**Profiles.** A radio group with one row for each profile the core listed (at most 64): a native radio, the name, the tool's name
dim, the pinned selection in mono (`prod-eu · payments`, `—` when it pins nothing) and, for a production profile, the word
`production` (warn, underlined, with the mark `!` before it). The row of the profile in the form has the rose wash and a 2 px
rose bar on its left. A name is text: markup in a name is those characters. `＋ New` sits below the list; with no profile the
list says "No profile yet."

**Profile.** The legend is `Profile`, or `New profile` while adding. In this order:

- `Name`, a text field of at most 64 characters.
- `Tool`, a radio group of the tools the core lists, with the core's names. Changing the tool keeps the name and the switches
  and empties the fields the new tool does not select.
- The fields the chosen tool selects, taken from the core's list of its selections (`Context` and `Namespace` for
  Kubernetes, `Context` and `Node` for Talos, `Subscription` for Azure, `Section` for GAM). Each is a text field of at most 253
  characters whose placeholder is an example (`prod-eu`, `payments`, `node-1`, `00000000-0000-0000-0000-000000000000`, `work`);
  the page puts no value in a field the person did not type, and an empty field pins nothing. A field the tool does not select
  is not in the form and is sent as null.
- A checkbox `Production context` with the sentence "Actions on this context ask twice." The flag is the person's, loaded from
  the core and sent back with every save.
- A checkbox `Own configuration folder` with the sentence "Sign in again inside it. holzWave keeps it on this computer.", only
  for a tool that selects a subscription or a section (Azure and GAM).
- The line of the core's refusal of the profile, empty until there is one: the core's sentence, naming the profile and the field,
  in the danger colour, underlined, with the mark `!`, and `role="alert"`.

**Secret variables.** One row for each name in the profile's secret variables: the name in mono, a password field, one state
word, and the buttons `Set`, `Clear` and `Remove`. The field is empty on every open and after every call, has `autocomplete="off"`,
takes at most 1024 characters, and has no show toggle. The state word comes from the core's `has_value`: `saved` or `not set`.
`Set` is dimmed until the profile has been saved once with that name (the core refuses a value for a name the profile does not
declare; the row says "Save the profile first.") and until the field holds text; a press sends the value in one call, empties
the field at once and shows `saved`. `Clear` deletes the value and shows `not set`. `Remove` takes the name out of the profile at
the next `Save`; a saved value of that name is deleted first. Below the rows a text field `Variable name` and `Add` add a name
(at most 8); the name is sent with the next save, and the core checks it. The group says "Saved values are never shown. Type a
new one to replace it."

**Environment.** One row for each plain variable: a name field, a value field and `Remove`; `Add` adds an empty row (at most
32). The page applies no rule of its own to a name or a value. When the core refuses a row at a save, its sentence is shown on the
row that carries the name the sentence mentions, and otherwise on the line under the profile.

**Saving and deleting.** `Save` sends the whole profile in one call, in the shape of the profile file: the id, the tool, the
name, the four selection fields (empty text is null), the two flags, the plain variables as an object and the secret variables
as a list of names. The id of a profile that exists is its own; the id of a new profile is a lower-case form of its name (letters,
digits, `-` and `_`, at most 32), made unique among the listed ids so that a new profile can never replace another one. On
success the list is read again and the saved profile stays chosen; on a refusal nothing changes and the sentence is shown.
`Delete` (only for a saved profile) turns into `Delete?` on the first press and removes the profile, its secrets included, on the
second; any other press, a key typed in a field and `Escape` take the question back. After a removal the list is read again and the
form is empty.

**Leaving with changes.** The status line says `unsaved` (warn, and the word) while the form differs from the saved profile. A
press on another row or on `＋ New` says `Changes were not saved.` and shows the other profile. `Close`, `Escape` and the backdrop
ask first: the button reads `Discard changes?` and the line says "Unsaved changes. Close again to discard them."; any other
action takes the question back. A value typed into a secret field is never an unsaved change; it is sent by `Set` or lost when
the dialog closes.

**Keys and focus.**

| Key | Effect |
|---|---|
| open | the box takes the focus, then the chosen row, or `Name` for a new profile |
| `Tab`, `Shift+Tab` | stays inside the dialog; one stop for each radio group |
| arrows in a radio group | choose the profile or the tool |
| `Enter` in a text field | `Save`; in a secret field it is `Set` for that row |
| `Escape` | takes back an armed `Delete?`, else closes (asks first with unsaved changes) and returns the keyboard to the terminal |

![Integration profiles](design/integration-profiles.png)

**States and their second cue.**

| State | Colour | Second cue |
|---|---|---|
| profile in the form | rose wash | `●` radio, 2 px bar on the left |
| production profile | warn | the word `production` underlined with a `!` before it |
| unsaved changes | warn | the word `unsaved` in the status line |
| secret saved | ok | the word `saved` |
| secret not set | dim | the word `not set` |
| refusal of the core | danger | its sentence, underlined, `!` mark, `aria-invalid` on the row it names |
| armed delete | danger border | the text `Delete?` |
| armed close | danger border | the text `Discard changes?` |

**Sentences.**

| Sentence | Where |
|---|---|
| Integration profiles | the title |
| Profiles, Profile, New profile, Secret variables, Environment | the legends |
| ＋ New, Save, Delete, Delete?, Set, Clear, Remove, Add, Close, Discard changes? | the buttons |
| saved, not set | the state word of a secret |
| Production context / Actions on this context ask twice. | the checkbox and its sentence |
| Own configuration folder / Sign in again inside it. holzWave keeps it on this computer. | the checkbox and its sentence |
| Saved values are never shown. Type a new one to replace it. | under the secret rows |
| Save the profile first. | the hint of a secret row whose name is not saved yet |
| The profile file is synced between your computers. Secret values are kept in the keychain and synced inside the encrypted secrets. Paths of this computer are never part of a profile. | the note under the groups |
| No profile yet. | the empty list |
| Saved. / Deleted. / Saving… / unsaved / Changes were not saved. / Unsaved changes. Close again to discard them. | the status line |

**Not built.** A show toggle for a secret, a copy of a secret, an import from a kubeconfig or a cloud account, a test of the
connection, an edit of the id of a profile, a path of this computer in a profile, and a rule of the core written into the page.

**Narrow screens (640 px and narrower).** The dialog is the screen, as in 6.13: full width and height, no radius, no shadow, the
fields stacked, rows, options, inputs and buttons at least 44 px high, input text at least 16 px. The secret rows and the
environment rows wrap: the name above, the field and the buttons below. On a desktop the same targets keep their 28 px minimum.

**Accessibility.** Every control has a visible label; the secret field is named `Value of {NAME}`, the environment fields
`Name` and `Value of {NAME}`; a row that the core refused is `aria-invalid` and `aria-describedby` its sentence; the status line
is `role="status"`; the list and the tools are radio groups. Nothing moves, so `motion-reduce` has nothing to remove. Every
hairline is `--hw-line`, so `contrast-high` holds. Under forced colours the chosen row and the armed buttons get a system-colour
outline.

### 6.28 Signing in

The browser build (6.30) has a page that a person sees before they are signed in, with no window around it: the ground
(`--hw-ground`) and one box in the middle. The box is the dialog box of the Settings (a floating layer: hairline, 8 px
radius, the one shadow), 26 rem wide, centred. Four screens use it: signing in, setting up the first administrator, an
invitation and unlocking the saved passwords. This section is binding for the page: the screens, their words, their keys and
the rules below. The server answers with a status and a code and never with a sentence; the page chooses the words from the
table at the end of this section.

**The rules.** They hold for every screen of this section.

1. **One sentence for a failed sign-in.** A wrong password, an unknown account, a wrong code and a wrong passkey all show
   `Sign-in failed. Check your details or ask an admin.` The same words in the same place with the same look: the text and the
   timing of the page give no hint whether the account exists or which detail was wrong. The page adds no delay of its own
   and shows no count of failures.
2. **A throttled sign-in says how long to wait and nothing else:** `Too many attempts. Try again in 12 minutes.` The time is
   the server's `retry_seconds` rounded up to whole minutes (`Try again in 1 minute.`); under a minute it is in seconds
   (`Try again in 40 seconds.`, `Try again in 1 second.`); above 90 minutes it is in whole hours, rounded up (`Try again in
   2 hours.`); a missing time or one of zero says `Try again in a moment.` The line names no cause, no account and no address,
   and it does not tick down.
3. **Tokens stay out of the URL path and query.** The token of an invitation and the setup token of the first administrator
   arrive in the URL fragment (`#invite=…`, `#setup=…`), which a browser sends to no server and no `Referer`. The page reads
   the fragment once, clears it with `history.replaceState` at once, keeps the token in memory until the step ends and sends it
   in the body of a request. It is never in a query string, in the path, in the page title, in `localStorage`,
   `sessionStorage` or a cookie.
4. **Secrets are shown once.** The authenticator secret, its `otpauth://` link and the recovery code are on the page only while
   their step is shown. They are never put into the URL, the title, a status line or a history entry; they are removed from
   the page when the step ends; and the recovery step cannot be left until the person confirms that the code was saved (Escape
   and a click outside do nothing there, and the page asks the browser to confirm before a reload or a close).
5. **A field that holds a secret is never echoed.** A password, a passphrase, a code or a token is not copied into a status
   line, a title, an error line, an accessible name or a live region. Password fields are masked and have no show toggle. After a
   refusal the page empties the secret fields and keeps the username.
6. **Every field names what it is to the browser:** `Username` has `autocomplete="username"`; the password of a sign-in
   `current-password`; a new password or passphrase `new-password`; `Code` `one-time-code` with `inputmode="numeric"`; the
   setup token, the recovery code and the unlock passphrase `off`, so that a password manager never offers the wrong entry.
7. **A refused new password or passphrase** shows `Use at least 12 characters. Not your username, not one repeated character.`
   under the field, with the error mark and underline of the failed state below. The server decides; the page does not
   count characters before it asks.
8. **A provider that did not sign the person in** (any failure after the single sign-on redirect) shows the same sentence as
   rule 1. The page removes `?error=oidc` from the address at once.

**Signing in.** The box, top to bottom. Only the methods that the server offers (`GET /api/session` answers which are on) are
drawn; nothing is drawn for a method that is off.

1. The wordmark `holzWave` in bold mono, and at the right the name of the server in mono and dim (a label, not a link).
2. The passkey button `Sign in with a passkey`, solid (`--hw-rose-deep` with `--hw-on-accent`), only when the server offers
   passkeys. It is the one solid button of the box: with no passkey, `Continue` is the solid one.
3. One hairline button for each single sign-on provider, `Continue with NAME`, in the order and with the names the server
   sends. A press leaves the page for the provider.
4. A hairline with the label `or` in the middle, an uppercase tracked `.label`, drawn only when a button above it and the
   password form below it are both there.
5. The password form: `Username`, `Password` (labels above the fields, `.label` style) and the hairline button `Continue`.
6. The line under the form: one place for the sentence of the current state (below). It is a `role="status"` region while
   idle and a `role="alert"` while it holds an error.

**The code step.** `Continue` sends the username and the password. When they are accepted the box shows the second step in
place of the first: the wordmark and the server, the sentence `Signing in as NAME. Enter the six-digit code of your authenticator
app.` with the name in bold, the field `Code`, the solid button `Sign in`, the error line and a text button `Back`. `Back`
returns to the first step with the username kept and the password empty. The code step is only for an account that has an
authenticator; an account that signs in with a passkey or a provider never sees it.

**States.** The second cue is never colour alone.

| State | Words | Second cue |
|---|---|---|
| idle | none | the 2 px focus ring on the first field |
| working | the pressed button keeps its label and adds `…` (`Continue…`, `Sign in…`); the fields are read-only | `aria-busy="true"` on the box and the `…`; nothing moves |
| failed | `Sign-in failed. Check your details or ask an admin.` | `!` before the line, the line underlined, `role="alert"`, the field that has the focus has `aria-invalid="true"` |
| throttled | `Too many attempts. Try again in 12 minutes.` | the same mark, underline and `role="alert"`; the box is the same box |
| session ended | `Your session ended. Sign in again.` | the words; plain ink and no mark, because nothing failed |
| server not reachable | `Could not reach the server. Try again.` | the error mark and underline |
| a passkey prompt that the person cancelled | none | the box returns to idle; a cancelled prompt is not a failure |

**Keys.** On load the first field has the focus: `Username`, or `Password`, `Code` or the passkey button when the form has no
earlier control. `Tab` order is the order on the screen: passkey, providers, username, password, `Continue`; `Shift+Tab` goes
back. `Enter` in a field presses the solid button of the step (the button of the form, `Continue` or `Sign in`). The focus
ring is 2 px. After a refusal the focus goes to the first field that was emptied, and the line is read out by its live region.
In the code step the focus starts in `Code`.

**Accessibility.** The box is the `main` landmark of the page (nothing is behind it, so it is not a modal dialog) and the
wordmark is its `h1`. Hairlines use `--hw-line` and dim text `--hw-ink-2` under `contrast-high`; under `forced-colors` the
box, the fields and the buttons keep their borders and the solid button gets a system-colour outline; the busy state shows
`…` instead of motion, so `motion-reduce` has nothing to remove. Every size is in `rem`; buttons and fields are at least 28
px high. The page is for a computer with a keyboard; a mobile layout of it is not drawn.

![Signing in](design/web-signin.png)

**Not drawn on the sign-in screen.** A show toggle for a password that is on by default, a `remember me` box, a language picker, a link that mails a
password or a code, a link that creates an account (an account exists only through an invitation or the first-administrator
screen), a challenge to prove a person is not a program, and a count of failed attempts.

**First administrator.** Shown instead of the sign-in, and only while the server has no administrator (`GET /api/session`
says `bootstrap`); once the first account exists the screen cannot be reached again. The box, top to bottom:

1. The wordmark and the name of the server, as in the sign-in.
2. The title `Set up this server` and the lead `There is no administrator yet. The account you make here becomes the first one.`
3. `Setup token`: a masked field with the hint `The token the server printed when you started it.` The operator prints the token
   with the server's command. When the address has `#setup=…` the page fills the field from it and clears the fragment at
   once (rule 3); otherwise the person types or pastes it.
4. `Username` and `Display name`.
5. `Sign-in method`, a radio group in this order: `Passkey`, `Password and authenticator app`. The invitation uses the same
   words in the same order. The choice of a password adds the field `Password` (`new-password`) under the group.
6. The solid button `Continue`.

`Continue` sends the first step. A passkey goes to the prompt of the browser; a password goes to the authenticator step of
the invitation below. Both end in the recovery step, and then the person is signed in as an administrator.

**Invitation.** An administrator gives a person a link whose fragment holds the token (`#invite=…`). The page peeks at the
token before it draws anything, so a link that is no longer good never shows a form. The steps are one box each, with the
wordmark and the server above:

1. **Details.** The title `You were invited` and the role as a label (`administrator` or `member`, an uppercase tracked chip
   in the title row). `Username` is a field, or a fixed mono line when the administrator chose it; `Display name` (kept when
   the administrator preset it); the radio group `Sign-in method` as in the first-administrator screen; `Password` for the
   second choice; the solid button `Continue`.
2. **Authenticator** (password only). The lead `Add this key to your authenticator app, then enter the code it shows.` The
   label `Key` over the secret in mono groups of four characters with the button `Copy`; the label `Link` over the
   `otpauth://` link on one line, cut with an ellipsis, with the button `Copy link` (the whole link is copied); the field
   `Code` and the solid button `Continue`. There is no QR code. `Copy` puts the key without its spaces on the clipboard and says
   `Copied.` in the line below the box, and nothing else.
3. **Recovery code** (always, as the last step). The title `Save your recovery code` and the lead `If you forget how you
   unlock your saved passwords, this code unlocks them. It is shown once and cannot be shown again.` The code in mono in
   groups of four joined by hyphens, in one bordered block, with the button `Copy`; the checkbox `I have saved this code`; the
   button `Continue`, which is disabled while the box is not ticked, with the line `Tick the box to continue.` under it. The
   step cannot be left: `Escape`, a click outside and the browser's back action do nothing, and a reload or a close asks the
   browser to confirm (rule 4). A person who lost the code in spite of that makes a new one in the account dialog (6.29).
   When `Continue` is pressed the code is removed from the page, not hidden.

A link that is unknown, expired or used shows the box with the title and one line instead of the form:
`This link has expired or was used. Ask an admin for a new one.` The first-administrator screen shows
`This token has expired or was used. Ask the person who runs this server for a new one.` in the same way, because there is no
admin to ask. Both lines have the error mark and the underline.

**Unlock.** The saved passwords of a person (the passwords and keys of connections, the keys of providers) are kept in a store
that a sign-in opens by itself: a password sign-in, and a passkey whose authenticator supports the PRF extension, unlock it
at the same moment and never show this dialog. A person who signed in through a single sign-on provider, or with a passkey
without the extension, unlocks it with their passphrase. Until then the window works (panes run, the layout is kept) and
the status line shows the mark `secrets locked` with a text button `Unlock` (6.30).

The dialog is the box of the Settings over the window with the backdrop of the other dialogs, 24 rem wide, and is a real
modal (`role="dialog"`, `aria-modal`, the focus stays inside). It opens when the person presses `Unlock` in the status line or
chooses `Unlock…` in the palette (the entry exists only while the store is locked). It does not open by itself, neither when the
window loads locked nor when a command is refused for a locked store: the mark is the way, and the keyboard stays with the terminal. Top to bottom:
the title `Your saved passwords are locked`; the lead `Enter your passphrase to use your saved passwords and keys.`; the field
`Passphrase`; the error line; the buttons `Not now` (hairline) and `Unlock` (solid); the text button `Use recovery code`; and,
in the dim style, `If you lost both, an admin can reset your secrets. That deletes your saved passwords and keys, and keeps
your account, layout and history.` The text button swaps the field for `Recovery code` (the lead changes with it) and
becomes `Use passphrase`. A wrong passphrase and a wrong recovery code say the same: `That did not unlock your secrets.` A
throttled try says the sentence of rule 2; a server that does not answer says `Could not reach the server. Try again.` `Not now` and `Escape` close the dialog and leave the mark in the status line;
when the store is unlocked somewhere else (another browser tab of the person) the open dialog closes and the mark goes.
Closing the dialog gives the keyboard back to the terminal.

**States of the setup, the invitation and the unlock.**

| State | Words | Second cue |
|---|---|---|
| token not good | the expired-link line of the screen | `!`, underline, `role="alert"`; no field is drawn |
| username taken | `That username is taken. Choose another.` | `!`, underline, `aria-invalid` on `Username`, focus on it |
| new password refused | the sentence of rule 7 | `!`, underline, `aria-invalid` on the field, focus on it |
| wrong code in the authenticator step | `That code did not match. Try again.` | `!`, underline, `aria-invalid` on `Code`, the field emptied |
| passkey not saved | `That passkey was not saved. Try again.` | `!`, underline; a prompt that was cancelled shows nothing |
| recovery step, not ticked | `Tick the box to continue.` | the box `☐`, `Continue` dim and `disabled` |
| recovery step, ticked | none | the box `☑`, `Continue` solid |
| unlock failed | `That did not unlock your secrets.` | `!`, underline, `aria-invalid`, the field emptied, focus on it |
| unlock working | the button reads `Unlock…` | `aria-busy`, the field read-only |

**Keys.** First focus: the first field of a step (`Setup token`, `Username`, `Passphrase`); on the authenticator step and the
recovery step the button `Copy`, because the person copies first. `Tab` order is the order on the screen: for the setup and
the invitation `Setup token`, `Username`, `Display name`, the radio group (one stop, arrows move), `Password`, `Continue`; for
the authenticator step `Copy`, `Copy link`, `Code`, `Continue`; for the recovery step `Copy`, the checkbox, `Continue` (not a stop
while disabled); for the unlock `Passphrase`, `Not now`, `Unlock`, `Use recovery code`. `Enter` presses the solid button of the
step. The focus ring is 2 px. `Escape` closes the unlock dialog and does nothing on the other three screens.

![First administrator](design/web-first-admin.png)
![Invitation](design/web-invite.png)
![Unlock](design/web-unlock.png)

**Sentences.** The page chooses every word from the code of the answer; the server sends none.

| Where | Answer | Sentence |
|---|---|---|
| sign-in | 401 `invalid` | Sign-in failed. Check your details or ask an admin. |
| any step | 429 `throttled` | Too many attempts. Try again in 12 minutes. |
| any step | 503 `busy` | The server is busy. Try again in a moment. |
| any step | no answer | Could not reach the server. Try again. |
| after an expired session | none | Your session ended. Sign in again. |
| invitation | 410 `gone` | This link has expired or was used. Ask an admin for a new one. |
| first administrator | 410 `gone` | This token has expired or was used. Ask the person who runs this server for a new one. |
| username | 409 `conflict` on `username` | That username is taken. Choose another. |
| new password or passphrase | 400 `bad_request` on the field | Use at least 12 characters. Not your username, not one repeated character. |
| authenticator step | 401 `invalid` | That code did not match. Try again. |
| unlock | 401 `invalid` | That did not unlock your secrets. |
| recovery step | none | Tick the box to continue. |

**Accessibility.** The same as the sign-in: hairlines `--hw-line`, dim text `--hw-ink-2` under `contrast-high`, the box and the
fields keep their borders under `forced-colors` and the solid button gets a system-colour outline, nothing moves, every size
is in `rem` and every target is at least 28 px high. Each error line is a `role="alert"` and the field it concerns is
`aria-invalid` and `aria-describedby` the line. The key and the recovery code are plain text that a screen reader reads
character by character in their groups.

**Not drawn.** A QR code, a download of the recovery code as a file, a print button, a choice of several recovery codes,
and a way to skip the recovery step.

### 6.29 Account and people

Two dialogs of the browser build, both the box of the Settings dialog (a floating layer: hairline, 8 px radius, the one
shadow). They come from the palette and nowhere else: the entries `Account…`, `People…` (administrators only; for a member
the entry is absent, not disabled) and `Sign out` (ends this session), and the text button `Unlock` of the locked mark in
the status line (6.30). The Settings dialog gets no row for them and the title bar no new control. One dialog at a time;
`Escape` and `Close` close it and the keyboard goes back to the terminal.

**Account.** The box is 40 rem wide and scrolls inside. From the top: the title `Account`; one status line
(`role="status"`, one line, for `Saved.` and for an error; never a secret); then six groups, each a `fieldset` with
`role="group"` under a `.label` legend in the manner of 6.21; then `Close`. Each group has one row: the name, the state as a
word at the right, and the button of the change. A group is drawn only when it applies: `Password` and `Authenticator` only
for an account that has a password, the others always.

| Group | State word | Buttons |
|---|---|---|
| Password | `set` | `Change` |
| Authenticator | `set` | `Replace` |
| Passkeys | the name reads `N passkeys` (`1 passkey`, `No passkeys`); the state is `saved` or `none` | `Add passkey`; under it one row for each passkey with its name, `used today`, `used 3 days ago` or `not used yet`, and `Remove` |
| Unlock passphrase | `set` or `not set` | `Change` or `Set`; while the saved passwords are locked `Unlock` instead, which opens the dialog of 6.28 |
| Recovery code | `saved` or `none` | `Make a new code`; while the saved passwords are locked `Unlock` instead |
| Sessions | none | `Sign out` on every row but this one, and `Sign out everywhere` |

A seventh group, `Devices`, follows `Sessions` (6.35). It is drawn whether or not a device switch is on: a device must always be possible to end, and the page cannot tell a member what the switches say.

A state word is its own cue: `set` and `saved` are in the ok colour, `not set` and `none` are dim; the colour is never alone.
A change opens its form in the place of the group's row, with `Save` and `Cancel`: the password asks `Current password` and `New password`
(`current-password`, `new-password`, the refusal sentence of 6.28 rule 7 under `New password`, the sentence of rule 1 under `Current
password`; there is no repeat field); the authenticator shows the key, the link and `Code` of
the invitation step (a wrong code says `That code did not match. Try again.`, empties `Code` and keeps the step); a passkey asks for a `Name` and then shows the prompt of the browser
(a prompt that was closed says nothing, a failed one says `That passkey was not saved. Try again.`); the passphrase asks for `New
passphrase`; a new recovery code says `The new code replaces the old one.` before it is made, with the buttons `Make code` and `Cancel`, and is then shown
in the same once-only step as in 6.28, during which `Close` is disabled and `Escape` does nothing. Removing the last way to sign in is refused with `This is your last way to sign in. Add another first.`
A change that was made says `Saved.` in the status line and the dialog reads the account again; a refusal in a form is said under its fields with the mark `!` and the underline of 6.28 and
empties the secret fields. A change that the server refuses for another reason says `That change was not made. Try again.`; one that needs the saved
passwords while they are locked says `Your saved passwords are locked. Unlock them first.`; a server that does not answer says `Could not reach the server. Try again.`, which is also what
the dialog says when it cannot read the account (then only `Close` is left). `Remove` and `Sign out everywhere` ask twice: the first press changes the words to
`Click again to remove` and `Click again to sign out everywhere` and gives the button the danger border and text, leaving the button disarms it. A secret that the dialog
shows (the key, the recovery code) is removed from the page when its step ends, not hidden, and no secret is ever put into the status line, an attribute, the title, the
address or the storage of the browser.

A session row shows the device (the browser and system, as a short summary), the address in mono, and when it was last seen
(`now` for this session, then `just now`, `3 min ago`, `3 h ago`, `2 days ago`). The
session that shows the dialog is first, marked with the words `this session` and a filled dot `●`, and has no `Sign out`; every other row
has one, newest first. `Sign out everywhere` ends every session including this one, and the window then shows the start page (the location is replaced, so the browser's back
button does not return to the dialog). The palette entry `Sign out` ends this session only and does the same, and it does so even when the server cannot be reached. `Account…` and `Sign out` are palette entries of the browser build only.

**Confirm it is you.** Every change asks for a sign-in no older than five minutes. The page sends the change; when the answer
says that a recent sign-in is needed, it shows this layer once and repeats the change after it. A layer above the dialog,
with its own backdrop over the dialog, the same box as the other dialogs, centred, 24 rem wide. It shows the account name in
mono in the sentence `Signed in as NAME. Enter your password to make this change.`, so that no other dialog can pass for it
(an administrator sees the same box for an admin action). Then the field `Password` (`current-password`, focused), the buttons `Cancel` and `Confirm`
(solid) and a text button `Use a passkey` when the account has a passkey. An account with neither a password nor a passkey has
`Sign in again with NAME` (the provider's name) in place of the field and `Confirm`; it leaves the page like a sign-in. A
wrong password says the sentence of 6.28 rule 1 (a throttled one the sentence of rule 2). `Escape` is `Cancel`; the focus stays inside the layer and returns to the
button that opened it. A confirmation does not extend the session. The layer asks once: after a confirmation the change is sent again one time,
and a second refusal is the change's own, with no second layer. Two changes that need it at the same moment share one layer. The words follow the
ways the account has: with a password and a passkey the layer is the one above (`Use a passkey` is the text button, last in the order of the keys:
`Password`, `Cancel`, `Confirm`, `Use a passkey`); with a passkey and no password the sentence is `Signed in as NAME. Use your passkey to make this
change.`, there is no field and the solid button is `Use a passkey`; with a provider only the sentence is `Signed in as NAME. Sign in again to make this
change.` and the link `Sign in again with NAME` is the solid button. A prompt of the browser that the person closed says nothing and leaves the layer
as it was; a browser that fails says the sentence of 6.28 rule 1. An account with none of the three ways says `There is no way to confirm it here. Sign
out and sign in again.` with `Cancel` only, and a layer that cannot read the account says `Could not reach the server. Try again.` with `Cancel` only.

**People.** The box is 41 rem wide and scrolls inside. The title `People` with the hairline button `Invite` at the right, the
status line, then a table with the columns `Name`, `Role`, `State`, `Last sign-in` and an actions column, the sentence `This list
shows names, roles and states only. It never shows anyone's secrets, terminals, commands or layout.` and `Close`. A row is the display name with the
username in dim mono, the role (`admin` or `member`), the state, when the person last signed in (`just now`, `5 min ago`, `3 h ago`,
`yesterday`, `3 weeks ago`, `never`) and a button `⋯` named `Actions for NAME` (`aria-haspopup="menu"`). The row of the open menu has the rose
wash and the button the rose ring.

| State | Shown as | Second cue |
|---|---|---|
| `active` | `● active`, ok | the filled dot and the word |
| `disabled` | `○ disabled`, dim | the hollow ring and the word |
| `locked out` | `! locked out`, warn | the mark `!`, an underline and the word |

The menu, in this order: `Make admin` or `Make member`, `Disable` or `Enable`, `Unlock sign-in` (only while the person is locked
out), then after a hairline `Reset secrets` and `Delete` (danger colour, last). The two below the hairline ask first, in a
confirmation of the same box (24 rem) that names the person: `Delete NAME?` says `They can no longer sign in and their terminals stop. Their files stay on the
server, and nobody can open their saved passwords and keys again.`; `Reset secrets of NAME?` says `Their saved passwords and keys are
deleted. Their account, layout and history stay.` The buttons are `Cancel` and the action's name in a hairline button with the
danger border and text (armed, as in 6.27); rose is not used for a destructive action. An administrator cannot disable, delete or
demote the last active administrator: those three entries stay in the menu, dim and `aria-disabled`, and a press on one does nothing; the menu says
why under them in the dim style, `The only active administrator.` If another administrator acts at the same moment and the server refuses a change for
that reason, the status line says `There must be one active administrator.` and the list stays as it was. A person whose role was taken away while the
dialog was open gets `Only administrators can open this.` from the first answer that says so, and the dialog then offers `Close` only. A person who
is gone (the server answers that there is no such person) is not named in a sentence: the list is read again. Every action also asks for a recent sign-in,
with the layer above; a request that was not made says nothing when the person cancelled that layer, and `Could not reach the server. Try again.` when
the server did not answer. While a change is on its way its buttons are disabled, so a double press sends one request.

`Invite` opens a small form in a layer of the same box: `Role` (a radio group, `Member` first, then `Administrator`) and `Days` (a radio group of
`1`, `3`, `7`, `14` and `30`, 7 at first; a list that drops down would be a `select`, which a page under a strict content security policy cannot draw in
every engine), `Cancel` and the solid `Create link`; a refusal is said in the form, under the choices. The layer then shows the link once: the title `Invitation link`, the
sentence `Shown once. Copy it now and send it to the person. It works for 7 days, as a member.` (with the real days and role), the link in
a bordered block in mono cut with an ellipsis, `Copy link` and the solid `Done`. The link is never listed again, is not put into
the status line or the title, and is removed from the page when `Done` is pressed or the layer closes.

The dialog also has the groups `Devices` and `Device access` below the table (6.35), drawn while both switches are off as well: the switches are how a feature is turned on.

No dialog of this section shows any person's secrets, terminal output, commands, scrollback or layout, and there is no audit viewer. An
administrator reads and changes the account rows and nothing else.

**Keys.** `Tab` goes through the groups and buttons of the account dialog in reading order, and through the actions button of each
row in the people dialog (one stop for each row). `Enter` or `Space` on `⋯` opens the menu with the first entry focused; the arrows
move, `Enter` chooses, `Escape` closes the menu and returns to its button. Dialogs are modal (`aria-modal`), the focus stays inside
and returns to where it was. The focus ring is 2 px.

**Accessibility.** Hairlines use `--hw-line` and dim text `--hw-ink-2` under `contrast-high`; under `forced-colors` the dialogs, the row
of the open menu and the armed buttons keep their borders or get a system-colour outline; nothing moves, so `motion-reduce` has nothing
to remove. Every size is in `rem`, every target at least 28 px high. The table has real header cells; the menu is `role="menu"`.

![Account](design/web-account.png)
![People](design/web-people.png)

**Not drawn.** A password that an administrator sets for someone, impersonating a person, a view of anyone's data, groups, bulk actions,
a search of the people, an audit viewer, API tokens (the Devices list of 6.35 shows signed-in devices and never a token), management of SSH keys, a list of shared panes, and a name that an administrator
chooses for a new account in the invitation form (an invitation is for a person who chooses their own).

### 6.30 The web window

The window of the browser build is the desktop window of 6.1 to 6.3 with the changes below; nothing else differs. The three levels,
the numbered tabs, the sidebar, the status line and every dialog keep their shapes and tokens. The window has **no window buttons and
no drag region**: the browser draws its own bar above the page, and the page's bar starts at the left edge with the first workspace
pill. One window, labelled `main`, is mirrored by every browser tab of the person: a second tab shows the same workspaces, tabs and
panes, and nothing can be torn off.

**Status line marks.** Each mark is a segment of the status line (6.2) with a word and a shape, never colour alone; they are
`role="status"` regions and carry no secret.

| Mark | Words | Shape | When | Second cue |
|---|---|---|---|---|
| reconnecting | `reconnecting` | dotted ring `◌` (as in 6.8) | one second after the connection to the server was lost; a shorter break draws nothing | warn colour, the ring and the word |
| offline | `offline` | `!` | the browser reports that it has no network | danger colour, the mark and an underline |
| session ends | `session ends in 4 min` | quarter circle `◔` | for the last five minutes of the idle time; the minutes count down once a minute, the last is `session ends in 1 min` | warn colour, the glyph and the words |
| session ended | `session ended` | `!` | the session is over (the server says so, or closes the connection with the code 4401): a layer opens over the window with the sentence `Your session ended. Sign in again.` and the one button `Sign in`, which has the focus and goes to the sign-in page, whose line says the sentence again; the page never leaves by itself, `Escape` does nothing, the terminal output stays readable behind the layer, the panes keep running and are there again after the sign-in | danger colour, the mark and an underline |
| secrets locked | `secrets locked` and a text button `Unlock` | a lock glyph, drawn as a small box with a shackle | the saved passwords are locked (6.28); `Unlock` opens the dialog | plain ink; the glyph, the words and the button |

The idle mark is announced when it first appears and again at one minute, not every minute. Any activity of the person ends it. While the
connection is down the panes show their last screen. When the browser reports that it has no network the mark `offline` takes the place of
`reconnecting`, and it shows at once. When the server refuses the page (the close code 4403: the address or the host is not the one it serves)
the mark `offline` stays for good, no layer opens and nothing reconnects. The marks stand in this order: session ended (alone; nothing else
is shown then), offline or reconnecting, session ends, secrets locked. The status line shows no mark while the page cannot learn its session.

**Upload by drop.** While a file is dragged over a pane that takes it (a pane that runs a shell, a chat, or a file browser), the pane shows a hairline
dashed edge (1 px, `--hw-rose`, with `--hw-rose-wash` inside, 2 px inside the pane's box so that it does not meet a divider) and, centred, a small box
with the label `Drop to upload` (uppercase, tracked). Once the page has seen an answer of the server the box also says `to` and the upload folder in mono;
the first drop after the page loads shows the label alone, because the page does not know the folder yet. The label and the edge are the same on the
three kinds of pane; what the pane does with the file is told below. Rose marks the target of the action. A drag that carries no file shows nothing; a
pane that takes nothing (a preview, the monitor, a tool view) shows no edge, and the cursor says so. A drag of files is the page's wherever it is: a drop
on the title bar, the status line, a pane that takes nothing or while a layer is open uploads nothing, and the browser never opens the dropped file or
leaves the page; a link dragged in from another page is not followed either, and a text field keeps its own drop.

Each file goes to the server on its own, one after the other, at most 16 for a drop (the 17th and the rest are not sent, and the status line says
so). The path the server answers is then given to the pane under the drop, and what that means depends on the pane:

| Pane | What the path becomes | Never |
|---|---|---|
| shell | its path, quoted for the shell with single quotes, is typed into the pane like a paste; several paths are separated by a space; a path with a line break in it is quoted and goes through the confirmation of every paste | followed by `Enter` |
| chat | the same quoted path is added to the message being written, where the caret is (in place of a selection), with one space between it and the words before it and none at the start or after a space or a line break; the composer keeps the keyboard and its count follows | sent |
| file browser | nothing is typed. When the list shows the upload folder (the same folder of this computer, not a saved connection), it is read again and the new file is the selected row, scrolled into view; the status line says `Uploaded`. When the list shows any other folder, or the files of a saved connection, it does not move, nothing is selected, and the status line names the folder: `Uploaded to <folder>` (`Uploaded 3 files to <folder>`; a long folder is cut at its start) | taking the person away from the folder they are in |

The status line says what happens in words: `Uploading <name>` for the file on its way (a long name is cut with an ellipsis), then `Uploaded`
(`Uploaded 3 files`, with `to <folder>` after the count in the one case above) when the run ends. A refusal says why and gives the pane nothing:
`Not uploaded: too large.`, `Not uploaded: name not allowed.`, `Not uploaded: try again.` (too many requests, or the server is busy),
`Not uploaded: secrets are locked.`, `Not uploaded: sign in again.` and `Not uploaded: upload failed.` for any other answer, an answer that is not an
upload and a connection that failed. After a run with more than one outcome the sentences follow each other (`Uploaded 2 files. Not uploaded: too large.`),
and the sentences `Only 16 files at a time.` and `Folders are not uploaded.` end the run when they apply; a folder is never sent. When the session has ended
nothing is sent.

**Download.** A file that the core grants (it grants images, video, audio and PDF files) can be taken out by an explicit act only: the entry
`Download` of the row menu of a file in the file browser (6.5), and the palette entry `Download`, which acts on the selected file of the
focused file browser or on the file of a focused preview and is not listed otherwise. Nothing is downloaded or granted by selecting a row, by
opening the menu or by looking at the palette. The act asks the core for a short-lived grant and follows its address on the page's own origin
with a download link, so the browser saves the file under its own name. The page keeps at most 16 grants of its own for downloads, ends the
oldest when a 17th is made, and ends all of them when the page is closed.

A refusal says why, in the status line, in words, as an upload's refusal is said. The core's own text of a refusal is for people and changes, so the
page asks the host for the reason after a refusal and reads one name from its answer; the sentence follows from the name and from nothing else, and
the file's name, path or the core's text are never in it:

| Reason | Sentence |
|---|---|
| the secrets are locked and the file lives on a saved connection | `Not downloaded: secrets are locked.` |
| the file is gone | `Not downloaded: file not found.` |
| the person may not read it | `Not downloaded: not allowed to read it.` |
| it is not an image, a video, audio or a PDF (or it is a folder) | `Download works for images, video, audio and PDF files.` |
| any other reason, a question that failed, an answer the page does not know, an address that is not the page's own | `Not downloaded: download failed.` |

The core refuses no grant for the size of a file, so there is no sentence for one.

**The row menu.** In the browser build a file row of the file browser (and no other row; a folder row keeps the system's menu) opens a menu
by the context-menu gesture (a right press, or the long press the system maps to it), by the context-menu key and by `Shift+F10`; the row becomes
the selected one. The menu is a floating layer, as the palette is (the hairline, the 8 px radius and the one shadow of the floating layers), with one
entry, `Download`, at least 28 px high, which has the keyboard when the menu opens; at the pointer it opens at the pointer, from the keyboard just
under the left part of the row, and it is kept inside the window. `Enter`, `Space` or a click runs the entry; `ArrowDown`, `ArrowUp`, `Home` and
`End` keep the one entry focused and the list below does not move; while the menu is open it takes every key. `Escape`, a press anywhere else, the
entry losing the focus and a change of the panes' layout close it (the focus moving to another pane does not), and each of them puts the keyboard
back in the terminal. The entry shows the focus with the 2 px ring and a rose wash; the words are the second cue. The desktop has no such menu.

**Notifications.** The Settings dialog gets a group `Notifications` after Accessibility, in the browser build only, in the manner of 6.24: one
row with the name `Notify me`, the dim meta `bell and attention`, the state as a word at the right, one sentence of advice and one button.
They are off until the person turns them on, and the browser is asked only when `Turn on` is pressed, never at load.

| State | Shown as | Tone and second cue | Advice | Button |
|---|---|---|---|---|
| `off` | off | plain; the word | Bell and attention marks also show in the tab title. Turn this on to get a browser notification as well. | `Turn on` |
| `on` | on | ok; the word | A browser notification appears when a pane out of view rings the bell or needs you. It never contains terminal text. | `Turn off` |
| `blocked` | blocked by the browser | warn; the word, an underline and `!` | The browser blocks notifications for this site. Allow them in the site settings of the browser, then choose Check again. | `Check again` |

A notification says only the words of the mark (`Waits for you`, `Rang the bell`, `Command failed`, `Command finished`), never a pane name, a command or
output, and it has no sound. It is made only while the page is hidden and the state is `on`, and at most one for each kind in five seconds. Pressing
it brings the tab forward. The choice is one flag in the local storage of the browser on this device and nothing else is stored. The button is one
button that changes with the state; `Check again` reads the permission of the browser again and never asks. The group is absent where the browser
has no notification API, and on the desktop. The row is drawn in the three states on the web window view (off, on, blocked by the browser).

**Tab marks.** While the page is hidden (the tab is in the background or the window is minimised) the title of the browser tab starts with the
glyphs of the kinds of 6.23 that happened or wait, in the fixed order `?`, `▲`, `!`, `✓`, each followed by a space, then `holzWave` (`? ▲ holzWave`);
with none, the title is `holzWave`. The icon of the tab changes to the holzWave mark with a small dot in `--hw-rose` at its top right corner: rose
is the colour of the thing that asks for the person's action, and the dot is the one place where it marks attention outside the page. Both are put back
the moment the tab is visible again and are never touched while it is visible. The question mark shows while a card waits; the other three begin
again each time the tab is hidden. The title never holds anything a pane wrote: no pane title, directory, command or output. The two icon files
(`favicon.svg` and `favicon-attention.svg`, the second the first plus the dot) are served from the same origin, and the page adds the icon link
itself in the browser build only.

**Server sessions.** The Connections view lists the server the page was loaded from as a row named `This server` (6.34). It has no state dot beyond `signed in`, no `Sign in…` and no `Sign out`, because the browser's own sign-in is the credential; its `New session`, its session rows, the writer label and `Take over` are those of 6.34. The session is drawn by the person's own worker for the browser, so its scroll position and selection are shared by every tab of the person, as for every pane of the window. The Account and People groups of 6.35 and the page `Authorize device` are drawn only here, and the choice of the sync target (6.9) is not.

**Controls the browser does not have.** Tear-off entries (`Move pane to new window`, `Move tab to new window`), the text size from the system,
the group `Local network` and `Reveal in folder` are absent from menus, the palette and the Settings, not disabled. When a stored shortcut
or a script calls one of them anyway, the answer is `Not available in the browser.`, said as the notice of the status line for as long as a
notice lasts (rule 5 of section 6), and nothing is asked of the server. A pane or tab that is dragged out of the window and released says the
same sentence and stays where it was; no outline is asked for on the way out. The palette lists every other entry as it does on the desktop.
Copying and pasting use the clipboard of the browser (section 7).

**Links.** A link in the visible output of a pane, a printed address or a link the program marked (6.31), opens by the person's deliberate act, the
same act as on the desktop: a click with `Ctrl` or `Cmd` held, either of the two. While the modifier is held the link under the pointer gets the
underline and the pointer cursor (section 2, rule 4: the link underline); without it a click selects as always. Only `http`, `https` and `mailto`
addresses are links in the browser, after the same checks as on the desktop. A web address opens in a new browser tab that cannot reach the page
(`noopener`, `noreferrer`) and never in the page's own tab; a mail address is handed to the browser. When the visible text of a link and its
target differ, the click asks first in the dialog of 6.31 (`Open this link?`), which looks the same here. A `file` link and a file path are not
offered at all (no underline, no click, and hint mode skips them), because a folder cannot be shown from a browser. The links in a reply of the AI
chat and the items with an address in the sidebar open in a new tab by a plain click, as they open the system browser on the desktop.

**Full screen and the key lock.** The palette has the entry `Toggle full screen` in the browser build only (it is not drawn on the desktop) and it has no
shortcut. It enters full screen from the press that runs it and leaves again when it is run once more; the page never enters full screen by itself.
In a browser that has the keyboard lock (Chromium) the entry also asks it to pass the keys the browser keeps (`Ctrl+W`, `Ctrl+T`, `Ctrl+N`, `Ctrl+Tab`,
`Ctrl+PageUp`, `Ctrl+PageDown`) and `Escape` to the page, only while the page is in full screen, and the keys are let go on every way out. The status
line then says `Browser shortcuts are off in full screen. Hold Escape to leave.` A browser without the lock keeps its chords and the status line says
nothing about locking; a lock that the browser refuses says nothing either.

**Keys.** The browser keeps some chords for itself; the web build has its own table of defaults without them and labels them with `Ctrl` and
`Alt` (section 7). The shortcuts view and the palette show the chords of that table (close pane `Alt+Shift+W`, new tab `Alt+Shift+T`, new workspace `Alt+Shift+N`); a chord that has none shows `–`.

![The web window](design/web-window.png)
![The browser as a platform](design/platforms/web.png)

**Accessibility.** The marks are words and shapes, so colour is never the only cue; the dashed edge and its label are the target's cues;
hairlines use `--hw-line` and dim text `--hw-ink-2` under `contrast-high`; under `forced-colors` the marks and the label box use system
colours and the dashed edge keeps its dashes; nothing moves, so `motion-reduce` has nothing to remove. Every size is in `rem`.

**Not drawn.** SSH key management, API tokens, shared panes, an audit viewer, a mobile layout of the page, a button that installs the page
as an app, and a control that sets the upload folder (the server's configuration does).

### 6.31 Links and images in a pane

A program can mark a stretch of its output as a link (the terminal sequence OSC 8) and can draw pictures into the grid (the Kitty graphics protocol). Both
are content of the program, so they are drawn inside the pane body and follow the rules of the grid: nothing here adds a control, and rose appears only
where section 2 rule 4 names it. A printed address (`https://…`) keeps the look and the gesture it has; this section says what a marked link adds.

**The link at rest.** A cell range that the program marked as a link carries a dotted underline of 1 px in the cell's own foreground colour, at the
underline position. The dots are a second cue that does not depend on colour. A cell that already has an underline from the program keeps that
underline and gets no extra dotted line; such a link is found by the hover rules below, so a link is never lost, only its resting cue. A link that
wraps onto the next line is two ranges, and each is a link on its own.

**Hover.** With the pointer over a link the underline becomes solid (the second cue) and the real target appears in the status line (the target
rule below). Without the open gesture a click selects as always.

**The gesture** is the one a printed address has: `Cmd` held on macOS, `Ctrl` held on Windows and Linux, either of the two in a browser. With the
gesture held the link under the pointer gets a solid underline in `--hw-rose` and the pointer cursor.

| State | What is drawn | Tone and second cue |
|---|---|---|
| At rest | dotted underline, 1 px, in the cell's foreground colour | the dots; no colour of its own |
| Pointer over it | solid underline in the same colour; the target in the status line | the solid line in place of the dots, and the `LINK` segment |
| Gesture held | solid underline in `--hw-rose`, pointer cursor | rose as the action colour, and the cursor shape |
| The program already underlined the text | its own underline stays, no dotted line | found by hover and by hint mode |
| A scheme that is not allowed | nothing: plain text | no underline, no click |
| A `file` link in a server pane or in a browser | nothing: plain text | no underline, no click |
| A link that cannot be opened | the notice segment of the status line holds the host's sentence for the usual time; the text is not marked | the words; the `LINK` segment may stay beside it |

**The target in the status line.** A segment at the left of the status line holds the label `LINK` (uppercase, tracked, `.label`) and the target in
mono, with the scheme and the host in `--hw-ink` and the rest in `--hw-ink-2`. A target longer than the segment is cut in the middle with `…` so that
the scheme, the host and the last 12 characters stay. It is there while the pointer is over a link and gone when it leaves. It does not use the
segment of the notice. The full target is also the accessible name of the segment. It follows the cells under a pointer that does not move: when the program
changes the link there, the segment shows the new address, and it goes when the link goes.

**A link that cannot be opened.** A click, or a link chosen in hint mode with `Shift` held, that the host refuses or cannot carry out (a scheme taken off the list since, a `file` link whose file is gone, no program to open it) is answered in the notice segment of the status line (section 2 rule 5) for the usual time. The words are the host's own fixed sentence, which never repeats the address; any other failure is the one sentence `The link could not be opened.` There is no dialog and no mark on the text, and the `LINK` segment may stay beside the notice while the pointer stays on the link. A click that succeeds says nothing. The browser build opens a tab itself and never asks the host, so it has no such notice.

**The question.** When the visible text of the link and its target differ, the click with the gesture opens a dialog of the shape of the close
confirmation (title, body lines, two buttons, centred over a backdrop): the title `Open this link?`, a line `The text says` with the visible text in
mono (cut at 120 characters), a line `The link goes to` with the full target in mono, wrapped and never cut, and two buttons, `Cancel` (focused
first, the default) and `Open` (solid rose, section 2 rule 4). `Escape` and a click outside the dialog are `Cancel`. The dialog is a floating layer,
so it may have a shadow. The two texts are compared with the host in lower case and a trailing slash left out. A scheme the text states (`http`, `https` or
`mailto:`) must be the target's own, so `https://` over an `http://` target asks, as does `mailto:` over a web address; a text that looks like a mail
address is only equal to a mail target; a text without a scheme is compared after the target's scheme. When they are equal the click opens at once, as for a printed address. Every string in the dialog is set as text and never as markup.

**Schemes.** Only the schemes of the allowed list are links. The list is `http`, `https`, `mailto` and `file` unless the settings file says
otherwise. A range with another scheme is text and gets no underline. The list and the memory below are settings in the settings file, described in
`docs/config.md`, like the `shell` setting; the Settings dialog has no row for either.

**The `file` link.** On the desktop, in a local pane, a click with the gesture shows the file in its folder, as `Reveal` does for a file of the Files
pane, and never opens or starts it; the question above applies to it when the text differs. In a pane of a server (an SSH pane, or any pane in a
browser) a `file` link is not a link: no underline, no click, and hint mode skips it.

**Hint mode** lists the marked links with the same labels as the printed ones, in reading order with the marked links ahead of the printed ones on their row (a printed address that lies over a marked link is not listed twice). Choosing one with `Shift` held does what the click does, including the question; choosing it without `Shift` copies its address, as for a printed one.

**Pictures: layers.** A picture is drawn at its cells in one of three layers that the program chooses: below the cell backgrounds, between the
backgrounds and the text, or above the text; within a layer the program's own order decides. It is clipped to the pane body, never over the pane
header, the scrollbar or a neighbouring pane, and it moves with the text when the pane scrolls. The cursor, the selection wash and the marks of
search and copy mode are drawn above every layer. A picture has no border, no rounded corner and no shadow (section 2 rule 4); transparent
pixels show what lies under them.

**Pictures: cells of the placeholder character.** Cells that carry the placeholder character of the protocol are drawn as the program's picture and
never as a text glyph, and copying them copies spaces.

**Pictures: animation.** An animated picture plays frame by frame at the program's own timing. Under reduced motion (the setting, or the system
preference when the setting is `system`; the root class `motion-reduce`) it does not move: a picture that was never started shows its first frame,
and a picture that was running stays on the frame it shows. There is no control to start it, and when reduced motion ends the animation goes on from
where it stopped.

**Pictures: memory.** When the picture memory of a pane is full the oldest pictures disappear, without a mark.

**What is not drawn.** No row of the Settings dialog for the picture memory or the allowed schemes. A picture has no text alternative, and the
accessible text of the terminal leaves it out. Under `forced-colors` and `contrast-high` pictures are drawn as sent, because they are content; the
link underline takes the system link colour under `forced-colors` and keeps its dotted and solid forms.

![Links in a pane](design/links.png)
![The link dialog](design/link-ask.png)
![Pictures in a pane](design/images.png)


### 6.32 Host shell in a Flatpak

A Flatpak runs an app in a sandbox, and a terminal in a sandbox sees none of the person's own tools. holzWave can start a local pane on the computer itself through `flatpak-spawn --host`, which takes that pane out of the sandbox and gives it the person's rights. That is a choice about safety, so it is a switch of the Settings dialog, and it is off. The group exists only inside a Flatpak. On macOS, Windows, a Linux session that is not a Flatpak, a phone and the browser build it is not drawn at all (section 7), and there is no phone drawing.

**The rule.**

1. Inside a Flatpak a local pane starts **in the sandbox** by default. With `Host shell` on, new panes start on the host through `flatpak-spawn --host`. A pane that is already open keeps running where it started, as it does when the `shell` setting changes.
2. The switch is a fact of this computer and is **never synced**. `settings.json` travels to every computer, and a second computer must not inherit a way out of the sandbox; the value is kept in a file of this computer, in the manner of `local_network.json` (6.24).
3. The switch cannot grant the permission. The permission to run commands on the host (`org.freedesktop.Flatpak`) is fixed in the manifest when the Flatpak is built, so the group can only use it or say that it is missing. If a Flatpak tool revokes it, the group reads `not available`.
4. Nothing asks by itself: there is no notice at the start and no timer. The way to the switch is the Settings dialog.

**The group `Host shell`.** In the Settings dialog, after Accessibility and before the buttons, inside a Flatpak only. A `fieldset` with `role="group"` labelled by its legend, in the manner of the group of 6.24: one toggle row with the name `Run panes on the host`, the dim meta `Flatpak`, the state at the right and one sentence of advice below it. The toggle is a real checkbox with the accent colour of the page and the 2 px focus ring. The state is words first; a state that needs attention is also underlined or marked.

| State word | Shown as | Tone and second cue | Advice |
|---|---|---|---|
| `off` | off | plain; the word; the box is empty | New panes start inside the Flatpak sandbox. Your own tools and the files outside it are not reachable from them. |
| `on` | on | warn; the word, an underline, the mark `!` and the tick in the box | New panes start on this computer through flatpak-spawn, with your rights and outside the sandbox. Panes that are already open keep running where they are. |
| `unavailable` | not available | plain; the word, the mark `?`, a dashed box and a disabled checkbox | This Flatpak was started without the permission to run commands on the host (org.freedesktop.Flatpak), so panes can only start in the sandbox. See the Flatpak page of the documentation. |

The words come from a table of the page keyed by the host's state word `off`, `on` or `unavailable`; a word that is in no table reads as `unavailable`. These three keys and the two notice sentences below are the contract between the page and the host. The state is read when the Settings dialog opens and after the switch is pressed. A press that the host refuses is said in words in the line of the group (`role="status"`) and the box goes back to what the host holds.

**The pane notice.** While the state is `off` or `unavailable` and the app runs in a Flatpak, each new local pane shows one line before the shell's own output. It is the pane's existing notice line (the line `Install wsh on this host` uses): one line in the warn colour on the screen only, never sent to the shell, so there is no new drawing. The sentences are:

- `off`: `holzWave: this pane runs inside the Flatpak sandbox. To use the tools on this computer, turn on Host shell in Settings.`
- `unavailable`: `holzWave: this pane runs inside the Flatpak sandbox. This Flatpak has no permission to run commands on the host; see the Flatpak page of the documentation.`

With the switch `on` the pane says nothing. A pane of a saved SSH connection shows no such line, because it starts no local shell. The notice is shown for every new pane, not once, because a pane that cannot reach the person's tools is the thing the person must be able to see where it happens.

**Limits a reader must know.** The switch cannot grant the permission, so this design needs the manifest to ship it (the reference manifest does); a build without it always reads `not available`. Whether `flatpak-spawn --host` gives the shell a controlling terminal (job control, `sudo`) has not been run and stays unverified until row L10 of [test-plan.md](test-plan.md). The directory of a host pane is tracked only through OSC 7; the directory of a sandbox pane is read from the process, as everywhere else. The integrations (Kubernetes, Talos, Azure CLI, GAM7) stay unavailable in a Flatpak whatever this switch says. No Flatpak has been started on a desktop for this section, so the look in a real Flatpak is not run.

**Accessibility.** Hairlines use `--hw-line`, so `contrast-high` holds; the 2 px focus ring holds for the checkbox and for the row; under forced colours the row gets a system-colour outline and the group keeps its border; nothing moves, so `motion-reduce` has nothing to remove. Every size is in `rem`; the checkbox row is at least 28 px high. The state word is the second cue in every state, so colour is never the only one: `on` has the mark `!`, an underline and the tick, `not available` has the mark `?` and the dashed box.

![Host shell](design/host-shell.png)

![Host shell states](design/host-shell-states.png)

**Not drawn.** A control for `flatpak override`, for Flatseal or for a link outside the app (holzWave draws only what it builds: the advice names the permission, and the Flatpak page of the documentation says how to grant it), a notice at the first start, a badge, and a setting for panes that are already open.


### 6.33 Window frame

On Windows and Linux the page's bar is the window bar. The system's title bar is gone, the free space of the title row drags the window, and the three window buttons sit in that row in the style of the system the window runs on. macOS keeps the overlay bar with the traffic lights, unchanged (6.1, section 7). Android and the browser have no window buttons (6.6, 6.30). This reverses the earlier rule that Windows and Linux keep the system's title bar above the page; that rule is still available as the setting `System`.

**The rule.**

1. With the setting `Frame` on `Auto` or `Own`, a window on Windows and Linux has no system title bar. The title row (32 px) holds the workspace pills, the free space, the host name and the clock, and the window buttons. With `System` the system keeps its title bar above the page's bar, as before, and the page draws no window buttons.
2. The free space of the bar drags the window, and a double click on it maximises the window or restores it. The edges of the window resize it; the page draws no resize handles. The window buttons, the pills and the tabs are not drag regions.
3. In full screen the group of buttons is not drawn, because the system draws no bar there; the page keeps its own bar.
4. The style, the side and the set of buttons are read from the system and can be overridden by the setting `Buttons` on Linux. Both settings are facts of this computer and are never synced.

**The three styles.** The style follows the system.

| Style | Where | Hit box | Drawn |
|---|---|---|---|
| Flat | Windows, and every Linux desktop that is not GNOME | 46 px wide and 32 px high (the whole title row), side by side without a gap, flush with the window edge: the bar's side padding does not apply to them | the glyph alone, 10 px, drawn by the stylesheet with 1 px strokes in the current colour, so it is the same on every platform and does not depend on a font |
| Round | GNOME | 28 by 28 px, 6 px between them, the group inset from the window edge by the bar's padding and centred in the 32 px row | a circle of 24 px inside the hit box (a 1 px `--hw-line` ring) and an 8 px glyph in the same drawing |

When the desktop cannot be read, the style is flat, on the right, with all three buttons. A hit box is never smaller than 28 by 28 px at any UI scale: sizes are in `rem` and the floor is written as `max(28px, …)`.

**Window buttons.** The same four drawings in both styles; the name is the accessible name and the `title`.

| Button | Glyph | Name |
|---|---|---|
| Minimise | a horizontal line | `Minimize` |
| Maximise | a square outline | `Maximize` |
| Restore (while maximised) | two square outlines, the back one offset by 2 px and drawn by its top and right edge | `Restore` |
| Close | two crossing diagonals | `Close` |

**Side and set.** The side and the buttons come from the system.

- Windows: the three on the right, in the order Minimize, Maximize, Close.
- GNOME: the desktop's own button layout decides the side, the order and which buttons exist. Stock GNOME (layout `appmenu:close`) shows one round close button on the right; Ubuntu (`appmenu:minimize,maximize,close`) shows three; a layout that starts with the buttons (`close,minimize,maximize:`) puts them on the left in that order. A layout that names no window button is read as the close button alone, because a window that cannot be closed by pointer is worse than one button the person did not ask for. If a layout puts buttons on both sides, the side that holds `close` is drawn and the other side's buttons are not. When a layout names window buttons but no close button, the side of its first window button is drawn with the buttons it names. When the layout cannot be read, the right side with all three buttons is drawn.
- Every other Linux desktop: flat, on the right, all three.
- The group on the right sits after the host name and the clock, separated by the bar's padding. The group on the left sits before the first workspace pill, separated by the bar's padding. The page does not move a button for the resize strips.

**States.** Every state has a second cue besides colour.

| State | Fill and glyph | Second cue |
|---|---|---|
| Rest | no fill; glyph `--hw-dim` | the drawn glyph |
| Hover | the wash `--hw-rose-wash` fills the drawn shape (the whole hit box when flat, the 24 px circle when round); glyph `--hw-ink` | fill and tone, as the other buttons of the bar |
| Windows close under the pointer | fill `--hw-danger`, glyph `--hw-ground` (inverted) | the filled box and the inverted glyph |
| Linux close under the pointer | the normal wash, not red | as hover |
| Keyboard focus | the 2 px ring `outline: 2px solid var(--hw-ink)`, offset -2 px, on the hit box | the ring |
| Maximised | the Maximize button shows the restore glyph; its name and title are `Restore` | a shape and a word, not a colour |

`--hw-danger` as a fill with `--hw-ground` as the glyph keeps at least 4.5 to 1 in the three built-in themes: 7.4 to 1 in the dark default, 6.5 to 1 in Schiefer and 6.0 to 1 in Papier, whose `--hw-danger` is a dark red. The red close button is Windows only; on Linux the close button is as quiet as the other two.

**The group `Window frame` of the Settings dialog.** On Windows and Linux only (a Flatpak is Linux), after Accessibility and the group `Host shell` where that is drawn, and before the buttons. It is absent, not disabled, on macOS, on Android and in the browser. A `fieldset` with `role="group"` labelled by its legend, with two rows in the manner of the dialog's choice rows (the accessibility rows `System / On / Off` are the pattern).

| Part | Words |
|---|---|
| Legend | `Window frame` |
| Row | `Frame`, with the dim meta `Windows and Linux` and the choices `Auto`, `Own`, `System` |
| Advice under `Frame` | `Auto draws holzWave's own bar as the window bar. System keeps the system's title bar above holzWave's bar.` |
| Advice under `Frame`, Linux only, as a second sentence | `Under Wayland the own frame has not been tried; if the window cannot be moved or resized, choose System in this dialog (open it from the palette).` |
| Row, drawn on Linux only | `Buttons`, with the dim meta `Linux` and the choices `Auto`, `Flat`, `Round` |
| Advice under `Buttons` | `Auto follows the desktop: round on GNOME, flat on every other desktop.` |
| Status line (`role="status"`, empty until it is needed) | `Applies after the next start.` |
| Failure line, when a command is refused with no reason to show (the same line, `role="status"`) | `The command failed.` |

The status line shows its sentence whenever the stored choice differs from the one the open windows were built with. The words of the host's answer are `auto`, `own`, `system` for the first row and `auto`, `flat`, `round` for the second; a word that is in no table reads as `auto`. A refusal that carries a reason shows that reason in the same line, as text, with control characters blanked and at most 240 characters. The failure sentence is the dialog's one sentence for a command that failed, the same words as in the group `Host shell` (6.32), so a person reads one sentence for one thing. These words and the sentences above are the contract between the page and the host. Both choices are a fact of this computer and are never synced: `settings.json` travels to every computer, and a GNOME computer's `round` must not reach a KDE one; the values are kept in a file of this computer, in the manner of `local_network.json` (6.24).

**Limits a reader must know.**

- The outer edge of the window is the system's resize area: on Linux the outer 5 px and on Windows the top border belong to the resize handler, so the top and right edge of the close button lose that much of their hit box to it. The page does not work around it.
- The Windows 11 snap layout flyout over the Maximize button is not built, because it needs a native surface that a web view does not give. Dragging the window to an edge and the Win key with the arrow keys still snap it.
- **The own frame was never seen on a real Windows or Linux window by anyone.** Dragging, resizing, the buttons, the drawn states and the behaviour under Wayland are all unverified, and the author works on a Mac. The pictures below are drawings of the design, not screenshots.
- Under Wayland `Auto` is the own frame as well. Whether such a window can be moved and resized is not known and cannot be detected while the app runs; the setting `System` is the way out, and the advice under `Frame` says so on Linux.
- Not run on a real window, besides the line above: the drag by the free space of the bar, the double click that maximises or restores, the resize edges, the pointer drop target across two windows without a frame, and the look of the glyphs and states in WebView2 and WebKitGTK (they were read in WebKit and Chromium on a Mac, as computed styles and shapes, not as a picture of a real window). Windows high contrast themes and the forced-colours case were emulated by the test engines and not seen on Windows.
- On GNOME the button layout is read through `gsettings` with a deadline of 300 ms, and the start of the first window waits for it for that long at most; a timeout or an unreadable answer costs the layout, never the start (flat or round per the rules above, right, three buttons). Whether `gsettings` exists and sees the host's layout inside a Flatpak is not known; if it does not answer, GNOME gets the default, round buttons on the right, even where the host has one close button. The 300 ms is a guess about real answer times that nobody measured on a loaded session.
- On macOS the first window is now built in the shell's start-up, by the path the tear-off windows already used, and no longer from the window configuration. The overlay bar and the traffic lights are meant to be unchanged, and nobody has looked at that first window since the change.
- At a UI scale of 75 percent the title row is 24 px high, and the 28 px floor of the hit box makes the buttons 4 px taller than the row: they reach into the tab row. The spec's floor was kept and the row was not changed; nobody has looked at it.
- The window buttons are reached by the keyboard like every other control of the bar (6.1): `Shift+F6` lands on the last control, which is the close button when the buttons are on the right, and `Tab` and `Shift+Tab` walk the bar. This was run in WebKit and Chromium against a stub window with Playwright's keyboard; it was not run on a real keyboard, in the web view of a real Windows or Linux window, or with a screen reader.

**Accessibility.** The glyphs are drawn with `currentColor`, so `contrast-high` reaches them through `--hw-dim`, which it swaps for the stronger token like the rest of the bar. Under forced colours the strokes are `ButtonText`, hover and focus use `Highlight`, and the ring stays 2 px. The 160 ms transition of the hover fill is removed under `motion-reduce`, as for the other bar buttons. Every button is a real `button` with its name, reachable by the keyboard (6.1: `F6`, then `Tab`), with the 2 px ring. Every size is in `rem`, with the 28 px floor above.

![Window frame](design/window-frame.png)

![Window frame settings](design/window-frame-settings.png)

**Not drawn.** The Windows 11 snap layout flyout, a menu on a right click of the bar, resize handles, a drawing for a phone and buttons in the browser (a page cannot draw or move the window it is shown in, 6.30), and a setting for the side of the buttons (the system decides it).


### 6.34 Server connection and sessions

A holzWave server ([server.md](server.md)) can keep terminal sessions running with no window open, and the app can show one of them in a pane. The server is a source of sessions in the Connections view (6.3), a session is a pane of the window (6.1), and nothing else is new: no dialog of its own, no title-bar button and no palette entry for sessions. All of it exists only when the operator has turned `Remote sessions` on (6.35) and the person has signed in to the server (6.35). With that switch off the server row says so and nothing else is drawn. The window of the browser build lists the server it was loaded from in the same place (6.30).

**The rule.**

1. The server is one row of the Connections view, above the saved connections, in the manner of a saved connection. Its running sessions are listed under it. `Attach` shows one in a pane; `New session` makes one and shows it.
2. A remote pane keeps the pane header of 6.1 and adds the writer label between the title and the context slot. The three levels stay apart: the label is a small square chip, never a round pill.
3. A session has **one writer at a time**. Every other pane that shows the session watches: what it types is dropped. The write right is free when its holder has been silent for 30 seconds; it never moves by itself, so everyone sees `FREE` until someone presses `Take over`. A pane that detaches gives the right up at once.
4. The × of a remote pane **detaches**; it never ends the session. Its title and accessible name are `Detach session`, it asks for nothing, because the session keeps running, and the session stays in the list. Ending a session is the button `End` of the list.
5. The session is drawn by the person's own client from the bytes the server sends. Scroll position, selection and search belong to the pane, so they are never shared between two panes that show the same session.

**The connection source.** The row and its sessions sit in a group under the `.label` legend `Server`, above the list of saved connections. The row has the name of the server (its host), the address in mono, a state dot with a word, and the buttons. The dot is a shape and the word is the cue; the state is read again when the view opens.

| State | Dot | Word | Buttons |
|---|---|---|---|
| signed in | filled | `signed in` | `Sign out`, and `New session` while remote sessions are on |
| not signed in | hollow | `not signed in` | `Sign in…` |
| signing in | dotted ring | `signing in…` | none (the step of 6.35 is open) |
| unreachable | hollow with a dash | `unreachable` | `Sign out` |

`Sign in…` opens the step of 6.35. `Sign out` forgets the sign-in of this device and asks with a second click, like `Turn off` in 6.9 (the words of the second press are `Click again to sign out`); the address stays, so the next `Sign in…` goes straight to the code. Under a signed-in row the sessions are listed, one row each: the title (the program's title, else `Session N`, where N is the place in the list, oldest first), the working directory in mono with `~`, the size (`120×40`), the start (`14:02` today, `Mon 14:02` within a week, the date otherwise), how many are attached (`N attached`, `0 attached` when nobody is), the word `held` when somebody writes and `free` when nobody does, and the buttons `Attach` and `End`. `held` has a filled square before it and `free` a dashed one, the shapes of the writer label. `End` asks with a second click (`Click again to end`). `Attach` on a session that this window already shows moves the keyboard to that pane. A line under the row says in the dim colour what a session is:

| Line | When |
|---|---|
| `A session keeps running on the server with no window open. It ends when the server restarts or after a day with nobody connected.` | the page knows the idle limit of the server, and says it in words (`a day`, `6 hours`) |
| `A session keeps running on the server with no window open. It ends when the server restarts or after the idle limit of the server with nobody connected.` | the page does not know it |

When the server has remote sessions off, the row is the signed-in row without `New session`, no list is drawn and the dim line `Remote sessions are off on this server.` takes the place of the sessions. When the server has the sync target off nothing changes here. On Android, while the saved secrets are locked, the buttons of the row are replaced by the dim line `Locked: unlock to connect` (the pattern of 6.6): the token of the device lives in the vault.

**The remote pane.** The header keeps the dot, the type icon (`⌁`), the title and the context slot, and the hit targets of 6.1. Between the title and the context slot sits the writer label, a `.label` chip with a shape and a word, and after it, on `WATCHING` and `FREE` only, the hairline button `Take over`.

| Label | Shape | Meaning | Tone |
|---|---|---|---|
| `WRITING` | filled square | this pane holds the write right | ink |
| `WATCHING` | hollow square | another pane writes | secondary ink |
| `FREE` | dashed square, dashed border | nobody writes | dim |

The label never names the other writer or the device that writes: names stay in the Devices list (6.35). Rose is not used for the label: it is neither focus nor action. The word is the cue and the shape the second one; both survive `forced-colors` and `contrast-high`, and the chip is a `role="status"` region, so a change from `WATCHING` to `WRITING` is announced. `Take over` is a real button: 24 px tall in the strip with a hit box of 28 px on the desktop (the extra 2 px above and below are not drawn), 44 px on a phone and a tablet, with the 2 px focus ring; it has no chord and no palette entry in this phase. Pressing it moves the write right to this pane and sends this pane's size once (the grid rule below); every other pane sees its label change.

The context slot says the server (its host), then the size of the session (`120×40`) when it differs from the tile. While a state below lasts, it says the state's word instead.

**The states of a pane.** Each state is a dot shape and a word in the context slot, and one dim line on the screen. The writer label stays only where the page knows it.

| State | Dot | Header context | Line on the screen | Writer label |
|---|---|---|---|---|
| attaching | dotted ring | `Attaching…` | `Attaching…` | not shown |
| loading older lines | as before | `Loading older lines…` | `Loading older lines…` | shown |
| resyncing | dotted ring | `Resyncing…` | `Resyncing…` | shown |
| session ended | hollow with a dash | `Session ended · exit N` (warn, underlined, `!`) | `Session ended · exit N` | not shown |
| reconnecting | dotted ring | `Reconnecting…` | `Reconnecting…` | not shown |
| server unreachable | hollow with a dash | `Server unreachable` (warn, underlined, `!`) | `Server unreachable. Press Enter to try again.` | not shown |
| input dropped | as before | `Input dropped` (warn, underlined, `!`) | none: the screen stays the session's | shown |

On attach the server sends the current screen and a bounded tail of the scrollback. `Loading older lines…` is shown when the person has scrolled to the top of what the page holds and the server has older lines: it is automatic and draws no new control, because it is a new attach with a longer tail, and the screen keeps its place. The phone has the same rule and no second limit. `Resyncing…` is the server asking for a fresh start because the pane was too slow. A session that ended stays in the pane with its last screen and the notice and closes on `Enter` or on ×, like a held terminal in 6.26. `Input dropped` is the server saying that the program of the session leaves more than 1 MiB of its input unread (it is busy, stopped or not reading at all), so what this pane typed after that did not reach it. It is said in the header only, because a line on the screen would move the session's own text, and it goes 5 seconds after the server last said it; the server says it at most once a second while input is being dropped. While the connection is down what is typed is not sent and not kept (unlike an SSH pane, 6.8: a session may have another writer when the connection is back); after the tries are used the pane waits for `Enter` for one more try.

**The grid rule.** A terminal program lays out for one size, so a session has one size: the size of the **writer's tile**. A watcher draws the same grid from the top-left corner, clipped by its tile when the tile is smaller and over empty ground when it is larger; it never resizes the session by resizing its own tile. The context slot says the size while it differs from the tile. When someone takes over, the new writer's size is sent once and becomes the size of the session.

**Phone and tablet.** The Connections view is a full-screen sheet on a phone, and the rows are at least 44 px high: the server row, every session row and every button. A session row on a phone says the title and `free` or `held` and wraps the rest (directory, size, start, attached count) on the lines under it. The writer label takes the empty place of the top strip (6.6) and `Take over` follows it at 44 px; the name of the pane is cut first, nothing else moves. On the tablet the header is 44 px (6.6) and holds both.

![Server sessions](design/server-sessions.png)

![Server pane states](design/server-pane-states.png)

![Server sessions on a phone](design/server-sessions-phone.png)

**The words.** One table; a unit test compares the page with it.

| Word | Where |
|---|---|
| `WRITING`, `WATCHING`, `FREE` | the writer label (the page writes them in capitals itself) |
| `Take over` | the button after `WATCHING` and `FREE` |
| `Detach session` | the title and the accessible name of the × of a remote pane |
| `signed in`, `not signed in`, `signing in…`, `unreachable` | the state word of the server row |
| `Server` | the legend of the group |
| `Sign in…`, `Sign out`, `Click again to sign out`, `New session` | the buttons of the server row |
| `Attach`, `End`, `Click again to end` | the buttons of a session row |
| `free`, `held`, `N attached`, `Session N` | a session row |
| `Remote sessions are off on this server.` | the row, while the server has them off |
| `Locked: unlock to connect` | the row on Android while the secrets are locked |
| `This server` | the row in the browser build (6.30) |
| `Attaching…`, `Loading older lines…`, `Resyncing…`, `Input dropped`, `Session ended · exit N`, `Reconnecting…`, `Server unreachable`, `Server unreachable. Press Enter to try again.` | the states of a pane |

**Limits a reader must know.**

- A session ends when the server restarts, and when the worker of the person ends after the idle limit of the server (a day by default) with nobody connected. It survives every client leaving. The 64 panes and 8 sockets of a person are shared with the browser window.
- Typed bytes are not kept while the connection is down.
- **Nothing here was run on a real phone, on a real desktop against a real server over a real network, behind a real reverse proxy, with two real people, or across a real server restart.** The pictures are drawings of the design, not screenshots.

**Accessibility.** The label and the states are words and shapes, so colour is never the only cue. Hairlines use `--hw-line` and dim text `--hw-ink-2` under `contrast-high`. Under `forced-colors` the squares are drawn in system colours (the filled one as `CanvasText`, the others as borders, the dashed one with its dashes) and the dot shapes keep their borders. Nothing moves, so `motion-reduce` has nothing to remove. Every size is in `rem`; `Take over`, `Attach`, `End` and the buttons of the row are at least 28 px high on the desktop and 44 px on a phone.

**Not drawn.** A palette entry for sessions, sharing a session with another person, a second server, a menu on the header or the label, a session on both sync targets, renaming a session, a button in the header that ends it, and a key for `Take over`.

### 6.35 Devices and signing in to a server

An app (desktop or phone) signs in to the server with the person's own login in a browser and keeps a token of its own, a **device**, in the OS keychain. The server never learns the passphrase of the sync and never sees a file in plain text. Two switches of the operator, `Sync target` and `Remote sessions`, are off by default, and while both are off the sign-in of an app cannot be reached (the lists of devices and the revocation still work, so a device can always be ended).

**The step in the app.** A small dialog over the window, in the box of the Settings dialog, opened by `Sign in…` of the server row (6.34) and of the sync popover (6.9). Two screens. The first asks for the address; once an address is known it is not shown again (`Sign out` forgets the token and keeps the address, so `Sign in…` goes straight to the second screen). The second shows the request.

| Screen | Words |
|---|---|
| 1: heading | `Sign in to the server` |
| 1: field | `Server address`, with the help line `The address of your holzWave server, starting with https://.` |
| 1: refusal of an address that is not an `https` origin | `Use an address that starts with https://.` |
| 1: read-only row | `Device`, and the name of the computer (shown, never asked) |
| 1: buttons | `Continue`, `Cancel` |
| 2: the code | the user code in mono in large type (`BCDF-GHJK`) |
| 2: read-only row | `Server`, and the address the request goes to, in mono |
| 2: lines | `Opening your browser…` while the page opens, then `Enter this code only if you started this here.` |
| 2: buttons | `Open again`, `Cancel`; after a failure `Start again`, `Cancel`; after success `Close` |

The states of the second screen are a dot shape and words; the failures also carry the mark `!` and an underline.

| State | Dot | Words |
|---|---|---|
| waiting | dotted ring | `Waiting for approval…` |
| approved | filled | `Signed in` |
| denied | `!` and an underline | `The request was denied.` |
| expired | `!` and an underline | `The code expired. Start again.` |
| too many | `!` and an underline | `Too many devices or requests. Try again later.` |
| unreachable | `!` and an underline | `The server cannot be reached.` |
| off | `!` and an underline | `Device sign-in is off on this server.` |

A failure that the first screen meets (the app could not start the request) is said in place of the refusal line of the field, in the same words (`The server cannot be reached.`, `Device sign-in is off on this server.`), and the field keeps what was typed. A refusal of the app itself that has no word in this table (for example a keychain that cannot be written) shows the app's own fixed sentence in the style of a failure, never a text that came from the server. While the saved secrets are locked on Android, the buttons that need them (`Continue`, `Open again`, `Start again`) give way to the dim line `Locked: unlock to connect`; `Cancel` stays, and the buttons return by themselves after the unlock. After a failure the second screen offers `Start again` and `Cancel`, and the polling has stopped.

**The page `Authorize device`.** A dialog of the web page in the box of the sign-in page (6.28), 24 rem wide. It opens from the fragment `#device=<code>` of the address the app opens (the server's own root), after a sign-in no older than five minutes: the sign-in page keeps the fragment, and the layer `Confirm it is you` of 6.29 shows once when the sign-in is older. It also opens from the palette entry `Authorize device…`, which first asks for a typed code (browser only): the field `Code`, the buttons `Look up` and `Cancel`. The dialog has the title `Authorize device`, the rows `Device`, `Platform`, `Address` (the network address the request came from, in mono) and `Requested` (`just now`, `3 min ago`), the code in mono to compare with the app, the sentence `Approve only if you started this on your own device.` and the buttons `Deny` and `Approve`. The result is said in the status line of the dialog, and the two buttons then give way to `Close` (nothing is left to decide):

| Result | Words |
|---|---|
| approved | `Device approved.` |
| denied | `Request denied.` |
| unknown or expired code | `That code is not valid or has expired.` |

![Device sign-in](design/device-authorize.png)

**The Devices group.** In the Account dialog (6.29), after Sessions, drawn whether or not a device switch is on (a device must always be possible to end): one row for each device of the person with its name, `macOS · just now` (platform and last seen: `just now`, `3 min ago`, `3 h ago`, `2 days ago` or `never`) and the button `Revoke`, which asks with a second click (`Click again to revoke`). With none: `No devices.` In the People dialog an administrator has the same list for everybody, below the table of people, with the extra column `Owner`; `Revoke` asks twice there too and needs a recent sign-in. A revoked device is refused on its next request and its sockets close.

**The group `Device access`.** In the People dialog, below the Devices group: two switches, `Sync target` and `Remote sessions`, each the switch of the Android Security row (6.6) at the desktop's size (28 px high): the word `ON` or `OFF` is inside the knob, `ON` has the knob at the right, filled, a rose outline and the rose wash, `OFF` has the knob at the left, hollow, and a dim hairline. Both are `OFF` by default. A switch that the operator's configuration has turned off for good is drawn `OFF` with a dashed outline, is not operable (`aria-disabled`) and has the dim line `Turned off in the server configuration.`. One dim line sits under the group: `Off by default. Turning one off ends the matching connections; devices stay signed in.` Changing a switch asks for a fresh sign-in like every change in that dialog.

![Device lists](design/device-lists.png)

**The words.**

| Word | Where |
|---|---|
| `Sign in to the server`, `Server address`, `Device`, `Continue`, `Cancel` | the first screen of the step |
| `Use an address that starts with https://.` | the refusal of an address |
| `Opening your browser…`, `Enter this code only if you started this here.`, `Open again`, `Start again`, `Close`, `Server` | the second screen |
| `Waiting for approval…`, `Signed in`, `The request was denied.`, `The code expired. Start again.`, `Too many devices or requests. Try again later.`, `The server cannot be reached.`, `Device sign-in is off on this server.` | the states of the second screen |
| `Authorize device`, `Authorize device…`, `Platform`, `Address`, `Requested`, `Code`, `Look up` | the page and its palette entry |
| `Approve only if you started this on your own device.`, `Approve`, `Deny` | the page |
| `Device approved.`, `Request denied.`, `That code is not valid or has expired.` | the results of the page |
| `Devices`, `Owner`, `Revoke`, `Click again to revoke`, `No devices.` | the lists |
| `Device access`, `Sync target`, `Remote sessions`, `ON`, `OFF` | the switches |
| `Turned off in the server configuration.`, `Off by default. Turning one off ends the matching connections; devices stay signed in.` | the lines of the group |

**Limits a reader must know.**

- A device token is for the app, never shown, never in a file, a log or an address, and revocable. It is **not bound to the device**: whoever holds it can use it until it is revoked or has been unused for 90 days.
- Approving a device in a browser can be phished: a person can be led to approve a code that an attacker started. The page says what it says, shows the device, the platform and the address, and tells the person to approve only what they started.
- A group of this section is drawn only in the browser build (6.30); the desktop app and the phone have the step and the row of 6.34, and `Sign out`.
- **Nothing here was tried with a real passkey or single sign-on from a real phone**, and nothing was run on a real phone or against a real server. The pictures are drawings of the design.

**Accessibility.** The step and the page are dialogs (`aria-modal`), the focus stays inside and returns to where it was, the code is real text (not an image) and the two switches are `role="switch"` with `aria-checked`. States are words and shapes or the mark `!` with an underline, so colour is never alone. Hairlines use `--hw-line` and dim text `--hw-ink-2` under `contrast-high`; under `forced-colors` the boxes, the switches and the dashed outline keep their borders; nothing moves. Every size is in `rem`, every target at least 28 px high (44 px on a phone).

**Not drawn.** A list of tokens or their values, a button that shows or copies a token, a second server, a password or passkey field in the app (the login is the browser's), a setting for the lifetime of a device, and a control that moves files from one sync target to the other.
