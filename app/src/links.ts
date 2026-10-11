// Clickable targets in the visible output: URLs, file paths and hashes, found in plain text (OSC 8 hyperlinks are
// deferred). Detection is a pure function over one row given as one string per cell column, so node imports this
// file for the unit tests; the DOM and the core are only reached from `initLinks`, by dynamic import.
//
// What the terminal prints decides what looks clickable (T-02-75), so the rules are narrow: a URL has the scheme
// http, https or mailto and no control character, a path is only ever revealed (never opened or run), and the page
// sends the matched text as it is; the core checks scheme, length and characters again.
import { rowCells } from "./frames.js";
import { gridRect } from "./geometry.js";
import { state } from "./state.js";

export type TargetKind = "url" | "path" | "hash";

export interface Target {
  kind: TargetKind;
  /** First and last cell column the target covers (both ends inclusive; a wide character covers two). */
  startCol: number;
  endCol: number;
  /** The URL, the path (without a :line:col suffix) or the hash, exactly as printed. */
  text: string;
  /** From a `:12` or `:12:3` suffix of a path. */
  line?: number;
  col?: number;
}

/** The command and arguments a click on a target sends; the argument names are the golden's (protocol.json). */
export type LinkAction =
  | { command: "open_url"; args: { url: string } }
  | { command: "reveal_path"; args: { path: string; base: string | null } };

/** The longest URL the core opens, in bytes; a longer one is not offered. */
export const MAX_URL = 2048;
/** The longest path that is offered. */
const MAX_PATH = 4096;

// ---- detection --------------------------------------------------------------------------------------

