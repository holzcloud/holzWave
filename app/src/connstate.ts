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
}

/** A saved connection, as the core lists it. The secret itself never reaches the page, only whether one is saved. */
export interface Connection {
  id: string;
  name: string;
  kind: "ssh" | "wsl" | "docker";
  host: string;
  port: number;
  user: string;
  auth: "key" | "password";
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
    out.push({
      pane: o.pane,
      profile: o.profile.slice(0, 64),
      name: o.name.slice(0, 256),
      state: o.state as ConnStateName,
      attempt: count(o.attempt),
      of: count(o.of),
      secs: count(o.secs),
    });
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
      auth: o.auth === "password" ? "password" : "key",
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

/** The word in front of a connection's name in the palette and the lists: `ssh`, `wsl` or `docker`. */
export function kindLabel(kind: Connection["kind"]): string {
  return kind;
}

/** What the pane header says in its context slot for a pane with a connection: the name, and the state if it is not fine. */
export function connText(c: PaneConn): string {
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

/** The status line's segment: how many panes run a connection, and how many of them are down. Empty without any. */
export function connSummary(list: Iterable<PaneConn>): string {
  let all = 0;
  let down = 0;
  for (const c of list) {
    all += 1;
    if (c.state !== "connected") down += 1;
  }
  if (all === 0) return "";
  return down === 0 ? `ssh ${all}` : `ssh ${all} · ${down} down`;
}
