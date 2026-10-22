// The card of an integration action (docs/design.md section 6.26): a floating layer in the window that shows what the core will
// run before an action that changes something or opens a session with the person's credentials, and relays one decision.
// The core prepared the card (the argument list spelled element by element, the target, the guard and a hash over all of it) and
// enforces it: only a confirm that repeats the card's ticket and hash runs anything, and it resolves the request again at that
// moment. This module draws the core's fields in the core's order with text nodes and native controls only, and sends the
// ticket and the hash it was given and nothing else. It builds no argument, name, program or hash, and it keeps nothing for
// later: every action asks again, and the only memory is the card on screen.
//
// The card never takes the keyboard. Only a person decides: a click or key a script made (`isTrusted` false) is ignored, `Allow
// once` is ignored for its first half second, a card of a production context needs a second press, `Escape` with the focus
// inside the card denies, and nothing else answers. The rules are the pure function `nextMode` of integratestate.ts; this
// file draws its answer and makes the calls.
import { registerAction, unregisterAction } from "./actions.js";
import { call } from "./bridge.js";
import { el, loadStyles } from "./dom.js";
import {
  CARD_WORDS, argumentRows, effectWords, guardWords, hashShown, modeWords, nextMode, readCard, readConfirm, readFault, refusalOf, targetRows,
  titleWords,
} from "./integratestate.js";
import type { Card, CardAct, CardEvent, Fault, Mode, Outcome } from "./integratestate.js";
import { loadTools, toolOf } from "./integratetools.js";
import { focusInput } from "./keyboard.js";
import { showNotice } from "./status.js";
import type { View } from "./types.js";

/** What the card needs of the page around it, handed over so that this module does not import the scene. */
export interface ConfirmDeps {
  /** Show a layout (the answer of an Exec verb that opened a pane). */
  applyView(v: View): void;
}

let deps: ConfirmDeps = { applyView: () => {} };

/** Hand over how a returned layout is shown. Call once at start-up. */
export function initIntegrationConfirm(handed: ConfirmDeps): void {
  deps = handed;
}

/** How a card ended, as `requestAction` and `showIntegrationCard` answer. */
export type ActionOutcome =
  | { kind: "done"; outcome: Outcome }
  | { kind: "denied" }
  | { kind: "ended" }
  | { kind: "refused"; fault: Fault };

/** The id of the palette entry that is there while a card waits. */
export const CARD_ACTION = "integration.card_focus";

/** A card that has an outcome stays on screen this long (milliseconds), so the sentence can be read; shorter when another card waits. */
const LINGER_MS = 4000;
const LINGER_QUEUED_MS = 1500;
/** The page never waits longer for a ticket than this, whatever the card says (seconds). */
const EXPIRY_MAX_SECS = 300;

interface Item {
  /** The card, or null for a request the page cannot read (then only `Deny`). */
  card: Card | null;
  /** The ticket to cancel: the card's, or the text of an unreadable answer when it looks like one. */
  ticket: string | null;
  mode: Mode;
  outcome: Outcome | null;
  /** The core's sentence of a refusal that keeps the card. */
  fault: Fault | null;
  /** A sentence that stays above the buttons of a card drawn again after a changed request. */
  notice: string;
  shownAt: number;
  armedAt: number | null;
  /** Asks the core for the same action again; null for a card that cannot be asked for again. */
  again: (() => Promise<unknown>) | null;
  settle(outcome: ActionOutcome): void;
  settled: boolean;
  expiry: ReturnType<typeof setTimeout> | undefined;
  leave: ReturnType<typeof setTimeout> | undefined;
}

const queue: Item[] = [];
let node: HTMLElement | null = null;
let titleNumber = 0;
/** The item the card on screen was last drawn for, to tell a card that came to the front from one that changed. */
let front: Item | null = null;
let listed = false;
let styled: Promise<void> | null = null;

/** A button of the card: a native control that is a field of the page, so a typed character reaches no terminal. */
function button(parent: Element, cls: string, key: string, text: string): HTMLButtonElement {
  const b = el("button", cls, parent);
  b.type = "button";
  b.dataset.field = "";
  b.dataset.key = key;
  b.textContent = text;
  return b;
}

/** A row of name and value: both are text, the value is wrapped and never cut. */
function row(parent: Element, name: string, value: string): void {
  const r = el("div", "ic-row", parent);
  el("span", "ic-name", r).textContent = name;
  el("span", "ic-value", r).textContent = value;
}

