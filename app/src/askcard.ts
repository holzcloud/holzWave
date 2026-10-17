// The card of a `wsh ai` request (docs/design.md section 6.20): a floating layer in the window of the pane that asked. It draws the
// oldest request of an `AskQueue` with text nodes and native controls only; every word that is not the core's comes from
// `askstate.ts`, and the prompt is the one text it shows, in full, with the characters a person cannot see written as escapes.
// No command is called here: a press calls `answer` of the controller, which makes the one call.
//
// The card never takes the keyboard when it appears or changes. Only a person decides: a click or a key press that a script
// made (`isTrusted` false) is ignored, `Send once` is ignored for its first half second, `Escape` with the focus inside the card
// refuses, and nothing else answers. While a card waits the palette has one entry that moves the keyboard to `Don't send`.

import { registerAction, unregisterAction } from "./actions.js";
import { AskQueue } from "./ask.js";
import { WORDS, findingLines, inputWords, placeWords, promptWords, routeWords } from "./askstate.js";
import type { AskItem } from "./askstate.js";
import { call, listen } from "./bridge.js";
import { BEST_EFFORT } from "./consentstate.js";
import { el, loadStyles } from "./dom.js";
import { focusInput } from "./keyboard.js";
import { showNotice } from "./status.js";

/** The id of the palette entry that is there while a card waits. */
export const ASK_ACTION = "ai.ask_focus";

/** Numbers the titles, so each card names itself. */
let titleNumber = 0;

/** A button of the card: a native control that is a field of the page, so a typed character reaches no terminal. */
function button(parent: Element, cls: string, key: string, text: string): HTMLButtonElement {
  const b = el("button", cls, parent);
  b.type = "button";
  b.dataset.field = "";
  b.dataset.key = key;
  b.textContent = text;
  return b;
}

/** A list of findings as one line of words each; nothing is drawn for none. */
function findings(parent: Element, words: readonly string[]): void {
  if (words.length === 0) return;
  const list = el("ul", "ask-findings", parent);
  for (const text of words) el("li", "ask-finding", list).textContent = text;
}

/**
 * Mount the card of `queue` into `host`. It shows the oldest request and counts the others. Answers the function that removes
 * the card and its listener.
 */
