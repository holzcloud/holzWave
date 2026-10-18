// What the page knows of the sync: the shape of the core's `sync_status` answer, read defensively, and the words that
// say it. Pure functions, so they are unit tested without a browser.

export type SyncPhase = "off" | "idle" | "syncing" | "error";

export interface SyncStatus {
  configured: boolean;
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
  configured: false, endpoint: "", user: "", state: "off", last: null, files: 0, conflicts: 0, error: null, restart: false, removed: 0,
};

const PHASES: ReadonlySet<string> = new Set(["off", "idle", "syncing", "error"]);

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
    case "error":
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

/** The host of the endpoint, for the line under the title; the whole address if it does not parse. */
export function endpointText(s: SyncStatus): string {
  try {
    const u = new URL(s.endpoint);
    return `${u.host}${u.pathname.replace(/\/$/, "")} · ${s.user}`;
  } catch {
    return `${s.endpoint} · ${s.user}`;
  }
}
