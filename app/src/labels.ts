// Text the chrome shows, as pure functions: no DOM, no state, so node can import and test this module.

/** The label of a tab: its number (the digit of its Cmd chord), a space and its title. */
export function tabLabel(index: number, title: string): string {
  const number = String(index + 1);
  return title === "" ? number : `${number} ${title}`;
}

const two = (n: number): string => String(n).padStart(2, "0");

/** The local time as HH:MM, zero padded; `--:--` for a date that is not valid. */
export function clockText(now: Date): string {
  if (Number.isNaN(now.getTime())) return "--:--";
  return `${two(now.getHours())}:${two(now.getMinutes())}`;
}

/** The pane count segment of the status line: `1 pane`, `2 panes`. */
export function paneCountText(count: number): string {
  const n = Number.isFinite(count) && count > 0 ? Math.floor(count) : 0;
  return `${n} ${n === 1 ? "pane" : "panes"}`;
}

/** How long a notice stays in the status line. */
export const NOTICE_MS = 5000;

/**
 * What the status line says when a pane or tab was not given a window of its own: `reason` is the shell's (a tab or pane
 * that is alone in its window, the window system's refusal); without one the shell only cancelled the gesture. Cut short
 * and stripped of control characters, because the status line is one line.
 */
export function refusalText(kind: "pane" | "tab", reason?: unknown): string {
  const what = kind === "pane" ? "Pane" : "Tab";
  const text = typeof reason === "string" ? reason.replace(/[\u0000-\u001f\u007f]+/g, " ").trim().slice(0, 80) : "";
  return text === "" ? `${what} stays here: it cannot get a window of its own` : `${what} not moved: ${text}`;
}

/** What each platform calls a user's home directory: the part up to and including the user name. */
const HOME: Record<string, RegExp> = {
  macos: /^\/Users\/(?!Shared(?:\/|$))[^/]+(?=\/|$)/,
  linux: /^\/home\/[^/]+(?=\/|$)/,
  windows: /^\/?[A-Za-z]:[\\/]Users[\\/][^\\/]+(?=[\\/]|$)/,
};

/**
 * A working directory with the user's home prefix shown as `~`. The page does not know the home directory, so the
 * prefix is the platform's convention for it (`/Users/<name>`, `/home/<name>`, `C:\Users\<name>`); an unknown
 * platform leaves the path alone.
 */
export function shortenHome(path: string, os: string): string {
  const home = Object.hasOwn(HOME, os) ? HOME[os] : undefined;
  return home ? path.replace(home, "~") : path;
}

/**
 * The renderer segment: the renderer name and the average draw time; the name alone before one was measured (NaN).
 * Zero is a measurement: a timer coarser than a draw reads 0 ms.
 */
export function rendererText(name: string, avgMs: number): string {
  if (!Number.isFinite(avgMs) || avgMs < 0) return name;
  return `${name} ${avgMs.toFixed(2)} ms`;
}
