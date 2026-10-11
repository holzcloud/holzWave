# Test plan

Three parts: how to run everything locally, the per-platform manual list for hardware the owner owns, and the record of
the macOS acceptance of Phase 2. All of it runs locally; nothing here needs GitHub (D-04).

## Part 1: run everything locally

```sh
. scripts/env.sh                      # Rust stable and Zig 0.16 on PATH (the pinned engine needs exactly 0.16)
npm --prefix app ci                   # once: typescript, the Tauri CLI and playwright-core, nothing else
(cd app && npx playwright-core install webkit chromium)   # once: the two browsers of the browser tests, cached

cargo test --workspace --offline      # core, app shell (headless, real shells), the project policy, the spike
npm test --prefix app                 # builds app/dist with tsc, then the node unit tests
npm run test:browser --prefix app     # WebKit and Chromium against app/dist with an in-page Tauri stub

cargo test --release -p holzwave-core --offline --test budget -- --ignored --nocapture   # the budgets
npm run bench:build --prefix app      # builds app/dist-bench/index.html, the renderer benchmark (open it in a browser)
cargo run -p holzwave-app             # the app itself, for the lists below
```

Notes:

- `npm run test:browser` takes `--only <part of a file name>` and `--engine webkit|chromium`; a test fails on any page
  error or Content-Security-Policy violation, even when its own assertions passed.
- The budget tests are `#[ignore]` because a debug build is meant to be slower than the budget. They print the measured
  update plus encode time and the key-to-frame time; the numbers of 2026-10-11 are in
  [architecture.md](architecture.md) section 12. They are measured without the webview.
- The dev profile builds `libghostty` in Zig debug mode and is slow on very large feeds; use `--release` for anything
  that moves megabytes.
- `UPDATE_FIXTURES=1 cargo test --offline` rewrites the two golden files (`app/tests/fixtures/frames.json` and
  `protocol.json`); review the diff before committing it.
- `core/tests/project_policy.rs` is part of `cargo test --workspace`. It fails when a dependency, a network API, a remote
  origin or a CI workflow appears (DESK-26, D-03, D-04), and when [config.md](config.md) no longer matches the loaders.

## Part 2: per-platform manual list

The same numbered list for macOS, Windows and Linux (X11 and Wayland), so Phase 2.2 only has to run it. Every item has
an expected result. Where the list says `Cmd+W`, Windows and Linux use the table that Phase 2.2 ships
(`Ctrl+Shift+W` is the plan); `Option` is `Alt` there, and a layout's third level is `AltGr`.

