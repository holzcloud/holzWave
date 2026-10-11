// Scrollback search (DESK-21): a bar at the top right of the focused pane with an input, a counter, previous and
// next buttons and a close button. The search itself is the core's (D-06): the page sends the needle with `search`
// and shows what the frame header of the focused pane says (`search.total`, `search.current`) and the match boxes
// the overlay draws from `matches`. Nothing about a match is kept here.
//
// The needle goes out once, 150 ms after the last change, whole and trimmed; Enter before that sends it at once.
// While the input has the keyboard it takes every key (the key handler stack): nothing reaches the terminal behind
// it, and the shortcuts that edit text (Cmd+A, C, V, X, Z) stay the field's. Every string is `textContent`.
import { registerAction } from "./actions.js";
import { call } from "./bridge.js";
import { el, loadStyles, setRect, setText } from "./dom.js";
import { paneRect } from "./geometry.js";
import { focusInput } from "./keyboard.js";
import { pushKeyHandler } from "./keystack.js";
import { onOverlayRender } from "./overlay.js";
import { state } from "./state.js";

/** The quiet period after the last change of the needle before it is sent. */
export const SEARCH_QUIET_MS = 150;

/** The core refuses a needle over 1 KiB; the page cuts first so a long paste still searches its start. */
export const MAX_NEEDLE_BYTES = 1024;

interface Bar {
  host: HTMLElement;
  input: HTMLInputElement;
  counter: HTMLElement;
}

let bar: Bar | null = null;
/** The pane the bar is open for, or null while it is closed. */
let boundPane: number | null = null;
/** The needle the core last heard of for the bound pane ("" for none): a send is skipped when nothing changed. */
let sentNeedle = "";
let timer = 0;
let popKeys: (() => void) | null = null;

/** `text` trimmed and cut to at most `MAX_NEEDLE_BYTES` UTF-8 bytes, after whole characters. */
export function clipNeedle(text: string): string {
  const trimmed = text.trim();
  const encoder = new TextEncoder();
  if (encoder.encode(trimmed).length <= MAX_NEEDLE_BYTES) return trimmed;
  let bytes = 0;
  let out = "";
  for (const ch of trimmed) {
    const n = encoder.encode(ch).length;
    if (bytes + n > MAX_NEEDLE_BYTES) break;
    bytes += n;
    out += ch;
  }
  return out.trim();
}

/** The counter text: the place of the current match (1-based) and the total, `0/0` when there is no search. */
export function counterText(search: { total: number; current: number | null } | null | undefined): string {
  const total = search && Number.isInteger(search.total) && search.total > 0 ? search.total : 0;
  const at = search?.current;
  const current = total > 0 && typeof at === "number" && Number.isInteger(at) && at >= 0 && at < total ? at + 1 : 0;
  return `${current}/${total}`;
}

function button(parent: HTMLElement, label: string, text: string, onClick: () => void): HTMLButtonElement {
  const b = el("button", "", parent);
  b.type = "button";
  b.setAttribute("aria-label", label);
  b.textContent = text;
  b.addEventListener("click", onClick);
  return b;
}

function createBar(): Bar {
  const host = el("div", "search-host", document.getElementById("stage") ?? document.body);
  host.hidden = true;
  const box = el("div", "search-bar", host);
  box.setAttribute("role", "search");
  const input = el("input", "", box);
  input.type = "text";
  input.setAttribute("aria-label", "Search scrollback");
  input.setAttribute("autocomplete", "off");
  input.setAttribute("autocorrect", "off");
  input.setAttribute("autocapitalize", "off");
  input.spellcheck = false;
  const counter = el("span", "search-count", box);
  counter.setAttribute("aria-live", "polite");
  counter.textContent = "0/0";

  button(box, "Previous match", "↑", () => step("prev", true));
  button(box, "Next match", "↓", () => step("next", true));
  button(box, "Close search", "×", () => close());

  // Typing is not a request: a change waits for the quiet period. A composition in progress is not a needle yet.
  input.addEventListener("input", (e) => {
    if ((e as InputEvent).isComposing) return;
    schedule();
  });
  input.addEventListener("compositionend", () => schedule());
  return { host, input, counter };
}

function schedule(): void {
  window.clearTimeout(timer);
  timer = window.setTimeout(sendNeedle, SEARCH_QUIET_MS);
}

