// The consent card (docs/design.md section 6.14): the strip in which the user shares a pane with the chat, ticks what to
// attach and sees what would go out. It draws the model of a `Consent` with text nodes and native controls only; every
// word that is not the core's comes from `consentstate.ts`. No command is called here: a press calls a method of the
// controller, which makes the calls.
//
// The card is rebuilt when the mode or its content changes and updated in place when only a tick changes, so a keyboard
// user who ticks a box keeps the focus on it. Opening moves the focus to the first control, and closing returns it to the
// toggle `Auto` when the focus was in the card. The strip also holds the approval cards (docs/design.md section 6.15),
// so this card adds and removes only its own `section.chat-consent`, looks only inside it and answers only to events that
// come from inside it.

import type { Consent } from "./consent.js";
import {
  AUTO_ASK_WORDS, BEST_EFFORT, KIND_WORDS, cardTitle, destinationWords, findingWords, persistWords, ruleTag, ruleWords, sizeWords, totalWords, validForWords,
} from "./consentstate.js";
import type { CtxKind } from "./consentstate.js";
import { el } from "./dom.js";

/** What the card needs to know of the page around it. */
export interface CardView {
  /** The words for the pane in the title and the question: `pane 1 · shell`. */
  paneTitle(pane: number): string;
  /** The name of a saved provider, or its id. */
  profileName(id: string): string;
  /** What the pane shows: a terminal, a preview or anything else. */
  paneKind(pane: number): "terminal" | "preview" | "other";
  /** Open the providers dialog. */
  providers(): void;
  /** Give the keyboard to the toggle `Auto`. */
  focusButton(): void;
}

/** A button of the card. */
function button(parent: Element, cls: string, key: string, text: string, onClick: () => void): HTMLButtonElement {
  const b = el("button", cls, parent);
  b.type = "button";
  b.dataset.field = "";
  b.dataset.key = key;
  b.textContent = text;
  b.addEventListener("click", onClick);
  return b;
}

/**
 * Mount the card of `consent` into `host`. Answers the function that removes the card and its listeners.
 */
