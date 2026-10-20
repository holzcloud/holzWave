// The attention marks of the bar (docs/design.md section 6.23): which kinds of attention wait on the tabs and the
// workspace pills, derived from the panes that are out of view. Pure: no DOM, no bridge and no page state, so node can
// test it. The store holds pane ids and kinds only, never a command, an output or the text of a card.
//
// Two sources feed it. Events of the core (a bell, a command end) are latched per pane until the pane is shown. A
// waiting question (an approval card, a consent card, a `wsh ai` request) is not latched: a provider says which panes
// have one right now, and the mark exists exactly while that is so and the pane is out of view.

/** What a pane out of view wants: `ask` waits for an answer, `bell` rang, `failed` and `done` are a command that ended. */
export type Kind = "ask" | "bell" | "failed" | "done";

/**
 * The order the marks are drawn in, left to right: ask, bell, failed, done. A tab or a pill shows every kind that is
 * present, side by side; this list fixes only the places, it ranks nothing.
 */
export const PRIORITY: readonly Kind[] = ["ask", "bell", "failed", "done"];

/** The glyph (a character of the bundled mono face) and the words of each kind. The words are the second cue. */
export const MARKS: Readonly<Record<Kind, { glyph: string; words: string }>> = {
  ask: { glyph: "?", words: "waits for you" },
  bell: { glyph: "▲", words: "rang the bell" },
  failed: { glyph: "!", words: "command failed" },
  done: { glyph: "✓", words: "command finished" },
};

/** The kinds that are latched per pane until it is shown; `ask` is derived and never latched. */
export type LatchedKind = Exclude<Kind, "ask">;

/** One drawn mark. */
export interface Mark {
  kind: Kind;
  glyph: string;
  words: string;
}

/** Everything one tab or pill shows: its marks in the fixed order, and their words joined for the title. */
export interface Marking {
  marks: Mark[];
  /** The words of every kind present, in the fixed order, joined with a comma. */
  words: string;
}

/** The marks of the bar, by index: `null` where nothing shows (the shown tab, the shown workspace, nothing waiting). */
export interface BarMarks {
  tabs: (Marking | null)[];
  workspaces: (Marking | null)[];
}

/** The part of a view the store reads. `pane_ids` is optional here so a view from before it existed marks nothing. */
export interface MarkedView {
  workspaces: readonly { active: boolean; pane_ids?: unknown }[];
  tabs: readonly { active: boolean; pane_ids?: unknown }[];
}

/** The most panes the store keeps latched kinds for; the oldest is dropped beyond it. */
export const MAX_LATCHED = 256;

/** The most pane ids read from one tab or workspace of a view. */
const MAX_IDS = 1024;

/** The pane ids of a view entry: an array of safe integers, at most 1024; anything else is no panes. */
export function idsOf(raw: unknown): number[] {
  if (!Array.isArray(raw)) return [];
  const out: number[] = [];
  for (const id of raw) {
    if (typeof id === "number" && Number.isSafeInteger(id)) out.push(id);
    if (out.length >= MAX_IDS) break;
  }
  return out;
}

/** The marking of a set of kinds in the fixed order; null for an empty set. */
function marking(kinds: ReadonlySet<Kind>): Marking | null {
  if (kinds.size === 0) return null;
  const marks: Mark[] = [];
  for (const kind of PRIORITY) {
    if (kinds.has(kind)) marks.push({ kind, glyph: MARKS[kind].glyph, words: MARKS[kind].words });
  }
  return { marks, words: marks.map((m) => m.words).join(", ") };
}

export class Attention {
  /** The panes whose tab is shown (every pane of the shown tab). */
  private shown = new Set<number>();
  /** The latched kinds per pane, oldest pane first. */
  private latched = new Map<number, Set<LatchedKind>>();
  /** The providers of waiting questions, by source: each answers which panes have one now. */
  private sources = new Map<string, () => Iterable<number>>();
  /** The panes the providers named when subscribers were last told, as text, so a repeat is no change. */
  private asking = "";
  private listeners = new Set<() => void>();

  /** Register `source`, which says which panes wait for an answer; the result takes it away again. */
  provide(source: string, fn: () => Iterable<number>): () => void {
    this.sources.set(source, fn);
    return () => {
      if (this.sources.get(source) === fn) this.sources.delete(source);
      this.changed();
    };
  }

  /** Be told after a real change of what the bar should show (a latch, a forget, a change of who waits). */
  subscribe(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  }

