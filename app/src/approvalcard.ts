// The approval card (docs/design.md section 6.15): one strip entry for each call that waits for a decision. It draws the
// records of an `Approvals` controller with text nodes and native controls only: every field the core sent is drawn in its
// order, and every word that is not the core's comes from `approvalstate.ts`. No command is called here: a press calls
// `decide` of the controller, which makes the one call.
//
// A card never takes the keyboard when it appears or changes. It is added to the strip before the consent card and removed
// one node at a time, so the consent card is never touched. Only a person decides: a click or a key press that a script
// made (`isTrusted` false) is ignored, `Escape` with the focus inside a card denies that call, and nothing else does.

import type { Approvals, CardModel } from "./approval.js";
import {
  EFFECT_WORDS, TOOL_WORDS, WORDS, cannotWords, destinationParts, diffHead, gapWords, hashWords, moreWords, showText, stateMark, stateWord,
  targetWords, taintWords, toggleWords,
} from "./approvalstate.js";
import type { Destination, Diff, ToolRow } from "./approvalstate.js";
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

/** Numbers the heads of the diff boxes, so each box is named by its own head line. */
let diffNumber = 0;

/**
 * The changes of a write as the core sent them: a head line (kind, counts, sizes) and one row for each row of the core, in its
 * order. A row has the old and the new line number (empty where the core sent none), the sign the page draws from the
 * operation (`+`, `-` or none) and the text, a text node in its own cell. A gap row says how many unchanged lines it stands for;
 * a note row says the core's sentence. The page works out nothing from a row's text.
 */
function diffBox(parent: Element, diff: Diff): HTMLElement {
  const box = el("div", "approval-diff", parent);
  box.tabIndex = 0;
  box.dataset.field = "diff";
  box.dataset.key = "diff";
  box.dataset.kind = diff.kind;
  box.setAttribute("role", "group");
  const words = diffHead(diff);
  const head = el("p", "diff-head", box);
  head.id = `diff-head-${++diffNumber}`;
  box.setAttribute("aria-labelledby", head.id);
  el("span", "diff-kind", head).textContent = words.kind;
  el("span", "diff-count", head).textContent = words.counts;
  el("span", "diff-size", head).textContent = words.size;
  const list = el("ol", "diff-rows", box);
  for (const row of diff.rows) {
    const li = el("li", "diff-row", list);
    li.dataset.op = row.op;
    if (row.op === "gap") {
      el("span", "diff-gap", li).textContent = `⋯ ${gapWords(row.skipped)}`;
    } else if (row.op === "note") {
      el("span", "diff-note", li).textContent = row.text;
    } else {
      el("span", "diff-old", li).textContent = row.old === null ? "" : String(row.old);
      el("span", "diff-new", li).textContent = row.new === null ? "" : String(row.new);
      el("span", "diff-sign", li).textContent = row.op === "add" ? "+" : row.op === "del" ? "-" : "";
      el("span", "diff-text", li).textContent = row.text;
    }
  }
  return box;
}

