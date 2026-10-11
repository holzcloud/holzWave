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
| Window bar | traffic lights, workspace chips, tabs, version label right-aligned in mono |
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
| UI scale | **none** | the user's `ui_scale` (75 to 200 %) and the font size actions; see below |

The web view reports no OS text-size preference on macOS, so the scalable UI font is the user's own setting rather than
an OS signal. Windows and Linux get theirs in Phase 2.2. The requirement DESK-24 says so in its wording; nothing in
the page pretends otherwise.

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

**Rules**

1. **One look everywhere.** The window chrome is drawn by holzWave, with no native title bar or tab control, so
   macOS, Windows, Linux, iOS and Android show the same thing. Only the OS window buttons and the system keyboard
   differ.
2. **Mono everywhere in the chrome.** Labels, tabs and the status line are JetBrains Mono; Manrope only appears in
   previews. Labels are uppercase and tracked, as in the holzcloud family.
3. **Flat and hairline.** No shadows between panes, no gradients, no blur. Panes are separated by a 1 px
   `--hw-edge` line. Only floating layers (command palette) have a shadow and a 8 px radius.
4. **Rose is rationed.** It appears for the prompt chevron, the cursor, the focused pane's top line, the active tab
   marker and the active status segment. Everything else is warm grey.
5. **tmux-style status line.** A bottom strip of segments (`workspace`, panes, renderer, encoding, version); the
   active workspace is a solid rose segment.
6. **Numbered tabs.** Tabs carry their number (`1 shell`) so `⌘1…9` is discoverable.
7. **Command palette first.** Actions are reachable by name with a fuzzy palette (`⌘K`); the chrome stays small.
8. **Responsive by layout, not by style.** On a phone the same `View` shows one pane per screen, the tab strip
   moves to the bottom, and an accessory row (`esc tab ctrl alt ← ↑ ↓ →`) sits above the system keyboard. Colours,
   type and spacing tokens are identical.

**Size tokens for touch:** minimum target 44 px on mobile and 28 px on desktop; the accessory row keys are 44 px high.

### Candidates considered

The five earlier mockups stay as the record of the alternatives: [Glas](design/1-glas.png),
[Planke](design/2-planke.png), [Haarlinie](design/3-haarlinie.png), [Welle](design/4-welle.png),
[Papier](design/5-papier.png). The cream *Papier* look remains a candidate for a **light theme** later, because the
terminal palette and the tokens are shared by every chrome style.
