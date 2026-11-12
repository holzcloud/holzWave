// What the page knows about the connections of its panes: the last answer of `pane_connections`, and the words for
// it. Pure: no DOM, so node can test it. Every name comes from the core and is put on the screen with `textContent`.

export type ConnStateName = "connecting" | "connected" | "reconnecting" | "failed" | "closed";

/** The connection of one pane, as the core reports it. */
export interface PaneConn {
  pane: number;
  profile: string;
  name: string;
  state: ConnStateName;
  /** While reconnecting: the attempt, the attempts in all, and the seconds until the next one. */
  attempt: number;
  of: number;
  secs: number;
  /** Only for a pane that shows a session of a holzWave server; absent for every other pane. */
  remote?: RemoteConn;
}

/** Who writes, from this pane's own view: it holds the right, another pane holds it, or nobody does. */
export type Writer = "writing" | "watching" | "free";
/** What a pane of a session is busy with; the empty text when it shows the session. */
export type Phase = "" | "attaching" | "loading_older" | "resyncing";

/** What a pane that shows a session of a server adds to its connection. Nothing here is text the server wrote. */
export interface RemoteConn {
  writer: Writer;
  session: number;
  /** The grid of the session in cells. */
  cols: number;
  rows: number;
  /** The server holds lines older than the ones the pane has. */
  more: boolean;
  phase: Phase;
}

/** The closed sets of the remote member, as the core spells them. */
export const WRITERS: readonly Writer[] = ["writing", "watching", "free"];
export const PHASES: readonly Phase[] = ["", "attaching", "loading_older", "resyncing"];

/** The writer label of a pane header (docs/design.md 6.34): the word, written in capitals by the page, and the shape of its square. */
export const WRITER_LABELS: Readonly<Record<Writer, { word: string; shape: "filled" | "hollow" | "dashed" }>> = {
  writing: { word: "WRITING", shape: "filled" },
  watching: { word: "WATCHING", shape: "hollow" },
  free: { word: "FREE", shape: "dashed" },
};
/** The button after `WATCHING` and `FREE`. */
export const TAKE_OVER = "Take over";
/** The title and the accessible name of the × of a pane that shows a session. */
export const DETACH = "Detach session";

/** Read the remote member; null when it is absent or not well formed, so the pane reads as one without it. */
export function readRemote(raw: unknown): RemoteConn | null {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return null;
  const o = raw as Record<string, unknown>;
  const whole = (v: unknown, min: number, max: number): number | null =>
    typeof v === "number" && Number.isSafeInteger(v) && v >= min && v <= max ? v : null;
  const writer = WRITERS.find((w) => w === o.writer);
  const session = whole(o.session, 0, 0xffff_ffff);
  const cols = whole(o.cols, 1, 1000);
  const rows = whole(o.rows, 1, 500);
  if (writer === undefined || session === null || cols === null || rows === null || typeof o.more !== "boolean") return null;
  // A phase this page does not know reads as none: the pane then shows the session as it is.
  const phase = PHASES.find((p) => p === o.phase) ?? "";
  return { writer, session, cols, rows, more: o.more, phase };
}

/** A saved connection, as the core lists it. The secret itself never reaches the page, only whether one is saved. */
export interface Connection {
  id: string;
  name: string;
  kind: "ssh" | "wsl" | "docker";
  host: string;
  port: number;
  user: string;
  /**
   * `agent`: the person's ssh-agent signs the login, nothing is stored, `key_path` may name a public key file.
   * `interactive`: the server asks the questions (a one-time code, a password prompt) and the person answers them.
   */
  auth: "key" | "password" | "agent" | "interactive";
  key_path: string;
  jump: string | null;
  startup: string;
  reconnect_attempts: number;
  reconnect_delay_secs: number;
  shell: string;
  /** SSH only: the person allows the `wsh` tunnel on this host. Absent from the core's answer means false. */
  wsh: boolean;
  has_secret: boolean;
}

const STATES: ReadonlySet<string> = new Set(["connecting", "connected", "reconnecting", "failed", "closed"]);
const KINDS: ReadonlySet<string> = new Set(["ssh", "wsl", "docker"]);

/** The connection of each pane that has one. Replaced as a whole by every answer. */
export const paneConns = new Map<number, PaneConn>();

function count(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? Math.floor(value) : 0;
}

/** Read the answer of `pane_connections`; anything that is not a well formed entry is dropped. */
export function readPaneConns(raw: unknown): PaneConn[] {
  if (!Array.isArray(raw)) return [];
  const out: PaneConn[] = [];
  for (const item of raw) {
    if (typeof item !== "object" || item === null) continue;
    const o = item as Record<string, unknown>;
    if (typeof o.pane !== "number" || typeof o.profile !== "string" || typeof o.name !== "string") continue;
    if (typeof o.state !== "string" || !STATES.has(o.state)) continue;
    const conn: PaneConn = {
      pane: o.pane,
      profile: o.profile.slice(0, 64),
      name: o.name.slice(0, 256),
      state: o.state as ConnStateName,
      attempt: count(o.attempt),
      of: count(o.of),
      secs: count(o.secs),
    };
    const remote = readRemote(o.remote);
    if (remote !== null) conn.remote = remote;
    out.push(conn);
  }
  return out;
}