/** Send the input's needle for the bound pane when the core has not heard it yet; an empty one is `clear`. */
function sendNeedle(): void {
  window.clearTimeout(timer);
  timer = 0;
  const pane = boundPane;
  if (pane === null || !bar) return;
  const needle = clipNeedle(bar.input.value);
  if (needle === sentNeedle) return;
  sentNeedle = needle;
  const op = needle === "" ? { kind: "clear" } : { kind: "set", needle };
  void call("search", { pane, op }).catch(() => {});
}

/** Whether the input holds a needle the core has not heard yet. */
function unsent(): boolean {
  return bar !== null && clipNeedle(bar.input.value) !== sentNeedle;
}

/** Move to the next (down in reading order) or previous match of the bound pane, or of the focused one. */
function step(kind: "next" | "prev", fromBar = false): void {
  if (unsent()) {
    sendNeedle(); // the needle first: next of a search the core does not have is nothing
  } else {
    const pane = boundPane ?? state.view?.focus;
    if (pane !== undefined) void call("search", { pane, op: { kind } }).catch(() => {});
  }
  if (fromBar) bar?.input.focus({ preventScroll: true });
}

/** Show the bar for the focused pane (moving it when it was open for another) and give it the keyboard. */
function open(): void {
  const view = state.view;
  if (!view) return;
  bar ??= createBar();
  const pane = view.focus;
  if (boundPane !== null && boundPane !== pane) {
    // The old pane's search ends with its bar; the new pane gets the needle that is in the field.
    void call("search", { pane: boundPane, op: { kind: "clear" } }).catch(() => {});
    sentNeedle = "";
  }
  boundPane = pane;
  bar.host.hidden = false;
  popKeys ??= pushKeyHandler(onKey);
  renderSearch();
  bar.input.focus({ preventScroll: true });
  bar.input.select();
  sendNeedle();
}

/** Clear the search, hide the bar and give the keyboard back to the terminal. */
function close(): void {
  if (boundPane === null || !bar) return;
  window.clearTimeout(timer);
  timer = 0;
  const pane = boundPane;
  boundPane = null;
  sentNeedle = "";
  bar.host.hidden = true;
  popKeys?.();
  popKeys = null;
  void call("search", { pane, op: { kind: "clear" } }).catch(() => {});
  focusInput();
}

/** Shortcuts that edit the text of a field: with the input focused they are the field's, not an action's. */
const FIELD_CHORDS = new Set(["a", "c", "v", "x", "z"]);

function onKey(e: KeyboardEvent): boolean {
  // The terminal has the keyboard again (the person clicked into it): its keys are its own.
  if (!bar || document.activeElement !== bar.input) return false;
  if (e.metaKey) return FIELD_CHORDS.has(e.key.toLowerCase());
  if (e.key === "Escape") {
    e.preventDefault();
    close();
    return true;
  }
  if (e.key === "Enter") {
    e.preventDefault();
    step(e.shiftKey ? "prev" : "next");
    return true;
  }
  return true; // typing: the field takes the character, nothing behind it sees the key
}

/** Follow the view: place the bar over the top right of its pane, show the counter, close when its pane is gone. */
function renderSearch(): void {
  if (!bar || boundPane === null) return;
  const view = state.view;
  const entry = view?.panes.find(([id]) => id === boundPane);
  if (!view || !entry || view.focus !== boundPane) {
    close(); // the focus moved on or the pane left the layout: the search belongs to the focused pane
    return;
  }
  const r = paneRect(state.metrics, entry[1]);
  const top = r.y + state.headerPx;
  setRect(bar.host, { x: r.x, y: top, w: r.w, h: Math.max(0, r.h - state.headerPx) });
  setText(bar.counter, counterText(state.frames.get(boundPane)?.search));
}

/** Register the actions and the stylesheet. Call once at start-up. */
export async function initSearch(): Promise<void> {
  registerAction({ id: "search.open", title: "Search scrollback", run: open });
  registerAction({ id: "search.next", title: "Next search match", run: () => step("next") });
  registerAction({ id: "search.prev", title: "Previous search match", run: () => step("prev") });
  onOverlayRender(renderSearch);
  try {
    await loadStyles("./search.css");
  } catch {
    // the bar still works without its stylesheet
  }
}
