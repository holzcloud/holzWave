// What the page knows of the sync: the shape of the core's `sync_status` answer, read defensively, and the words that
// say it. Pure functions, so they are unit tested without a browser.

import { LOCKED_LINE, NOT_SIGNED_IN, SIGNED_IN, UNREACHABLE } from "./serverstate.js";
import type { ServerView } from "./serverstate.js";
import { STRINGS } from "./vaultstate.js";

/**
 * The phases of the core's `sync_status`. `signed_out` is a server that refused the token of this device: no run is made until the
 * person signs in again, and there is no error text.
 */
export type SyncPhase = "off" | "idle" | "syncing" | "paused" | "signed_out" | "error";

/** The target of the sync: a WebDAV folder (the default, so an answer without the member reads as before) or the holzWave server. */
export type Transport = "webdav" | "server";

export interface SyncStatus {
  configured: boolean;
  transport: Transport;
  endpoint: string;
  user: string;
  state: SyncPhase;
  /** Seconds since the epoch of the last good run. */
  last: number | null;
  files: number;
  conflicts: number;
  error: string | null;
  /** Something was downloaded; it is applied at the next start. */
  restart: boolean;
  /** How many files another computer removed in the last run (copies are kept in `sync-conflicts`). */
  removed: number;
}

export const OFF: SyncStatus = {
  configured: false, transport: "webdav", endpoint: "", user: "", state: "off", last: null, files: 0, conflicts: 0, error: null, restart: false, removed: 0,
};

const PHASES: ReadonlySet<string> = new Set(["off", "idle", "syncing", "paused", "signed_out", "error"]);

function text(o: Record<string, unknown>, key: string, max: number): string {
  const v = o[key];
  return typeof v === "string" ? v.slice(0, max) : "";
}

function count(o: Record<string, unknown>, key: string): number {
  const v = o[key];
  return typeof v === "number" && Number.isFinite(v) && v >= 0 ? Math.floor(v) : 0;
}

/** Read the answer of `sync_status`; anything that is not well formed is the off state. */
export function readSync(raw: unknown): SyncStatus {
  if (typeof raw !== "object" || raw === null) return OFF;
  const o = raw as Record<string, unknown>;
  const state = typeof o["state"] === "string" && PHASES.has(o["state"]) ? (o["state"] as SyncPhase) : "off";
  const last = typeof o["last"] === "number" && Number.isFinite(o["last"]) ? o["last"] : null;
  const error = typeof o["error"] === "string" && o["error"] !== "" ? o["error"].slice(0, 400) : null;
  return {
    configured: o["configured"] === true,
    transport: o["transport"] === "server" ? "server" : "webdav",
    endpoint: text(o, "endpoint", 300),
    user: text(o, "user", 100),
    state,
    last,
    files: count(o, "files"),
    conflicts: count(o, "conflicts"),
    error,
    restart: o["restart"] === true,
    removed: count(o, "removed"),
  };
}

/** The word on the title bar button: the state is a word, not only a colour. */
export function syncLabel(s: SyncStatus): string {
  switch (s.state) {
    case "off":
      return "☁ sync off";
    case "syncing":
      return "☁ syncing…";
    case "paused":
      return STRINGS.syncButton;
    case "error":
    case "signed_out":
      // The record has four words for the button. A sync that waits for a new sign-in needs the person's attention like an error does,
      // and takes the word, the colour and the underline of one; the popover says why, in the words of the state line.
      return "☁ sync error";
    default:
      return "☁ synced";
  }
}

/** `14:31` of a time in seconds, in the local zone. */
export function clock(seconds: number): string {
  const d = new Date(seconds * 1000);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

/** "14:31 · 3 files", or "not yet". */
export function lastText(s: SyncStatus): string {
  if (s.last === null) return "not yet";
  const files = s.files === 1 ? "1 file" : `${s.files} files`;
  return `${clock(s.last)} · ${files}`;
}

/** "none", or "1 · the other version is kept". */
export function conflictText(s: SyncStatus): string {
  return s.conflicts === 0 ? "none" : `${s.conflicts} · the other version is kept`;
}

/** The line under the list when another computer removed files in the last run; empty at zero. */
export function removedText(s: SyncStatus): string {
  return s.removed === 0 ? "" : `Removed on another computer: ${s.removed}. Copies are kept in sync-conflicts.`;
}

/** The title of the popover for a target: the account name for WebDAV, `holzWave server` for the server. */
export function targetTitle(transport: Transport): string {
  return transport === "server" ? TRANSPORT.server : "Nextcloud";
}

/**
 * The line under the title of a connected popover: the host and the user for WebDAV; for the server its host and whether this device is
 * signed in (the state `signed_out` of a run is a refused token).
 */
export function endpointText(s: SyncStatus): string {
  if (s.transport === "server") {
    let host = s.endpoint;
    try {
      host = new URL(s.endpoint).host;
    } catch {
      // the whole address, as before
    }
    return `${host} · ${s.state === "signed_out" ? NOT_SIGNED_IN : SIGNED_IN}`;
  }
  try {
    const u = new URL(s.endpoint);
    return `${u.host}${u.pathname.replace(/\/$/, "")} · ${s.user}`;
  } catch {
    return `${s.endpoint} · ${s.user}`;
  }
}

// ---- the choice of the target (docs/design.md 6.9) ----------------------------------------------------------------------------------

/** The words of the choice: the name of the group and of its two options, and the sentence under it. */
export const TRANSPORT = { group: "Transport", webdav: "WebDAV", server: "holzWave server" } as const;
export const CHANGE_SENTENCE =
  "Changing the target keeps your passphrase. Files are not copied from one target to the other; the next run reads the new one.";
export const SERVER_ADDRESS = "Server address";
/** The dim `–` of the address row while there is none. */
export const NO_ADDRESS = "–";
export const SYNC_OFF_ON_SERVER = "Sync is off on this server.";

/** The state line of the server variant: a shape, the words, whether the link `Sign in…` follows, and whether `Connect` can be pressed. */
export interface TargetLine {
  dot: "hollow" | "filled" | "dash" | "mark";
  word: string;
  /** The words are a failure: the mark `!`, the warning colour and an underline. */
  bad: boolean;
  link: boolean;
  /** `Connect` can be pressed; when false the state line is its description. */
  connect: boolean;
}

/**
 * The state line for the link to the server. `refreshed` is whether the server has answered since the popover opened: until then the
 * switch of the sync target is not known, so the line does not say that it is off.
 */
export function describeTarget(view: ServerView, refreshed: boolean): TargetLine {
  if (view.locked) return { dot: "dash", word: LOCKED_LINE, bad: false, link: false, connect: false };
  switch (view.state) {
    case "signed_in":
      if (refreshed && !view.syncOn) return { dot: "mark", word: SYNC_OFF_ON_SERVER, bad: true, link: false, connect: false };
      return { dot: "filled", word: SIGNED_IN, bad: false, link: false, connect: true };
    case "unreachable":
      return { dot: "dash", word: UNREACHABLE, bad: false, link: false, connect: false };
    default:
      return { dot: "hollow", word: NOT_SIGNED_IN, bad: false, link: true, connect: false };
  }
}