/** The title of a card by its mode: the question, or the reason it can only be denied. */
function titleWords(card: CardModel): string {
  if (card.mode !== "cannot") return WORDS.title;
  return card.seen.why === "large" ? WORDS.cannotTitleLarge : WORDS.cannotTitle;
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
  /** The line that counts the calls beyond the cards that are drawn; made when there are some. */
  let more: HTMLElement | null = null;

  /** The first section of the consent card, which the approval cards stay in front of. */
  const anchor = (): Element | null => strip.querySelector(":scope > section.chat-consent");

  /** A press on a button: only a real one decides, and the keyboard goes back to the message box. */
  const press = (e: Event, id: string, verdict: "allow" | "deny"): void => {
    if (!e.isTrusted) return;
    void approvals.decide(id, verdict);
    view.focusComposer();
  };

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
    title.append(document.createTextNode(` ${titleWords(card)}`));
    el("span", "approval-tool", head).textContent = toolWords(seen.tool);
    if (a !== null) {
      el("span", "approval-where", head).textContent = targetWords(a.pane, a.target);
      if (a.remote) el("span", "approval-remote", head).textContent = WORDS.remote;
      const effect = Object.hasOwn(EFFECT_WORDS, a.effect) ? EFFECT_WORDS[a.effect] : undefined;
      if (effect !== undefined) el("p", "approval-effect", section).textContent = effect;
      // A write shows what it would change before the fields of the request, so the person reads the changes first.
      if (a.diff !== undefined) diffBox(section, a.diff);

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
      const line = el("p", "approval-dest", section);
      for (const part of destinationParts(view.destination(), a.effect)) {
        if (part.kind === "text") {
          line.append(document.createTextNode(part.text));
          continue;
        }
        const piece = el("span", `approval-${part.kind}`, line);
        piece.textContent = part.text;
        if (part.kind === "place") piece.dataset.leaves = part.leaves ? "true" : "false";
      }
      // Text the user did not write is in this turn: the core's labels, in words, and what that text can do.
      if (a.tainted) {
        const words = taintWords(a.taint, a.effect);
        const banner = el("div", "approval-taint", section);
        banner.dataset.tone = "warn";
        const mark = el("span", "approval-taint-mark", banner);
        mark.textContent = "!";
        mark.setAttribute("aria-hidden", "true");
        el("span", "approval-taint-title label", banner).textContent = words.title;
        el("p", "approval-taint-text", banner).textContent = words.text;
      }
    }
    if (seen.hash !== null) el("p", "approval-hash", section).textContent = hashWords(seen.hash);
    if (card.mode === "cannot") {
      el("p", "approval-verdict", section).textContent = seen.hash === null ? WORDS.noHash : cannotWords(seen.why);
    }
    if (card.fault !== null) {
      const fault = el("p", "approval-fault", section);
      fault.dataset.kind = "error";
      fault.textContent = card.fault.text;
    }
    const canAsk = card.mode === "ask" || card.mode === "sending";
    if (canAsk || (card.mode === "cannot" && seen.hash !== null)) {
      const actions = el("div", "approval-actions", section);
      const busy = card.mode === "sending";
      if (canAsk) {
        const allow = button(actions, "approval-allow", "allow", WORDS.allow);
        allow.disabled = busy;
        allow.addEventListener("click", (e) => press(e, seen.id, "allow"));
      }
      const deny = button(actions, "approval-deny", "deny", WORDS.deny);
      deny.disabled = busy;
      deny.addEventListener("click", (e) => press(e, seen.id, "deny"));
    }
    if (card.mode === "sending") el("p", "approval-verdict", section).textContent = WORDS.sending;
    if (card.mode === "sent") el("p", "approval-verdict", section).textContent = card.verdict === "allow" ? WORDS.allowed : WORDS.denied;
    if (card.mode === "ended") el("p", "approval-verdict", section).textContent = WORDS.ended;
    if (canAsk) el("p", "approval-foot", section).textContent = WORDS.foot;
    section.addEventListener("keydown", (e) => {
      if (e.key !== "Escape" || e.isComposing) return;
      // The key is the card's own: it never reaches the message box (which would stop the reply) or the consent card.
      e.preventDefault();
      e.stopPropagation();
      if (!e.isTrusted) return;
      void approvals.decide(seen.id, "deny");
      view.focusComposer();
    });
    return section;
  };

  const render = (): void => {
    if (!alive) return;
    const before = anchor();
    const cards = approvals.cards;
    const live = new Set(cards.map((c) => c.id));
    for (const [id, one] of drawn) {
      if (live.has(id)) continue;
      // A card that goes while the focus is in it hands the keyboard to the message box, not to the page body.
      const held = one.node.contains(document.activeElement);
      one.node.remove();
      drawn.delete(id);
      if (held) view.focusComposer();
    }
    const ordered: HTMLElement[] = [];
    for (const card of cards) {
      const sig = JSON.stringify([card.seen.hash, card.mode, card.verdict, card.fault]);
      let one = drawn.get(card.id);
      if (one === undefined || one.rec !== card || one.sig !== sig) {
        const node = build(card);
        if (one === undefined) {
          strip.insertBefore(node, before);
        } else {
          // The control that had the focus is drawn again in place; a card that has none left hands the focus to the message box.
          const active = document.activeElement;
          const held = one.node.contains(active) ? ((active as HTMLElement).dataset.key ?? "") : null;
          one.node.replaceWith(node);
          if (held !== null) {
            const same = node.querySelector<HTMLElement>(`[data-key="${held}"]`);
            if (same !== null && !(same instanceof HTMLButtonElement && same.disabled)) same.focus({ preventScroll: true });
            else view.focusComposer();
          }
        }
        one = { node, rec: card, sig };
        drawn.set(card.id, one);
      }
      ordered.push(one.node);
    }
    if (approvals.hidden > 0) {
      more ??= el("p", "approval-more");
      const words = moreWords(approvals.hidden);
      if (more.textContent !== words) more.textContent = words;
      ordered.push(more);
    } else if (more !== null) {
      more.remove();
      more = null;
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
    more?.remove();
    more = null;
  };
}

/** Numbers the result boxes, so a toggle can name the box it opens. */
let resultNumber = 0;

/**
 * One tool call of the transcript: a mark, the words of the tool, the core's one-line summary and the state word, and, when
 * there is a result, a toggle that opens it as text in a box labelled untrusted. The summary and the result are text nodes:
 * they are never parsed, linked or turned into elements. `toggle` is called by a press on the toggle.
 */
export function toolRowNode(row: ToolRow, open: boolean, toggle: () => void): HTMLElement {
  const node = el("div", "chat-tool");
  node.dataset.call = row.id;
  node.dataset.state = row.state;
  node.dataset.tool = showText(row.name);
  const mark = el("span", "tool-mark", node);
  mark.textContent = stateMark(row.state);
  mark.setAttribute("aria-hidden", "true");
  el("span", "tool-words", node).textContent = toolWords(row.name);
  el("span", "tool-summary", node).textContent = row.summary;
  el("span", "tool-state", node).textContent = stateWord(row.state);
  if (row.content !== null) {
    const toggler = button(node, "tool-toggle", `result-${row.id}`, toggleWords(open, row.bytes));
    toggler.setAttribute("aria-expanded", open ? "true" : "false");
    toggler.addEventListener("click", (e) => {
      if (e.isTrusted) toggle();
    });
    if (open) {
      const id = `tool-result-${++resultNumber}`;
      toggler.setAttribute("aria-controls", id);
      el("div", "tool-untrusted label", node).textContent = WORDS.resultLabel;
      const box = el("pre", "tool-result", node);
      box.id = id;
      box.textContent = row.content;
    }
  }
  return node;
}
