// The pure half of the palette actions of the chat and of the reply action (docs/design.md section 6.19): every sentence the
// page says for them, the choice of the source an explanation attaches, the verdict on a code block of a reply and the one
// slot that keeps an intent until its chat is drawn. It imports only a type, touches no DOM and no bridge, and nothing here
// decides consent, a provider or a pane: the core says what exists, the person decides what leaves.
import type { CtxKind } from "./consentstate.js";

export const EXPLAIN_ACTION = "ai.explain_error";
export const DESCRIBE_ACTION = "ai.command_from_description";
export const EXPLAIN_TITLE = "Explain error";
export const DESCRIBE_TITLE = "Command from description";

/** The message `Explain error` writes into the composer. */
export const EXPLAIN_TEXT =
  "Explain the error in the attached terminal output: what went wrong, the most likely cause, and the command or change that fixes it.";
/** The start of the message `Command from description` writes: it ends with a colon and one space, where the person types. */
export const DESCRIBE_TEXT =
  "Write one shell command for this task. Reply with the command alone in one code block and, after it, one sentence saying what it does. Task: ";

/** The longest command, in characters (code points), that `Insert at prompt` offers. */
export const MAX_COMMAND = 1024;
/** The most buttons and notes of one reply. */
export const MAX_BUTTONS = 16;

/** The sentences of the two actions and of the reply action. */
export const WORDS = Object.freeze({
  explainReady: "Nothing is sent until you press Send.",
  describeReady: 'Describe the task after "Task:", then press Send.',
  notTerminal: "Focus a terminal pane to explain its error.",
  insertLabel: "Insert at prompt",
  notInsertable: "! Not insertable: it holds characters that are not shown.",
  noTerminal: "There is no terminal pane to insert into.",
  fullScreen: "That pane shows a full-screen program, so nothing was inserted.",
  refused: "The terminal did not take the text, so nothing was inserted.",
});

/** What the person is told after a command was put at the prompt of pane `pane`. */
export function insertedWords(pane: number): string {
  if (!Number.isInteger(pane) || pane < 0) return "Inserted at the prompt. It was not run.";
  return `Inserted at the prompt of pane ${pane}. It was not run.`;
}

/**
 * What an action hands to the chat: the text for the composer, the kinds of context to offer in order of preference when
 * `first` is set (else all of them), and the sentence for the status line once everything is prepared.
 */
export interface ChatIntent {
  text: string;
  kinds: CtxKind[];
  first: boolean;
  ready: string;
}

/** Which kinds `Explain error` asks the card to tick. */
export interface ExplainPlan {
  kinds: CtxKind[];
  first: boolean;
}

/** Whether a pane shows a selection: only the presence of a span is read, never the spans. */
export function hasSelection(mirror: { selection: readonly unknown[] } | null | undefined): boolean {
  if (mirror === null || mirror === undefined) return false;
  const spans: unknown = mirror.selection;
  return Array.isArray(spans) && spans.length > 0;
}

/**
 * The smallest source that answers the question: the selection when the pane shows one, else the last command and its
 * output, else the visible screen. The core lists what exists and the card ticks the first listed of the preference.
 */
export function explainPlan(mirror: { selection: readonly unknown[] } | null | undefined): ExplainPlan {
  if (hasSelection(mirror)) return { kinds: ["selection"], first: false };
  return { kinds: ["last_command", "screen"], first: true };
}

/** The verdict on one code block of a reply. */
export type Verdict = { kind: "command"; text: string } | { kind: "none" } | { kind: "hidden" };

/** What an insert needs to know of the pane the chat acts for at the moment of the click. */
export interface InsertTarget {
  pane: number | null;
  terminal: boolean;
  inView: boolean;
  fullScreen: boolean;
}

/** The longest code block that is looked at, in UTF-16 units: a longer one is not scanned at all. */
const MAX_SCAN = 65536;

/**
 * A character that is not shown or that changes what is shown: a control character (the line feed and the tab are looked at
 * apart), a format character (bidirectional controls, zero-width and tag characters, the soft hyphen), a private-use character,
 * an unpaired surrogate, a line or paragraph separator, and a space that is not the plain space. Unicode categories, so no list
 * of code points can fall behind.
 */
const HIDDEN = /(?![\n\t ])[\p{Cc}\p{Cf}\p{Co}\p{Cs}\p{Zl}\p{Zp}\p{Zs}]/u;

/**
 * What a code block of a reply is for the button under it: `command` when it is one clean line that may go to a prompt (the
 * text without one trailing line break and without spaces at the ends), `hidden` when it holds a character that is not shown
 * (the whole block is refused, before any other rule), and `none` when it is empty, has several lines or a tab, or is longer
 * than `MAX_COMMAND` characters (code points).
 */
export function commandVerdict(code: string): Verdict {
  if (code.length > MAX_SCAN) return { kind: "none" };
  let text = code;
  if (text.endsWith("\r\n")) text = text.slice(0, -2);
  else if (text.endsWith("\n")) text = text.slice(0, -1);
  if (HIDDEN.test(text)) return { kind: "hidden" };
  if (text.includes("\n") || text.includes("\t")) return { kind: "none" };
  const line = text.replace(/^ +| +$/g, "");
  if (line === "") return { kind: "none" };
  let characters = 0;
  for (const _ of line) {
    characters += 1;
    if (characters > MAX_COMMAND) return { kind: "none" };
  }
  return { kind: "command", text: line };
}

/**
 * The sentence that says why nothing may be inserted into the pane the chat acts for, or null when the pane is a terminal pane
 * of the shown tab that is not showing a full-screen program. A missing terminal comes before a full-screen program.
 */
export function insertRefusal(target: InsertTarget): string | null {
  if (target.pane === null || !target.terminal || !target.inView) return WORDS.noTerminal;
  if (target.fullScreen) return WORDS.fullScreen;
  return null;
}

/** One intent that waits for its chat to be drawn; at most one, and only for the chat it was made for. */
export class IntentSlot {
  private conversation: string | null = null;
  private intent: ChatIntent | null = null;

  /** Keep `intent` for `conversation`; an earlier one is gone. */
  park(conversation: string, intent: ChatIntent): void {
    this.conversation = conversation;
    this.intent = intent;
  }

  /** The intent kept for `conversation`, once; null for any other chat, which leaves the slot as it was. */
  take(conversation: string): ChatIntent | null {
    if (this.intent === null || this.conversation !== conversation) return null;
    const intent = this.intent;
    this.clear();
    return intent;
  }

  /** Forget the intent when it belongs to `conversation`. */
  drop(conversation: string): void {
    if (this.conversation === conversation) this.clear();
  }

  clear(): void {
    this.conversation = null;
    this.intent = null;
  }
}
