// The approval card (docs/design.md section 6.15): one strip entry for each call that waits for a decision. It draws the
// records of an `Approvals` controller with text nodes and native controls only: every field the core sent is drawn in its
// order, and every word that is not the core's comes from `approvalstate.ts`. No command is called here: a press calls
// `decide` of the controller, which makes the one call.
//
// A card never takes the keyboard when it appears or changes. It is added to the strip before the consent card and removed
// one node at a time, so the consent card is never touched.

import type { Approvals, CardModel } from "./approval.js";
import { EFFECT_WORDS, TOOL_WORDS, WORDS, hashWords, showText, targetWords } from "./approvalstate.js";
import type { Destination } from "./approvalstate.js";
import { sizeWords } from "./consentstate.js";
import { el } from "./dom.js";

/** What the card needs to know of the page around it. */
export interface CardView {
  /** Where the result of a call goes, from the route of the turn; null when it is not known. */
  destination(): Destination | null;
  /** Give the keyboard to the message box. */
  focusComposer(): void;
}

/** Numbers the titles, so each card names itself. */
let titleNumber = 0;

/** A button of the card. */
function button(parent: Element, cls: string, key: string, text: string): HTMLButtonElement {
  const b = el("button", cls, parent);
  b.type = "button";
  b.dataset.field = "";
  b.dataset.key = key;
  b.textContent = text;
  return b;
}

/** The words of a tool: the fixed words of a tool the page knows, else its name as the core sent it, made visible. */
function toolWords(tool: string): string {
  return Object.hasOwn(TOOL_WORDS, tool) ? (TOOL_WORDS[tool] ?? tool) : showText(tool);
}

/**
 * Mount the cards of `approvals` into `strip`. Answers the function that removes the cards and their listeners.
 */
export function mountApprovals(strip: HTMLElement, approvals: Approvals, view: CardView): () => void {
  let alive = true;
  const drawn = new Map<string, { node: HTMLElement; rec: CardModel; sig: string }>();

  /** The first section of the consent card, which the approval cards stay in front of. */
  const anchor = (): Element | null => strip.querySelector(":scope > section.chat-consent");

  const build = (card: CardModel): HTMLElement => {
    const seen = card.seen;
    const a = seen.approval;
    const section = el("section", "chat-approval");
    const titleId = `approval-title-${++titleNumber}`;
    section.setAttribute("role", "group");
    section.setAttribute("aria-labelledby", titleId);
    section.dataset.call = seen.id;
    section.dataset.mode = card.mode;
    section.dataset.tool = showText(seen.tool);
    if (a !== null) {
      section.dataset.effect = showText(a.effect);
      section.dataset.remote = a.remote ? "true" : "false";
      section.dataset.tainted = a.tainted ? "true" : "false";
    }
    if (card.mode === "sending") section.setAttribute("aria-busy", "true");

    const head = el("div", "approval-head", section);
    const title = el("span", "approval-title label", head);
    title.id = titleId;
    const mark = el("span", "approval-mark", title);
    mark.textContent = "!";
    mark.setAttribute("aria-hidden", "true");
    title.append(document.createTextNode(` ${WORDS.title}`));
    el("span", "approval-tool", head).textContent = toolWords(seen.tool);
    if (a !== null) {
      el("span", "approval-where", head).textContent = targetWords(a.pane, a.target);
      if (a.remote) el("span", "approval-remote", head).textContent = WORDS.remote;
      const effect = Object.hasOwn(EFFECT_WORDS, a.effect) ? EFFECT_WORDS[a.effect] : undefined;
      if (effect !== undefined) el("p", "approval-effect", section).textContent = effect;

      const box = el("div", "approval-payload", section);
      box.tabIndex = 0;
      box.dataset.field = "";
      box.dataset.key = "payload";
      box.setAttribute("role", "group");
      box.setAttribute("aria-label", WORDS.fieldsLabel);
      const list = el("ul", "approval-rows", box);
      for (const row of a.rows) {
        const li = el("li", "approval-row", list);
        li.dataset.name = row.name;
        el("span", "approval-name", li).textContent = row.name;
        el("span", "approval-value", li).textContent = row.value;
        el("span", "approval-bytes", li).textContent = sizeWords(row.bytes);
      }
      const dest = view.destination();
      const line = el("p", "approval-dest", section);
      if (dest === null) {
        line.textContent = WORDS.destinationUnknown;
      } else {
        line.append(document.createTextNode("The result is sent to "));
        el("span", "approval-model", line).textContent = dest.model;
        if (dest.host !== null && dest.host !== "") {
          line.append(document.createTextNode(" at "));
          el("span", "approval-host", line).textContent = dest.host;
        }
        if (dest.place !== null) {
          line.append(document.createTextNode(" ("));
          const place = el("span", "approval-place", line);
          place.dataset.leaves = dest.leaves ? "true" : "false";
          place.textContent = dest.place;
          line.append(document.createTextNode(")"));
        }
        line.append(document.createTextNode("."));
      }
    }
    if (seen.hash !== null) el("p", "approval-hash", section).textContent = hashWords(seen.hash);
    if (card.mode === "ask" || card.mode === "sending") {
      const actions = el("div", "approval-actions", section);
      const allow = button(actions, "approval-allow", "allow", WORDS.allow);
      const deny = button(actions, "approval-deny", "deny", WORDS.deny);
      if (card.mode === "sending") {
        allow.disabled = true;
        deny.disabled = true;
      }
      allow.addEventListener("click", () => {
        void approvals.decide(seen.id, "allow");
        view.focusComposer();
      });
      deny.addEventListener("click", () => {
        void approvals.decide(seen.id, "deny");
        view.focusComposer();
      });
    }
    if (card.mode === "sending") el("p", "approval-verdict", section).textContent = WORDS.sending;
    if (card.mode === "sent") el("p", "approval-verdict", section).textContent = card.verdict === "allow" ? WORDS.allowed : WORDS.denied;
    el("p", "approval-foot", section).textContent = WORDS.foot;
    return section;
  };

  const render = (): void => {
    if (!alive) return;
    const before = anchor();
    const cards = approvals.cards;
    const live = new Set(cards.map((c) => c.id));
    for (const [id, one] of drawn) {
      if (live.has(id)) continue;
      one.node.remove();
      drawn.delete(id);
    }
    const ordered: HTMLElement[] = [];
    for (const card of cards) {
      const sig = JSON.stringify([card.seen.hash, card.mode, card.verdict, card.fault]);
      let one = drawn.get(card.id);
      if (one === undefined || one.rec !== card || one.sig !== sig) {
        const node = build(card);
        if (one === undefined) strip.insertBefore(node, before);
        else one.node.replaceWith(node);
        one = { node, rec: card, sig };
        drawn.set(card.id, one);
      }
      ordered.push(one.node);
    }
    // In the order of the list, all of them before the consent card.
    let next: Node | null = before;
    for (let i = ordered.length - 1; i >= 0; i--) {
      const node = ordered[i];
      if (node === undefined) continue;
      if (node.nextSibling !== next) strip.insertBefore(node, next);
      next = node;
    }
  };

  const stop = approvals.subscribe(render);
  render();

  return () => {
    alive = false;
    stop();
    for (const one of drawn.values()) one.node.remove();
    drawn.clear();
  };
}