  /** The panes that wait for an answer now, from every provider. A provider that throws names none. */
  private askingPanes(): Set<number> {
    const out = new Set<number>();
    for (const fn of this.sources.values()) {
      let panes: Iterable<number> = [];
      try {
        panes = fn();
      } catch {
        // a provider is a hint; one that fails marks nothing
      }
      for (const pane of panes) if (Number.isSafeInteger(pane)) out.add(pane);
    }
    return out;
  }

  /** Tell the subscribers once if who waits for an answer is not what it was when they were last told. */
  changed(): void {
    const now = [...this.askingPanes()].sort((a, b) => a - b).join(",");
    if (now === this.asking) return;
    this.asking = now;
    this.notify();
  }

  private notify(): void {
    for (const fn of [...this.listeners]) fn();
  }

  /**
   * Latch `kind` on `pane`. A pane in view latches nothing. Returns whether anything changed; subscribers are told
   * once per change and not at all for a repeat.
   */
  note(pane: number, kind: LatchedKind): boolean {
    if (!Number.isSafeInteger(pane) || this.shown.has(pane)) return false;
    const kinds = this.latched.get(pane);
    if (kinds?.has(kind)) return false;
    if (kinds) {
      kinds.add(kind);
    } else {
      while (this.latched.size >= MAX_LATCHED) {
        const oldest = this.latched.keys().next();
        if (oldest.done) break;
        this.latched.delete(oldest.value);
      }
      this.latched.set(pane, new Set([kind]));
    }
    this.notify();
    return true;
  }

  /** The pane ended or left this window: nothing of it stays. Returns whether it held anything. */
  forget(pane: number): boolean {
    if (!this.latched.delete(pane)) return false;
    this.notify();
    return true;
  }

  /**
   * Take the panes in view and the panes the window knows. Showing a pane clears its latched kinds (the person has seen
   * it); a pane that is in no tab any more is dropped. It does not notify: the caller draws the bar next.
   */
  follow(shown: Iterable<number>, known: Iterable<number>): void {
    this.shown = new Set(shown);
    const all = new Set(known);
    for (const pane of [...this.latched.keys()]) {
      if (this.shown.has(pane) || !all.has(pane)) this.latched.delete(pane);
    }
  }

  /** The kinds a pane holds that is out of view: its latched ones and a waiting question. */
  private kindsOf(pane: number, asking: ReadonlySet<number>, into: Set<Kind>): void {
    if (this.shown.has(pane)) return;
    for (const kind of this.latched.get(pane) ?? []) into.add(kind);
    if (asking.has(pane)) into.add("ask");
  }

  /**
   * The marks of every tab and every workspace pill of `view`. The shown tab and the shown workspace are always null.
   * A pill holds the kinds of all the panes of its workspace together, once each. A view without `pane_ids` marks nothing.
   */
  marks(view: MarkedView): BarMarks {
    const asking = this.askingPanes();
    const of = (entry: { active: boolean; pane_ids?: unknown }): Marking | null => {
      if (entry.active) return null;
      const kinds = new Set<Kind>();
      for (const pane of idsOf(entry.pane_ids)) this.kindsOf(pane, asking, kinds);
      return marking(kinds);
    };
    return { tabs: view.tabs.map(of), workspaces: view.workspaces.map(of) };
  }
}

/** What the chat module tells about one conversation: how many tool calls wait for a decision, whether a consent card asks. */
export interface ChatWait {
  id: string;
  approvalsWaiting: number;
  consentAsking: boolean;
}

/** The part of a pane's block that says which pane shows which chat. */
export interface ChatBlock {
  pane: number;
  kind: string;
  /** For a chat, the id of the conversation. */
  path: string;
}

/**
 * The panes whose chat waits for the person: a conversation with an approval card waiting, or a consent card in mode
 * `ask`, marks the chat pane that shows it. Sorted and unique. A conversation with no chat pane (the sidebar form) marks
 * nothing, and a block of another kind is not a chat pane. Only ids are read, never a card's text.
 */
export function chatWaitingPanes(sessions: Iterable<ChatWait>, blocks: Iterable<ChatBlock>): number[] {
  const waiting = new Set<string>();
  for (const s of sessions) if (s.approvalsWaiting > 0 || s.consentAsking) waiting.add(s.id);
  const out = new Set<number>();
  if (waiting.size === 0) return [];
  for (const b of blocks) if (b.kind === "chat" && waiting.has(b.path) && Number.isSafeInteger(b.pane)) out.add(b.pane);
  return [...out].sort((a, b) => a - b);
}

/** The one store of the page. */
export const attention = new Attention();
