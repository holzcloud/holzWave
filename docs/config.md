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
| `ui_scale` | number | 75 to 200 (percent) | 100 | at once; scales the chrome (tabs, status line, dialogs); on Windows and Linux the chrome also follows the OS text size on top of it (see Accessibility) |
| `option_as_alt` | boolean | | `false` | at once, in every pane: Option sends `ESC` plus the key instead of the layout's character |
| `cursor_blink` | string | `"system"`, `"on"`, `"off"` | `"system"` | at once |
| `motion` | string | `"system"`, `"reduce"`, `"full"` | `"system"` | at once |
| `contrast` | string | `"system"`, `"normal"`, `"high"` | `"system"` | at once |
| `scrollback_lines` | number | 100 to 100000 | 10000 | for panes started afterwards; a running pane keeps its own |
| `confirm_multiline_paste` | boolean | | `true` | at once |
| `smooth_scroll` | boolean | | `false` | at once; eases a scrollback jump over at most 120 ms (below) |
| `cursor_trail` | boolean | | `false` | at once; fades a short trail behind a cursor that jumps (below) |
| `shell` | string | one program: an absolute path or a bare name, at most 4096 bytes, no control characters; empty means the platform default; a value that cannot be started falls back to the platform default with a warning | `""` | for panes started afterwards; a running pane keeps its shell (below) |
| `keybindings` | object | action id to chord text, at most 512 entries, each text at most 128 bytes | `{}` | at once |

