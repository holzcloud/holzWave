// What the page knows about block panes (a file browser or a preview in place of a terminal), and the words and
// icons of a file list. Pure: no DOM, so node can test it. Every name comes from the core or from a remote machine and
// is put on the screen with `textContent` only.

import { isChatId } from "./chatstate.js";

export type BlockKind = "files" | "preview" | "monitor" | "chat";

/** What a block pane shows, as the core lists it. */
export interface BlockInfo {
  pane: number;
  kind: BlockKind;
  /** `local` or the id of a saved SSH connection. */
  source: string;
  /** A directory for a browser, a file for a preview, the id of the conversation for a chat. */
  path: string;
  /** The name of the source for the header. */
  source_label: string;
  /** A preview that opens its file in the editor at once (`wsh edit`). Present only when the core says `true`. */
  edit?: boolean;
}

export interface FileEntry {
  name: string;
  kind: "file" | "dir" | "other";
  link: boolean;
  size: number;
  modified: number | null;
  hidden: boolean;
}

export interface Listing {
  path: string;
  parent: string | null;
  entries: FileEntry[];
  truncated: boolean;
}

/** The kind of a preview, as the core classified the file. */
export type Content = "markdown" | "image" | "pdf" | "video" | "audio" | "text" | "binary";

export interface Opened {
  name: string;
  size: number;
  content: Content;
  mime: string;
  language: string | null;
  html: string | null;
  text: string | null;
  /** The file's modification time (seconds), sent back with a save to notice a change on disk. */
  modified: number | null;
  /** The SHA-256 of the bytes the core read (64 lowercase hex characters), sent back with a save for an exact check. */
  digest: string | null;
}

/** A digest as the core writes it: 64 lowercase hex characters. Anything else is no digest. */
export function readDigest(raw: unknown): string | null {
  return typeof raw === "string" && /^[0-9a-f]{64}$/.test(raw) ? raw : null;
}

/** The answer of `file_save`: the new change time and digest. A bare number is the older answer (a time only). */
export function readSaved(raw: unknown): { modified: number | null; digest: string | null } {
  if (typeof raw === "number") return { modified: Number.isFinite(raw) ? raw : null, digest: null };
  if (typeof raw !== "object" || raw === null) return { modified: null, digest: null };
  const o = raw as { modified?: unknown; digest?: unknown };
  const modified = typeof o.modified === "number" && Number.isFinite(o.modified) ? o.modified : null;
  return { modified, digest: readDigest(o.digest) };
}

/** The block of each pane that has one. Replaced as a whole by every answer of `pane_blocks`. */
export const paneBlocks = new Map<number, BlockInfo>();

const KINDS: ReadonlySet<string> = new Set(["files", "preview", "monitor", "chat"]);

/** Read the answer of `pane_blocks`; an entry that is not well formed is dropped. */
export function readBlocks(raw: unknown): BlockInfo[] {
  if (!Array.isArray(raw)) return [];
  const out: BlockInfo[] = [];
  for (const item of raw) {
    if (typeof item !== "object" || item === null) continue;
    const o = item as Record<string, unknown>;
    if (typeof o.pane !== "number" || typeof o.kind !== "string" || !KINDS.has(o.kind)) continue;
    if (typeof o.source !== "string" || typeof o.path !== "string") continue;
    // A chat's path becomes the argument of chat commands: one that is not a plausible chat id never leaves this list.
    if (o.kind === "chat" && !isChatId(o.path)) continue;
    const block: BlockInfo = {
      pane: o.pane,
      kind: o.kind as BlockKind,
      source: o.source.slice(0, 64),
      path: o.path.slice(0, 4096),
      source_label: typeof o.source_label === "string" ? o.source_label.slice(0, 256) : o.source,
    };
    // Like the core, which leaves the member out when it is false: a strict `true` and nothing else, and only a preview has it.
    if (o.edit === true && o.kind === "preview") block.edit = true;
    out.push(block);
  }
  return out;
}

/** Make `list` the blocks of the panes. True when something changed. */
export function setPaneBlocks(list: BlockInfo[]): boolean {
  const same =
    list.length === paneBlocks.size && list.every((b) => JSON.stringify(paneBlocks.get(b.pane)) === JSON.stringify(b));
  if (same) return false;
  paneBlocks.clear();
  for (const b of list) paneBlocks.set(b.pane, b);
  return true;
}

/** Read the answer of `files_list`; null when it is not a listing. */
export function readListing(raw: unknown): Listing | null {
  if (typeof raw !== "object" || raw === null) return null;
  const o = raw as Record<string, unknown>;
  if (typeof o.path !== "string" || !Array.isArray(o.entries)) return null;
  const entries: FileEntry[] = [];
  for (const item of o.entries) {
    if (typeof item !== "object" || item === null) continue;
    const e = item as Record<string, unknown>;
    if (typeof e.name !== "string" || e.name === "") continue;
    entries.push({
      name: e.name,
      kind: e.kind === "dir" ? "dir" : e.kind === "file" ? "file" : "other",
      link: e.link === true,
      size: typeof e.size === "number" && Number.isFinite(e.size) && e.size >= 0 ? e.size : 0,
      modified: typeof e.modified === "number" && Number.isFinite(e.modified) ? e.modified : null,
      hidden: e.hidden === true,
    });
  }
  return { path: o.path, parent: typeof o.parent === "string" ? o.parent : null, entries, truncated: o.truncated === true };
}