export function mountCard(host: HTMLElement, consent: Consent, view: CardView): () => void {
  let drawnMode = consent.model.mode;
  let signature = "";
  /** The one section of this card in the strip; null while the card is closed. */
  let current: HTMLElement | null = null;
  /** The card opened and has not yet put the focus on a control. */
  let wantFocus = false;
  let alive = true;

  const ticks = (): void => {
    if (current === null) return;
    const m = consent.model;
    for (const box of current.querySelectorAll<HTMLInputElement>("input[data-kind]")) {
      box.checked = m.ticked.includes(box.dataset.kind as CtxKind);
    }
    for (const box of current.querySelectorAll<HTMLInputElement>("input[data-item]")) {
      box.checked = !m.off.includes(Number(box.dataset.item));
    }
    for (const b of current.querySelectorAll<HTMLButtonElement>("button.consent-preview")) b.disabled = m.ticked.length === 0;
    const total = current.querySelector(".consent-total");
    if (total !== null && m.offer !== null) {
      const going = m.offer.items.filter((item) => !m.off.includes(item.id));
      const words = totalWords(going.length, going.reduce((sum, item) => sum + item.bytes, 0));
      if (total.textContent !== words) total.textContent = words;
    }
  };

  const build = (): void => {
    const m = consent.model;
    const state = m.state;
    const pane = m.pane;
    const paneWords = pane === null ? "" : view.paneTitle(pane);
    const section = el("section", "chat-consent");
    section.setAttribute("role", "group");
    section.setAttribute("aria-label", "Share context");
    section.dataset.mode = m.mode;
    if (m.mode === "busy") section.setAttribute("aria-busy", "true");
    const head = el("div", "consent-head", section);
    el("span", "consent-title label", head).textContent = cardTitle(m.mode, state, paneWords, m.busy);
    if (state !== null && m.mode !== "busy") {
      const where = destinationWords(m.offer !== null && m.mode === "offer" ? m.offer : state);
      const dest = el("span", "consent-dest", head);
      el("span", "consent-host", dest).textContent = where.host;
      const place = el("span", "consent-place", dest);
      place.dataset.leaves = where.leaves ? "true" : "false";
      place.textContent = where.place;
    }
    const actions = document.createElement("div");
    actions.className = "consent-actions";

    if (m.mode === "ask" && state !== null && pane !== null) {
      const what = view.paneKind(pane) === "preview" ? "The file this pane shows" : "Terminal content";
      el("p", "consent-text", section).textContent = `${what} is only sent when you share it.`;
      const name = view.profileName(state.profile);
      const via = state.model === "" ? name : `${name} · ${state.model}`;
      const goes = destinationWords(state);
      el("p", "consent-text", section).textContent = `Share ${paneWords} with ${via}? It goes to ${goes.host} (${goes.place}).`;
      el("p", "consent-text", section).textContent = m.viaAuto ? AUTO_ASK_WORDS : "You still see every item before a message is sent.";
      button(actions, "consent-share", "share", `Share pane ${pane}`, () => void consent.share());
      button(actions, "consent-no", "no", "No", () => consent.close());
    } else if (m.mode === "pick" && state !== null) {
      if (!state.required) {
        el("p", "consent-text", section).textContent = "Asking is off for models on this computer. You still see every item.";
      }
      if (m.fault !== null) {
        const fault = el("p", "consent-text", section);
        fault.dataset.kind = "error";
        fault.textContent = m.fault.text;
      }
      const kinds = el("fieldset", "consent-kinds", section);
      kinds.setAttribute("aria-label", "What to attach");
      for (const kind of state.kinds) {
        const label = el("label", "consent-kind", kinds);
        label.dataset.kind = kind;
        label.dataset.field = "";
        const box = el("input", "", label);
        box.type = "checkbox";
        box.dataset.field = "";
        box.dataset.kind = kind;
        box.dataset.key = `kind-${kind}`;
        box.addEventListener("change", () => consent.tick(kind, box.checked));
        el("span", "consent-kindword", label).textContent = KIND_WORDS[kind];
      }
      el("p", "consent-empty", section).textContent = "Nothing is attached yet.";
      if (state.kinds.includes("last_command")) {
        el("p", "consent-note", section).textContent =
          "On a pane that has just started, the last command can be the start-up text of the shell. Untick it when in doubt.";
      }
      button(actions, "consent-preview", "preview", "Preview", () => void consent.preview());
      if (state.shared) button(actions, "consent-unshare", "unshare", "Stop sharing", () => void consent.unshare());
      button(actions, "consent-close", "close", "Close", () => consent.close());
    } else if (m.mode === "offer" && m.offer !== null) {
      const list = el("ul", "consent-items", section);
      for (const item of m.offer.items) {
        const row = el("li", "consent-item", list);
        row.dataset.id = String(item.id);
        row.dataset.kind = item.kind;
        const label = el("label", "consent-row", row);
        label.dataset.field = "";
        const box = el("input", "", label);
        box.type = "checkbox";
        box.dataset.field = "";
        box.dataset.item = String(item.id);
        box.dataset.key = `item-${item.id}`;
        box.addEventListener("change", () => consent.toggleItem(item.id, box.checked));
        el("span", "consent-label", label).textContent = item.label;
        el("span", "consent-size", label).textContent = sizeWords(item.bytes);
        if (item.findings.length > 0) {
          const findings = el("ul", "consent-findings", row);
          for (const finding of item.findings) {
            const entry = el("li", "consent-finding", findings);
            entry.dataset.rule = ruleTag(finding.rule);
            entry.dataset.tone = ruleWords(finding.rule).tone;
            entry.textContent = findingWords(finding);
          }
        }
      }
      if (m.offer.empty.length > 0) {
        el("p", "consent-empty", section).textContent = `Nothing there: ${m.offer.empty.map((kind) => KIND_WORDS[kind]).join(", ")}.`;
      }
      el("p", "consent-total", section);
      el("p", "consent-persist", section).textContent = persistWords(m.offer.persist);
      el("p", "consent-valid", section).textContent = validForWords(m.offer.ttl_seconds);
      el("p", "consent-best", section).textContent = BEST_EFFORT;
      button(actions, "consent-again", "again", "Read again", () => void consent.preview());
      button(actions, "consent-discard", "discard", "Discard", () => consent.discard());
    } else if (m.mode === "blocked") {
      const text = el("p", "consent-text", section);
      text.dataset.kind = "error";
      text.textContent = m.fault?.text ?? "";
      if (m.fault?.reason === "local_only" || m.fault?.reason === "destination") {
        button(actions, "consent-providers", "providers", "Providers…", () => view.providers());
      }
      button(actions, "consent-close", "close", "Close", () => consent.close());
    }
    if (actions.childElementCount > 0) section.append(actions);
    if (current !== null && current.parentElement === host) current.replaceWith(section);
    else host.append(section);
    current = section;
  };

  const render = (): void => {
    if (!alive) return;
    const m = consent.model;
    const focusInside = current?.contains(document.activeElement) ?? false;
    const wasInside = focusInside && document.activeElement !== null;
    const key = wasInside ? ((document.activeElement as HTMLElement).dataset.key ?? "") : "";
    if (m.mode === "closed") {
      const had = drawnMode !== "closed";
      current?.remove();
      current = null;
      signature = "";
      drawnMode = "closed";
      wantFocus = false;
      if (had && focusInside) view.focusButton();
      return;
    }
    if (drawnMode === "closed") wantFocus = true;
    drawnMode = m.mode;
    const next = JSON.stringify([
      m.mode, m.busy, m.pane, m.state, m.offer === null ? null : m.offer.offer, m.fault, m.state?.kinds, m.offer?.items.length, m.viaAuto,
    ]);
    if (next !== signature) {
      signature = next;
      build();
      if (key !== "") {
        // The control that had the focus is rebuilt in place; one that is gone hands the focus to the first control later.
        const same = current?.querySelector<HTMLElement>(`[data-key="${key}"]`) ?? null;
        if (same !== null) same.focus({ preventScroll: true });
        else wantFocus = true;
      }
    }
    ticks();
    if (wantFocus) {
      const first = current?.querySelector<HTMLElement>("input, button") ?? null;
      const active = document.activeElement;
      // Not taken from a field the user went to meanwhile: only from the button that opened the card, the card itself, the
      // page body, or a container of the card (a browser that does not focus a pressed button focuses its nearest container).
      const free =
        active === null || active === document.body || current?.contains(active) === true || active.contains(host) ||
        (active as HTMLElement).classList.contains("chat-ctx");
      if (first !== null && free) {
        first.focus({ preventScroll: true });
        wantFocus = false;
      }
    }
  };

  const inside = (e: Event): boolean => current !== null && e.target instanceof Node && current.contains(e.target);
  const onKey = (e: KeyboardEvent): void => {
    if (e.key !== "Escape" || e.isComposing || !inside(e)) return;
    e.preventDefault();
    e.stopPropagation();
    consent.close();
  };
  // A pressed control has the focus, also in a browser that does not give a pressed button the focus: the card then knows
  // that the user is inside it, and closing it returns the focus to `Auto`. Capturing, so it runs before the control's own
  // handler, which may close the card.
  const onPress = (e: MouseEvent): void => {
    if (!inside(e)) return;
    const target = e.target;
    if (target instanceof HTMLButtonElement || target instanceof HTMLInputElement) target.focus({ preventScroll: true });
  };
  host.addEventListener("click", onPress, true);
  host.addEventListener("keydown", onKey);
  const stop = consent.subscribe(render);
  render();

  return () => {
    alive = false;
    stop();
    host.removeEventListener("keydown", onKey);
    host.removeEventListener("click", onPress, true);
    current?.remove();
    current = null;
  };
}
