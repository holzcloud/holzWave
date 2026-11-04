// Clickable targets in the visible output: URLs, file paths and hashes, found in plain text, and the links a program
// marked itself (OSC 8, docs/design.md 6.31). Detection is a pure function over one row given as one string per cell
// column, so node imports this file for the unit tests; the DOM and the core are only reached from `initLinks`, by
// dynamic import.
//
// What the terminal prints decides what looks clickable (T-02-75), so the rules are narrow: a URL has the scheme
// http, https or mailto and no control character, a path is only ever revealed (never opened or run), and the page
// sends the matched text as it is; the core checks scheme, length and characters again. A marked link is only a link
// when its scheme is in the person's list (and, in the browser, one of the three web schemes; `file` only on the desktop
// in a pane with no connection), and a click on one whose text names another place than its target asks first.
import { paneConns } from "./connstate.js";
import { INVERSE } from "./frame.js";
import { rowCells } from "./frames.js";
import type { PaneMirror } from "./frames.js";
import { gridRect } from "./geometry.js";
import { DEFAULT_LINK_SCHEMES } from "./settings-model.js";
import { state } from "./state.js";
import { LINK_FEATURES, LINK_TARGET, webLinkModifier } from "./webstate.js";

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
  /** `osc8` for a link the program marked; absent for a target found in the text. */
  source?: "osc8";
  /** For a marked link: the text the person sees over its cells (blanks as the grid has them, spacers dropped). */
  shown?: string;
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

// ---- links the program marked (OSC 8) -------------------------------------------------------------------

/** The schemes no list can make a link: they run code in the page that shows them (the core refuses them as well). */
const NEVER_SCHEMES: ReadonlySet<string> = new Set(["javascript", "data", "vbscript", "blob", "about"]);
/** The schemes a browser page opens whatever the list says. */
const WEB_SCHEMES: ReadonlySet<string> = new Set(["http", "https", "mailto"]);
const SCHEME_OF = /^([A-Za-z][A-Za-z0-9+.-]*):/;

/** What decides whether a marked link is a link on this page. */
export interface LinkPolicy {
  /** The person's list (lower case). */
  schemes: readonly string[];
  /** The browser build: only the web schemes, no `file`. */
  web: boolean;
  /** The pane has a connection profile (SSH, WSL, Docker): a `file` link would name a file on another machine. */
  remote: boolean;
}

/**
 * Whether `uri`, the address of a marked link, is a link here: a scheme that is in the list, that no list can enable,
 * that the browser opens (in the browser build), and for `file` the desktop and a pane that is on this computer. An
 * address with a blank or a control character, or longer than the core cleans to, is none. The opener asks again.
 */
