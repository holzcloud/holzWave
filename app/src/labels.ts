// Text the chrome shows, as pure functions: no DOM, no state, so node can import and test this module.

/** The label of a tab: its number (the digit of its Cmd chord), a space and its title. */
export function tabLabel(_index: number, _title: string): string {
  return "";
}

/** The local time as HH:MM, zero padded; `--:--` for a date that is not valid. */
export function clockText(_now: Date): string {
  return "";
}

/** The pane count segment of the status line: `1 pane`, `2 panes`. */
export function paneCountText(_count: number): string {
  return "";
}

/**
 * A working directory with the user's home prefix shown as `~`. The page does not know the home directory, so the
 * prefix is the platform's convention for it (`/Users/<name>`, `/home/<name>`, `C:\Users\<name>`); an unknown
 * platform leaves the path alone.
 */
export function shortenHome(_path: string, _os: string): string {
  return "";
}

/** The renderer segment: the renderer name and the average draw time; the name alone before one was measured. */
export function rendererText(_name: string, _avgMs: number): string {
  return "";
}