function lineOf(item: Item): string {
  return modeWords(item.mode, item.outcome) || item.notice;
}

function build(item: Item, count: number, index: number): HTMLElement {
  const card = item.card;
  const section = el("section", "dlg ic");
  const titleId = `ic-title-${++titleNumber}`;
  const leadId = `ic-lead-${titleNumber}`;
  section.setAttribute("role", "alertdialog");
  section.setAttribute("aria-labelledby", titleId);
  section.setAttribute("aria-describedby", leadId);
  section.dataset.mode = item.mode;
  section.dataset.production = card !== null && card.production !== null ? "true" : "false";
  if (item.mode === "sending") section.setAttribute("aria-busy", "true");

  const head = el("div", "ic-head", section);
  el("span", "ic-who label", head).textContent = CARD_WORDS.head;
  if (count > 1) el("span", "ic-count label", head).textContent = CARD_WORDS.count(index, count);
  const title = el("h2", "ic-title", section);
  title.id = titleId;
  const lead = el("p", "ic-effect", section);
  lead.id = leadId;
  if (card === null) {
    title.textContent = CARD_WORDS.cannotTitle;
    lead.hidden = true;
  } else {
    title.textContent = titleWords(card, toolOf(card.tool)?.label ?? null, toolOf(card.tool)?.verbs.find((v) => v.id === card.verb)?.label ?? null);
    lead.textContent = effectWords(card);
    const target = el("div", "ic-target", section);
    for (const [name, value] of targetRows(card)) row(target, name, value);
    const args = el("div", "ic-args", section);
    args.tabIndex = 0;
    args.dataset.field = "";
    args.dataset.key = "args";
    args.setAttribute("role", "group");
    args.setAttribute("aria-label", "Command");
    for (const [name, value] of argumentRows(card)) row(args, name, value);
    if (card.production !== null) {
      // The mark `!` is drawn by the style sheet; the words and the underline are the other cues.
      el("p", "ic-guard", section).textContent = guardWords(card.production);
    }
    el("p", "ic-hash", section).textContent = `hash ${hashShown(card.hash)}`;
  }
  const line = el("p", "ic-line", section);
  line.setAttribute("role", "status");
  line.textContent = lineOf(item);
  line.hidden = line.textContent === "";
  const fault = el("p", "ic-fault", section);
  if (item.fault === null) fault.hidden = true;
  else fault.textContent = item.fault.text;

  const waiting = item.mode === "ask" || item.mode === "armed" || item.mode === "sending" || item.mode === "changed" || item.mode === "cannot";
  if (waiting) {
    const actions = el("div", "ic-actions", section);
    // `Allow once` comes first in the document and `Deny` second (6.26); the style sheet draws `Deny` at the left.
    const yes = card === null ? null : button(actions, "ic-yes", "yes", item.mode === "armed" ? CARD_WORDS.yesArmed : CARD_WORDS.yes);
    const no = button(actions, "ic-no", "no", CARD_WORDS.no);
    // Only a real press counts; `nextMode` applies the half second and the other rules.
    no.addEventListener("click", (e) => step(item, { type: "deny", trusted: e.isTrusted }));
    no.disabled = item.mode === "sending";
    if (card !== null && yes !== null) {
      yes.addEventListener("click", (e) =>
        step(item, { type: "allow", now: Date.now(), shownAt: item.shownAt, armedAt: item.armedAt, trusted: e.isTrusted, production: card.production !== null }),
      );
      yes.disabled = item.mode === "sending" || item.mode === "changed";
      if (item.mode === "armed") yes.dataset.armed = "true";
      el("p", "ic-foot", section).textContent = CARD_WORDS.foot;
    }
  }
  section.addEventListener("keydown", (e) => {
    if (e.key !== "Escape" || e.isComposing) return;
    // The key is the card's own: it never reaches the terminal or a field behind the card.
    e.preventDefault();
    e.stopPropagation();
    step(item, { type: "deny", trusted: e.isTrusted });
  });
  return section;
}

function live(item: Item): boolean {
  return item.mode !== "done" && item.mode !== "ended";
}

