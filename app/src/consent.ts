// The controller of one chat's sharing (docs/design.md section 6.14): which step the card is at, and the three context
// commands that move it. No DOM, no bridge: the commands go through the injected `invoke`, so a test drives it with a fake,
// and nothing is asked of the core at construction.
//
// The controller calls a command only from a user act (`open`, `share`, `unshare`, `preview`, each reached from a press, a
// button of the card or `attachContext`). It sends ids, kind names and a flag, never text, and it holds the labels, sizes and
// findings of an offer but never the text: the core has no command that returns one. It stores no text of any item and
// builds no sentence from a pane, a path or a label.

import { faultText } from "./chatview.js";
import { decideMode, heldOf, readContextState, readOffer } from "./consentstate.js";
import type { CtxKind, CtxState, Held, Mode, Offer, Route } from "./consentstate.js";

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

/** What the card says when an answer is not one the page understands. */
const NOT_UNDERSTOOD = "The answer was not understood.";

/** A rejection read as `{reason, text}`: the core's code `context` names a reason; any other code has no reason. */
function readFault(raw: unknown): { reason: string | null; text: string } {
  let reason: string | null = null;
  if (typeof raw === "object" && raw !== null) {
    const r = raw as { code?: unknown; reason?: unknown };
    if (r.code === "context" && typeof r.reason === "string") reason = r.reason.slice(0, 40);
  }
  return { reason, text: faultText(raw) };
}

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
   * (`ask`) and picking (`pick`). `prefill` names kinds to tick once the pane may be read. Ignored while a command is in
   * flight.
   */
  async open(prefill: CtxKind[] = []): Promise<void> {
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
      this.fail(e, "blocked");
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
      this.fail(e, "blocked");
      return;
    }
    if (this.late(token)) return;
    this.take(raw);
    await this.follow();
  }

  /** Read the ticked kinds and show what would go out. */
  async preview(): Promise<void> {
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
      this.fail(e, "pick");
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

  /** What a send names: the offer and its ticked items; null unless the card shows an offer with at least one ticked. */
  held(): Held | null {
    const offer = this.m.offer;
    if (this.disposed || this.m.mode !== "offer" || offer === null) return null;
    return heldOf(offer, this.m.off);
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
    this.m.fault = null;
    this.m.ticked = state.kinds.filter((kind) => this.m.ticked.includes(kind));
    this.m.mode = decideMode(state);
    this.notify();
  }

  /** A command was refused: show the core's words and go to `mode`. */
  private fail(raw: unknown, mode: "blocked" | "pick"): void {
    this.m.busy = null;
    this.m.fault = readFault(raw);
    this.m.mode = mode;
    this.notify();
  }

  /** After an answer that left the pick list: the kinds a caller asked for are ticked and read at once. */
  private async follow(): Promise<void> {
    if (this.m.mode !== "pick" || this.m.prefill.length === 0 || this.m.state === null) return;
    const wanted = this.m.state.kinds.filter((kind) => this.m.prefill.includes(kind));
    this.m.prefill = [];
    if (wanted.length === 0) return;
    this.m.ticked = wanted;
    this.notify();
    await this.preview();
  }
}
