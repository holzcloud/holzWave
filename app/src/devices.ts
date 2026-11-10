// The device features of the web page (docs/design.md 6.35): the dialog `Authorize device`, where a person decides whether an app
// that asked to sign in may have a device of their own. Every word is a constant of devicestate.ts, which is the record's table
// in code.
//
// Nothing here is secret except what it is told never to keep: the code of a request is a request for approval and not a credential,
// it stands in one element of the dialog and in the body of two requests, and it is kept nowhere else (no storage, no address, no
// log line). A token never reaches the page at all: the lists carry none. Every change goes through `withStepUp`, so a refusal that
// asks for a recent sign-in shows `Confirm it is you` once and the change is repeated once. Every string the server sends is put in
// with `textContent`; the only request function is `http` of the bridge.
import { http } from "./bridge.js";
import {
  APPROVE,
  APPROVE_SENTENCE,
  APPROVED,
  AUTHORIZE_TITLE,
  CLOSE,
  CODE_LABEL,
  DENIED,
  DENY,
  NOT_VALID,
  ROW_ADDRESS,
  ROW_DEVICE,
  ROW_PLATFORM,
  ROW_REQUESTED,
  codeMessage,
  normaliseDeviceCode,
  readLookup,
  requestedText,
} from "./devicestate.js";
import type { Lookup } from "./devicestate.js";
import { el } from "./dom.js";
import { StepUpCancelled, withStepUp } from "./stepup.js";
import { openLayer } from "./weblayer.js";
import { failureOf } from "./websessionstate.js";

/** The style sheets of every layer of this file: the web dialogs' own and the rules of this file. */
const SHEETS = ["./websession.css", "./devices.css"];

/** The outcome of a change: it was made, the person cancelled the confirmation, or the server refused it. */
export type Changed<T> = { ok: true; value: T } | { ok: false; cancelled: true } | { ok: false; cancelled: false; status: number; body?: Record<string, unknown> };

/** Run a change through the step-up rule and say how it ended; it never throws. */
export async function change<T>(run: () => Promise<T>): Promise<Changed<T>> {
  try {
    return { ok: true, value: await withStepUp(run) };
  } catch (e) {
    if (e instanceof StepUpCancelled) return { ok: false, cancelled: true };
    return { ok: false, cancelled: false, ...failureOf(e) };
  }
}

// ---- the dialog `Authorize device` ------------------------------------------------------------------------------------------

/** The dialog or the small layer that is on screen: there is one at a time. */
let shown: { end: () => void } | null = null;

/** Whether one of the layers of this file is open. */
export function authorizeOpen(): boolean {
  return shown !== null;
}

/** What the status line of the dialog says: its words, and the second cue besides colour. */
type Tone = "approved" | "denied" | "bad";

const GLYPH: Record<Tone, string> = { approved: "●", denied: "○", bad: "!" };

/**
 * Open the dialog for a code that is well formed (`normaliseDeviceCode` made it): the lookup comes first, with a recent sign-in, and
 * the dialog shows who asks and from where; `Approve` and `Deny` decide it. A code that is not valid says so and shows nothing else.
 * A second call while a dialog is open does nothing.
 */
