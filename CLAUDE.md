# holzWave: instructions for Claude

holzWave is a terminal emulator: a Rust core (`core/`), a Tauri 3 shell (`app/src-tauri`) and a TypeScript page with no
framework and no bundler (`app/src`). The plan is in `docs/roadmap.md`, the build in `docs/building.md`.

## Design is binding

Every change that a user can see (chrome, panes, dialogs, overlays, status line, colours, type, spacing, motion, icons,
mobile layout, README and website images) follows the design spec. Do not invent a look.

1. **Read first:** `docs/design.md` (tokens in sections 2 to 4, components in 5, the window in 6) and, for the picture,
   `docs/design/haarlinie-voll.html` (open the view that matches your change). Section 7 lists the platform differences.
2. **Use the tokens.** Colours come from `--hw-*` variables (`app/src/app.css`, `app/src/chrome.css`), never a literal
   hex value. Sizes are in `rem` so the UI scale moves everything. Mono (JetBrains Mono) for chrome and anything a
   machine wrote; Manrope only in previews. Labels are uppercase and tracked (`.label`).
3. **Keep the three levels apart.** Workspace = round pill with a diamond, in the title bar. Tab = square, numbered,
   in its own row. Pane = tile with a header. Do not give one level the shape of another.
4. **Flat and hairline.** 1 px `--hw-line` between things, no gradients, no blur, no shadows except floating layers
   (palette, dialogs, popovers). Rose marks focus and action only, never decoration.
5. **Every state has a second cue** besides colour (underline, dot shape, label). Keep `contrast-high`,
   `motion-reduce` and `forced-colors` working; the focus ring is 2 px for the keyboard.
6. **One look on every platform.** Only the OS window buttons and the shortcut labels differ (design.md section 7).
7. **Touch targets** are at least 44 px on mobile and 28 px on desktop.
8. **Do not draw what is not built.** A control for a planned feature (SSH, sync, previews, AI) is added in the phase
   that builds the feature, in the place the design gives it.

If the design has no answer, pick the smallest thing that fits the rules above and **add it to `docs/design.md` and to
`docs/design/haarlinie-voll.html` in the same change**, so the spec stays the source of truth. If a change makes the
spec wrong, change the spec first.

### Images

The PNGs in `docs/design/`, `docs/design/platforms/` and the banner (`assets/brand/banner.svg`, `banner.png`) are
rendered from `docs/design/haarlinie-voll.html`. When the page changes, render them again (Playwright, the view picker
buttons `#views button[data-v=…]` and the platform buttons `#os button[data-os=…]`, element `#stage` at 1120×680, device
scale factor 2) and update the README and `docs/design.md`. `assets/brand/render-banner.cjs` renders the banner.

## Language and text

Everything in the repository is English: code, comments, docs, commit messages, UI text. The user may write German in
chat; answer them in German, but do not put German into files.

## Checks before you finish

- `cd app && npm run build` (TypeScript must pass) and `npm test` for the unit tests.
- `npm run test:browser` for any change to the page. It needs Chromium and WebKit through Playwright.
- `cargo test --workspace` for any change to `core/` or `app/src-tauri`.
- Update the tests together with the code; a layout change that moves an element changes its test in the same commit.

## Git

Work on the branch you were given, commit with a clear message, push, and do not open a pull request unless asked.
