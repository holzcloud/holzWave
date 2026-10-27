// The browser build's view of its own session (docs/design.md 6.30): the page asks once at start what the session is, and
// then follows the events of the server and the state of the connection. What it learns shows in the status line as marks
// (a glyph and a word, never colour alone), and the one action a mark has is a real button. A session that ended is announced
// with a layer over the window; the page never navigates by itself.
//
// Nothing here starts on the desktop: `initWebSession` returns at once there, makes no request, registers no action and
// listens to nothing. The only request function is `http` of the bridge, and the only imports from it are `http`,
// `onConnection`, `onSession` and `backend`. A failed first lookup leaves the page as it is: no mark, no retry and no text.
import { registerAction, unregisterAction } from "./actions.js";
import { backend, http, onConnection, onSession } from "./bridge.js";
import { el, loadStyles } from "./dom.js";
import { connection, fromInfo, marks, newModel, nextChange, readSessionInfo, reduce, setOffline } from "./websessionstate.js";
import type { Account, Mark, Model } from "./websessionstate.js";
import { openLayer } from "./weblayer.js";

/** The sentence of the layer for a session that ended (the sign-in page says it too). */
export const ENDED_SENTENCE = "Your session ended. Sign in again.";
/** Where the button of that layer goes: the sign-in page, which says the sentence above. */
export const SIGN_IN_ADDRESS = "/?ended=1";

let model: Model = newModel();
let account: Account | null = null;
let started = false;
let changed: () => void = () => {};
let drawn = "";
let drawnIds: string[] = [];
/** True once an event has told the state of the secret store: an older answer of the first lookup must not undo it. */
let storeFromEvent = false;
let unlockActionOn = false;
let endedShown = false;
let timer: ReturnType<typeof setTimeout> | undefined;
let unlockApi: typeof import("./unlock.js") | null = null;

/** The account of the last answer of `GET /api/session`, or null (none yet, or the desktop). The people dialog reads the role from it. */
export function sessionAccount(): Account | null {
  return account;
}

/** Whether the session is over, as far as the page knows. */
export function sessionEnded(): boolean {
  return model.ended;
}

/** One timer, armed from the time of the next change: the end of the one second of a loss, the end of a minute of the idle mark. */
function arm(): void {
  clearTimeout(timer);
  timer = undefined;
  const wait = nextChange(model, Date.now());
  if (wait === null) return;
  timer = setTimeout(() => {
    timer = undefined;
    changed();
    arm();
  }, wait);
}

/** Take a new model: keep the actions and the timer in line with it and redraw when something shows differently. */
function setModel(next: Model): void {
  const was = model;
  model = next;
  syncActions();
  arm();
  changed();
  if (next.ended && !was.ended) void showEnded();
}

/** The palette entry `Unlock…` exists while the store is locked and is withdrawn when it is not. */
function syncActions(): void {
  const want = model.store === "locked" && !model.ended;
  if (want && !unlockActionOn) {
    registerAction({ id: "unlock.open", title: "Unlock…", run: () => void openUnlockDialog() });
    unlockActionOn = true;
  } else if (!want && unlockActionOn) {
    unregisterAction("unlock.open");
    unlockActionOn = false;
  }
}

async function openUnlockDialog(): Promise<void> {
  unlockApi ??= await import("./unlock.js");
  await unlockApi.openUnlock(() => setModel(reduce(model, { state: "unlocked" }, Date.now())));
}

/** The text button of a mark was pressed. */
function act(action: NonNullable<Mark["action"]>): void {
  if (action === "unlock") void openUnlockDialog();
}

/**
 * The layer of a session that ended: the sentence and one button that has the focus. The terminal output stays readable
 * behind it and the page does not leave by itself; the button replaces the location with the sign-in page.
 */
async function showEnded(): Promise<void> {
  if (endedShown) return;
  endedShown = true;
  const layer = await openLayer({
    cls: "web-ended",
    labelledBy: "web-ended-text",
    alert: true,
    returnTo: null,
    sheets: ["./websession.css"],
  });
  const text = el("p", "web-lead", layer.box);
  text.id = "web-ended-text";
  text.textContent = ENDED_SENTENCE;
  const actions = el("div", "dlg-actions", layer.box);
  const go = el("button", "web-btn web-solid", actions);
  go.type = "button";
  go.textContent = "Sign in";
  go.addEventListener("click", () => location.replace(SIGN_IN_ADDRESS));
  go.focus();
}

/** Fill the `web` segment of the status line from the marks; nothing is drawn when there are none, and nothing is touched when they did not change. */
export function renderWeb(segment: HTMLElement): void {
  if (!started) return;
  const list = marks(model, Date.now());
  const key = JSON.stringify(list);
  if (key === drawn) return;
  drawn = key;
  const before = drawnIds;
  drawnIds = list.map((m) => m.id);
  segment.textContent = "";
  for (const mark of list) {
    const node = el("span", "web-mark", segment);
    node.dataset.mark = mark.id;
    node.dataset.tone = mark.tone;
    // The idle mark is announced when it first appears and again at its last minute, not every minute.
    if (mark.id === "expiring") {
      const first = !before.includes("expiring");
      node.setAttribute("aria-live", first || mark.word.endsWith(" 1 min") ? "polite" : "off");
    }
    const glyph = el("span", "web-glyph", node);
    glyph.setAttribute("aria-hidden", "true");
    glyph.dataset.glyph = mark.glyph === "lock" ? "lock" : "char";
    if (mark.glyph !== "lock") glyph.textContent = mark.glyph;
    el("span", "web-word", node).textContent = mark.word;
    if (mark.action === "unlock") {
      const button = el("button", "web-act", node);
      button.type = "button";
      button.textContent = "Unlock";
      button.addEventListener("click", () => act("unlock"));
    }
  }
}

/**
 * Start on the web backend: subscribe to the session events, the connection and the browser's network state, and ask
 * `GET /api/session` once. `onChange` draws the status line again. The promise never rejects and resolves when the first
 * lookup has ended, with any result. On the desktop it is resolved at once and nothing starts.
 */
export function initWebSession(onChange: () => void): Promise<void> {
  if (backend !== "web" || started) return Promise.resolve();
  started = true;
  changed = onChange;
  // The rules of the marks; a sheet that cannot load leaves the marks as plain text.
  void loadStyles("./websession.css").catch(() => {});
  onSession((event) => {
    const next = reduce(model, event, Date.now());
    if (next.store !== model.store) storeFromEvent = true;
    setModel(next);
    // The secrets were unlocked elsewhere (another tab): an open dialog has nothing left to ask.
    if (next.store === "unlocked") unlockApi?.closeUnlock();
  });
  onConnection((info) => setModel(connection(model, info, Date.now())));
  window.addEventListener("offline", () => setModel(setOffline(model, true)));
  window.addEventListener("online", () => setModel(setOffline(model, false)));
  if (navigator.onLine === false) setModel(setOffline(model, true));
  return http("GET", "/api/session").then(
    (raw) => {
      const info = readSessionInfo(raw);
      if (info === null) return;
      account = info.account;
      const next = fromInfo(model, info, Date.now());
      setModel(storeFromEvent ? { ...next, store: model.store } : next);
    },
    () => {
      // A failed lookup leaves the page as it is.
    },
  );
}