/** A URL candidate: the scheme and everything up to a blank, a control or format character, `<`, `>`, `"` or a backtick. */
const URL_RE = /(?:https?:\/\/|mailto:)[^\s\p{C}<>"`]+/gu;
/** A path candidate: a prefix and then name characters and slashes; `:`, brackets, quotes and blanks end it. */
const PATH_RE = /(?:~\/|\.{1,2}\/|\/(?!\/))[\p{L}\p{N}\p{M}_.@%+#~/-]+/gu;
/** A `:line` or `:line:col` suffix right after a path. */
const SUFFIX_RE = /:(\d{1,9})(?::(\d{1,9}))?(?!\d)/y;
/** A run of letters, digits and underscores: the unit a hash is looked for in. */
const WORD_RE = /[\p{L}\p{N}_]+/gu;
const HASH_RE = /^[0-9a-f]{7,40}$/;
/**
 * A character that may not come right before a path: it would make the slash part of a word or of another path.
 * ASCII only: text in a script without blanks runs straight into a path or a URL.
 */
const NOT_BEFORE_PATH = /[A-Za-z0-9_.~/\\-]/;
/** A character a URL may not start after. */
const NOT_BEFORE_URL = /[A-Za-z0-9_]/;

const OPENERS: Readonly<Record<string, string>> = { ")": "(", "]": "[", "}": "{" };
const SENTENCE = new Set([".", ",", ";", ":", "!", "?", "'", '"']);

/** The last code point of `text[0..end)`, or "" at the start. */
function before(text: string, end: number): string {
  if (end <= 0) return "";
  const hi = text.charCodeAt(end - 1);
  if (hi >= 0xdc00 && hi <= 0xdfff && end >= 2) {
    const lo = text.charCodeAt(end - 2);
    if (lo >= 0xd800 && lo <= 0xdbff) return text.slice(end - 2, end);
  }
  return text.charAt(end - 1);
}

/**
 * The end of a URL candidate after cutting what belongs to the sentence around it: trailing punctuation and quotes,
 * and a closing bracket that has no opening one inside the URL. Linear in the length of the candidate.
 */
function urlEnd(s: string): number {
  const open: Record<string, number> = { "(": 0, "[": 0, "{": 0 };
  const close: Record<string, number> = { ")": 0, "]": 0, "}": 0 };
  for (const ch of s) {
    if (ch in open) open[ch] = (open[ch] ?? 0) + 1;
    else if (ch in close) close[ch] = (close[ch] ?? 0) + 1;
  }
  let end = s.length;
  while (end > 0) {
    const last = s.charAt(end - 1);
    const opener = OPENERS[last];
    if (SENTENCE.has(last)) {
      end--;
    } else if (opener !== undefined && (close[last] ?? 0) > (open[opener] ?? 0)) {
      close[last] = (close[last] ?? 0) - 1;
      end--;
    } else {
      break;
    }
  }
  return end;
}

/** Whether `url` is something the page offers to open: an exact lower-case scheme, a body and no control character. */
function isOpenableUrl(url: string): boolean {
  if (url.length > MAX_URL || /[\s\p{C}]/u.test(url)) return false;
  if (url.startsWith("mailto:")) return /^mailto:[^@]+@[^@]+$/.test(url);
  return /^https?:\/\/[^/?#]/.test(url);
}

interface Claim {
  start: number;
  end: number;
}

const overlaps = (claims: readonly Claim[], start: number, end: number): boolean =>
  claims.some((c) => start < c.end && end > c.start);

/**
 * The targets of one row. `cells` has one string per cell column: a wide character is followed by an empty string
 * for its spacer, and the tail of a grapheme cluster is part of its cell. The targets come back in column order and
 * do not overlap: a URL wins over a path and a path over a hash inside it.
 */
export function findTargets(cells: string[]): Target[] {
  // The row as one string, with the first and last column of the cell each UTF-16 unit came from.
  let text = "";
  const first: number[] = [];
  const last: number[] = [];
  for (let c = 0; c < cells.length; c++) {
    const cell = cells[c];
    if (typeof cell !== "string" || cell === "") continue;
    let end = c;
    while (end + 1 < cells.length && cells[end + 1] === "") end++;
    for (let i = 0; i < cell.length; i++) {
      first.push(c);
      last.push(end);
    }
    text += cell;
  }
  if (text === "") return [];

  const found: { start: number; end: number; target: Target }[] = [];
  const claims: Claim[] = [];
  const add = (start: number, end: number, target: Omit<Target, "startCol" | "endCol">): void => {
    const startCol = first[start];
    const endCol = last[end - 1];
    if (startCol === undefined || endCol === undefined) return;
    claims.push({ start, end });
    found.push({ start, end, target: { ...target, startCol, endCol } });
  };

  // URLs
  for (const m of text.matchAll(URL_RE)) {
    const start = m.index ?? 0;
    if (NOT_BEFORE_URL.test(before(text, start))) continue;
    const url = m[0].slice(0, urlEnd(m[0]));
    if (!isOpenableUrl(url)) continue;
    add(start, start + url.length, { kind: "url", text: url });
  }

  // Paths
  for (const m of text.matchAll(PATH_RE)) {
    const start = m.index ?? 0;
    if (NOT_BEFORE_PATH.test(before(text, start))) continue;
    let path = m[0];
    while (path.endsWith(".")) path = path.slice(0, -1); // the full stop of a sentence
    const prefix = path.startsWith("~/") ? 2 : path.startsWith("../") ? 3 : path.startsWith("./") ? 2 : 1;
    if (path.length <= prefix || path.length > MAX_PATH) continue;
    let end = start + path.length;
    if (overlaps(claims, start, end)) continue;
    const target: Omit<Target, "startCol" | "endCol"> = { kind: "path", text: path };
    if (path.length === m[0].length) {
      SUFFIX_RE.lastIndex = end;
      const s = SUFFIX_RE.exec(text);
      if (s) {
        target.line = Number(s[1]);
        if (s[2] !== undefined) target.col = Number(s[2]);
        end += s[0].length;
      }
    }
    if (overlaps(claims, start, end)) continue;
    add(start, end, target);
  }

  // Hashes
  for (const m of text.matchAll(WORD_RE)) {
    const word = m[0];
    if (!HASH_RE.test(word) || !/\d/.test(word)) continue;
    const start = m.index ?? 0;
    if (overlaps(claims, start, start + word.length)) continue;
    add(start, start + word.length, { kind: "hash", text: word });
  }

  return found.sort((a, b) => a.start - b.start).map((f) => f.target);
}

/** The target that covers cell column `col`, or null. */
export function targetAt(targets: readonly Target[], col: number): Target | null {
  return targets.find((t) => col >= t.startCol && col <= t.endCol) ?? null;
}

/**
 * What a click on `target` asks of the core: a URL is opened, a path is revealed with the pane's working directory
 * as the base (relative paths are resolved by the core), a hash is not a link. A URL that is not http, https or
 * mailto is refused here as well, whatever made the target.
 */
export function linkAction(target: Target, base: string | null): LinkAction | null {
  if (target.kind === "url") {
    return isOpenableUrl(target.text) ? { command: "open_url", args: { url: target.text } } : null;
  }
  if (target.kind === "path") {
    if (target.text === "" || /\p{C}/u.test(target.text)) return null;
    return { command: "reveal_path", args: { path: target.text, base } };
  }
  return null;
}

/** Whether the modifier of a link click is held: Cmd on macOS, Ctrl elsewhere (Ctrl+click is the menu on macOS). */
export function isLinkModifier(e: { metaKey?: boolean; ctrlKey?: boolean }, os: string): boolean {
  return os === "macos" ? e.metaKey === true : e.ctrlKey === true;
}

// ---- the page side ------------------------------------------------------------------------------------

/**
 * Put the link handler on the pointer stack: Cmd+click (Ctrl+click off macOS) on a URL or a path opens or reveals
 * it and starts neither a selection nor a mouse report; with the modifier held the target under the pointer gets an
 * underline and the pointer cursor. Detection runs for the row under the pointer, on a pointer event, never per
 * frame. Call once at start-up, after `initPointer`.
 */
export async function initLinks(): Promise<void> {
  const [{ pushPointerHandler }, { call }, { setCellBoxes }, { canvas }] = await Promise.all([
    import("./pointer.js"),
    import("./bridge.js"),
    import("./overlay.js"),
    import("./scene.js"),
  ]);
  type Hit = { pane: number; cell: [number, number] };

  /** The target under the pointer, when the pointer is inside the cells of its pane's grid (not the header or padding). */
  const under = (e: PointerEvent, hit: Hit | null): { hit: Hit; target: Target } | null => {
    if (!hit) return null;
    const pane = state.view?.panes.find(([id]) => id === hit.pane);
    const mirror = state.frames.get(hit.pane);
    if (!pane || !mirror) return null;
    const m = state.metrics;
    const g = gridRect(m, pane[1], state.headerPx);
    const origin = canvas().getBoundingClientRect();
    const x = e.clientX - origin.left - g.x;
    const y = e.clientY - origin.top - g.y;
    if (x < 0 || y < 0 || x >= mirror.cols * m.cellW || y >= mirror.rows * m.cellH) return null;
    const target = targetAt(findTargets(rowCells(mirror, hit.cell[1])), hit.cell[0]);
    return target && target.kind !== "hash" ? { hit, target } : null;
  };

  const act = (pane: number, target: Target): void => {
    const action = linkAction(target, state.cwds.get(pane) ?? null);
    if (action) void call(action.command, action.args).catch(() => {});
  };

  // What is drawn: an underline over the target and the pointer cursor.
  let shown = "";
  const show = (found: { hit: Hit; target: Target } | null): void => {
    const sig = found ? `${found.hit.pane}:${found.hit.cell[1]}:${found.target.startCol}:${found.target.endCol}` : "";
    if (sig === shown) return;
    shown = sig;
    canvas().classList.toggle("link-hover", found !== null);
    setCellBoxes(
      "links",
      found
        ? [{
            key: "hover", cls: "linkhover", pane: found.hit.pane, y: found.hit.cell[1],
            x0: found.target.startCol, x1: found.target.endCol,
          }]
        : [],
    );
  };

  /** The last pointer event over the canvas, so the underline follows the modifier without a pointer move. */
  let lastEvent: PointerEvent | null = null;
  let lastHit: Hit | null = null;
  let armed: { pointer: number; pane: number; row: number; target: Target } | null = null;

  pushPointerHandler((e, hit) => {
    lastEvent = e;
    lastHit = hit;
    if (e.type === "pointerdown") {
      armed = null;
      if (e.button !== 0 || !isLinkModifier(e, state.os)) return false;
      const found = under(e, hit);
      if (!found || linkAction(found.target, null) === null) return false;
      armed = { pointer: e.pointerId, pane: found.hit.pane, row: found.hit.cell[1], target: found.target };
      return true; // the press is the link's: no selection and no report for the program
    }
    if (e.type === "pointerup") {
      const a = armed;
      if (!a || a.pointer !== e.pointerId) return false;
      armed = null;
      const found = under(e, hit);
      const same = found && found.hit.pane === a.pane && found.hit.cell[1] === a.row && found.target.startCol === a.target.startCol;
      if (same) act(a.pane, a.target);
      return true;
    }
    if (e.type === "pointercancel") {
      armed = null;
      return false;
    }
    show(isLinkModifier(e, state.os) ? under(e, hit) : null);
    return false;
  });

  const follow = (e: KeyboardEvent): void => {
    if (e.key !== "Meta" && e.key !== "Control") return;
    show(isLinkModifier(e, state.os) && lastEvent ? under(lastEvent, lastHit) : null);
  };
  window.addEventListener("keydown", follow);
  window.addEventListener("keyup", follow);
  window.addEventListener("blur", () => show(null));
  canvas().addEventListener("pointerleave", () => {
    lastEvent = null;
    lastHit = null;
    show(null);
  });
}
