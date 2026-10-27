// The browser build's view of its own session (docs/design.md 6.30): the page asks once at start what the session is, and
// then follows the events of the server and the state of the connection. What it learns shows in the status line as marks
// (a glyph and a word, never colour alone), and the one action a mark has is a real button.
//
// Nothing here starts on the desktop: `initWebSession` returns at once there, makes no request and registers no action.
// The only request function is `http` of the bridge. A failed first lookup leaves the page as it is: no mark, no retry and no text.
import { registerAction, unregisterAction } from "./actions.js";
import { backend, http, onSession } from "./bridge.js";
import { el, loadStyles } from "./dom.js";
import { fromInfo, marks, newModel, readSessionInfo, reduce } from "./websessionstate.js";
import type { Account, Mark, Model } from "./websessionstate.js";

let model: Model = newModel();
let account: Account | null = null;
let started = false;
let changed: () => void = () => {};
let drawn = "";
/** True once an event has told the state of the secret store: an older answer of the first lookup must not undo it. */
let storeFromEvent = false;
let unlockOpen = false;

/** The account of the last answer of `GET /api/session`, or null (none yet, or the desktop). The people dialog reads the role from it. */
export function sessionAccount(): Account | null {
  return account;
}

/** Ask the status line to draw again. */
const redraw = (): void => changed();

/** Take a new model: keep the actions in line with it and redraw when something shows differently. */
function setModel(next: Model): void {
  model = next;
  syncActions();
  redraw();
}

/** The palette entry `Unlock…` exists while the store is locked and is withdrawn when it is not. */
function syncActions(): void {
  const want = model.store === "locked";
  if (want && !unlockOpen) {
    registerAction({ id: "unlock.open", title: "Unlock…", run: () => void openUnlockDialog() });
    unlockOpen = true;
  } else if (!want && unlockOpen) {
    unregisterAction("unlock.open");
    unlockOpen = false;
  }
}

async function openUnlockDialog(): Promise<void> {
  const { openUnlock } = await import("./unlock.js");
  await openUnlock(() => setModel(reduce(model, { state: "unlocked" })));
}

/** The text button of a mark was pressed. */
function act(action: NonNullable<Mark["action"]>): void {
  if (action === "unlock") void openUnlockDialog();
}

/** Fill the `web` segment of the status line from the marks; nothing is drawn when there are none, and nothing is touched when they did not change. */
export function renderWeb(segment: HTMLElement): void {
  if (!started) return;
  const list = marks(model);
  const key = JSON.stringify(list);
  if (key === drawn) return;
  drawn = key;
  segment.textContent = "";
  for (const mark of list) {
    const node = el("span", "web-mark", segment);
    node.dataset.mark = mark.id;
    node.dataset.tone = mark.tone;
    const glyph = el("span", "web-glyph", node);
    glyph.setAttribute("aria-hidden", "true");
    glyph.dataset.glyph = mark.glyph;
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
 * Start on the web backend: subscribe to the session events and ask `GET /api/session` once. `onChange` draws the status line
 * again. The promise never rejects and resolves when the first lookup has ended, with any result. On the desktop it is
 * resolved at once and nothing starts.
 */
export function initWebSession(onChange: () => void): Promise<void> {
  if (backend !== "web" || started) return Promise.resolve();
  started = true;
  changed = onChange;
  // The rules of the marks; a sheet that cannot load leaves the marks as plain text.
  void loadStyles("./websession.css").catch(() => {});
  onSession((event) => {
    const next = reduce(model, event);
    if (next.store !== model.store) storeFromEvent = true;
    setModel(next);
  });
  return http("GET", "/api/session").then(
    (raw) => {
      const info = readSessionInfo(raw);
      if (info === null) return;
      account = info.account;
      const next = fromInfo(model, info);
      setModel(storeFromEvent ? { ...next, store: model.store } : next);
    },
    () => {
      // A failed lookup leaves the page as it is.
    },
  );
}