| # | What to do | Expected |
|---|---|---|
| 1 | Launch the app. Move and resize the window, maximise it, quit, launch again | The window comes back at the same position and size, maximised if it was. With a monitor disconnected it opens on a monitor that is there |
| 2 | In a pane `cd` to a directory, split the pane, `cd` elsewhere in the new one, make a new tab, quit, launch again | Each pane's shell starts in the directory it had; a split and a new tab start in the directory of the pane they came from |
| 3 | Run `vim`, `less` on a long file and `htop`. Use the arrow keys, the wheel, and the mouse (in htop; in vim after `:set mouse=a`). Hold Shift and drag over text while the program tracks the mouse | Arrows work in application mode. The wheel scrolls inside `less` and `vim`. The mouse clicks and drags in `htop` and `vim`. Shift+drag selects text in the terminal anyway. Quitting each program restores the screen |
| 4 | At a prompt run `printf '\e[6n'` | The terminal answers: the reply (something like `^[[5;1R`) arrives as input, nothing hangs and the prompt stays usable |
| 5 | In `cat -v` press the arrows, Home, End, Backspace, Delete, F1 to F12, and Ctrl+C, Ctrl+D, Ctrl+L at a shell prompt | Each key shows its own escape sequence in `cat -v` and none shows plain text; Backspace shows `^?`. Ctrl+C interrupts, Ctrl+D ends input, Ctrl+L clears |
| 6 | Switch to a Japanese input source (IME), compose a word, open the candidate list, commit it, press Enter. Type an accented character with a dead key where the layout has one | The composition shows while it is open, the candidate window appears beside the cursor, the committed text arrives once, and Enter after the commit does not run the line |
| 7 | Type the special characters of your layout with Option (AltGr) held: on a Swiss layout `@`, `\|`, `[`, `]`, `{`, `}`. Then turn `option_as_alt` on and press Alt+F in `cat -v` | With the default they type normally. With `option_as_alt` on, Alt+F gives `^[f` |
| 8 | Select text with a drag, a double click (word) and a triple click (line). Copy it and paste it into another app. Paste it back | The selection follows the pointer; the text arrives intact in both directions |
| 9 | Paste several lines into a plain prompt (`cat`), and into a shell that uses bracketed paste (zsh, bash 5) | In `cat` a confirmation asks before the text is written. In a bracketed-paste shell no question comes and the lines arrive as one paste that does not run until Enter |
| 10 | Run `seq 1 200000` and select a word, scroll up while it runs, copy again | The viewport stays where it was scrolled to, the selection stays on its text, and the copy returns the text that was selected |
| 11 | Search the scrollback (`Cmd+F`) for a word that occurs several times. Click a URL and a file path with the link modifier. Show the hints (`Cmd+Shift+Space`) and press a label. Enter copy mode (`Cmd+Shift+X`), move with `h j k l`, `w`, `b`, `0`, `$`, `g`, `G`, select with `v`, yank with `y`, leave with `q` | The count and the current match show, `Enter` and `Shift+Enter` step through them. The URL opens in the browser, the path is revealed in the file manager and never run. A hint label does the same as its click. Copy mode selects and yanks the text under the cursor |
| 12 | Split right and down, focus the panes with the shortcuts and the pointer, close one with `Cmd+W`, make tabs and switch with `Cmd+1` to `Cmd+9`, make a workspace and switch with `Cmd+Alt+1` to `Cmd+Alt+9`, drag a divider | Closing a pane ends its shell; the last pane closes the app. The divider follows the pointer for the whole gesture and the layout is written once the gesture is over |
| 13 | Switch between the three themes (settings dialog and palette). Put a theme file in the `themes/` folder of the data directory (the example of [config.md](config.md)), focus the window | All three change the whole window at once and survive a restart. The user theme appears in the list without a restart |
| 14 | Change the font size (`Cmd+=`, `Cmd+-`, `Cmd+0`) and the UI scale (75, 100, 150, 200 %) in the settings dialog | The grid is re-measured and every pane keeps its shell's size correct (`stty size`); at 150 % and 200 % nothing overlaps or is cut off |
| 15 | Turn on the OS settings for reduced motion and increased contrast, and turn the cursor blink off in the settings | The chrome stops animating, the focus line and hairlines get stronger, a program's blinking cursor stops blinking. Turning them off again restores the look without a restart |
| 16 | Open a few panes and tabs, quit with the shortcut of the platform, and look for the shells (`pgrep -fl zsh` on macOS and Linux; `tasklist` on Windows) | No shell the app started is left |
| 17 | Leave the app idle for a minute with the cursor steady and read its CPU use (Activity Monitor, `top`, Task Manager) | Near zero; note the number |
| 18 | In one pane run `yes \| head -n 2000000`, and in another `cat` a file of 100 MB or more. Meanwhile switch tabs and type | The window stays responsive, typing echoes, and the output ends. Read the status line's draw time and fps and note them |
| 19 | Open a pane and run `cat` on a file with wide characters, emoji and combining marks | The glyphs are aligned to the cells, nothing overlaps, no glyph is corrupted |

## Part 3: macOS acceptance record of Phase 2

One row for each item that the automated suites cannot verify (no screen capture, no real input method, no physical
layout, no native menu, no real window). The Result column is empty until the owner has run the row on the Mac: write
`pass`, or `fail` with a line of what was seen. A row that is not run stays empty; nothing here is claimed verified.

Run: date ____ machine ____ macOS ____ build commit ____ (`git rev-parse --short HEAD`, built with
`cargo run -p holzwave-app`)

