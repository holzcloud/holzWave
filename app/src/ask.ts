// The controller of the cards of `wsh ai` requests (docs/design.md section 6.20). It keeps one record for each request that
// waits in this window and makes the page's two commands through the injected `invoke`: the list of what waits (once, at start)
// and the one answer. No DOM and no bridge: the card (askcard.ts) draws the records and relays a person's press to `answer`,
// and nothing else calls it.
//
// Nothing is asked of the core at construction, on `receive`, on a closed event or on an error. An answer names the request,
// one of two words and the seal of the card exactly as it was read; it carries no text. A card is answered once, and a request
// that the core no longer knows is dropped, never retried. Nothing is stored and no timer runs.

import { WORDS, addAsk, answerFault, canRefuse, canSend, closedNotice, readAskEvent, readPending, removeAsk } from "./askstate.js";
import type { AskItem, AskView, Verdict } from "./askstate.js";

/** What the controller needs of the page around it. */
export interface AskDeps {
  /** Make a command of the shell. The list and the answer are the only commands this module makes. */
  invoke(cmd: string, args?: Record<string, unknown>): Promise<unknown>;
  /** The time in milliseconds. */
  now(): number;
  /** One sentence in the status line. */
  say(text: string): void;
}

export class AskQueue {
  private list: AskItem[] = [];
  private readonly listeners = new Set<() => void>();
  private signature = "";
  private disposed = false;
  /** The request that is on screen: its half second starts when it comes to the front. */
  private shown = "";

  constructor(private readonly deps: AskDeps) {}

  /** Call `fn` after every real change; the answer stops it. */
  subscribe(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  }

  /** The requests that wait in this window, oldest first. */
  get items(): readonly AskItem[] {
    return this.list;
  }

  /**
   * Ask the core once for the requests that wait for this window and merge them in: a request that is held keeps the time it was
   * first seen and takes the card as the core sent it now (a new seal), and none is removed. A refusal changes nothing.
   */
  async load(): Promise<void> {
    if (this.disposed) return;
    let raw: unknown;
    try {
      raw = await this.deps.invoke("ai_ask_pending");
    } catch {
      return;
    }
    if (this.disposed) return;
    for (const view of readPending(raw)) this.take(view);
    this.notify();
  }

  /** The core told the window something: a request waits, or one is over. Anything that cannot be read is ignored. */
  receive(raw: unknown): void {
    if (this.disposed) return;
    const event = readAskEvent(raw);
    if (event === null) return;
    if (event.kind === "waiting") {
      this.take(event.view);
      this.notify();
      return;
    }
    const held = this.list.find((item) => item.view.ask === event.ask);
    if (held === undefined) return;
    this.list = removeAsk(this.list, event.ask);
    this.notify();
    const notice = closedNotice(event.why, held.view.pane);
    if (notice !== null) this.deps.say(notice);
  }

  /** Make a card the controller holds: a new one is added, a known one is updated in place. */
  private take(view: AskView): void {
    const known = this.list.find((item) => item.view.ask === view.ask);
    if (known !== undefined) {
      known.view = view;
      return;
    }
    this.list = addAsk(this.list, { view, since: this.deps.now(), sending: false, fault: null });
  }

  /**
   * Answer the request `ask` with `verdict`. It acts only on a request that waits and for a press that counts: `Send once` only
   * after the window and for a real press, `Don't send` for any real press. It makes the one answer call with the request, the
   * word and the card's own seal. A refusal is told and never retried.
   */
  async answer(ask: string, verdict: Verdict, trusted: boolean): Promise<void> {
    if (this.disposed) return;
    const item = this.list.find((held) => held.view.ask === ask);
    if (item === undefined) return;
    const counts = verdict === "send" ? canSend(item, this.deps.now(), trusted) : verdict === "refuse" && canRefuse(item, trusted);
    if (!counts) return;
    const { seal } = item.view;
    item.sending = true;
    item.fault = null;
    this.notify();
    try {
      await this.deps.invoke("ai_ask_answer", { ask: item.view.ask, verdict, seal });
    } catch (raw) {
      if (this.disposed || !this.list.includes(item)) return;
      this.refused(item, answerFault(raw));
      return;
    }
    if (this.disposed) return;
    // The core took the answer and closed the card: the request is no longer here.
    this.list = removeAsk(this.list, item.view.ask);
    this.notify();
  }

  /** What a refusal does: an unknown request goes, a changed one is read again, any other keeps the card with its sentence. */
  private refused(item: AskItem, fault: { reason: string | null; text: string }): void {
    item.sending = false;
    if (fault.reason === "unknown_ask") {
      this.list = removeAsk(this.list, item.view.ask);
      this.notify();
      this.deps.say(WORDS.notWaiting);
      return;
    }
    item.fault = fault;
    this.notify();
    if (fault.reason === "seal_mismatch") void this.load();
  }

  /** Drop every record and listener; an answer that arrives later changes nothing. */
  dispose(): void {
    this.disposed = true;
    this.list = [];
    this.listeners.clear();
  }

  private notify(): void {
    // The oldest request is the one on screen. A request that comes to the front (the one before it is over) has its own window
    // for `Send once`, counted from now.
    const front = this.list[0];
    if (front === undefined) this.shown = "";
    else if (front.view.ask !== this.shown) {
      this.shown = front.view.ask;
      front.since = this.deps.now();
    }
    const next = JSON.stringify(
      this.list.map((item) => [item.view.ask, item.view.seal, item.sending, item.fault?.reason ?? null, item.fault?.text ?? null]),
    );
    if (next === this.signature) return;
    this.signature = next;
    for (const fn of [...this.listeners]) fn();
  }
}
