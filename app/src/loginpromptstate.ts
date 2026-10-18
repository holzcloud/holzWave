// The questions of a keyboard-interactive login, as the page reads them. Pure: no DOM, so node can test it.
//
// The core sends `login-prompt` with the saved connection's name and host (from the person's own list) and the server's
// words, already cleaned and bounded. The page does not trust that either: it reads the payload strictly, cuts every string
// to its bound and drops what is not well formed. Everything it keeps is put on the screen with `textContent`.

/** The sentence that always stands above what the server wrote. */
export const serverTextLine = "This text comes from the server, not from holzWave.";

/** The line under the form of a connection whose login is answered by the person. */
export const INTERACTIVE_HINT = "The server asks for your answer when you connect. Nothing is stored.";

/** The bounds the core applies; the page applies them again. */
export const MAX_PROMPTS = 5;
const MAX_NAME = 60;
const MAX_INSTRUCTIONS = 300;
const MAX_TEXT = 200;
const MAX_CONNECTION = 256;
const MAX_WAITING = 16;

export interface LoginPromptLine {
  text: string;
  /** True: what is typed may be shown. False: a password or a code that stays hidden. */
  echo: boolean;
}

export interface LoginPrompt {
  id: string;
  /** The name and the host of the person's own saved connection. */
  connection: string;
  host: string;
  /** What the server wrote. Text only. */
  name: string;
  instructions: string;
  prompts: LoginPromptLine[];
}

// Control characters other than a line break, the bidirectional controls, the zero-width and invisible format characters.
const HIDDEN = /[\u0000-\u0009\u000b-\u001f\u007f-\u009f\u00ad\u061c\u180e\u200b-\u200f\u2028\u2029\u202a-\u202e\u2060-\u206f\ufeff\ufff9-\ufffb]/g;
const TAGS = /[\u{e0000}-\u{e007f}]/gu;

/** `text` without the characters nobody can see, cut to `max` characters (code points). */
function clean(text: string, max: number): string {
  return Array.from(text.replace(HIDDEN, "").replace(TAGS, "")).slice(0, max).join("").trim();
}

const ID = /^[0-9a-f]{32}$/;

/** Read the payload of `login-prompt`; null for anything that is not a well formed question. */
export function readPrompt(raw: unknown): LoginPrompt | null {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return null;
  const o = raw as Record<string, unknown>;
  if (typeof o.id !== "string" || !ID.test(o.id)) return null;
  if (typeof o.connection !== "string" || typeof o.host !== "string") return null;
  if (typeof o.name !== "string" || typeof o.instructions !== "string") return null;
  if (!Array.isArray(o.prompts) || o.prompts.length === 0 || o.prompts.length > MAX_PROMPTS) return null;
  const connection = clean(o.connection, MAX_CONNECTION);
  // No title without the person's own connection: the dialog never names itself from the server's words.
  if (connection === "") return null;
  const prompts: LoginPromptLine[] = [];
  for (const item of o.prompts) {
    if (typeof item !== "object" || item === null || Array.isArray(item)) return null;
    const line = item as Record<string, unknown>;
    if (typeof line.text !== "string" || typeof line.echo !== "boolean") return null;
    const text = clean(line.text, MAX_TEXT);
    if (text === "") return null;
    prompts.push({ text, echo: line.echo });
  }
  return {
    id: o.id,
    connection,
    host: clean(o.host, MAX_CONNECTION),
    name: clean(o.name, MAX_NAME),
    instructions: Array.from(o.instructions.replace(HIDDEN, "").replace(TAGS, "")).slice(0, MAX_INSTRUCTIONS).join("").trim(),
    prompts,
  };
}

/** Read the payload of `login-prompt-end`: the id of the question that is over, or null. */
export function readEnd(raw: unknown): string | null {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return null;
  const o = raw as Record<string, unknown>;
  if (typeof o.id !== "string" || !ID.test(o.id)) return null;
  if (o.outcome !== "answered" && o.outcome !== "cancelled" && o.outcome !== "timed_out") return null;
  return o.id;
}

/** The questions that wait for their turn: one is shown at a time, the others queue (at most 16, an id once). */
export class PromptQueue {
  private shown: LoginPrompt | null = null;
  private waiting: LoginPrompt[] = [];

  /** The question on screen now. */
  get current(): LoginPrompt | null {
    return this.shown;
  }

  /** A question arrived. Returns it when it is to be shown now; null when it waits or is dropped. */
  push(prompt: LoginPrompt): LoginPrompt | null {
    if (this.shown?.id === prompt.id || this.waiting.some((p) => p.id === prompt.id)) return null;
    if (this.shown === null) {
      this.shown = prompt;
      return prompt;
    }
    if (this.waiting.length < MAX_WAITING) this.waiting.push(prompt);
    return null;
  }

  /**
   * The question `id` is over, here (answered or cancelled) or in the core (answered elsewhere, cancelled or timed out).
   * `closed` is true when it was the one on screen; `next` is then the question to show now, if one waited.
   */
  end(id: string): { closed: boolean; next: LoginPrompt | null } {
    if (this.shown?.id === id) {
      this.shown = this.waiting.shift() ?? null;
      return { closed: true, next: this.shown };
    }
    this.waiting = this.waiting.filter((p) => p.id !== id);
    return { closed: false, next: null };
  }
}
