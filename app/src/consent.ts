// The controller of one chat's sharing (docs/design.md section 6.14): which step the card is at, and the three context
// commands that move it. No DOM, no bridge: the commands go through the injected `invoke`, so a test drives it with a fake,
// and nothing is asked of the core at construction.
//
// The controller calls a command only from a user act (`open`, `share`, `unshare`, `preview`, each reached from a press, a
// button of the card or `attachContext`). It sends ids, kind names and a flag, never text, and it holds the labels, sizes and
// findings of an offer but never the text: the core has no command that returns one. It stores no text of any item and
// builds no sentence from a pane, a path or a label.

import { NOTES, contextFault, decideMode, heldOf, readContextState, readOffer, refusalStep, routeChange } from "./consentstate.js";
import type { ContextFault, CtxKind, CtxState, Held, Mode, Offer, Route } from "./consentstate.js";

/** What the controller needs from the page. */
export interface ConsentDeps {
  /** Run a command of the shell. A rejection is the object `{code, reason, text}`. */
  invoke(cmd: string, args: Record<string, unknown>): Promise<unknown>;
  /** The id of the chat. */
  conversation(): string;
  /** Where a message would go now: the pane, the provider, the saved entry and the two privacy settings. */
  route(): Route;
  /** The time in milliseconds. */
  now(): number;
  /** Say one line under the transcript. */
  say(text: string, kind: "note" | "error"): void;
}

/** What the card draws. */
export interface ConsentModel {
  mode: Mode;
  /** The command in flight: the state and the share are asked, the preview reads. */
  busy: "state" | "share" | "preview" | null;
  state: CtxState | null;
  offer: Offer | null;
  /** The kinds ticked in the pick list, in the order the state lists them. */
  ticked: CtxKind[];
  /** The numbers of the items of the offer that are unticked. */
  off: number[];
  fault: { reason: string | null; text: string } | null;
  /** When the offer was received, in milliseconds of `deps.now()`. */
  at: number;
  /** The pane the answers belong to. */
  pane: number | null;
  /** The kinds a caller asked to have ticked once the pane is shared. */
  prefill: CtxKind[];
}

/** How a caller wants its prefill read. */
export interface OpenOptions {
  /** The prefill is an order of preference: only the first kind of it that the state lists is ticked. */
  first?: boolean;
}

/** What the card says when an answer is not one the page understands. */
const NOT_UNDERSTOOD = "The answer was not understood.";

/** The sharing of one chat. */
export class Consent {
  private readonly m: ConsentModel = {
    mode: "closed",
    busy: null,
    state: null,
    offer: null,
    ticked: [],
    off: [],
    fault: null,
    at: 0,
    pane: null,
    prefill: [],
  };
  private readonly listeners = new Set<() => void>();
  /** Counts what makes an answer in flight out of date: a close, a dropped share, a disposal. */
  private generation = 0;
  /** The route of the latest question, kept to see a change. */
  private known: Route | null = null;
  private disposed = false;
  /** The prefill is an order of preference, kept with the prefill until the card closes. */
  private first = false;

  constructor(private readonly deps: ConsentDeps) {}

  /** Call `fn` after every change of the model; the returned function stops it. */
  subscribe(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => void this.listeners.delete(fn);
  }

  get model(): Readonly<ConsentModel> {
    return this.m;
  }

  private notify(): void {
    for (const fn of [...this.listeners]) fn();
  }

  // ---- the three commands ------------------------------------------------------------------------------------------

  /**
   * Ask the core where this chat's message would go and whether the pane is shared; the answer decides between asking
   * (`ask`) and picking (`pick`). `prefill` names kinds to tick once the pane may be read; with `options.first` it is an order
   * of preference and only the first kind of it that the state lists is ticked. Ignored while a command is in flight.
   */
  async open(prefill: CtxKind[] = [], options: OpenOptions = {}): Promise<void> {
    if (this.disposed || this.m.busy !== null) return;
    const route = this.deps.route();
    if (route.pane === null) return;
    this.known = route;
    this.m.pane = route.pane;
    this.m.offer = null;
    this.m.ticked = [];
    this.m.off = [];
    this.m.fault = null;
    this.m.prefill = [...prefill];
    this.first = options.first === true;
    const token = this.begin("state");
    let raw: unknown;
    try {
      raw = await this.deps.invoke("ai_context_state", {
        conversation: this.deps.conversation(),
        pane: route.pane,
        profile: route.profile,
        model: route.model,
      });
    } catch (e) {
      if (this.late(token)) return;
      await this.refuse(e, "state");
      return;
    }
    if (this.late(token)) return;
    this.take(raw);
    await this.follow();
  }

  /** Share the pane with this chat, for this route. */
  async share(): Promise<void> {
    await this.setShared(true);
  }

  /** Stop sharing the pane with this chat; an attached offer is dropped with it. */
  async unshare(): Promise<void> {
    await this.setShared(false);
  }