function render(): void {
  const first = queue[0];
  if (first === undefined) {
    if (node !== null) {
      // A card that goes while the focus is in it hands the keyboard back to the terminal.
      const held = node.contains(document.activeElement);
      node.remove();
      node = null;
      if (held) focusInput();
    }
    front = null;
    syncAction();
    return;
  }
  const fresh = build(first, queue.length, 1);
  const active = document.activeElement;
  const held = node !== null && node.contains(active) ? ((active as HTMLElement).dataset.key ?? "") : null;
  if (node === null) document.body.append(fresh);
  else node.replaceWith(fresh);
  node = fresh;
  if (held !== null) {
    // The control that had the focus is drawn again in place; a card that has none left hands the focus back.
    const same = fresh.querySelector<HTMLElement>(`[data-key="${held}"]`);
    if (same !== null && !(same instanceof HTMLButtonElement && same.disabled)) same.focus({ preventScroll: true });
    else focusInput();
  }
  if (front !== first) {
    // A card that comes to the front has its own half second, counted from now.
    front = first;
    first.shownAt = Date.now();
    first.armedAt = null;
    if (live(first)) showNotice(CARD_WORDS.waiting);
  }
  syncAction();
}

/** The palette entry exists only while a card waits for a decision. */
function syncAction(): void {
  const waiting = queue.some((i) => i.mode === "ask" || i.mode === "armed" || i.mode === "cannot");
  if (waiting && !listed) {
    registerAction({ id: CARD_ACTION, title: CARD_WORDS.palette, run: () => document.querySelector<HTMLElement>("section.ic button.ic-no")?.focus() });
    listed = true;
  } else if (!waiting && listed) {
    unregisterAction(CARD_ACTION);
    listed = false;
  }
}

function remove(item: Item): void {
  clearTimeout(item.expiry);
  clearTimeout(item.leave);
  const at = queue.indexOf(item);
  if (at >= 0) queue.splice(at, 1);
  render();
}

/** The outcome is known: tell the caller once, and let a card that has words keep them on screen for a few seconds. */
function finish(item: Item, outcome: ActionOutcome, linger: boolean): void {
  if (!item.settled) {
    item.settled = true;
    item.settle(outcome);
  }
  clearTimeout(item.expiry);
  clearTimeout(item.leave);
  if (!linger) {
    remove(item);
    return;
  }
  item.leave = setTimeout(() => remove(item), queue.length > 1 ? LINGER_QUEUED_MS : LINGER_MS);
}

/** Cancel a ticket at the core. A refusal or a missing ticket changes nothing: the ticket ends by itself. */
function cancel(ticket: string | null): void {
  if (ticket === null) return;
  void call("integration_cancel", { ticket }).catch(() => {});
}

/** Apply an event to a card through the pure machine and do what it says. */
function step(item: Item, event: CardEvent): void {
  if (!queue.includes(item)) return;
  const before = item.mode;
  const next = nextMode(before, event);
  if (next.mode === before && next.act === "none") return;
  item.mode = next.mode;
  act(item, next.act, before);
}

function act(item: Item, what: CardAct, before: Mode): void {
  switch (what) {
    case "arm":
      item.armedAt = Date.now();
      item.fault = null;
      item.notice = "";
      render();
      return;
    case "confirm":
      item.fault = null;
      item.notice = "";
      render();
      void confirm(item);
      return;
    case "cancel":
      cancel(item.ticket);
      showNotice(CARD_WORDS.denied);
      finish(item, { kind: "denied" }, false);
      return;
    default:
      render();
      if (before !== item.mode && item.mode === "ended") {
        showNotice(CARD_WORDS.ended);
        finish(item, { kind: "ended" }, queue[0] === item);
      }
  }
}

/** The one call that decides. It names the ticket and the hash the core gave and nothing else. */
async function confirm(item: Item): Promise<void> {
  const card = item.card;
  if (card === null) return;
  try {
    const raw = await call<unknown>("integration_confirm", { ticket: card.ticket, hash: card.hash });
    const answer = readConfirm(raw);
    const outcome: Outcome = answer === null ? { kind: "ran", note: "", pane: null } : { kind: answer.kind, note: answer.note, pane: answer.pane };
    item.outcome = outcome;
    step(item, { type: "ran" });
    if (answer !== null && answer.kind === "launched" && answer.view !== null) deps.applyView(answer.view);
    showNotice(modeWords("done", outcome));
    finish(item, { kind: "done", outcome }, true);
  } catch (e) {
    refused(item, readFault(e));
  }
}

