// The pure half of the conversation list of the sidebar column (aisidebar.ts, docs/design.md section 6.18): how the core's
// list of stored chats is read, how old a chat is in words, and which rows the list shows. No DOM and no bridge, so node can
// test it. Every text in it is stored user or model text (the first line of what someone typed, a model name, a note about
// a file): it is read member by member, cleaned of control and direction characters, cut, and only ever put on the screen as
// a text node.
import { isChatId } from "./chatstate.js";
import { titleWords } from "./chatview.js";
import { ago } from "./historystate.js";

/** The most chats the page reads: the core keeps at most this many. */
export const MAX_CHATS = 500;
/** The most notes about the chat folder the page shows. */
export const MAX_NOTES = 5;
/** The longest note, in characters. */
export const MAX_NOTE = 300;
/** The title of a chat with no message yet. */
export const NEW_CHAT = "new chat";
/** The title of a stored chat whose title is empty. */
export const UNTITLED = "untitled chat";
/** The line of the row of a chat that has no file yet. */
export const NOT_SENT = "nothing sent yet";

/** One chat of the core's list: the six members of `ConversationSummary`, with no profile or model as the empty string. */
export interface StoredChat {
  id: string;
  title: string;
  /** Unix seconds, as the core stamps them. */
  created: number;
  /** Unix seconds. */
  updated: number;
  profile: string;
  model: string;
}

/** The core's answer to `ai_conversation_list`. */
export interface ChatList {
  chats: StoredChat[];
  notes: string[];
}

/** One row of the list as it is drawn. */
export interface RowView {
  id: string;
  mark: "●" | "○";
  title: string;
  meta: string;
  current: boolean;
  /** The pane of this window that shows the chat, or null. */
  pane: number | null;
  canDelete: boolean;
}

/** Control, direction and zero-width characters: the ones that can hide or reorder text. */
const UNSAFE = /[\p{Cc}\u{202A}-\u{202E}\u{2066}-\u{2069}\u{200B}-\u{200F}\u{2060}-\u{2064}\u{FEFF}\u{E0000}-\u{E007F}]/gu;

/** `text` on one line: unsafe characters become blanks, blanks collapse, cut at `max` characters. */
function oneLine(text: string, max: number): string {
  return Array.from(text.replace(UNSAFE, " ").replace(/\s+/g, " ").trim()).slice(0, max).join("");
}

function own(record: object, name: string): unknown {
  return Object.hasOwn(record, name) ? (record as Record<string, unknown>)[name] : undefined;
}

/** A time the core stamped: a finite, non-negative number, floored; anything else reads 0. */
function stamp(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? Math.floor(value) : 0;
}

function label(value: unknown): string {
  return typeof value === "string" ? titleWords(value) : "";
}

function readChat(raw: unknown): StoredChat | null {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return null;
  const id = own(raw, "id");
  if (!isChatId(id)) return null;
  return {
    id,
    title: label(own(raw, "title")),
    created: stamp(own(raw, "created")),
    updated: stamp(own(raw, "updated")),
    profile: label(own(raw, "profile")),
    model: label(own(raw, "model")),
  };
}

/**
 * Read the answer of `ai_conversation_list`. Every member is named here and none is copied by name from the input, an id
 * that is not a chat id drops its entry, every text is cleaned and cut, and the counts are capped. An answer that is not an
 * object with lists reads as an empty list.
 */
export function readChatList(raw: unknown): ChatList {
  const out: ChatList = { chats: [], notes: [] };
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return out;
  const chats = own(raw, "chats");
  if (Array.isArray(chats)) {
    for (const entry of chats) {
      if (out.chats.length >= MAX_CHATS) break;
      const chat = readChat(entry);
      if (chat !== null) out.chats.push(chat);
    }
  }
  const notes = own(raw, "notes");
  if (Array.isArray(notes)) {
    for (const entry of notes) {
      if (out.notes.length >= MAX_NOTES) break;
      if (typeof entry !== "string") continue;
      const note = oneLine(entry, MAX_NOTE);
      if (note !== "") out.notes.push(note);
    }
  }
  return out;
}

/** How long ago a chat changed: `updated` and `now` are Unix seconds. A time that is not a positive number gives nothing. */
export function ageWords(updated: number, now: number): string {
  if (!Number.isFinite(updated) || updated <= 0 || !Number.isFinite(now)) return "";
  return ago(updated, now);
}

/** The title the head shows for a chat: the cleaned title, or `new chat` before there is one. */
export function headTitle(title: string): string {
  const words = titleWords(title);
  return words === "" ? NEW_CHAT : words;
}

/**
 * The rows of the list. The chat on screen is always a row: first when it is not stored yet, else in the core's place. The
 * core's order (newest first) is kept. A chat that a pane of this window shows (`panes`: chat id to pane number) says so
 * instead of its age and cannot be deleted.
 */
export function listRows(
  chats: readonly StoredChat[],
  current: string,
  shownTitle: string,
  panes: ReadonlyMap<string, number>,
  now: number,
): RowView[] {
  const rows: RowView[] = [];
  const seen = new Set<string>();
  const stored = chats.slice(0, MAX_CHATS);
  if (!stored.some((c) => c.id === current)) {
    const title = headTitle(shownTitle);
    rows.push({ id: current, mark: "●", title, meta: title === NEW_CHAT ? NOT_SENT : "", current: true, pane: null, canDelete: false });
    seen.add(current);
  }
  for (const chat of stored) {
    if (seen.has(chat.id)) continue;
    seen.add(chat.id);
    const pane = panes.get(chat.id) ?? null;
    const pieces: string[] = [];
    if (chat.model !== "") pieces.push(chat.model);
    const when = pane === null ? ageWords(chat.updated, now) : `in pane ${pane}`;
    if (when !== "") pieces.push(when);
    rows.push({
      id: chat.id,
      mark: chat.id === current ? "●" : "○",
      title: chat.title === "" ? UNTITLED : chat.title,
      meta: pieces.join(" · "),
      current: chat.id === current,
      pane,
      canDelete: pane === null,
    });
  }
  return rows;
}