export function linkAllowed(uri: string, policy: LinkPolicy): boolean {
  if (uri.length === 0 || uri.length > MAX_URL || /[\s\p{C}]/u.test(uri)) return false;
  const m = SCHEME_OF.exec(uri);
  if (!m) return false;
  const scheme = (m[1] as string).toLowerCase();
  if (NEVER_SCHEMES.has(scheme) || !policy.schemes.includes(scheme)) return false;
  if (policy.web && !WEB_SCHEMES.has(scheme)) return false;
  if (scheme === "file" && (policy.web || policy.remote)) return false;
  const body = uri.slice((m[0] as string).length);
  if (scheme === "http" || scheme === "https") {
    if (!/^\/\/[^/?#]/.test(body)) return false;
  } else if (scheme === "mailto") {
    if (!/^[^@]+@[^@]+$/.test(body)) return false;
  }
  if (policy.web && !isOpenableUrl(uri)) return false;
  return true;
}

/**
 * The marked links of row `y` of a mirror as targets, in column order, of which `allow` accepts the address. Each carries
 * the text over its cells. A span the mirror does not hold (it keeps only valid ones), a row nobody sent and a row
 * without links give none.
 */
export function osc8Targets(mirror: PaneMirror, y: number, allow: (uri: string) => boolean = () => true): Target[] {
  const spans = Number.isInteger(y) ? mirror.rows_[y]?.links : undefined;
  if (!spans || spans.length === 0) return [];
  let cells: string[] | null = null;
  const out: Target[] = [];
  for (const [x0, x1, uri] of spans) {
    if (!allow(uri)) continue;
    cells ??= rowCells(mirror, y);
    out.push({
      kind: "url", startCol: x0, endCol: x1, text: uri, source: "osc8",
      shown: cells.slice(x0, x1 + 1).join(""),
    });
  }
  return out.sort((a, b) => a.startCol - b.startCol);
}

/**
 * The targets of one row with the marked links ahead of the guessed ones, each group in column order: a guessed target
 * that overlaps a marked one is dropped, so a stretch is one link, and the one the program marked wins.
 */
export function mergeTargets(marked: readonly Target[], guessed: readonly Target[]): Target[] {
  if (marked.length === 0) return [...guessed];
  const kept = guessed.filter((g) => !marked.some((m) => g.startCol <= m.endCol && g.endCol >= m.startCol));
  return [...marked, ...kept];
}

/**
 * The colour the cell (`x`, `y`) of a mirror is drawn in (the foreground, or the background where the run is inverse), or the
 * mirror's foreground when the row does not say. Used for the line under a link the pointer is over.
 */
export function cellInk(mirror: PaneMirror, y: number, x: number): number {
  const row = Number.isInteger(y) ? mirror.rows_[y] : null;
  let col = 0;
  for (const run of row?.runs ?? []) {
    if (!Array.isArray(run)) continue;
    const [, cols, fg, bg, flags] = run;
    if (x < col + cols) return (flags & INVERSE ? bg : fg) & 0xffffff;
    col += cols;
  }
  return mirror.fg & 0xffffff;
}

const MAX_SHOWN = 120;

/** The visible text of a link cut for the question: at most 120 code points, then `…`. */
export function cutShown(text: string, max = MAX_SHOWN): string {
  const points = Array.from(text);
  return points.length > max ? `${points.slice(0, max).join("")}…` : text;
}

/** A host or a mail domain in lower case; the rest of the address as it is. */
function lowerHost(rest: string, mail: boolean): string {
  if (mail) {
    const at = rest.lastIndexOf("@");
    return at < 0 ? rest.toLowerCase() : rest.slice(0, at + 1) + rest.slice(at + 1).toLowerCase();
  }
  const end = rest.search(/[/?#]/);
  return end < 0 ? rest.toLowerCase() : rest.slice(0, end).toLowerCase() + rest.slice(end);
}

/** `text` or an address in the form that is compared: no spacers, trimmed, no leading `http://`, `https://` or `mailto:`, no trailing `/`, the host in lower case. */
function comparable(text: string): string {
  let t = text.replace(/\0/g, "").trim();
  const lead = /^(?:https?:\/\/|mailto:)/i.exec(t);
  if (lead) t = t.slice(lead[0].length);
  if (t.endsWith("/")) t = t.slice(0, -1);
  // A mail address: only the domain is lower-cased, the part before the @ is the person's.
  const mail = lead !== null ? lead[0].toLowerCase() === "mailto:" : /^[^/?#:@\s]+@[^/?#@\s]+$/.test(t);
  return lowerHost(t, mail);
}

/**
 * Whether the visible text of a link and its target say different things, so that a click has to ask first. They are
 * equal after trimming blanks, dropping a leading `http://`, `https://` or `mailto:` and one trailing `/`, and lower-casing
 * the host. A blank text differs from every target, and so does a text with another host or another path.
 */
export function linkTextDiffers(shown: string, target: string): boolean {
  const a = comparable(shown);
  return a === "" || a !== comparable(target);
}

/**
 * The target cut in the middle for a segment of `max` characters: the scheme and the host stay, then as much of the start
 * of the path as fits, `…`, and the last 12 characters. A target that fits, or that is not longer than the cut itself,
 * comes back whole. `max` is counted in code points.
 */
export function cutMiddle(target: string, max: number): string {
  const points = Array.from(target);
  const TAIL = 12;
  if (points.length <= max) return target;
  const head = Array.from(/^[A-Za-z][A-Za-z0-9+.-]*:(?:\/\/)?[^/?#]*/.exec(target)?.[0] ?? "").length;
  const keep = Math.max(head, max - 1 - TAIL);
  if (keep + 1 + TAIL >= points.length) return target;
  return `${points.slice(0, keep).join("")}…${points.slice(points.length - TAIL).join("")}`;
}

/** The part of a target that is its scheme and host (what the status line shows in the strong ink); the rest follows. */
export function hostPartLength(target: string): number {
  return /^[A-Za-z][A-Za-z0-9+.-]*:(?:\/\/)?[^/?#…]*/.exec(target)?.[0]?.length ?? 0;
}

/**
 * What a click on `target` asks of the core: a URL is opened, a path is revealed with the pane's working directory
 * as the base (relative paths are resolved by the core), a hash is not a link. A URL that is not http, https or
 * mailto is refused here as well, whatever made the target. Where nothing can be revealed (`canReveal` false: the
 * browser build cannot show a folder) a path is no link either.
 */
export function linkAction(target: Target, base: string | null, canReveal = true): LinkAction | null {
  if (target.kind === "url") {
    return isOpenableUrl(target.text) ? { command: "open_url", args: { url: target.text } } : null;
  }
  if (target.kind === "path") {
    if (!canReveal || target.text === "" || /\p{C}/u.test(target.text)) return null;
    return { command: "reveal_path", args: { path: target.text, base } };
  }
  return null;
}

/**
 * Whether the modifier of a link click is held: Cmd on macOS, Ctrl elsewhere (Ctrl+click is the menu on macOS). In the
 * browser build (`web`) either of the two.
 */
export function isLinkModifier(e: { metaKey?: boolean; ctrlKey?: boolean }, os: string): boolean {
  if (os === "web") return webLinkModifier(e);
  return os === "macos" ? e.metaKey === true : e.ctrlKey === true;
}

/** Whether this page can show a folder: not the browser build. */
export const canRevealPaths = (): boolean => state.os !== "web";

/**
 * Open `url` the way the platform opens a link. On the desktop `desktop` is called, as before: the caller keeps its own
 * `open_url` call. In the browser build the address is checked again (http, https or mailto, no blank or control
 * character) and opened in a new tab that cannot reach this page; a mail address goes through an anchor without a target
 * so that the page keeps its tab. It must be called from the gesture that is the person's act, and never waits for
 * anything first: a browser lets a tab open only from the gesture itself.
 */
export function openUrl(url: string, desktop: () => void): void {
  if (state.os !== "web") {
    desktop();
    return;
  }
  if (!isOpenableUrl(url)) return;
  if (url.startsWith("mailto:")) {
    const a = document.createElement("a");
    a.href = url;
    a.rel = "noopener noreferrer";
    a.hidden = true;
    document.body.append(a);
    a.click();
    a.remove();
    return;
  }
  window.open(url, LINK_TARGET, LINK_FEATURES);
}

// ---- opening ---------------------------------------------------------------------------------------------------

/** What opening a link needs from the page: the core's command and the question. Set once by `linkDeps` (the DOM is not touched before). */
interface LinkDeps {
  call: (cmd: string, args: Record<string, unknown>) => Promise<unknown>;
  /** The question `Open this link?`; `onOpen` is called synchronously from the click or key that confirms. Null: no question can be asked. */
  ask: ((shown: string, target: string, onOpen: () => void) => void) | null;
}

let deps: LinkDeps | null = null;

/** Load what opening a link needs (the bridge and the dialog) once; `activateLink` does nothing before this has run. */
export async function linkDeps(): Promise<void> {
  if (deps) return;
  const { call } = await import("./bridge.js");
  // The dialog's module is fetched at the first question, not at start-up: nothing new is loaded while the page starts.
  deps = { call, ask: (shown, target, onOpen) => void import("./linkask.js").then((m) => m.askOpenLink(shown, target, onOpen)).catch(() => {}) };
}

/** What decides whether a marked link in `pane` is a link: the person's list, the build and the pane's connection. */
export function pagePolicy(pane: number): LinkPolicy {
  return {
    schemes: state.settings?.settings.link_schemes ?? DEFAULT_LINK_SCHEMES,
    web: state.os === "web",
    remote: paneConns.has(pane),
  };
}

/**
 * Do what a click (or a chosen hint) on `target` of `pane` means. A text target keeps its commands: a URL is opened, a path
 * is revealed. A link the program marked is checked again against the list, and when its text and its address say different
 * things the person is asked first; the opening runs from the click that confirms (a browser lets a tab open only from the
 * gesture itself). It opens with `open_url` and the pane on the desktop, and in a new tab without access to the page in the browser.
 */
export function activateLink(pane: number, target: Target): void {
  const d = deps;
  if (!d) return;
  if (target.source !== "osc8") {
    const action = linkAction(target, state.cwds.get(pane) ?? null, canRevealPaths());
    if (action?.command === "open_url") {
      const { url } = action.args;
      openUrl(url, () => void d.call("open_url", { url }).catch(() => {}));
    } else if (action) void d.call(action.command, action.args).catch(() => {});
    return;
  }
  const url = target.text;
  if (!linkAllowed(url, pagePolicy(pane))) return;
  const open = (): void => openUrl(url, () => void d.call("open_url", { url, pane }).catch(() => {}));
  if (!linkTextDiffers(target.shown ?? "", url)) {
    open();
    return;
  }
  // The text may be anything; with no way to ask, nothing opens.
  d.ask?.(target.shown ?? "", url, open);
}

// ---- the page side ------------------------------------------------------------------------------------

/**
 * Put the link handler on the pointer stack: Cmd+click (Ctrl+click off macOS) on a URL, a path or a link the program
 * marked opens or reveals it and starts neither a selection nor a mouse report; with the modifier held the target under the
 * pointer gets an underline and the pointer cursor. Detection runs for the row under the pointer, on a pointer event, never per
 * frame. Call once at start-up, after `initPointer`.
 */
export async function initLinks(): Promise<void> {
  const [{ pushPointerHandler }, { setCellBoxes }, { canvas }, { setLinkTarget }] = await Promise.all([
    import("./pointer.js"),
    import("./overlay.js"),
    import("./scene.js"),
    import("./status.js"),
    linkDeps(),
  ]);
  type Hit = { pane: number; cell: [number, number] };
  type Found = { hit: Hit; target: Target };

  /**
   * The target under the pointer, when the pointer is inside the cells of its pane's grid (not the header or padding).
   * A link the program marked comes first, and where it overlaps a guessed target it is the one found. A target
   * found in the text is only looked for when `guessed` (the gesture is held).
   */
  const under = (e: PointerEvent, hit: Hit | null, guessed: boolean): Found | null => {
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
    const policy = pagePolicy(hit.pane);
    const marked = targetAt(osc8Targets(mirror, hit.cell[1], (uri) => linkAllowed(uri, policy)), hit.cell[0]);
    if (marked) return { hit, target: marked };
    if (!guessed) return null; // without the gesture only a marked link shows anything: no detection work per pointer move
    const target = targetAt(findTargets(rowCells(mirror, hit.cell[1])), hit.cell[0]);
    return target && target.kind !== "hash" && linkAction(target, null, canRevealPaths()) !== null ? { hit, target } : null;
  };

  // What is drawn: an underline over the target and, with the gesture held, the pointer cursor. A link the program marked
  // is underlined solid under the pointer without the gesture, with its address in the status line; a printed address
  // only shows its underline once the gesture is held, as it always did.
  let shown = "";
  const show = (found: Found | null, gesture: boolean): void => {
    const marked = found?.target.source === "osc8";
    const on = found !== null && (gesture || marked);
    const sig = on ? `${found.hit.pane}:${found.hit.cell[1]}:${found.target.startCol}:${found.target.endCol}:${gesture}` : "";
    if (sig === shown) return;
    shown = sig;
    canvas().classList.toggle("link-hover", on && gesture);
    setLinkTarget(on && marked ? found.target.text : null);
    setCellBoxes(
      "links",
      on
        ? [{
            key: "hover", cls: gesture ? "linkhover" : "linkon", pane: found.hit.pane, y: found.hit.cell[1],
            x0: found.target.startCol, x1: found.target.endCol,
            ...(gesture ? {} : { fg: cellInk(state.frames.get(found.hit.pane) as PaneMirror, found.hit.cell[1], found.target.startCol) }),
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
      const found = under(e, hit, true);
      if (!found) return false;
      armed = { pointer: e.pointerId, pane: found.hit.pane, row: found.hit.cell[1], target: found.target };
      return true; // the press is the link's: no selection and no report for the program
    }
    if (e.type === "pointerup") {
      const a = armed;
      if (!a || a.pointer !== e.pointerId) return false;
      armed = null;
      const found = under(e, hit, true);
      const same = found && found.hit.pane === a.pane && found.hit.cell[1] === a.row && found.target.startCol === a.target.startCol;
      if (same) activateLink(a.pane, a.target);
      return true;
    }
    if (e.type === "pointercancel") {
      armed = null;
      return false;
    }
    const gesture = isLinkModifier(e, state.os);
    show(under(e, hit, gesture), gesture);
    return false;
  });

  const follow = (e: KeyboardEvent): void => {
    if (e.key !== "Meta" && e.key !== "Control") return;
    const gesture = isLinkModifier(e, state.os);
    show(lastEvent ? under(lastEvent, lastHit, gesture) : null, gesture);
  };
  window.addEventListener("keydown", follow);
  window.addEventListener("keyup", follow);
  window.addEventListener("blur", () => show(null, false));
  canvas().addEventListener("pointerleave", () => {
    lastEvent = null;
    lastHit = null;
    show(null, false);
  });
}