  private async setShared(shared: boolean): Promise<void> {
    this.check();
    const route = this.known;
    if (this.disposed || this.m.busy !== null || route === null || this.m.mode === "closed") return;
    if (!shared) {
      this.m.offer = null;
      this.m.off = [];
      this.m.ticked = [];
    }
    const token = this.begin("share");
    let raw: unknown;
    try {
      raw = await this.deps.invoke("ai_context_share", {
        conversation: this.deps.conversation(),
        pane: route.pane,
        profile: route.profile,
        model: route.model,
        shared,
      });
    } catch (e) {
      if (this.late(token)) return;
      await this.refuse(e, "share");
      return;
    }
    if (this.late(token)) return;
    this.take(raw);
    await this.follow();
  }

  /** Read the ticked kinds and show what would go out. */
  async preview(): Promise<void> {
    this.check();
    const route = this.known;
    const state = this.m.state;
    if (this.disposed || this.m.busy !== null || route === null || state === null) return;
    if (this.m.mode !== "pick" && this.m.mode !== "offer") return;
    const kinds = state.kinds.filter((kind) => this.m.ticked.includes(kind));
    if (kinds.length === 0) return;
    const token = this.begin("preview");
    let raw: unknown;
    try {
      raw = await this.deps.invoke("ai_context_preview", {
        conversation: this.deps.conversation(),
        pane: route.pane,
        profile: route.profile,
        model: route.model,
        kinds,
      });
    } catch (e) {
      if (this.late(token)) return;
      await this.refuse(e, "preview");
      return;
    }
    if (this.late(token)) return;
    const offer = readOffer(raw);
    if (offer === null) {
      this.m.busy = null;
      this.m.offer = null;
      this.m.fault = { reason: null, text: NOT_UNDERSTOOD };
      this.m.mode = "blocked";
      this.notify();
      return;
    }
    this.m.busy = null;
    this.m.offer = offer;
    this.m.off = [];
    this.m.at = this.deps.now();
    this.m.fault = null;
    this.m.mode = "offer";
    this.notify();
  }

  // ---- the model ---------------------------------------------------------------------------------------------------

  /** Tick or untick a kind of the pick list; only a kind the state lists, and only while picking. */
  tick(kind: CtxKind, on: boolean): void {
    const state = this.m.state;
    if (this.disposed || state === null || !state.kinds.includes(kind)) return;
    if (this.m.mode !== "pick" && this.m.mode !== "offer") return;
    const ticked = new Set(this.m.ticked);
    if (on) ticked.add(kind);
    else ticked.delete(kind);
    this.m.ticked = state.kinds.filter((k) => ticked.has(k));
    this.notify();
  }

  /** Tick or untick one item of the offer; an unticked item does not go out. */
  toggleItem(id: number, on: boolean): void {
    const offer = this.m.offer;
    if (this.disposed || this.m.mode !== "offer" || offer === null || !offer.items.some((item) => item.id === id)) return;
    const off = new Set(this.m.off);
    if (on) off.delete(id);
    else off.add(id);
    this.m.off = [...off].sort((a, b) => a - b);
    this.notify();
  }

  /** Throw the offer away and go back to the pick list. */
  discard(): void {
    if (this.disposed || this.m.mode !== "offer") return;
    this.m.offer = null;
    this.m.off = [];
    this.m.fault = null;
    this.m.mode = "pick";
    this.notify();
  }

  /**
   * What a send names: the offer and its ticked items; null unless the card shows an offer with at least one ticked item
   * that has not expired.
   */
  held(): Held | null {
    const offer = this.m.offer;
    if (this.disposed || this.m.mode !== "offer" || offer === null || this.expired()) return null;
    return heldOf(offer, this.m.off);
  }

  /** Whether the attached offer is older than the time the core gave it. An expired offer is never sent. */
  expired(): boolean {
    const offer = this.m.offer;
    return offer !== null && this.deps.now() - this.m.at >= offer.ttl_seconds * 1000;
  }

  /**
   * One sentence when a send must wait: the card is asking or reading, or a kind is ticked that was never previewed, so the
   * message would go out without what the user ticked. Null when the send may go on.
   */
  blocksSend(): string | null {
    if (this.disposed) return null;
    if (this.m.mode === "busy") return NOTES.wait;
    if (this.m.mode === "pick" && this.m.ticked.length > 0) return NOTES.previewFirst;
    return null;
  }

  /**
   * A send was rejected. A refusal that says the offer is no longer good (`stale_offer`, `not_shared`, `bad_pick`, or the
   * same content sent twice) drops it, closes the card and says why; the draft stays. Any other rejection keeps the offer
   * and answers false, so the caller tells it as it tells any refusal.
   */
  refused(raw: unknown): boolean {
    if (this.disposed) return false;
    const fault = contextFault(raw);
    const dead =
      fault.code === "context_duplicate" ||
      (fault.code === "context" && (fault.reason === "stale_offer" || fault.reason === "not_shared" || fault.reason === "bad_pick"));
    if (!dead) return false;
    this.drop();
    this.notify();
    this.deps.say(fault.text, "error");
    return true;
  }