/** The core refused the press: a ticket that is gone ends the card, a changed request is read again, any other sentence is kept. */
function refused(item: Item, fault: Fault): void {
  const refusal = refusalOf(fault);
  step(item, { type: "refused", refusal });
  if (refusal === "changed") {
    void readAgain(item);
    return;
  }
  if (refusal === "other") {
    item.fault = fault;
    render();
  }
}

/** A changed request: the old ticket is dropped and the core is asked once for a card for the same action, which is drawn unarmed. */
async function readAgain(item: Item): Promise<void> {
  cancel(item.ticket);
  const failed = (fault: Fault | null): void => {
    item.fault = fault;
    item.mode = "ended";
    render();
    showNotice(CARD_WORDS.ended);
    finish(item, { kind: "ended" }, true);
  };
  if (item.again === null) return failed(null);
  let raw: unknown;
  try {
    raw = await item.again();
  } catch (e) {
    return failed(readFault(e));
  }
  const card = readCard(raw);
  if (!queue.includes(item)) {
    // The person denied while the core was asked: the new ticket is dropped too.
    if (card !== null) cancel(card.ticket);
    return;
  }
  if (card === null) return failed(null);
  item.card = card;
  item.ticket = card.ticket;
  item.notice = CARD_WORDS.changed;
  item.fault = null;
  item.outcome = null;
  item.shownAt = Date.now();
  item.armedAt = null;
  arm(item);
  step(item, { type: "fresh" });
}

/** The page's clock for a ticket: the card says how long the core keeps it. A card that ends here never confirms. */
function arm(item: Item): void {
  clearTimeout(item.expiry);
  const secs = Math.min(item.card?.expires ?? EXPIRY_MAX_SECS, EXPIRY_MAX_SECS);
  item.expiry = setTimeout(() => {
    if (queue[0] === item) {
      step(item, { type: "expired" });
      return;
    }
    // A card that waits behind another and runs out is dropped quietly: it would only be refused.
    if (item.mode === "ask" || item.mode === "armed" || item.mode === "cannot") {
      item.mode = "ended";
      showNotice(CARD_WORDS.ended);
      finish(item, { kind: "ended" }, false);
    }
  }, secs * 1000);
}

function ready(): Promise<void> {
  styled ??= Promise.all([loadStyles("./dialogs.css"), loadStyles("./integrate.css")]).then(
    () => {},
    () => {
      // the card still works without its style sheets
    },
  );
  return styled;
}

/**
 * Show the card the core answered with and wait for the decision. `raw` is the answer of `integration_prepare` exactly as it came:
 * it is read here, so nothing can show a card that was not read. An answer that is not the contract is a card that can only be
 * denied. `again` asks the core for the same action once more; it is used when the core says the request changed. A second card
 * waits behind the first.
 */
export function showIntegrationCard(raw: unknown, again: (() => Promise<unknown>) | null = null): Promise<ActionOutcome> {
  const card = readCard(raw);
  const said = typeof raw === "object" && raw !== null ? (raw as Record<string, unknown>).ticket : undefined;
  const ticket = card?.ticket ?? (typeof said === "string" && said.length > 0 && said.length <= 128 ? said : null);
  return new Promise<ActionOutcome>((settle) => {
    const item: Item = {
      card, ticket, mode: card === null ? "cannot" : "ask", outcome: null, fault: null, notice: "", shownAt: Date.now(), armedAt: null,
      again, settle, settled: false, expiry: undefined, leave: undefined,
    };
    queue.push(item);
    arm(item);
    // The style sheets come first, so a card is never drawn bare. Its half second starts when it is drawn at the front.
    void ready().then(() => {
      if (queue.includes(item)) render();
    });
  });
}

/**
 * Ask the core for the card of an action that changes something or opens a session, show it, and answer how it ended. The page
 * sends the tool, the verb, the pane and the action's own arguments and nothing it built itself. A request the core refuses is
 * `refused` with the core's sentence and shows no card.
 */
export async function requestAction(tool: string, verb: string, pane: number | null, args: Record<string, string>): Promise<ActionOutcome> {
  try {
    await loadTools();
  } catch {
    // the title falls back to the core's ids
  }
  const ask = (): Promise<unknown> => call<unknown>("integration_prepare", { tool, verb, pane, args });
  let raw: unknown;
  try {
    raw = await ask();
  } catch (e) {
    return { kind: "refused", fault: readFault(e) };
  }
  return showIntegrationCard(raw, ask);
}

/** Whether a card waits or shows its outcome. For the tests of the window around it. */
export function cardsWaiting(): number {
  return queue.length;
}
