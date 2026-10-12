# Configuration files

Everything holzWave remembers is a plain JSON file that can be read and edited by hand. The files do not depend on how a
later phase moves them between machines (F25). This document is written from the code of Phase 2 and the examples in
it are parsed by `core/tests/project_policy.rs`, so a format change that forgets the document fails a test.

- [Where the files live](#where-the-files-live)
- [`settings.json`](#settingsjson)
- [Themes](#themes)
- [Key bindings](#key-bindings)
- [`session.json`](#sessionjson)
- [`layout.json`](#layoutjson)
- [Accessibility](#accessibility)
- [Privacy](#privacy)

## Where the files live

All files are in the app data directory of the identifier `ch.holzcloud.holzwave`, as the Tauri path resolver names
it for the platform:

| Platform | Directory |
|---|---|
| macOS | `~/Library/Application Support/ch.holzcloud.holzwave/` |
| Linux | `$XDG_DATA_HOME/ch.holzcloud.holzwave/`, by default `~/.local/share/ch.holzcloud.holzwave/` |
| Windows | `%APPDATA%\ch.holzcloud.holzwave\` |

The macOS path is the one the app is built and tested for; the Linux and Windows paths are what the resolver returns for
the same identifier and are confirmed in Phase 2.2. The tests run against scratch directories and never touch the real
one.

| File | Holds | Written | Machine-local |
|---|---|---|---|
| `settings.json` | the user settings and key binding overrides | at once, when a setting changes | no |
| `themes/*.json` | one user theme per file | by you, never by the app | no |
| `layout.json` | workspaces, tabs, panes and split ratios | 250 ms after the first change of a burst, and on quit | no |
| `session.json` | the working directory of each pane and the window frame | 2 s after the first change of a burst, and on quit | yes |
| `<name>.bad` | the bytes of a file that could not be used | when such a file is loaded | |

Rules that hold for every file:

- **Atomic writes.** The bytes go to `<name>.tmp`, are flushed, and replace the file with one rename. A crash leaves the
  old content or the new one, never half of it.
- **Bounded reads.** A file larger than its limit is not read: `settings.json`, `session.json` and `layout.json`
  1 MiB, a theme file 64 KiB. An oversized file is moved to `<name>.bad`.
- **The app always starts.** A file that cannot be used is replaced by the defaults, and the original bytes are kept
  as `<name>.bad` before the next save can overwrite them. Keep a `.bad` copy if you want it: the next one replaces it.
- **Hand edits are welcome.** An unknown key is ignored, so a file written by a newer version still loads. Edit while
  the app runs: when its window gains focus the page calls `reload_settings`, which looks at `settings.json` and
  `themes/` (sizes and modification times only) and applies what changed. A half-saved edit that cannot be parsed leaves the settings in force and shows a
  warning in the settings dialog.

## `settings.json`

One flat JSON object. It is read key by key: a key with a value of the wrong type costs only that key (it keeps its
default), a number outside its range is clamped, and an unknown key is ignored. The file is rewritten only when a
setting is changed from the dialog, the palette or a shortcut, and then in full.

| Key | Type | Range or values | Default | Applied |
|---|---|---|---|---|
| `version` | number | `1` | `1` | the file format version; it cannot be set |
| `theme` | string | a theme name (below); an unknown name resolves to `aubergine` | `"aubergine"` | at once, in every pane |
| `font_size` | number | 8 to 40 | 14 | at once; the terminal font in CSS pixels |
| `ui_scale` | number | 75 to 200 (percent) | 100 | at once; scales the chrome (tabs, status line, dialogs) |
| `option_as_alt` | boolean | | `false` | at once, in every pane: Option sends `ESC` plus the key instead of the layout's character |
| `cursor_blink` | string | `"system"`, `"on"`, `"off"` | `"system"` | at once |
| `motion` | string | `"system"`, `"reduce"`, `"full"` | `"system"` | at once |
| `contrast` | string | `"system"`, `"normal"`, `"high"` | `"system"` | at once |
| `scrollback_lines` | number | 100 to 100000 | 10000 | for panes started afterwards; a running pane keeps its own |
| `confirm_multiline_paste` | boolean | | `true` | at once |
| `keybindings` | object | action id to chord text, at most 512 entries, each text at most 128 bytes | `{}` | at once |

`option_as_alt` is off by default so that a Swiss layout types `|`, `[` and `{` with Option (D-12).
`confirm_multiline_paste` asks before a multi-line paste into a program that did not turn on bracketed paste (D-15).

<!-- example: settings.json -->
```json
{
  "version": 1,
  "theme": "Schiefer",
  "font_size": 15,
  "ui_scale": 110,
  "option_as_alt": false,
  "cursor_blink": "system",
  "motion": "system",
  "contrast": "system",
  "scrollback_lines": 20000,
  "confirm_multiline_paste": true,
  "keybindings": {
    "pane.split_right": "Cmd+Shift+R",
    "pane.close": ""
  }
}
```

## Themes

A theme is one JSON file in `themes/` (the directory is created by you). The three built-in themes are `aubergine`
(the default, dark), `Schiefer` (neutral dark grey with the rose accent) and `Papier` (light, cream with aubergine
ink). A user theme has the same shape and is listed after them in the settings dialog and the palette, in file name
order.

| Key | Meaning |
|---|---|
| `version` | `1`; may be left out |
| `name` | 1 to 64 letters, digits, spaces, dashes or underscores, without a space at either end. This is what `settings.json` refers to; the file name is never used |
| `kind` | `"dark"` or `"light"` |
| `terminal.foreground`, `terminal.background`, `terminal.cursor` | the grid's default colours and the cursor |
| `terminal.selection`, `terminal.selection_alpha` | the selection colour, drawn over the cell background at this opacity, 0.05 to 0.9 |
| `terminal.ansi` | exactly 16 colours: black, red, green, yellow, blue, magenta, cyan, white, then the bright row |
| `chrome.ground`, `ground_2`, `pane`, `edge` | the window ground, a second ground (bars), the pane ground and the 1 px hairlines |
| `chrome.ink`, `ink_2`, `ink_3` | primary, secondary and dim text |
| `chrome.rose`, `rose_deep` | the accent and its fill colour (the text on the fill is chosen by the page: white or black, whichever is more readable) |
| `chrome.ok`, `warn`, `danger`, `info` | state colours |

Every colour is `#rrggbb`, six hex digits, either case. Short forms, colour names and functions are refused; nothing
else can reach the page as a CSS value. A theme file is skipped, with one warning in the settings view, when it is not
valid JSON of this shape, has the wrong number of ANSI colours, is a symbolic link or not a regular file, is larger than
64 KiB, or has a name that a built-in or an earlier file already uses (letter case is ignored). At most 64 files are
read. Unknown keys, such as a `"comment"`, are ignored.

<!-- example: themes/moos.json -->
```json
{
  "version": 1,
  "name": "Moos",
  "kind": "dark",
  "terminal": {
    "foreground": "#E7EBDD",
    "background": "#10150F",
    "cursor": "#E3C25A",
    "selection": "#E3C25A",
    "selection_alpha": 0.3,
    "ansi": [
      "#1A2118", "#E07A6A", "#8FCB6B", "#E3C25A", "#7AA2D8", "#D88FC0", "#6FCFC0", "#CFD6C4",
      "#6F7B66", "#F09A8A", "#A9E08A", "#F2D87C", "#9BBDEB", "#EBABD6", "#95E3D6", "#EEF1E6"
    ]
  },
  "chrome": {
    "ground": "#10150F",
    "ground_2": "#182016",
    "pane": "#141B12",
    "edge": "#2A3526",
    "ink": "#E7EBDD",
    "ink_2": "#A6AB9F",
    "ink_3": "#8E9A84",
    "rose": "#E3C25A",
    "rose_deep": "#8A6A12",
    "ok": "#8FCB6B",
    "warn": "#E3C25A",
    "danger": "#E07A6A",
    "info": "#7AA2D8"
  }
}
```

**Contrast guidance.** A user theme is validated for its shape, not for how it looks, so readability is yours to
keep. The built-in themes are held by tests to these WCAG 2 ratios, and the example above meets all of them:

| Pair | Ratio |
|---|---|
| `terminal.foreground` on `terminal.background` | at least 7:1 |
| ANSI 1 to 7 and 9 to 15 on `terminal.background` | at least 4.5:1 |
| ANSI 8 (bright black, used for suggestions and comments) and `terminal.cursor` on `terminal.background` | at least 3:1 |
| the foreground and ANSI 1 to 7 and 9 to 15 on the selection (`selection` blended at `selection_alpha` over the background) | at least 3:1 |
| `chrome.ink` on `chrome.ground` | at least 7:1 |
| `chrome.ink_3` on `chrome.ground` | at least 4.5:1 |

`chrome.ground` is the terminal background in all three built-ins, and `ink_2` is `ink` at 70 % over the ground as a
solid colour. Bright black is the one place where the built-in `aubergine` departs from the design table (`#7C6A86`
instead of `#4A3652`, which measured 1.79:1 and is used by programs for text).

## Key bindings

Overrides live in `settings.json` under `keybindings`: action id to chord text. The settings dialog has an editor that
records a chord, unbinds an action and resets it, and shows the core's refusals beside the row.

**Chord grammar.** Parts separated by `+`: the modifiers `Cmd` (also `Command`, `Meta`, `Super`; the Windows or Super key
on other platforms), `Ctrl` (`Control`), `Alt` (`Option`, `Opt`) and `Shift`, each at most once and in any case, then
one key. The key is a single printable character (`D`, `]`, `1`, `,`) or a name: `Space`, `Enter` (`Return`), `Tab`,
`Escape` (`Esc`), `Backspace`, `Delete`, `Insert`, `Up`, `Down`, `Left`, `Right`, `Home`, `End`, `PageUp`, `PageDown`,
`F1` to `F24`. `+` is not a key of the grammar (write `Shift+=`). The canonical spelling, which the editor writes, is
`Cmd+Ctrl+Alt+Shift+Key` in that modifier order with letters in upper case: `Cmd+Shift+D`.

**Bindability.** The terminal owns the plain keys, so a chord must hold `Cmd`, or at least two modifiers one of which is
`Ctrl` or `Alt`. `D`, `Shift+D`, `Ctrl+D`, `Alt+D` and `Escape` are refused; `Cmd+D`, `Ctrl+Alt+D` and
`Ctrl+Shift+PageDown` are fine.

**Resolution**, in this order:

1. An action id the build does not know is ignored, with a warning.
2. The empty string `""` unbinds a known action (no warning).
3. A chord that does not parse, or is not bindable, is ignored and the default stays (a warning names the reason).
4. Two overrides on one chord: the alphabetically earlier action id keeps it, the other keeps its default.
5. An override beats a default: the action that had the chord by default is unbound, with a warning.

Afterwards no chord belongs to two actions. `Cmd+C` (copy) is a fixed shortcut with no action in the table; another
action can take the chord, and Reset in the editor gives it back.

The defaults (the macOS table; Windows and Linux pick their own in Phase 2.2):

| Action id | Default | Action id | Default |
|---|---|---|---|
| `pane.split_right` | `Cmd+D` | `workspace.new` | `Cmd+Shift+N` |
| `pane.split_down` | `Cmd+Shift+D` | `workspace.select_1` to `_9` | `Cmd+Alt+1` to `Cmd+Alt+9` |
| `pane.close` | `Cmd+W` | `palette.open` | `Cmd+K` |
| `pane.focus_next` | `Cmd+]` | `search.open` | `Cmd+F` |
| `pane.focus_prev` | `Cmd+[` | `copy_mode.enter` | `Cmd+Shift+X` |
| `tab.new` | `Cmd+T` | `hints.open` | `Cmd+Shift+Space` |
| `tab.select_1` to `_9` | `Cmd+1` to `Cmd+9` | `font.increase` | `Cmd+=` |
| `font.decrease` | `Cmd+-` | `font.reset` | `Cmd+0` |
| `settings.open` | `Cmd+,` | | |
| `tab.rename` | none | `pane.rename` | none |
| `workspace.rename` | none | | |

The line-editing actions send fixed bytes to the shell's line editor (`edit.word_back` and `edit.word_forward` send
`ESC b` and `ESC f`, `edit.line_start` and `edit.line_end` send `Ctrl+A` and `Ctrl+E`, `edit.delete_word_back` sends
`ESC DEL`, `edit.delete_to_line_start` sends `Ctrl+U`). Each platform has its own standard as the default, and every
chord can be changed or unbound like any other. They apply on the normal screen only: a program on the alternate screen
(vim, less, htop) gets the key itself.

| Action id | macOS | Windows and Linux |
|---|---|---|
| `edit.word_back` | `Alt+Left` | `Ctrl+Left` |
| `edit.word_forward` | `Alt+Right` | `Ctrl+Right` |
| `edit.line_start` | `Cmd+Left` | none: `Home` already reaches the shell |
| `edit.line_end` | `Cmd+Right` | none: `End` already reaches the shell |
| `edit.delete_word_back` | `Alt+Backspace` | `Ctrl+Backspace` |
| `edit.delete_to_line_start` | `Cmd+Backspace` | none |

A single `Alt` or `Ctrl` is bindable on the navigation keys (`Left`, `Right`, `Up`, `Down`, `Home`, `End`,
`Backspace`, `Delete`) and nowhere else, so `Option+7` on a Swiss layout still types its character.

The three rename actions start the in-place editor on the shown tab, the focused pane and the shown workspace; a double
click on a tab, a pane header or a workspace chip does the same. A name is cut to 128 characters, loses control
characters and blanks at both ends, and the empty name gives the automatic one back (`shell`, the program's title,
`ws N`). Names of panes are kept in `layout.json` (`pane_names`); tabs and workspaces keep theirs in their own entry.

Actions without a chord in the table (`keybindings.open`, the theme entries of the palette) are reached from the
palette and the dialogs and can be bound by adding them to the table in a later version.

## `session.json`

What is restored on the next start, kept apart from `layout.json` because a window position and a directory name do
not belong on another machine. Everything in it is a hint: a damaged file costs the hint and never the start.

```json
{
  "version": 1,
  "window": { "x": 120, "y": 80, "width": 1000, "height": 640, "maximized": false },
  "cwds": { "3": "/Users/me/src/holzWave" }
}
```

- `window` is in physical pixels. The position is restored only when at least 100 by 50 pixels of the window lie on a
  monitor that is connected now; otherwise the window system places it. A maximised window keeps the geometry it had
  before. Size 200 to 16384, position -32768 to 32768 (clamped).
- `cwds` maps a pane id to its working directory, from the shell's own report (OSC 7). A directory is used only while
  it is still an absolute path to an existing directory; it is only ever the working directory argument of a spawn.
  Stored paths are absolute, at most 4096 bytes and free of control characters; at most 256 are kept, and the
  directories of panes that no longer exist are dropped on save.
- A pane made by a split or a new tab inherits the directory of the pane it came from.

<!-- example: session.json -->
```json
{
  "version": 1,
  "window": { "x": 120, "y": 80, "width": 1000, "height": 640, "maximized": false },
  "cwds": { "3": "/Users/me/src/holzWave" }
}
```

## `layout.json`

Workspaces, tabs and the split tree of each tab, with ratios in 0.1 to 0.9. The app owns it; it is not meant for hand
editing, but it is plain JSON and a damaged one is repaired rather than trusted:

- an unusable text (not JSON, not a layout, nothing left after repair) gives a fresh one-pane layout and a `.bad` copy
  of the original;
- a usable text with defects is repaired and the original is kept as `.bad`: a repeated workspace, tab, pane or split id
  drops the later one, a workspace without tabs goes, an active index or a focus that points nowhere is moved to a
  pane or tab that exists, a ratio outside 0.1 to 0.9 is clamped, the id counter is raised above every id in use, and
  names and titles lose control characters and are cut to 128 characters;
- limits: 64 panes per tab, 256 panes in all, a split nesting of 32, ids below 2^53 (the page receives them as JSON
  numbers);
- a file over 1 MiB is moved to `layout.json.bad` unread.

<!-- example: layout.json -->
```json
{
  "workspaces": [
    {
      "id": 1,
      "name": "main",
      "tabs": [
        {
          "id": 2,
          "title": "shell",
          "root": {
            "Split": {
              "id": 5,
              "axis": "Row",
              "ratio": 0.5,
              "a": { "Pane": 3 },
              "b": { "Pane": 4 }
            }
          },
          "focus": 3
        }
      ],
      "active": 0
    }
  ],
  "active": 0,
  "next_id": 6
}
```

`axis` is `"Row"` for two panes side by side and `"Column"` for two panes on top of each other.

## Accessibility

What follows the operating system, and what is yours to set (DESK-24). The rules of the look are in
[design.md](design.md) section 5.

| Preference | Follows the OS through | Setting | Notes |
|---|---|---|---|
| Reduced motion | the `prefers-reduced-motion` media query | `motion`: `system`, `reduce`, `full` | removes every chrome transition; the terminal grid never moves |
| Increased contrast | `prefers-contrast: more` and `forced-colors` | `contrast`: `system`, `normal`, `high` | 2 px focus line, hairlines and dim text use the stronger tokens (`ink_3`, `ink_2`); under forced colours the chrome uses system colours |
| Cursor blink | the reduced-motion preference; there is no OS signal of its own | `cursor_blink`: `system`, `on`, `off` | only a cursor the program asked to blink does; with `system` it stops under reduced motion |
| UI font size | **nothing**: the web view reports no OS text-size signal on macOS | `ui_scale` (75 to 200 %) and `font_size` (8 to 40) | the requirement's wording allows this; the OS-driven part comes per platform in Phase 2.2 (D-25) |

An explicit setting beats the OS in both directions: `motion: "full"` keeps transitions although the OS asks to reduce
them, `contrast: "normal"` ignores the OS preference, `cursor_blink: "on"` blinks under reduced motion. The OS
preferences are followed while the app runs, without a restart. Whether macOS's Reduce motion and Increase contrast
reach the web view is part of the human checklist in [test-plan.md](test-plan.md).

## Privacy

holzWave has no telemetry and no account, anywhere (DESK-26, F33). It makes no network connection of its own: the one
outward action is a link you click, which is handed to the platform's opener (`open`, `xdg-open`, `explorer`) for
`http`, `https` and `mailto` links, and the file manager reveal of a path. There is no update check, no crash report
and no usage counter.

That claim is a test, not a promise: `core/tests/project_policy.rs` fails when

- a workspace crate or the app package gains a dependency that the test does not list;
- the dependency closure of the app contains a telemetry or HTTP client crate that is not a named exception;
- first-party Rust or page sources use a network API or an absolute URL;
- the page policy allows a remote origin, or an updater or a CI workflow appears.

One honest exception is named in the test: the Tauri 3 alpha links `reqwest` for a dev-server proxy that a desktop
build never starts (the app sets no `devUrl`).