  /**
   * Compare where a message would go now with where the share was made. A different pane, provider, model, saved address or
   * privacy setting ends the page's knowledge of the share and drops an attached offer; the status line says so when
   * something was attached or shared, and the next `open` asks the core again. Asks the core nothing.
   */
  check(): void {
    const before = this.known;
    if (this.disposed || before === null) return;
    const now = this.deps.route();
    const change = routeChange(before, now);
    if (change === "same") return;
    if (change === "adopt") {
      this.known = now;
      return;
    }
    const attached = this.m.offer !== null;
    const shared = this.m.state !== null && this.m.state.shared;
    const open = this.m.mode !== "closed";
    this.drop();
    this.m.state = null;
    this.known = null;
    if (change === "provider" && (attached || shared)) this.deps.say(NOTES.provider, "note");
    else if (change === "pane" && attached) this.deps.say(NOTES.pane, "note");
    else if (change === "settings" && attached) this.deps.say(NOTES.settings, "note");
    if (open) this.notify();
  }

  /** The offer is spent by a send: the card closes and nothing is attached. The share stays in the core. */
  sent(): void {
    if (this.disposed) return;
    this.drop();
    this.notify();
  }

  /** Close the card. An attached offer is dropped with it, so nothing rides along unseen. */
  close(): void {
    if (this.disposed) return;
    this.drop();
    this.notify();
  }

  /** Make every answer in flight harmless and stop telling the listeners. */
  dispose(): void {
    this.disposed = true;
    this.generation += 1;
    this.listeners.clear();
  }

  // ---- the steps ---------------------------------------------------------------------------------------------------

  /** Forget the offer and the ticks and close; an answer in flight is out of date. */
  private drop(): void {
    this.generation += 1;
    this.m.mode = "closed";
    this.m.busy = null;
    this.m.offer = null;
    this.m.off = [];
    this.m.ticked = [];
    this.m.fault = null;
    this.m.prefill = [];
    this.first = false;
  }

  /** Mark a command as started and answer the token that tells later whether its answer is still wanted. */
  private begin(kind: "state" | "share" | "preview"): number {
    this.m.busy = kind;
    this.m.mode = "busy";
    this.notify();
    return this.generation;
  }

  /** Whether the answer of the command that began with `token` is out of date. */
  private late(token: number): boolean {
    return this.disposed || token !== this.generation;
  }

  /** Take the answer of the state or the share command: it decides the mode. */
  private take(raw: unknown): void {
    this.m.busy = null;
    const state = readContextState(raw);
    if (state === null) {
      this.m.state = null;
      this.m.fault = { reason: null, text: NOT_UNDERSTOOD };
      this.m.mode = "blocked";
      this.notify();
      return;
    }
    this.m.state = state;
    this.m.ticked = state.kinds.filter((kind) => this.m.ticked.includes(kind));
    this.m.mode = decideMode(state);
    // A pane that offers nothing is told in the words of the core's reason for it.
    this.m.fault =
      this.m.mode === "blocked"
        ? { reason: "nothing_to_share", text: contextFault({ code: "context", reason: "nothing_to_share", text: "" }).text }
        : null;
    this.notify();
  }

  /**
   * A command was refused: the step it leads to follows the reason (`refusalStep`). `not_shared` from a preview asks the
   * state once more (the share ended in the core) and the answer decides; every other refusal shows the core's words.
   */
  private async refuse(raw: unknown, from: "state" | "share" | "preview"): Promise<void> {
    const fault: ContextFault = contextFault(raw);
    const step = refusalStep(fault, from);
    this.m.busy = null;
    if (step === "again") {
      this.m.offer = null;
      this.m.off = [];
      this.m.fault = null;
      await this.again();
      return;
    }
    if (from === "preview") {
      this.m.offer = null;
      this.m.off = [];
    }
    this.m.fault = { reason: fault.reason, text: fault.text };
    this.m.mode = step;
    this.notify();
  }

  /** The one automatic second state call: after `not_shared` the core's answer says whether to ask for the share again. */
  private async again(): Promise<void> {
    const route = this.known;
    if (route === null) return;
    const token = this.begin("state");
    let raw: unknown;
    try {
      raw = await this.deps.invoke("ai_context_state", {
        conversation: this.deps.conversation(),
        pane: route.pane,
        profile: route.profile,
        model: route.model,
      });
    } catch (e) {
      if (this.late(token)) return;
      const fault = contextFault(e);
      this.m.busy = null;
      this.m.fault = { reason: fault.reason, text: fault.text };
      this.m.mode = "blocked";
      this.notify();
      return;
    }
    if (this.late(token)) return;
    this.take(raw);
  }

  /** After an answer that left the pick list: the kinds a caller asked for are ticked and read at once. */
  private async follow(): Promise<void> {
    if (this.m.mode !== "pick" || this.m.prefill.length === 0 || this.m.state === null) return;
    const listed = this.m.state.kinds;
    const preferred = this.first ? this.m.prefill.find((kind) => listed.includes(kind)) : undefined;
    const wanted = this.first ? (preferred === undefined ? [] : [preferred]) : listed.filter((kind) => this.m.prefill.includes(kind));
    this.m.prefill = [];
    if (wanted.length === 0) return;
    this.m.ticked = wanted;
    this.notify();
    await this.preview();
  }
}