export function mountAsk(host: HTMLElement, queue: AskQueue): () => void {
  let alive = true;
  let node: HTMLElement | null = null;
  let signature = "";

  const build = (item: AskItem, count: number): HTMLElement => {
    const view = item.view;
    const route = routeWords(view);
    const section = el("section", "dlg ask");
    const titleId = `ask-title-${++titleNumber}`;
    const leadId = `ask-lead-${titleNumber}`;
    section.setAttribute("role", "alertdialog");
    section.setAttribute("aria-labelledby", titleId);
    section.setAttribute("aria-describedby", leadId);
    section.dataset.leaves = route.leaves ? "true" : "false";
    section.dataset.state = item.sending ? "sending" : "waiting";
    if (item.sending) section.setAttribute("aria-busy", "true");

    const head = el("div", "ask-head", section);
    el("span", "ask-who label", head).textContent = WORDS.head(view.pane);
    if (count > 1) el("span", "ask-count label", head).textContent = WORDS.count(1, count);
    const title = el("h2", "ask-title", section);
    title.id = titleId;
    title.textContent = WORDS.title;
    const lead = el("p", "ask-lead", section);
    lead.id = leadId;
    lead.textContent = WORDS.lead(view.pane);

    const rows = el("dl", "ask-rows", section);
    const prompt = el("div", "ask-row", rows);
    prompt.dataset.row = "prompt";
    el("dt", "ask-name label", prompt).textContent = WORDS.promptLabel;
    const promptBody = el("dd", "ask-body", prompt);
    const text = el("div", "ask-text", promptBody);
    text.tabIndex = 0;
    text.dataset.field = "";
    text.dataset.key = "prompt";
    text.setAttribute("role", "group");
    text.setAttribute("aria-label", WORDS.promptLabel);
    if (view.prompt === "") text.dataset.empty = "true";
    text.textContent = promptWords(view);
    findings(promptBody, findingLines(view.prompt_findings));

    const input = el("div", "ask-row", rows);
    input.dataset.row = "input";
    el("dt", "ask-name label", input).textContent = WORDS.inputLabel;
    const inputBody = el("dd", "ask-body", input);
    el("p", "ask-counts", inputBody).textContent = inputWords(view.input);
    if (view.input !== null) {
      findings(inputBody, findingLines(view.input.findings));
      el("p", "ask-best", inputBody).textContent = BEST_EFFORT;
    }

    const to = el("div", "ask-row", rows);
    to.dataset.row = "to";
    el("dt", "ask-name label", to).textContent = WORDS.toLabel;
    const toBody = el("dd", "ask-body", to);
    el("span", "ask-route", toBody).textContent = route.route;
    el("span", "ask-host", toBody).textContent = route.host;
    const place = el("span", "ask-place", toBody);
    place.dataset.leaves = route.leaves ? "true" : "false";
    place.textContent = placeWords(route);

    el("p", "ask-foot", section).textContent = item.sending ? WORDS.sending : WORDS.foot(view.pane);
    const fault = el("p", "ask-fault", section);
    fault.dataset.kind = "error";
    if (item.fault === null) fault.hidden = true;
    else fault.textContent = item.fault.text;

    const actions = el("div", "ask-actions", section);
    const no = button(actions, "ask-no", "no", WORDS.no);
    const yes = button(actions, "ask-yes", "yes", WORDS.yes);
    no.disabled = item.sending;
    yes.disabled = item.sending;
    // Only a real press counts; the controller applies the half second and the other rules.
    no.addEventListener("click", (e) => void queue.answer(view.ask, "refuse", e.isTrusted));
    yes.addEventListener("click", (e) => void queue.answer(view.ask, "send", e.isTrusted));
    section.addEventListener("keydown", (e) => {
      if (e.key !== "Escape" || e.isComposing) return;
      // The key is the card's own: it never reaches the terminal or a field behind the card.
      e.preventDefault();
      e.stopPropagation();
      void queue.answer(view.ask, "refuse", e.isTrusted);
    });
    return section;
  };

  const render = (): void => {
    if (!alive) return;
    const items = queue.items;
    const first = items[0];
    if (first === undefined) {
      if (node !== null) {
        // A card that goes while the focus is in it hands the keyboard back to the terminal.
        const held = node.contains(document.activeElement);
        node.remove();
        node = null;
        signature = "";
        if (held) focusInput();
      }
      return;
    }
    const next = JSON.stringify([first.view, first.sending, first.fault, items.length]);
    if (node !== null && next === signature) return;
    const fresh = build(first, items.length);
    if (node === null) {
      host.append(fresh);
    } else {
      // The control that had the focus is drawn again in place; a card that has none left hands the focus back.
      const active = document.activeElement;
      const held = node.contains(active) ? ((active as HTMLElement).dataset.key ?? "") : null;
      node.replaceWith(fresh);
      if (held !== null) {
        const same = fresh.querySelector<HTMLElement>(`[data-key="${held}"]`);
        if (same !== null && !(same instanceof HTMLButtonElement && same.disabled)) same.focus({ preventScroll: true });
        else focusInput();
      }
    }
    node = fresh;
    signature = next;
  };

  const stop = queue.subscribe(render);
  render();
  return () => {
    alive = false;
    stop();
    node?.remove();
    node = null;
  };
}

/**
 * Load the style sheets, start the queue and draw its card. The event is listened to before the list is asked for, so a request
 * that arrives in between is not lost. Call once at start-up.
 */
export async function initAsk(): Promise<void> {
  try {
    await Promise.all([loadStyles("./dialogs.css"), loadStyles("./ask.css")]);
  } catch {
    // the card still works without its style sheets
  }
  const queue = new AskQueue({ invoke: (cmd, args) => call(cmd, args), now: () => Date.now(), say: showNotice });
  await listen("ai-ask", (raw: unknown) => queue.receive(raw));
  mountAsk(document.body, queue);
  // The palette entry exists only while a card waits.
  let listed = false;
  queue.subscribe(() => {
    const waiting = queue.items.length > 0;
    if (waiting && !listed) {
      registerAction({
        id: ASK_ACTION,
        title: WORDS.paletteTitle,
        run: () => document.querySelector<HTMLElement>("section.ask button.ask-no")?.focus(),
      });
      listed = true;
    } else if (!waiting && listed) {
      unregisterAction(ASK_ACTION);
      listed = false;
    }
  });
  await queue.load();
}
