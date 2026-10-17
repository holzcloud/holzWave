# Configuration files

Everything holzWave remembers is a plain JSON file that can be read and edited by hand. The files do not depend on how
sync moves them between machines (F25). This document is written from the code and the examples in
it are parsed by `core/tests/project_policy.rs` (the widget examples by `core/tests/widgets_schema.rs`), so a format
change that forgets the document fails a test.

- [Where the files live](#where-the-files-live)
- [`settings.json`](#settingsjson)
- [Themes](#themes)
- [Key bindings](#key-bindings)
- [`session.json`](#sessionjson)
- [`layout.json`](#layoutjson)
- [Accessibility](#accessibility)
- [Privacy](#privacy)
- [`ai.json`](#aijson)

## Where the files live

All files are in the app data directory of the identifier `ch.holzcloud.holzwave`, as the Tauri path resolver names
it for the platform:

| Platform | Directory |
|---|---|
| macOS | `~/Library/Application Support/ch.holzcloud.holzwave/` |
| Linux | `$XDG_DATA_HOME/ch.holzcloud.holzwave/`, by default `~/.local/share/ch.holzcloud.holzwave/` |
| Windows | `%APPDATA%\ch.holzcloud.holzwave\` |

The macOS path is the one the app is built and tested for; the Linux and Windows paths are what the resolver returns for
the same identifier and have not been confirmed on those platforms yet (rows of [test-plan.md](test-plan.md)). The tests run
against scratch directories and never touch the real one.

| File | Holds | Written | Machine-local |
|---|---|---|---|
| `settings.json` | the user settings and key binding overrides | at once, when a setting changes | no |
| `themes/*.json` | one user theme per file | by you, never by the app | no |
| `layout.json` | workspaces, tabs, panes and split ratios | 250 ms after the first change of a burst, and on quit | no |
| `session.json` | the working directory of each pane and the window frame | 2 s after the first change of a burst, and on quit | yes |
| `connections.json` | the saved SSH, WSL and Docker connections, without secrets | when a connection is saved or deleted | no |
| `pane_connections.json` | which pane runs which connection | when it changes | yes |
| `known_hosts` | the host keys SSH has learned (OpenSSH format) | when a new host is trusted | yes |
| `pane_blocks.json` | which pane shows a file browser, a preview, the system monitor or a chat, of which source and path (for a chat the path is the id of its conversation and the messages are not in this file) | when it changes | yes |
| `ai.json` | the AI provider profiles, without keys | when a profile is saved or deleted | yes |
| `chats/<id>.json` | one AI chat per file, readable by you only; terminal context is a marker unless `ai_persist_context` is on | when a message is stored | yes |
| `history.db` | the command history, with the host each command ran on | when a command is reported | yes |
| `widgets/<id>.json`, `widget_notes.json` | your widgets, and the texts of the note widgets | by you, and when a note changes | no |
| `<name>.bad` | the bytes of a file that could not be used | when such a file is loaded | |

Rules that hold for every file:

- **Atomic writes.** The bytes go to `<name>.tmp`, are flushed, and replace the file with one rename. A crash leaves the
  old content or the new one, never half of it.
- **Bounded reads.** A file larger than its limit is not read: `settings.json`, `session.json` and `layout.json`
  1 MiB, `ai.json` 256 KiB, a theme file 64 KiB. An oversized file is moved to `<name>.bad`.
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
| `ai_local_only` | boolean | | `false` | at once; refuses every AI request whose destination leaves this computer (below) |
| `ai_local_context` | string | `"ask"`, `"auto"` | `"ask"` | at once; whether a model on this computer asks before shared terminal content goes to it (below) |
| `ai_persist_context` | boolean | | `false` | for chats saved afterwards; keeps the terminal text and tool output of a chat in its file (below) |
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

`ai_local_only` refuses an AI request before any connection is made unless the destination is this computer; it also blocks
the local network, so a server on another machine of your own is refused too. `ai_local_context` is `"ask"` or `"auto"`: with
`"ask"` a model that runs on this computer asks before shared terminal content goes to it, the same way a cloud provider does;
with `"auto"` it does not ask again. `ai_persist_context` set to `false` keeps a marker instead of the terminal text and the
tool output of a chat in the chat's file; `true` keeps the text there. The three keys are part of `settings.json`, so they travel
with the synced settings between your own devices.

`shell` is the program a new pane starts, and only the program: one string that is never split at blanks and never run
through another shell, so arguments cannot be given here. The arguments follow from the program's file name, without a
`.exe` suffix and without case: `-l` (a login shell) for every shell on macOS and Linux and for `bash`, `zsh`, `fish`,
`sh` and `nu` on Windows, none for `cmd` and for any other program, and for `pwsh` and `powershell` `-NoLogo` plus a
prompt wrapper that reports the working directory, so that a split or a new tab opens where the pane is, and that also
marks each command (see [Command marks](#command-marks)). `cmd.exe` has
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
chords on Windows. Copy the file between machines only after translating `Cmd` (the settings sync does the same: it does not copy
the `keybindings` overrides across platforms as they are).

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
| `history.open` | `Cmd+Shift+H` | `Ctrl+Shift+H` | `Ctrl+Shift+H` |
| `sidebar.toggle` | `Cmd+B` | `Ctrl+Shift+B` | `Ctrl+Shift+B` |
| `ai.open` | `Cmd+Shift+A` | `Ctrl+Shift+A` | `Ctrl+Shift+A` |
| `ai.explain_error` | `Cmd+Shift+Y` | `Ctrl+Shift+Y` | `Ctrl+Shift+Y` |
| `ai.command_from_description` | `Cmd+Shift+G` | `Ctrl+Shift+G` | `Ctrl+Shift+G` |
| `font.increase` | `Cmd+=` | `Ctrl+Shift+=` | `Ctrl+Shift+=` |
| `font.decrease` | `Cmd+-` | `Ctrl+Shift+-` | `Ctrl+Shift+-` |
| `font.reset` | `Cmd+0` | `Ctrl+Shift+0` | `Ctrl+Shift+0` |
| `settings.open` | `Cmd+,` | `Ctrl+Shift+,` | `Ctrl+Shift+,` |
| `tab.rename`, `pane.rename`, `workspace.rename` | none | none | none |
| `pane.swap_next`, `pane.move_next_tab`, `pane.move_prev_tab` | none | none | none |
| `pane.move_to_new_window`, `tab.move_to_new_window` | none | none | none |
| `tab.close`, `workspace.close` | none | none | none |
| `ai.new_chat` | none | none | none |

`ai.open` shows the AI chat: it focuses the chat pane of the shown tab when there is one, and otherwise opens a new chat as
a pane to the right of the focused pane. `ai.new_chat` always starts a new conversation in a new pane. Both are also in the
command palette (`AI chat`, `New AI chat`), and the title bar's `✦ AI` button does what `ai.open` does.

`ai.explain_error` opens the AI chat where you are (the chat pane of the tab, the sidebar column when it is shown, else a new
pane), writes a fixed message into the composer and opens the context card for the pane you were working in with one item
chosen: your selection when the pane shows one, else the last command and its output where the shell hook marks command output
(see [Command marks](#command-marks)), else the visible screen. It shares nothing and sends nothing: you still see the item and
press Send, and it needs a terminal pane. `ai.command_from_description` opens the chat and writes the start of a message that
asks for one command; you finish the sentence and press Send, and no terminal content goes with it. A code block of one clean
line in a reply has the button `Insert at prompt`, which puts the line at the prompt of the pane the chat acts for the way the
history does and never runs it. `Y` stands for why and `G` for generate; `Ctrl+Shift+E` stays unused.

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
window). While you drag outside the window the other window shows the outline of the zone the pointer is over, live
(a tab is outlined as the tab strip, or the chip it is over); it goes when the pointer leaves that window or comes back,
on Escape and on the release, and nothing moves before the release. A release over no drop zone changes nothing. Without the mouse: `pane.move_to_new_window` gives the focused
pane, and `tab.move_to_new_window` the shown tab, a window beside this one (both unbound by default). The pane or tab
keeps its shell, its scrollback and its name; a pane or tab that is alone in its window cannot be torn off, and moving
the last pane out of a window closes that window. A tear-off that is refused (a lone pane or tab, no window to be had)
leaves everything where it is and says why in the status line for a few seconds.

Every tab, workspace chip and pane header has a × at its right end, always visible, that closes it after a question: the
name of what closes, how many shells end with it, and, when it is everything in the window, that the window closes (or
holzWave quits, when it is the only window). Enter or `Close` confirms, Escape or `Cancel` keeps everything, and no key
of the question reaches the terminal. Closing a tab or workspace ends the shells of all its panes and nothing else. The
actions that close a tab or a workspace ask the same question: `tab.close` and `workspace.close` (both unbound by
default, also in the palette) close the shown tab or workspace after it, and a key bound to them asks too, because they
end every shell of the group. Only `pane.close` (`Cmd+W`) closes at once, because it ends one shell. In every case the last pane of the last window cannot be removed: closing it ends the app and the layout on disk
stays as it was, so the next start has it all back.

Actions without a chord in the table (`keybindings.open`, the theme entries of the palette) are reached from the
palette and the dialogs and can be bound by adding them to the table in a later version.

## `connections.json`

The saved connections, one object per connection. **No password or passphrase is in this file**: those are in the
operating system's keychain under the service `ch.holzcloud.holzwave` and the account `ssh:<id>`.

```json
{
  "version": 1,
  "items": [
    { "id": "talos", "name": "Talos", "kind": "ssh", "host": "10.0.4.12", "port": 22, "user": "holz",
      "auth": "key", "key_path": "~/.ssh/id_ed25519", "jump": "bastion", "startup": "tmux attach -t main",
      "reconnect_attempts": 5, "reconnect_delay_secs": 3 }
  ]
}
```

| Field | Meaning |
|---|---|
| `id` | 1 to 64 characters of `a-z`, `0-9`, `-`, `_`; the key of the keychain entry |
| `kind` | `ssh`, `wsl` (a distribution, Windows) or `docker` (a running container) |
| `host` | SSH host or address; WSL distribution; Docker container. No blanks, no leading dash |
| `auth` | `key` (file at `key_path`, `~/` is the home directory; the passphrase, if any, is in the keychain) or `password` |
| `jump` | the `id` of another SSH connection to go through first (at most 3 deep, no loops) |
| `startup` | a line typed into the shell once it runs |
| `reconnect_attempts`, `reconnect_delay_secs` | how often and how fast a lost connection is made again; 0 attempts does not reconnect |
| `shell` | Docker only: the shell to start; empty is `sh` |

A connection that does not validate is dropped when the file is loaded, with a note in the log, and the original is kept
as `connections.json.bad`. A host key that is not in `known_hosts` is learned on first use; a key that **differs** from a
known one is always refused and the pane says so.

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

holzWave has no telemetry and no account, anywhere (DESK-26, F33). It opens a network connection only when you ask for
one: an SSH connection you start, the sync you set up, a link you click (handed to the platform's opener, `open`,
`xdg-open` or `explorer`, for `http`, `https` and `mailto` links, and the file manager reveal of a path) and, if you set
up a provider, the message you send to it. There is no update check, no crash report and no usage counter. Opening the
app, a chat, the providers dialog or a pane makes no request; a name lookup of a provider that is given by a host name
happens only when you press a control that needs its destination.

That claim is a test, not a promise: `core/tests/project_policy.rs` fails when

- a workspace crate or the app package gains a dependency that the test does not list;
- the dependency closure of the app contains a telemetry or HTTP client crate that is not a named exception;
- first-party Rust or page sources use a network API or an absolute URL;
- the page policy allows a remote origin, or an updater or a CI workflow appears.

The AI chat has tests of its own for the same claim: a send that nobody approved leaves nothing.
`core/tests/ai_egress.rs` counts the connections of an idle service (zero) and searches everything a provider receives
for planted secrets.

Two honest exceptions are named in the test, both for the crate `reqwest`. The Tauri 3 alpha links it for a dev-server
proxy that a desktop build never starts (the app sets no `devUrl`). `holzwave-core` links it for the AI chat client in
`core/src/ai/http.rs`, the only first-party file allowed to name it, which uses rustls with the platform's certificate
verifier; building that client opens no connection.

**The AI chat.** What leaves your computer is the text you type, the context you ticked (a pane has to be shared
first) and the results of the tool calls you allowed on their own cards, and nothing else. Terminal content leaves
only after you share that pane: every send shows chips with exactly what goes out, the page never holds the text of a
chip, and secrets of known shapes are replaced by a marker before the preview. That is best effort and no guarantee: a
secret in another form can still be sent. A pane is shared once, and the sharing ends when you change the provider,
the model or the address, and when the pane or the chat closes. `ai_local_context` decides whether a provider on this
computer, or the Apple model, asks the same way (`ask`, the default) or not (`auto`), and `ai_local_only` refuses
every destination that leaves the computer. Provider keys are in the operating system keychain only, are never shown
again, are bound to the address they were saved for and are never synced. `ai.json` holds the providers without keys.
`chats/` holds one file per chat, readable by you only, with terminal context stored as a marker unless
`ai_persist_context` is on (then the cleaned text is stored; turning it off removes it from existing chats). Neither
is synced. The answer of a model is untrusted text and never runs, and every tool action needs your approval on its
own card. holzWave asks OpenAI not to store a request (`store: false`) and OpenRouter not to collect it
(`data_collection: deny`) and cannot promise more: what a provider keeps is that provider's own policy.

## `sync.json` and `sync_state.json`

`sync.json` is `{"endpoint": "...", "user": "..."}`: the folder address and the account of the encrypted sync. The app
password and the passphrase are in the keychain (`sync:password`, `sync:passphrase`), never here. `sync_state.json` is
the device's random id and, per synced file, what the last run left behind (counter, ETag, SHA-256). `sync-incoming/`
holds downloads until the next start, `sync-conflicts/` the local versions that lost a conflict. See [sync.md](sync.md).

## `history.db`

The command history (SQLite, in the app data folder, readable by the user only, deleted rows are overwritten). Commands
arrive from the shell through `wsh`: add `eval "$(wsh init bash)"` to `~/.bashrc`, `eval "$(wsh init zsh)"` to `~/.zshrc`
or `wsh init fish | source` to `~/.config/fish/config.fish`. The hook does nothing outside a holzWave pane and never makes
the prompt wait. A command is **not** written down when it starts with a space (the shells' own "do not remember"
convention), holds control characters or is over 4096 bytes, or contains `password`, `passwd`, `secret`, `token`,
`api_key`, `api-key`, `apikey`, `authorization`, `bearer ` or `sshpass` (in any case). That filter is best effort and no
guarantee: a secret typed in another form is kept, so use a leading space for such a line, and `Clear all` or
`Shift+Delete` in the history dialog forgets. At most 100,000 uses are kept; the oldest tenth goes when it is passed.

**`^R`.** Plain `Ctrl+R` stays with the shell: nothing binds it unless you ask. `wsh init <shell> --ctrl-r` is an opt-in
addition that also binds `Ctrl+R` to open the history dialog of holzWave, the same one as `Cmd+Shift+H`. Put
`eval "$(wsh init bash --ctrl-r)"` in `~/.bashrc`, `eval "$(wsh init zsh --ctrl-r)"` in `~/.zshrc`,
`wsh init fish --ctrl-r | source` in `~/.config/fish/config.fish`, or, in PowerShell,
`wsh init powershell --ctrl-r | Out-String | Invoke-Expression` in `$PROFILE`, instead of the line without the flag (the
text with the flag holds the history hook first, so one line is enough; PowerShell needs no hook text, because holzWave
starts its own). It takes a new shell, or loading the line again.

The binding binds only `Ctrl+R` (in the emacs and the vi insert mode of the shell). When you press it, the shell asks
holzWave in the background, so the prompt never waits, and the dialog opens for the pane you are typing in: it opens only
for the pane that has the focus in its window, and a request from another pane is refused. A pick goes onto the prompt of
that pane and is never run. The half-typed line, the prompt and the exit status of the last command are kept, nothing
else is bound, and outside a holzWave pane nothing happens: the shell's own reverse search answers `Ctrl+R` and the rest
of your rc file keeps running.

The binding covers `bash` 4.0 or newer (the `bash` 3.2 that macOS ships gets no binding and keeps its own search), `zsh`,
`fish` and PowerShell with PSReadLine. Another tool that binds `Ctrl+R` (the key bindings of a fuzzy finder, a history tool)
wins or loses by the order of the lines in your rc file: the line that is loaded later wins.

**PowerShell.** The prompt wrapper that holzWave starts in its own PowerShell panes reports each command to the history,
without a line in your profile. The status is `0` when the command succeeded and `1` when it did not (PowerShell knows
success, not an exit code). A command is not reported when it starts with a space, spans several lines, is longer than 3000
characters, or is typed while the location is not a file system drive (a registry or certificate drive, for example); the
filter of the store above applies on top. The command and the directory reach the short-lived `wsh` process as base64
arguments, so that quotes and a trailing backslash survive in Windows PowerShell 5.1 and in PowerShell 7; like the arguments
of the hooks of the other shells, they are visible in the process list of your own user for a moment.

**Hosts.** Every use is stored with the host it ran on. A command typed in a pane that runs a saved connection is kept
under `ssh:{id}`, `wsl:{id}` or `docker:{id}` (`{id}` is the connection's id in `connections.json`), every other command
under `local`. The history dialog and the sidebar list show the commands of the host the focused pane runs on, so a pane on
a server finds what was typed on that server and a local pane finds the local ones. `Shift+Delete` and `Clear all` forget
on every host. A Files or Preview pane counts as local.

**Other hosts.** Commands typed on other hosts are not recorded yet. The shell on the far side of an SSH, WSL or Docker pane
has no `wsh` and no hook, and the hooks above do nothing outside a holzWave pane. The way in is `wsh` on the remote host
reaching holzWave over an SSH tunnel, which is not built; a command that does reach holzWave from a pane that runs a saved
connection is stored under that connection and found from the panes that run it. An `ssh` typed in a local pane is a local
pane: what you type after it is not recorded either. The directory of a command from another host is not recorded.

**Sync.** History is not synced. The sync format has no place for `history.db`, and deletions are not synced (a deleted
file comes back on the next run, see [sync.md](sync.md)), so a command forgotten with `Shift+Delete` would return. If
history sync is built it will be opt-in and needs deletions in sync first.

## Command marks

holzWave can tell where a command's output starts and ends only when the shell says so. The shell hooks write the
standard prompt marks of the terminal protocol OSC 133 for that: `A` when a prompt is shown, `B` and `C` when a command
starts (its output follows), and `D` with the exit status when it ended. A mark carries nothing else: no command text, no
directory, no host. With the marks the terminal knows which rows are the output of the last command, which is what a
feature such as sharing the last command and its output reads.

The marks come from the same hooks as the history, so the lines of [`history.db`](#historydb) install them:
`eval "$(wsh init bash)"`, `eval "$(wsh init zsh)"` or `wsh init fish | source`. In a PowerShell pane of holzWave nothing
needs to be installed, because the prompt wrapper of the `shell` setting writes them. A hook that you installed earlier
writes no marks: open a new shell. Loading the hook text again in a running shell is safe, it is only loaded once.

These shells write marks: `bash` 4.4 or newer (the `bash` 3.2 that macOS ships reports the history and writes no marks),
`zsh`, `fish` (a `fish` that marks prompts by itself, version 4 and newer, is left alone and the hook adds none), and
PowerShell 5.1 and 7 on Windows, where the marks of a command need PSReadLine to be loaded; without it a PowerShell
pane writes `A` at each prompt and nothing else.

The hooks never read or change the text of your prompt: `A` is written when the prompt hook runs, `B` and `C` when a
command starts and `D` when it ended. A prompt framework (starship, powerlevel10k, oh-my-posh and the like) that rebuilds
the prompt on every prompt therefore cannot remove a mark. A framework that writes OSC 133 marks itself should keep only
one of the two sources, or each command is marked twice. Outside a holzWave pane nothing happens: the hook text does
nothing, and the rest of your `~/.bashrc`, `~/.zshrc` or `config.fish` keeps running.

Limits: `tmux` and other multiplexers may swallow the marks, and then there is no output to find. Output that does not end
with a newline shares its last line with the next prompt. In `zsh` a prompt function that runs before the hook's one and
changes the exit status shows that status in the mark `D`. The marks are text that a program can print itself, so a
program or a file shown with `cat` can fake or hide the boundary of an output: text taken from between marks is untrusted
wherever it is used.

## `wsh ai`

`wsh ai [--] prompt...` asks the model of the running holzWave a question from a shell. The words are joined with single
spaces. A word that starts with a dash before the first ordinary word is reserved for options that do not exist yet and
is refused; `--` ends them, so `wsh ai -- --model x` asks `--model x`. Text piped into the command is sent along with
the prompt, but only when standard input is not a terminal, at most 1 MiB, as UTF-8 without a NUL byte, and never cut:
more than that is refused with exit status 2. `wsh` works only in a holzWave pane and does nothing elsewhere. The exit
status is 0 for an answer, 1 for a refusal or an app that does not answer, and 2 for a mistake in the command line or
in the input (the exit status of a mistake never means that something was sent). The answer is printed after the
characters a terminal could act on are removed, and standard error says how many were.

Nothing leaves this computer until you answer in the window. A card appears in the window of the pane that asked: it
shows the prompt in full, the piped text by counts and by what the secret filter hid (never the text itself), and where
the request goes, with the words `leaves this computer` when it does. Only `Send once`, pressed by a person after half a
second, sends this one request; `Don't send` and `Esc` refuse. A process in a pane can send a request but cannot answer
it: the local socket has no command for that. The card is skipped only when the destination is on this computer or on
the device and the setting `ai_local_context` is `auto`. `ai_local_only` refuses a destination that leaves the device
before any card is shown.

The provider is the one of the last chat turn in this run, else the only provider that is set up, else `wsh ai` says
what is missing: no flag chooses one yet. The piped text gets the same cleaning and secret filter as chat context,
which is best effort and no guarantee. An unanswered request ends after 120 seconds and nothing is sent; a request that
was refused or ended unanswered makes the same pane wait 10 seconds before it may ask again. At most two requests run
and four wait for an answer at the same time. The model gets no tool, so nothing can be read, written or run because of
a `wsh ai` request. Nothing is kept: the request is not saved as a chat and holzWave keeps no copy of it. While a card
waits, the command palette has the entry `Answer the wsh ai request`, which moves the keyboard to the card.

## `ai.json`

The AI providers you set up: for each one a name, the kind of provider, the wire it speaks, a base address and a model. The
file is read when the app starts and written when a profile is saved or deleted. It holds **no secret of any kind**: a key is
never in this file (see below).

The document is `{"version": 1, "profiles": [...], "acknowledged": [...]}`. A file with another version number, a file that is
not a JSON object and a file over 256 KiB are not read: the bytes are kept as `ai.json.bad` (an oversized file is moved there
unread) and the app starts without profiles; the reason names the limit or the version number and never quotes the file. A profile that breaks a
rule below is left out with a note and costs only itself; the rest are kept.

| Field | Type | Values |
|---|---|---|
| `id` | string | 1 to 40 characters of `a` to `z`, `0` to `9`, `-` and `_`; `apple` is reserved |
| `name` | string | 1 to 80 characters, no control character |
| `preset` | string | the kind of provider, one of the tags below |
| `wire` | string | `openai_chat`, `openai_responses`, `anthropic`, `gemini` or `ollama`; one the kind of provider speaks |
| `base_url` | string | the base address, see the rules below |
| `model` | string | at most 200 characters, no control character; empty until you choose one |
| `allow_lan_http` | boolean | allow plain http to a literal address on the local network for this profile; default `false` |
| `key_set` | boolean | only a marker that a key was saved in the keychain; a profile without a key never touches the keychain |
| `gateway_key_set` | boolean | the same marker for the gateway token of a Cloudflare AI Gateway profile |

At most 32 profiles are kept. Fields this version does not know are ignored.

**The base address** is checked when you save a profile, again when the file is read and again before every request, by the same
rule each time. It must be `https`, or plain `http` to this computer (a loopback address, `localhost` or a name below it) or, with
`allow_lan_http`, to a literal local-network address. It must not hold a user name or password, a query or a fragment, a
backslash or an encoded slash or dot, and it is at most 2048 characters. Link-local, cloud metadata, multicast, unspecified and
reserved addresses are refused. An address that still has a part in braces to fill in, such as the account of a gateway, is
refused as not yet complete. Nothing is looked up when the address is checked.

**Kinds of provider.** An empty address takes the default of the chosen wire. The first wire of a kind is its default.

| Tag | Name | Wires | Default address | Key |
|---|---|---|---|---|
| `anthropic` | Anthropic | `anthropic` | `https://api.anthropic.com` | required |
| `openai` | OpenAI | `openai_responses` | `https://api.openai.com/v1` | required |
| `gemini` | Google Gemini | `gemini` | `https://generativelanguage.googleapis.com/v1beta` | required |
| `ollama` | Ollama | `ollama` | `http://127.0.0.1:11434` | optional |
| `lm_studio` | LM Studio | `openai_chat` | `http://127.0.0.1:1234/v1` | optional |
| `llama_cpp` | llama.cpp | `openai_chat` | `http://127.0.0.1:9931/v1` | optional |
| `openrouter` | OpenRouter | `openai_chat` | `https://openrouter.ai/api/v1` | required |
| `vercel_gateway` | Vercel AI Gateway | `openai_chat`, `anthropic` | `https://ai-gateway.vercel.sh/v1`, for `anthropic` without `/v1` | required |
| `cloudflare_gateway` | Cloudflare AI Gateway | `anthropic`, `openai_responses`, `openai_chat`, `gemini` | none: `https://gateway.ai.cloudflare.com/v1/{account_id}/{gateway_id}/` plus `anthropic`, `openai` or `google-ai-studio/v1beta`, with your account and gateway names | optional, plus a gateway token |
| `litellm` | LiteLLM | `openai_chat` | `http://127.0.0.1:4000/v1` | optional |
| `custom` | Custom | all five | none | optional |

The header that carries a key follows the wire: `x-api-key` for `anthropic`, `x-goog-api-key` for `gemini`, and
`authorization` with `Bearer ` for the others. A Cloudflare AI Gateway profile has a second key, the gateway token, which is sent
as `cf-aig-authorization`; both are optional, because a gateway can be open or can hold the provider key itself. There is no field
for other headers: a profile cannot send a header of its own, and nothing that identifies you is added.

**Where a key lives.** Only in the operating system keychain, one entry per key: `ai:<id>` for the provider key and
`ai:<id>:gateway` for the gateway token. The entry holds the key together with the address it was saved for (scheme, host and
port). A key is never shown, never returned to the page, never written to a file or a log, and never synced. `key_set` is only a
marker. When the address of a profile changes, whether in the dialog, by editing this file or by importing one, a key saved for
the old address is not used for the new one; no request is sent until you enter the key again. Without a system keychain (for
example a system without a running secret service) a key cannot be saved, a profile that needs one is unavailable, and
nothing is kept in a file or in memory instead. A local server that needs no key works without touching the keychain. A key is
at most 2048 bytes of printable ASCII without spaces; spaces around it are removed when it is saved. No key is read from an
environment variable.

**Gateways and third parties.** OpenRouter, the Vercel and Cloudflare gateways, LiteLLM and a Custom address are servers that
see the whole request, and a gateway may log it. No request goes to such an address until it has been accepted once, because your
messages and the context you share go through it. The accepted address (scheme, host and port) is added to
`acknowledged`; a request to another host asks again, a change of path does not, and deleting the profile keeps the entry. An
address on this computer needs no acknowledgement. At most 64 addresses are kept, the oldest first to go. OpenRouter is also asked
not to let its providers collect your data (`provider.data_collection` set to `deny` in every request); a gateway can still log
what it receives, and that setting is not a promise about what it does.

The file and the keychain entries `ai:*` stay out of the sync. Copy the file to another machine and the keys do not come with it:
enter them again there.

<!-- example: ai.json -->
```json
{
  "version": 1,
  "profiles": [
    {
      "id": "claude",
      "name": "Claude",
      "preset": "anthropic",
      "wire": "anthropic",
      "base_url": "https://api.anthropic.com",
      "model": "",
      "allow_lan_http": false,
      "key_set": true,
      "gateway_key_set": false
    },
    {
      "id": "ollama",
      "name": "Ollama on this computer",
      "preset": "ollama",
      "wire": "ollama",
      "base_url": "http://127.0.0.1:11434",
      "model": "",
      "allow_lan_http": false,
      "key_set": false,
      "gateway_key_set": false
    },
    {
      "id": "openrouter",
      "name": "OpenRouter",
      "preset": "openrouter",
      "wire": "openai_chat",
      "base_url": "https://openrouter.ai/api/v1",
      "model": "",
      "allow_lan_http": false,
      "key_set": false,
      "gateway_key_set": false
    }
  ],
  "acknowledged": ["https://openrouter.ai:443"]
}
```

## `widgets/*.json` and `widget_notes.json`

Widgets of the sidebar. `notes` and `pomodoro` exist without a file; a file with the same id replaces them. Fields (no others
are accepted): `version` (1), `id` (default: the file name; lowercase letters, digits, `-`, `_`, at most 32), `title`
(1 to 40 characters), `kind`, and by kind:

<!-- example: widgets/focus.json -->
```json
{ "title": "Focus", "kind": "timer", "work_min": 50, "break_min": 10 }
```

<!-- example: widgets/deploy.json -->
```json
{ "title": "Deploy", "kind": "list", "items": [
  { "label": "Pods", "command": "kubectl get pods" },
  { "label": "Docs", "url": "https://example.org/docs" } ] }
```

<!-- example: widgets/scratch.json -->
```json
{ "title": "Scratch", "kind": "note" }
```

An item has either a one-line `command` (put on the prompt, never run) or an `http`/`https` `url`. A list has 1 to 50 items,
at most 24 widgets are shown, a file is at most 64 KiB. A file that is not valid is left out and the sidebar shows why.
`widget_notes.json` holds the texts of the note widgets (at most 64 KiB each). Both are synced; a downloaded one is
applied at the next start.

The files are described by [widget.schema.json](widget.schema.json), a JSON Schema (draft 2020-12) that an editor with a JSON
Schema setting can map to `widgets/*.json` for completion and checks (in Visual Studio Code, add an entry with the file
match `widgets/*.json` to `json.schemas`). The schema describes version 1: a later version of the file format gets a new
schema file next to this one, and `widget.schema.json` keeps describing version 1. Do not add a `$schema` member to a widget
file: it is refused like any other member the schema does not list, so the schema is set in the editor and not in the file.
Write whole numbers without a fraction or an exponent (`25`, not `25.0`). A test (`core/tests/widgets_schema.rs`) gives the
schema and the loader the same documents and fails when they differ, so the schema says what the app accepts.

What a schema cannot say, and the loader still checks:

- a file that names the same key twice is refused;
- a link of more than 2048 bytes is refused, and the schema counts characters, so it accepts some of them;
- an id taken from the file name (a file without an `id`) must follow the id rule too;
- the limits that lie outside one document: at most 24 widgets are shown (the two built-in ones count) and a file is at most
  64 KiB.