`option_as_alt` is off by default so that a Swiss layout types `|`, `[` and `{` with Option (D-12).
`confirm_multiline_paste` asks before a multi-line paste into a program that did not turn on bracketed paste (D-15).
`smooth_scroll` and `cursor_trail` are two optional effects of the drawing only (DESK-28); both are off by default and
both stay off while reduced motion is in effect, whether the operating system asks for it or the `motion` setting says
`"reduce"` (see [Accessibility](#accessibility)), and the change of either applies without a restart. With
`smooth_scroll` a jump in the scrollback (wheel, page keys, a search hit) slides in over at most 120 ms instead of
cutting; the core still scrolls by whole rows and a frame is drawn the moment it arrives. With `cursor_trail` a cursor
that moves by more than one cell leaves up to six fading ghost cells on the way. Neither effect runs on the alternate
screen of a full-screen program (nothing scrolls there) or while the cursor is hidden or in the off phase of its blink,
and nothing animates, so nothing is drawn, while the screen is idle.

`shell` is the program a new pane starts, and only the program: one string that is never split at blanks and never run
through another shell, so arguments cannot be given here. The arguments follow from the program's file name, without a
`.exe` suffix and without case: `-l` (a login shell) for every shell on macOS and Linux and for `bash`, `zsh`, `fish`,
`sh` and `nu` on Windows, none for `cmd` and for any other program, and for `pwsh` and `powershell` `-NoLogo` plus a
prompt wrapper that reports the working directory, so that a split or a new tab opens where the pane is. `cmd.exe` has
no such report: a split from a `cmd.exe` pane starts in your home directory. Empty means the default of the platform.
On macOS and Linux that is `$SHELL` (when it is unset or empty, `/bin/zsh` on macOS and `/bin/sh` on Linux). On Windows
it is, in this order, PowerShell 7 (`pwsh.exe` under `%ProgramFiles%\PowerShell\7` or in a directory of `PATH`), then
Windows PowerShell, then `%ComSpec%`, then `cmd.exe`. Only a file that exists is taken, and only a directory that
starts with a drive letter is searched.

A `shell` that cannot be started never leaves you without a terminal. It is used when it is an absolute path (a drive
path on Windows) to an executable file, or a bare name that is found in an absolute directory of `PATH` (on Windows
also in `%SystemRoot%\System32`, with `.exe` added when the name has no extension); a bare name is then started by
its full path. Anything else is unusable: a program that is not found, a directory, a file without the permission to
run, and a relative path such as `./sh` or `~/bin/fish` (it is never looked up, and `~` is not expanded). Then the
platform default above starts instead, the pane's first line says so (`holzWave: the shell setting "..." was not
found; ... is used instead`), and the settings dialog lists the same sentence among its warnings for as long as the
value stays unusable. Nothing is rewritten in `settings.json`: put a working program there, or delete the key. The
settings dialog has no row for it; edit the file.

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
  "smooth_scroll": false,
  "cursor_trail": false,
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

**A chord is text with a platform meaning.** `Cmd` is the Command key on macOS and the Super (Windows) key elsewhere,
which the operating system usually keeps for itself, so no default on Windows or Linux uses it. A `settings.json`
written on one platform therefore carries that platform's chords: a copy of `keybindings` from a Mac would bind Super
chords on Windows. Copy the file between machines only after translating `Cmd` (settings sync in Phase 4 has to do the
same, not copy the `keybindings` overrides across platforms as they are).

**Resolution**, in this order:

1. An action id the build does not know is ignored, with a warning.
2. The empty string `""` unbinds a known action (no warning).
3. A chord that does not parse, or is not bindable, is ignored and the default stays (a warning names the reason).
4. Two overrides on one chord: the alphabetically earlier action id keeps it, the other keeps its default.
5. An override beats a default: the action that had the chord by default is unbound, with a warning.

Afterwards no chord belongs to two actions.

**AltGr.** On a layout with a third level (Swiss, German and others) AltGr types `@ \ | { } [ ] ~ €`. Windows reports it
as `Ctrl` and `Alt` held together, so while AltGr is held the page ignores both for key presses and for chords: the
characters are typed and a chord such as `Ctrl+Alt+7` can never be recorded from AltGr+7. A real `Ctrl+Alt` press
without AltGr is still a chord and reaches the program with both modifiers. On Windows the core backs the page up: with
`Ctrl` and `Alt` held and a typed character that is not the US-layout character of that physical key, both modifiers are
dropped. No Windows default uses `Ctrl+Alt` with a printable key.

**Copy and paste** are fixed shortcuts outside the table, like `Cmd+C` always was; no action can be bound to them, and
a chord that another action takes is that action's. Plain `Ctrl+C` is never a copy on any platform: it always reaches the
program (SIGINT).

| | macOS | Windows | Linux |
|---|---|---|---|
| copy the selection | `Cmd+C` | `Ctrl+Shift+C`, `Ctrl+Insert` | `Ctrl+Shift+C`, `Ctrl+Insert` |
| paste | `Cmd+V` (Edit menu) | `Ctrl+Shift+V`, `Shift+Insert`, `Ctrl+V` | `Ctrl+Shift+V`, `Shift+Insert` |

A paste chord is not swallowed by the page: the web view pastes into the terminal input, and when it does not, the page
reads the clipboard itself once. Several lines still ask first, and a program that asked for bracketed paste gets it. In
a text field of the page (search, a name, the palette) none of these shortcuts is the page's: copy and paste are the
field's own.

The defaults. Windows and Linux use `Ctrl+Shift` as the terminal convention that leaves plain `Ctrl+letter` to the
shell. Tab numbers are `Alt+Shift+digit` because `Ctrl+Alt` is AltGr on Windows; nothing uses `Ctrl+Shift+E`, which
GNOME's input method takes for emoji.

| Action id | macOS | Windows | Linux |
|---|---|---|---|
| `pane.split_right` | `Cmd+D` | `Ctrl+Shift+D` | `Ctrl+Shift+D` |
| `pane.split_down` | `Cmd+Shift+D` | `Ctrl+Shift+O` | `Ctrl+Shift+O` |
| `pane.close` | `Cmd+W` | `Ctrl+Shift+W` | `Ctrl+Shift+W` |
| `pane.focus_next` | `Cmd+]` | `Ctrl+Shift+]` | `Ctrl+Shift+]` |
| `pane.focus_prev` | `Cmd+[` | `Ctrl+Shift+[` | `Ctrl+Shift+[` |
| `tab.new` | `Cmd+T` | `Ctrl+Shift+T` | `Ctrl+Shift+T` |
| `tab.select_1` to `_9` | `Cmd+1` to `Cmd+9` | `Alt+Shift+1` to `Alt+Shift+9` | `Alt+Shift+1` to `Alt+Shift+9` |
| `workspace.new` | `Cmd+Shift+N` | `Ctrl+Shift+N` | `Ctrl+Shift+N` |
| `workspace.select_1` to `_9` | `Cmd+Alt+1` to `Cmd+Alt+9` | `Ctrl+Shift+1` to `Ctrl+Shift+9` | `Ctrl+Shift+1` to `Ctrl+Shift+9` |
| `palette.open` | `Cmd+K` | `Ctrl+Shift+P` | `Ctrl+Shift+P` |
| `search.open` | `Cmd+F` | `Ctrl+Shift+F` | `Ctrl+Shift+F` |
| `copy_mode.enter` | `Cmd+Shift+X` | `Ctrl+Shift+X` | `Ctrl+Shift+X` |
| `hints.open` | `Cmd+Shift+Space` | `Ctrl+Shift+Space` | `Ctrl+Shift+Space` |
| `font.increase` | `Cmd+=` | `Ctrl+Shift+=` | `Ctrl+Shift+=` |
| `font.decrease` | `Cmd+-` | `Ctrl+Shift+-` | `Ctrl+Shift+-` |
| `font.reset` | `Cmd+0` | `Ctrl+Shift+0` | `Ctrl+Shift+0` |
| `settings.open` | `Cmd+,` | `Ctrl+Shift+,` | `Ctrl+Shift+,` |
| `tab.rename`, `pane.rename`, `workspace.rename` | none | none | none |
| `pane.swap_next`, `pane.move_next_tab`, `pane.move_prev_tab` | none | none | none |
| `pane.move_to_new_window`, `tab.move_to_new_window` | none | none | none |
| `tab.close`, `workspace.close` | none | none | none |

The line-editing actions send fixed bytes to the shell's line editor (`edit.word_back` and `edit.word_forward` send
`ESC b` and `ESC f`, `edit.line_start` and `edit.line_end` send `Ctrl+A` and `Ctrl+E`, `edit.delete_word_back` sends
`ESC DEL`, `edit.delete_to_line_start` sends `Ctrl+U`). Each platform has its own standard as the default, and every
chord can be changed or unbound like any other. They apply on the normal screen only: a program on the alternate screen
(vim, less, htop) gets the key itself. Windows has no default for any of the six: PowerShell (PSReadLine) and `cmd`
bind `Ctrl+Left`, `Ctrl+Right`, `Ctrl+Backspace`, `Home` and `End` themselves.

| Action id | macOS | Windows | Linux |
|---|---|---|---|
| `edit.word_back` | `Alt+Left` | none | `Ctrl+Left` |
| `edit.word_forward` | `Alt+Right` | none | `Ctrl+Right` |
| `edit.line_start` | `Cmd+Left` | none | none: `Home` already reaches the shell |
| `edit.line_end` | `Cmd+Right` | none | none: `End` already reaches the shell |
| `edit.delete_word_back` | `Alt+Backspace` | none | `Ctrl+Backspace` |
| `edit.delete_to_line_start` | `Cmd+Backspace` | none | none |

A single `Alt` or `Ctrl` is bindable on the navigation keys (`Left`, `Right`, `Up`, `Down`, `Home`, `End`,
`Backspace`, `Delete`) and nowhere else, so `Option+7` on a Swiss layout still types its character.

The three rename actions start the in-place editor on the shown tab, the focused pane and the shown workspace; a double
click on a tab, a pane header or a workspace chip does the same. A name is cut to 128 characters, loses control
characters and blanks at both ends, and the empty name gives the automatic one back (`shell`, the program's title,
`ws N`). Names of panes are kept in `layout.json` (`pane_names`); tabs and workspaces keep theirs in their own entry.

A pane is dragged by its header onto another pane: on the outer quarter of a side it splits that pane there, on the
middle the two swap places; onto a tab it moves into that tab; onto a workspace chip it moves into that workspace, next
to the focus pane of that workspace's shown tab (the shown workspace's own chip is no target). A tab is dragged to
another place in the strip, or onto a workspace chip to move into that workspace. Escape, or a drop on nothing, cancels. The pane keeps its shell, its
scrollback and its name; the core refuses a move that would pass the pane or depth limits and changes nothing. Without
the mouse: `pane.swap_next` swaps the focused pane with the next one of the tab, `pane.move_next_tab` and
`pane.move_prev_tab` move it into the next or previous tab (all three are unbound by default).

Several windows: a pane or a tab dragged out of the window, and released at least 24 px beyond its edge, becomes a
window of its own (the pointer decides where it opens); a release only just over the edge cancels. Released over
another window of holzWave it moves into that window instead: onto the side or the middle of a pane (split or swap), onto
a tab (next to that tab's pane), onto the free part of the tab strip (as a tab of its own), or onto a workspace chip (into
that workspace; a tab goes into the workspace of the chip it is over, and into the shown one anywhere else in the
window). A release over no drop zone changes nothing. Without the mouse: `pane.move_to_new_window` gives the focused
pane, and `tab.move_to_new_window` the shown tab, a window beside this one (both unbound by default). The pane or tab
keeps its shell, its scrollback and its name; a pane or tab that is alone in its window cannot be torn off, and moving
the last pane out of a window closes that window. A tear-off that is refused (a lone pane or tab, no window to be had)
leaves everything where it is and says why in the status line for a few seconds.

Every tab, workspace chip and pane header has a × at its right end, always visible, that closes it after a question: the
name of what closes, how many shells end with it, and, when it is everything in the window, that the window closes (or
holzWave quits, when it is the only window). Enter or `Close` confirms, Escape or `Cancel` keeps everything, and no key
of the question reaches the terminal. Closing a tab or workspace ends the shells of all its panes and nothing else. The
shortcuts ask nothing, because pressing one is deliberate: `pane.close` (`Cmd+W`) closes the focused pane at once, and
`tab.close` and `workspace.close` (both unbound by default, also in the palette) close the shown tab or workspace at
once. In every case the last pane of the last window cannot be removed: closing it ends the app and the layout on disk
stays as it was, so the next start has it all back.

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
| Reduced motion | the `prefers-reduced-motion` media query | `motion`: `system`, `reduce`, `full` | removes every chrome transition and switches the two grid effects below off, whatever their own settings say |
| Increased contrast | `prefers-contrast: more` and `forced-colors` | `contrast`: `system`, `normal`, `high` | 2 px focus line, hairlines and dim text use the stronger tokens (`ink_3`, `ink_2`); under forced colours the chrome uses system colours |
| Cursor blink | the reduced-motion preference; there is no OS signal of its own | `cursor_blink`: `system`, `on`, `off` | only a cursor the program asked to blink does; with `system` it stops under reduced motion |
| Smooth scrolling and cursor trail | the reduced-motion preference; there is no OS signal of their own | `smooth_scroll`, `cursor_trail`: `false`, `true` | off by default; each is drawn only while reduced motion is not in effect, and `motion: "full"` allows them although the OS asks to reduce |
| UI font size | Windows: the `TextScaleFactor` registry value of the Text size setting, read through `reg.exe`; Linux: the GNOME `text-scaling-factor` key, read through `gsettings`; macOS: **nothing**, it has no such signal | `ui_scale` (75 to 200 %) and `font_size` (8 to 40) | the chrome's size is `ui_scale` times the OS factor (100 to 300 %), held to 75 to 300 %; only the chrome follows, the terminal text stays at `font_size`; read at start and when the window gains focus, so a change arrives without a restart; a source that cannot be read counts as 100 % |

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