export async function openAuthorize(code: string): Promise<void> {
  if (shown !== null) return;
  const clean = normaliseDeviceCode(code);
  if (clean === null) return;
  let closed = false;
  let working = false;
  const layer = await openLayer({
    cls: "device-authorize",
    labelledBy: "dev-title",
    returnTo: null,
    sheets: SHEETS,
    onEscape: () => end(),
  });
  if (shown !== null) {
    layer.close();
    return;
  }
  function end(): void {
    if (closed) return;
    closed = true;
    shown = null;
    layer.close();
  }
  shown = { end };
  const box = layer.box;
  box.setAttribute("aria-busy", "true");
  const title = el("h2", "set-title dev-title", box);
  title.id = "dev-title";
  title.textContent = AUTHORIZE_TITLE;
  // The heading takes the first focus: nothing that decides is pre-selected, and the dialog is read from its name.
  title.tabIndex = -1;
  const body = el("div", "dev-body", box);
  const status = el("p", "set-status dev-status", box);
  status.setAttribute("role", "status");
  const actions = el("div", "dlg-actions", box);
  title.focus();

  const say = (tone: Tone | null, text: string): void => {
    status.textContent = "";
    if (tone === null) {
      status.textContent = text;
      delete status.dataset.tone;
      return;
    }
    status.dataset.tone = tone;
    const glyph = el("span", "dev-glyph", status);
    glyph.textContent = GLYPH[tone];
    glyph.setAttribute("aria-hidden", "true");
    el("span", "dev-word", status).textContent = text;
  };

  /** After a result the two buttons give way to `Close`. */
  const finish = (): void => {
    actions.textContent = "";
    const close = el("button", "web-btn", actions);
    close.type = "button";
    close.textContent = CLOSE;
    close.addEventListener("click", end);
    close.focus();
  };

  const draw = (who: Lookup): void => {
    const now = Date.now();
    const rows = el("dl", "dev-kv", body);
    const pairs: Array<[string, string, boolean]> = [
      [ROW_DEVICE, who.name, false],
      [ROW_PLATFORM, who.platform, false],
      [ROW_ADDRESS, who.addr, true],
      [ROW_REQUESTED, requestedText(who.requestedMs, now), false],
    ];
    for (const [label, value, mono] of pairs) {
      el("dt", "dev-k", rows).textContent = label;
      el("dd", mono ? "dev-v dev-mono" : "dev-v", rows).textContent = value;
    }
    const numbers = el("div", "dev-code", body);
    numbers.setAttribute("role", "group");
    numbers.setAttribute("aria-label", CODE_LABEL);
    numbers.textContent = clean;
    el("p", "dev-sentence", body).textContent = APPROVE_SENTENCE;
    const deny = el("button", "web-btn", actions);
    deny.type = "button";
    deny.textContent = DENY;
    const approve = el("button", "web-btn web-solid", actions);
    approve.type = "button";
    approve.textContent = APPROVE;
    const decide = (path: "approve" | "deny", ok: string, tone: Tone, pressed: HTMLButtonElement): void => {
      if (working) return;
      working = true;
      box.setAttribute("aria-busy", "true");
      say(null, "");
      deny.disabled = true;
      approve.disabled = true;
      void (async () => {
        const r = await change(() => http("POST", `/api/device/${path}`, { code: clean }));
        working = false;
        box.removeAttribute("aria-busy");
        if (closed) return;
        if (r.ok) {
          say(tone, ok);
          return finish();
        }
        deny.disabled = false;
        approve.disabled = false;
        if (r.cancelled) return void pressed.focus();
        // A code that was decided or ran out in between is as gone as one that never was.
        if (r.status === 404 || r.status === 410) {
          say("bad", NOT_VALID);
          body.textContent = "";
          return finish();
        }
        say("bad", codeMessage(r.status, r.body));
        pressed.focus();
      })();
    };
    approve.addEventListener("click", () => decide("approve", APPROVED, "approved", approve));
    deny.addEventListener("click", () => decide("deny", DENIED, "denied", deny));
  };

  working = true;
  const looked = await change(() => http("POST", "/api/device/lookup", { code: clean }));
  working = false;
  if (closed) return;
  box.removeAttribute("aria-busy");
  // The person did not confirm who they are: nothing was asked and nothing is decided, so there is nothing to show.
  if (!looked.ok && looked.cancelled) return end();
  const who = looked.ok ? readLookup(looked.value) : null;
  if (who === null) {
    say("bad", looked.ok ? NOT_VALID : codeMessage(looked.status, looked.body));
    return finish();
  }
  draw(who);
  title.focus();
}