/** Make `list` the current connections of the panes. True when something changed. */
export function setPaneConns(list: PaneConn[]): boolean {
  const same =
    list.length === paneConns.size && list.every((c) => JSON.stringify(paneConns.get(c.pane)) === JSON.stringify(c));
  if (same) return false;
  paneConns.clear();
  for (const c of list) paneConns.set(c.pane, c);
  return true;
}

/** Read the answer of `connections_list`; an entry that is not well formed is dropped. */
export function readConnections(raw: unknown): Connection[] {
  if (!Array.isArray(raw)) return [];
  const out: Connection[] = [];
  for (const item of raw) {
    if (typeof item !== "object" || item === null) continue;
    const o = item as Record<string, unknown>;
    if (typeof o.id !== "string" || typeof o.name !== "string" || typeof o.kind !== "string" || !KINDS.has(o.kind)) continue;
    const text = (v: unknown, max = 256): string => (typeof v === "string" ? v.slice(0, max) : "");
    out.push({
      id: o.id.slice(0, 64),
      name: o.name.slice(0, 256),
      kind: o.kind as Connection["kind"],
      host: text(o.host),
      port: typeof o.port === "number" ? o.port : 22,
      user: text(o.user),
      auth: o.auth === "password" || o.auth === "agent" || o.auth === "interactive" ? o.auth : "key",
      key_path: text(o.key_path),
      jump: typeof o.jump === "string" ? o.jump : null,
      startup: text(o.startup),
      reconnect_attempts: count(o.reconnect_attempts),
      reconnect_delay_secs: count(o.reconnect_delay_secs),
      shell: text(o.shell),
      wsh: o.wsh === true,
      has_secret: o.has_secret === true,
    });
  }
  return out;
}

/**
 * Whether a connection of `kind` opens on the platform `os` (the name `app_info` gives; empty while it is not known). A
 * WSL connection opens on Windows only, a Docker connection not on Android. The connection stays in the list and in the
 * sync all the same; the core refuses it with a sentence, and the palette just lacks the entry.
 */
export function opensHere(kind: Connection["kind"], os: string): boolean {
  if (kind === "wsl") return os === "" || os === "windows";
  if (kind === "docker") return os !== "android";
  return true;
}

/** The word in front of a connection's name in the palette and the lists: `ssh`, `wsl` or `docker`. */
export function kindLabel(kind: Connection["kind"]): string {
  return kind;
}

/**
 * What the pane header says in its context slot for a pane with a connection: the name, and the state if it is not fine. For a pane
 * that shows a session of a server: the server's host, then the size of the session when it differs from `tile` (the cells the pane
 * has), never a text of the server.
 */
export function connText(c: PaneConn, tile?: { cols: number; rows: number }): string {
  if (c.remote) {
    const r = c.remote;
    const differs = tile !== undefined && (tile.cols !== r.cols || tile.rows !== r.rows);
    return differs ? `${c.name} · ${r.cols}×${r.rows}` : c.name;
  }
  switch (c.state) {
    case "connecting":
      return `${c.name} · connecting`;
    case "reconnecting":
      return `${c.name} · ⟳ ${c.attempt}/${c.of}${c.secs > 0 ? ` in ${c.secs} s` : ""}`;
    case "failed":
      return `${c.name} · failed, Enter retries`;
    case "closed":
      return `${c.name} · closed`;
    default:
      return c.name;
  }
}

/** The state of a pane's dot: a connection that is not fine looks different from a running shell without colour. */
export function connDot(c: PaneConn | undefined): "wait" | "failed" | null {
  if (!c) return null;
  if (c.state === "reconnecting" || c.state === "connecting") return "wait";
  if (c.state === "failed" || c.state === "closed") return "failed";
  return null;
}

/** The pane of this window's layout that shows `session`, or null. */
export function paneOfSession(session: number, shown: Iterable<number>): number | null {
  for (const pane of shown) if (paneConns.get(pane)?.remote?.session === session) return pane;
  return null;
}

/** The status line's segment: how many panes run an SSH (or WSL, Docker) connection, and how many of them are down. Empty without any. */
export function connSummary(list: Iterable<PaneConn>): string {
  let all = 0;
  let down = 0;
  for (const c of list) {
    if (c.remote) continue; // a session of a server is not an SSH connection
    all += 1;
    if (c.state !== "connected") down += 1;
  }
  if (all === 0) return "";
  return down === 0 ? `ssh ${all}` : `ssh ${all} · ${down} down`;
}
