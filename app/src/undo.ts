// The controller of one chat's undo presses (docs/design.md section 6.16): one model for every finished write whose Undo button was
// drawn, and the one undo command through the injected `invoke`. No DOM and no bridge: the tool row (approvalcard.ts) draws the
// models and relays a person's press to `undo`, and nothing else calls it.
//
// Nothing is asked of the core at construction, on `subscribe` or on a read. A press names the chat and the call and nothing else:
// no path and no text. A refusal is told with the core's own sentence and never retried; the next press is the person's again.

import { UNDO_GONE, undoFault } from "./approvalstate.js";

/** What the controller needs of the page around it. */
export interface UndoDeps {
  /** Make a command of the shell. The undo command is the only one this module makes. */
  invoke(cmd: string, args: Record<string, unknown>): Promise<unknown>;
  /** The id of the chat. */
  conversation(): string;
}

/** Where an undo is: not pressed (or refused and open to another press), travelling, done, or no longer possible. */
export type UndoState = "idle" | "sending" | "done" | "gone";

/** One finished write as far as undo is concerned: its call id, where its undo is, and the refusal that is shown, if any. */
export interface UndoModel {
  id: string;
  state: UndoState;
  fault: { reason: string | null; text: string } | null;
}

export class Undos {
  private readonly models = new Map<string, UndoModel>();
  private readonly listeners = new Set<() => void>();
  private disposed = false;

  constructor(private readonly deps: UndoDeps) {}

  /** Call `fn` after every real change; the answer stops it. */
  subscribe(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  }

  /** The model of the call `id`; a call nobody pressed Undo for is `idle`. Reading asks the core nothing. */
  model(id: string): UndoModel {
    let model = this.models.get(id);
    if (model === undefined) {
      model = { id, state: "idle", fault: null };
      this.models.set(id, model);
    }
    return model;
  }

  /**
   * Press Undo for the call `id`. It acts only on a call that is `idle`, once at a time, and makes the one undo command with the
   * chat and the call. `done` and `gone` ignore it. A refusal is told and never retried: a refusal that says the change is no
   * longer known (`unknown_change`, `created`) leaves the call `gone`, any other leaves it `idle` for another press.
   */
  async undo(id: string): Promise<void> {
    if (this.disposed) return;
    const model = this.model(id);
    if (model.state !== "idle") return;
    model.state = "sending";
    model.fault = null;
    this.notify();
    try {
      await this.deps.invoke("ai_undo", { conversation: this.deps.conversation(), call_id: id });
    } catch (raw) {
      if (this.disposed) return;
      const fault = undoFault(raw);
      model.fault = fault;
      model.state = fault.reason !== null && UNDO_GONE.has(fault.reason) ? "gone" : "idle";
      this.notify();
      return;
    }
    if (this.disposed) return;
    model.state = "done";
    model.fault = null;
    this.notify();
  }

  /** Drop every model and listener; an answer that arrives later changes nothing. */
  dispose(): void {
    this.disposed = true;
    this.models.clear();
    this.listeners.clear();
  }

  private notify(): void {
    for (const fn of [...this.listeners]) fn();
  }
}
