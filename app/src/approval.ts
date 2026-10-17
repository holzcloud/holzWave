// The controller of one chat's waiting calls (docs/design.md section 6.15). It keeps one record per call that waits, keyed by
// its call id and the card's hash, and it makes the one decision command through the injected `invoke`. No DOM and no
// bridge: the card (approvalcard.ts) draws the records and relays a person's press to `decide`, and nothing else calls it.
//
// Nothing is asked of the core at construction, on `sync`, on a notification or on an error. A decision names the chat, the
// call, one of two words and the hash of the card exactly as it was read; it carries no path, no pattern and no text.
// A card is decided once and for its own hash only: nothing is kept for later.

import { approvalFault, canAllow, canDeny, readApproval } from "./approvalstate.js";
import type { Fault, Seen, Verdict } from "./approvalstate.js";
import type { PendingCall } from "./chatstate.js";

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

  /** The cards, in the order the assistant asked. */
  get cards(): readonly CardModel[] {
    return this.records;
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
    return { id: seen.id, seen, mode: "ask", verdict: null, fault: null, since: this.deps.now(), key, serial: ++this.serial };
  }

  /**
   * Decide the card `id` with `verdict`. It acts only on a card that waits, and makes the one decision call with the ids and
   * the card's own hash. A refusal is kept on the card and never retried.
   */
  async decide(id: string, verdict: Verdict): Promise<void> {
    if (this.disposed) return;
    const rec = this.records.find((r) => r.id === id);
    if (rec === undefined || rec.mode !== "ask") return;
    const hash = rec.seen.hash;
    if (hash === null || (verdict === "allow" ? !canAllow(rec.seen) : !canDeny(rec.seen))) return;
    rec.mode = "sending";
    rec.verdict = verdict;
    rec.fault = null;
    this.notify();
    try {
      await this.deps.invoke("ai_decide", { conversation: this.deps.conversation(), call_id: rec.seen.id, decision: verdict, hash });
    } catch (raw) {
      if (this.disposed || !this.records.includes(rec)) return;
      rec.mode = "ask";
      rec.fault = approvalFault(raw);
      this.notify();
      return;
    }
    if (this.disposed || !this.records.includes(rec)) return;
    rec.mode = "sent";
    this.notify();
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
