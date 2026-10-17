// The controller of one chat's waiting calls (docs/design.md section 6.15). It keeps one record per call that waits, keyed by
// its call id and the card's hash, and it makes the one decision command through the injected `invoke`. No DOM and no
// bridge: the card (approvalcard.ts) draws the records and relays a person's press to `decide`, and nothing else calls it.
//
// Nothing is asked of the core at construction, on `sync`, on a notification or on an error. A decision names the chat, the
// call, one of two words and the hash of the card exactly as it was read; it carries no path, no pattern and no text.
// A card is decided once and for its own hash only: nothing is kept for later.

import { MAX_CARDS, VERDICTS, WORDS, approvalFault, canAllow, canDeny, outcomeOf, readApproval } from "./approvalstate.js";
import type { Fault, Seen, Verdict } from "./approvalstate.js";
import type { PendingCall } from "./chatstate.js";

/**
 * An `Allow once` that comes this soon (in milliseconds) after the card was first seen is ignored: a click aimed at what
 * stood there before the card moved it cannot allow a call. `Deny` has no such window.
 */
export const ARM_MS = 500;

/** What the controller needs of the page around it. */
export interface ApprovalDeps {
  /** Make a command of the shell. The decision command is the only one this module makes. */
  invoke(cmd: string, args: Record<string, unknown>): Promise<unknown>;
  /** The id of the chat. */
  conversation(): string;
  /** The time in milliseconds. */
  now(): number;
  /** One line under the transcript. */
  say(text: string, kind: "note" | "error"): void;
  /** Ask the core once for the state of the chat. */
  refresh(): void;
}

/** Where a card is: waiting, travelling, answered, no longer waiting, or a request that cannot be allowed. */
export type CardMode = "ask" | "sending" | "sent" | "ended" | "cannot";

/** One card the page draws. */
export interface CardModel {
  id: string;
  seen: Seen;
  mode: CardMode;
  verdict: Verdict | null;
  fault: Fault | null;
  /** The time the page first saw this card with this hash. */
  since: number;
}

/** A record with the parts only this module needs. */
interface Rec extends CardModel {
  /** The call as it was read, so a card that changed under the same id is a new record. */
  key: string;
  serial: number;
}

export class Approvals {
  private records: Rec[] = [];
  private readonly listeners = new Set<() => void>();
  private signature = "";
  private serial = 0;
  private disposed = false;

  constructor(private readonly deps: ApprovalDeps) {}

  /** Call `fn` after every real change; the answer stops it. */
  subscribe(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  }

  /** The cards drawn, in the order the assistant asked: at most `MAX_CARDS`. */
  get cards(): readonly CardModel[] {
    return this.records.length > MAX_CARDS ? this.records.slice(0, MAX_CARDS) : this.records;
  }

  /** How many calls wait beyond the cards that are drawn. */
  get hidden(): number {
    return Math.max(0, this.records.length - MAX_CARDS);
  }

  /** How many calls wait for a decision: the cards that wait or travel, and those that can only be denied. */
  waiting(): number {
    let n = 0;
    for (const r of this.records) {
      if (r.mode === "ask" || r.mode === "sending" || (r.mode === "cannot" && r.seen.hash !== null)) n += 1;
    }
    return n;
  }

  /** Make the records follow the core's list of waiting calls. Asks nothing. */
  sync(pending: readonly PendingCall[]): void {
    if (this.disposed) return;
    const next: Rec[] = [];
    for (const entry of pending) {
      const seen = readApproval(entry);
      if (seen === null || next.some((r) => r.id === seen.id)) continue;
      const key = JSON.stringify(seen);
      const kept = this.records.find((r) => r.id === seen.id && r.key === key);
      next.push(kept ?? this.fresh(seen, key));
    }
    this.records = next;
    this.notify();
  }

  private fresh(seen: Seen, key: string): Rec {
    const mode: CardMode = seen.approval === null ? "cannot" : "ask";
    return { id: seen.id, seen, mode, verdict: null, fault: null, since: this.deps.now(), key, serial: ++this.serial };
  }

  /**
   * Decide the card `id` with `verdict`. It acts only on a card that waits: an `Allow once` only on one the page can show in
   * full and not within `ARM_MS` of its first sight, a `Deny` also on one that can only be denied. It makes the one decision
   * call with the ids and the card's own hash. A refusal is told and never retried.
   */
  async decide(id: string, verdict: Verdict): Promise<void> {
    if (this.disposed || !VERDICTS.includes(verdict)) return;
    const rec = this.records.find((r) => r.id === id);
    if (rec === undefined) return;
    const hash = rec.seen.hash;
    if (hash === null) return;
    if (verdict === "allow") {
      if (rec.mode !== "ask" || !canAllow(rec.seen) || this.deps.now() - rec.since < ARM_MS) return;
    } else if ((rec.mode !== "ask" && rec.mode !== "cannot") || !canDeny(rec.seen)) {
      return;
    }
    const before = rec.mode;
    rec.mode = "sending";
    rec.verdict = verdict;
    rec.fault = null;
    this.notify();
    try {
      await this.deps.invoke("ai_decide", { conversation: this.deps.conversation(), call_id: rec.seen.id, decision: verdict, hash });
    } catch (raw) {
      if (this.disposed || !this.records.includes(rec)) return;
      this.refused(rec, before, approvalFault(raw));
      return;
    }
    if (this.disposed || !this.records.includes(rec)) return;
    rec.mode = "sent";
    this.notify();
  }

  /** What a refusal does: an ended call becomes a quiet line, a changed request is read again, any other keeps the card. */
  private refused(rec: Rec, before: CardMode, fault: Fault): void {
    const outcome = outcomeOf(fault.reason);
    if (outcome === "ended") {
      rec.mode = "ended";
      rec.fault = null;
      this.notify();
      this.deps.say(WORDS.ended, "note");
      this.deps.refresh();
      return;
    }
    rec.mode = before;
    rec.fault = outcome === "changed" ? { reason: fault.reason, text: WORDS.changed } : fault;
    this.notify();
    if (outcome === "changed") this.deps.refresh();
  }

  /** Drop every record and listener; an answer that arrives later changes nothing. */
  dispose(): void {
    this.disposed = true;
    this.records = [];
    this.listeners.clear();
  }

  private notify(): void {
    const next = JSON.stringify(this.records.map((r) => [r.id, r.serial, r.mode, r.verdict, r.fault?.reason ?? null, r.fault?.text ?? null]));
    if (next === this.signature) return;
    this.signature = next;
    for (const fn of [...this.listeners]) fn();
  }
}