| # | Item | How | Expected | Result |
|---|---|---|---|---|
| A1 | `vim`, `less`, `htop` usable; the mouse and wheel in full-screen programs; Shift selects (D-24, success criterion 1) | Part 2 item 3, and `tmux` with `set -g mouse on` | As in Part 2 item 3; `tmux` takes clicks, drags and the wheel | |
| A2 | A device query does not hang a program (success criterion 2) | Part 2 item 4 | As in Part 2 item 4 | |
| A3 | Option on the Swiss layout; `Cmd+Alt+digit` selects a workspace where Option changes the typed character | Part 2 item 7, then `Cmd+Alt+1` and `Cmd+Alt+2` with two workspaces | `@`, `\|`, `[`, `]`, `{`, `}` type normally; Alt+F gives `^[f` with the setting on; the workspace shortcuts switch | |
| A4 | `Ctrl+[` and `Ctrl+I` behave as the engine's encoder says (D-13) | In `cat -v` press `Ctrl+[`, `Ctrl+I`, `Ctrl+M` | Whatever the engine's encoder sends; D-13 takes it unchanged. Note what `cat -v` shows: `Ctrl+[` ____, `Ctrl+I` ____, `Ctrl+M` ____ | |
| A5 | Japanese IME in WKWebView: composition, candidate window, one commit, Enter after commit; dead keys on the Swiss layout | Part 2 item 6 | As in Part 2 item 6 | |
| A6 | `Cmd+K` opens the palette with an IME active | Switch to the Japanese source, press `Cmd+K`, type, `Escape` | The palette opens, takes the typed text, closes; no composition is left in the terminal | |
| A7 | The editor records a chord on a layout that types a national character on a bracket key | Settings, Key bindings, Change on `Focus next pane`, press `Cmd+]` | The chord shown is `Cmd+]` (not the layout's character) | |
| B1 | `Cmd+W` reaches the page: it closes the focused pane and kills its shell; the native menu has no Close Window item | Look at the menu bar, press `Cmd+W` in a split | The menu has no Close Window; the pane and its shell go; `Cmd+Shift+W` does nothing | |
| B2 | `Cmd+C` and `Cmd+V` are not swallowed by the native Edit menu; Cut, Copy, Paste and Select All items are present | Select text, `Cmd+C`, paste elsewhere, `Cmd+V` back, use the Edit menu | Both shortcuts and the menu work | |
| B3 | Multi-line paste asks in a shell without bracketed paste and does not in one with (D-15) | Part 2 item 9 | As in Part 2 item 9 | |
| B4 | Selection gestures in the real window: drag, double and triple click; the highlight follows output and scrolling; a plain click does not flicker; `Cmd+C` reaches the system clipboard | Part 2 items 8 and 10 | As in those items | |
| C1 | Quit and pane close leave no shell children (DESK-14) | Part 2 item 16 with `pgrep -fl zsh`, after `Cmd+Q` and after closing the last window | No `zsh` of the app remains | |
| C2 | Relaunch restores layout, each pane's directory and the window frame; a position is restored only on a connected monitor (DESK-22) | Part 2 items 1 and 2, with an external monitor unplugged between runs | As in those items | |
| C3 | Layout and session are flushed by `Cmd+Q` and by closing the last window | Move a divider, change directory, `Cmd+Q` at once, relaunch | The divider and the directory are as left | |
| C4 | A real divider drag writes `layout.json` once per window and feels right | Drag a divider for several seconds while watching the modification time of `layout.json` | One write per 250 ms window at most; no stutter, the divider keeps up with the pointer | |
| C5 | The data directory is where [config.md](config.md) says | Look in `~/Library/Application Support/ch.holzcloud.holzwave/` | `layout.json`, `settings.json` and `session.json` are there after a run | |
| C6 | `Cmd+,` opens the settings with the macOS menu present; a hand edit of `settings.json` is picked up when the window gains focus; no CSP report in the web console | Edit `font_size` in the file, focus the window; open the web inspector of the debug build if it opens, and read its console | The size changes; the console shows no Content-Security-Policy message | |
| C7 | The platform opener really starts the browser and the file manager | Part 2 item 11 (link, path) | The URL opens in the default browser, the path is revealed in Finder and never run | |
| D1 | Three themes switch visually, survive a restart and look like [design.md](design.md) section 6 (D-01, D-20) | Part 2 item 13, at 100 % and 150 % UI scale | The whole window follows; rose appears only where design.md rations it | |
| D2 | Traffic lights clear the bar (80 px room), the free space of the bar drags the window, the overlay title bar lines up with the bar's hairline | Look at the top left, drag by an empty part of the bar | The lights sit clear of the workspace chips; the window drags | |
| D3 | The chrome reads right: proportions, numbered tabs, the 11 px labels against the 12 px chrome minimum, long paths cut on the left, the text on the shown status segment | Look at bar, pane headers and status line on the three themes; open a pane in a deep directory | Nothing is cramped or unreadable; the status segment text is legible on all three | |
| D4 | The bundled font, the hollow outline of an unfocused pane's cursor, the thickness of bar and underline cursors | `printf '\e[6 q'` (steady bar), `printf '\e[4 q'` (steady underline), focus another pane | JetBrains Mono renders; the unfocused pane's cursor is hollow; bar and underline are 1 CSS pixel thick (2 device pixels at 2x) | |
| D5 | Selection highlight and cursor visibility on every theme, in particular on Papier | Select text with a full screen of colour on each theme | The text stays readable inside the selection; the cursor is visible | |
| D6 | Search bar, hint labels, link underline and copy cursor on the three themes at 100 % and 150 % | Part 2 item 11 on each theme and scale | They are legible and do not overlap the text they point at | |
| D7 | The `n` and `N` flow after search and `Escape` (decision left open by the search plan) | `Cmd+F`, a needle, `Enter`, `Escape`, then `n` in copy mode | Judge it: `Escape` clears the search, so `n` has nothing to step to while the bar is closed. If that is awkward, ask for "keep the search while copy mode is active" | |
| D8 | The command palette: look, backdrop tint on Papier, the 160 ms fade | `Cmd+K` on each theme at 100 % and 150 % | A readable list over a tinted backdrop; the fade is gone under reduced motion | |
| D9 | The settings dialog and the key binding editor look right on the three themes | Open both on each theme at 100 % and 150 % | Spacing, radio rows and the light theme's chrome are right | |
| D10 | A CJK flood and clusters leave no corrupted glyph | Part 2 item 19, and `cat` of a large file with CJK text | Cells stay aligned after the flood | |
| E1 | Reduce motion, Increase contrast and Reduce transparency in System Settings reach the web view; forced colours | Part 2 item 15, then the editor and the chrome on the three themes at 100 % and 150 % | The chrome follows the OS settings live; the stronger look is readable | |
| E2 | The cursor stops blinking with the setting off | Part 2 item 15 | A blinking cursor of a program is steady | |
| F1 | A flood keeps the window responsive; draw time and fps (D-16: frames stay events, measured) | Part 2 item 18, read the status line | The UI answers during the flood; the numbers are noted: draw ____ ms, ____ fps | |
| F2 | A flood of queries that the program never reads does not grow memory without limit | `yes $'\e[c' \| head -c 20000000` in a pane, watch the app's memory in Activity Monitor | Memory settles; the pane stays usable afterwards | |
| F3 | The pixel size of a cell reaches the PTY | In a pane (needs `python3`) `python3 -c 'import fcntl,termios,struct;print(struct.unpack("HHHH",fcntl.ioctl(0,termios.TIOCGWINSZ,bytes(8))))'` | The last two numbers (width and height in pixels) are not zero | |
| F4 | The commands take snake_case arguments end to end in the real shell | Start the app: panes start and fill the pane, resize the window, paste a multi-line text and confirm it | Shells start at the right size, resizing updates `stty size`, the confirmed paste arrives | |
| F5 | Search over a full scrollback stays quick in a release build | `seq 1 10000`, then search for `9999` while typing the needle | The count appears without a visible stall | |
| F6 | The everyday behaviour is intact: tabs, workspaces, shortcuts, wheel scroll, resize, copy | Part 2 items 8, 10, 12 | As in those items | |
| F7 | Idle CPU | Part 2 item 17 | Near zero; the number is noted: ____ % | |

### What to expect

Known limits of this build, so they are not reported as defects:

- A link or path that cannot be opened (a refused scheme, a missing path, no opener) gives no visible answer yet.
- The palette matches action titles only, shows no highlight of the matched letters and has no recent-first order.
- Hint labels cover the first 676 targets of a pane in reading order, and a number of 7 to 40 digits counts as a hash.
- `Cmd+C` is a fixed shortcut. A chord the core refused stays in the overrides map of `settings.json` until it is reset.
- The UI scale and the font size are user settings; the macOS web view reports no OS text-size signal (D-25).
- Windows and Linux are not built yet (Phase 2.2); Part 2 is the list the owner runs there.