/** The last part of a path, whichever separator it uses. */
export function baseName(path: string): string {
  const trimmed = path.replace(/[\\/]+$/, "");
  if (trimmed === "") return path; // the root
  const at = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  return at < 0 ? trimmed : trimmed.slice(at + 1) || trimmed;
}

/** A directory and a name made into a path, with the separator the directory uses (a Windows path keeps its backslash). */
export function joinPath(dir: string, name: string): string {
  const sep = dir.includes("\\") && !dir.includes("/") ? "\\" : "/";
  return dir.endsWith(sep) ? `${dir}${name}` : `${dir}${sep}${name}`;
}

const IMAGE = new Set(["png", "jpg", "jpeg", "gif", "webp", "avif", "svg", "bmp", "ico", "heic"]);
const VIDEO = new Set(["mp4", "m4v", "webm", "mov", "mkv", "avi"]);
const AUDIO = new Set(["mp3", "flac", "wav", "ogg", "m4a", "opus"]);
const DOC = new Set(["pdf", "md", "markdown", "txt", "rtf"]);

/** The symbol before a name in the file list (design.md section 6.5): a shape, so the kind needs no colour. */
export function entryIcon(e: Pick<FileEntry, "name" | "kind">): string {
  if (e.kind === "dir") return "▸";
  const ext = e.name.includes(".") ? e.name.slice(e.name.lastIndexOf(".") + 1).toLowerCase() : "";
  if (IMAGE.has(ext)) return "▣";
  if (VIDEO.has(ext) || AUDIO.has(ext)) return "▶";
  if (DOC.has(ext)) return "▤";
  return e.kind === "other" ? "·" : "✎";
}

/** A size as a person reads it: 0 B, 1.5 KB, 12 MB. Folders have none. */
export function humanSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "";
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value >= 10 ? Math.round(value) : Math.round(value * 10) / 10} ${units[unit]}`;
}

/** The time of a change as the list shows it: `Oct 15 14:02` this year, `Oct 15  2025` before. UTC-free: local time. */
export function shortDate(secs: number | null, now: Date = new Date()): string {
  if (secs === null) return "";
  const d = new Date(secs * 1000);
  if (Number.isNaN(d.getTime())) return "";
  const months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
  const day = `${months[d.getMonth()]} ${String(d.getDate()).padStart(2, " ")}`;
  if (d.getFullYear() === now.getFullYear()) {
    return `${day} ${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  }
  return `${day}  ${d.getFullYear()}`;
}

/**
 * The title of a block pane's header. A chat block's title is `chat`; its context is the chat's own title (`blockContext`).
 */
export function blockTitle(b: BlockInfo): string {
  if (b.kind === "chat") return "chat";
  return b.kind === "files" ? "files" : b.kind === "monitor" ? "system" : baseName(b.path);
}

/** The most characters of a chat's title the header keeps. */
const CHAT_TITLE_MAX = 120;

/** The title of each chat that has one, by conversation id; the chat view fills it and the pane header reads it. */
export const chatTitles = new Map<string, string>();

/**
 * Remember the title of a chat for its pane header: control characters are removed and the text is cut at 120
 * characters; an empty title puts the words `new chat` back. True when the stored title changed.
 */
export function setChatTitle(conversation: string, title: string): boolean {
  const clean = Array.from(title.replace(/[\p{Cc}\u{202A}-\u{202E}\u{2066}-\u{2069}]/gu, ""))
    .slice(0, CHAT_TITLE_MAX)
    .join("");
  const before = chatTitles.get(conversation) ?? "";
  if (clean === "") chatTitles.delete(conversation);
  else chatTitles.set(conversation, clean);
  return clean !== before;
}

/**
 * The conversations whose chat pane left the list: the chats of `previous` that no chat block of `next` names any more.
 * A chat that moved to another pane is not gone, and a block of another kind is never listed.
 */
export function goneChats(previous: readonly BlockInfo[], next: readonly BlockInfo[]): string[] {
  const kept = new Set(next.filter((b) => b.kind === "chat").map((b) => b.path));
  const gone = new Set<string>();
  for (const b of previous) {
    if (b.kind === "chat" && !kept.has(b.path)) gone.add(b.path);
  }
  return [...gone];
}

/** The context of a block pane's header: where it reads from, and for a browser the folder. */
export function blockContext(b: BlockInfo): string {
  if (b.kind === "monitor") return `${b.source === "local" ? "local" : b.source_label || b.source} · 1 s`;
  if (b.kind === "chat") return chatTitles.get(b.path) ?? "new chat";
  return b.kind === "files" ? `${b.source_label} · ${b.path}` : `${b.source_label} · preview`;
}

/** The entries a browser shows: hidden ones only when asked. */
export function visibleEntries(entries: readonly FileEntry[], showHidden: boolean): FileEntry[] {
  return showHidden ? [...entries] : entries.filter((e) => !e.hidden);
}

/** The panes whose editor holds changes that are not saved. */
export const dirtyPanes = new Set<number>();
