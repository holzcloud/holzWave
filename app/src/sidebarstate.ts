// What the sidebar knows: the widgets as the core lists them (read defensively), the Pomodoro clock, and the state of a
// host in the list. Pure functions, so node tests them.
import type { PaneConn } from "./connstate.js";

export interface ListItem {
  label: string;
  command?: string;
  url?: string;
}

export type WidgetDef =
  | { id: string; title: string; kind: "note" }
  | { id: string; title: string; kind: "timer"; work_min: number; break_min: number }
  | { id: string; title: string; kind: "list"; items: ListItem[] };

export interface WidgetsAnswer {
  widgets: WidgetDef[];
  notes: Record<string, string>;
  warnings: string[];
}

const ID = /^[a-z0-9_-]{1,32}$/;

const text = (v: unknown, max: number): string => (typeof v === "string" ? v.slice(0, max) : "");
const minutes = (v: unknown, fallback: number, max: number): number =>
  typeof v === "number" && Number.isInteger(v) && v >= 0 && v <= max ? v : fallback;

/** Read the answer of `widgets_list`. The core validated every widget; the page still keeps only what it can draw. */
export function readWidgets(raw: unknown): WidgetsAnswer {
  const empty: WidgetsAnswer = { widgets: [], notes: {}, warnings: [] };
  if (typeof raw !== "object" || raw === null) return empty;
  const o = raw as Record<string, unknown>;
  const widgets: WidgetDef[] = [];
  for (const w of Array.isArray(o["widgets"]) ? (o["widgets"] as unknown[]).slice(0, 24) : []) {
    if (typeof w !== "object" || w === null) continue;
    const x = w as Record<string, unknown>;
    const id = text(x["id"], 32);
    const title = text(x["title"], 40);
    if (!ID.test(id) || title === "") continue;
    if (x["kind"] === "note") widgets.push({ id, title, kind: "note" });
    else if (x["kind"] === "timer") {
      widgets.push({ id, title, kind: "timer", work_min: Math.max(1, minutes(x["work_min"], 25, 180)), break_min: minutes(x["break_min"], 5, 60) });
    } else if (x["kind"] === "list" && Array.isArray(x["items"])) {
      const items: ListItem[] = [];
      for (const i of (x["items"] as unknown[]).slice(0, 50)) {
        if (typeof i !== "object" || i === null) continue;
        const y = i as Record<string, unknown>;
        const label = text(y["label"], 60);
        const command = typeof y["command"] === "string" ? y["command"].slice(0, 1024) : undefined;
        const url = typeof y["url"] === "string" && /^https?:\/\//.test(y["url"]) ? y["url"].slice(0, 2048) : undefined;
        if (label === "" || (command === undefined) === (url === undefined)) continue;
        items.push(command !== undefined ? { label, command } : { label, url: url as string });
      }
      widgets.push({ id, title, kind: "list", items });
    }
  }
  const notes: Record<string, string> = {};
  if (typeof o["notes"] === "object" && o["notes"] !== null) {
    for (const [k, v] of Object.entries(o["notes"] as Record<string, unknown>)) if (ID.test(k) && typeof v === "string") notes[k] = v.slice(0, 65536);
  }
  const warnings = Array.isArray(o["warnings"]) ? (o["warnings"] as unknown[]).filter((w): w is string => typeof w === "string").slice(0, 10).map((w) => w.slice(0, 300)) : [];
  return { widgets, notes, warnings };
}

// ---- the Pomodoro clock -----------------------------------------------------------------------------------------------

export interface TimerState {
  mode: "idle" | "work" | "break";
  /** When the running phase ends, in ms; for a paused clock unused. */
  endsAt: number;
  /** Set while paused: the ms the phase still has. */
  paused: number | null;
}

export const IDLE: TimerState = { mode: "idle", endsAt: 0, paused: null };

const MIN = 60_000;

export function start(def: { work_min: number }, now: number): TimerState {
  return { mode: "work", endsAt: now + def.work_min * MIN, paused: null };
}

export function pause(s: TimerState, now: number): TimerState {
  return s.mode === "idle" || s.paused !== null ? s : { ...s, paused: Math.max(0, s.endsAt - now) };
}

export function resume(s: TimerState, now: number): TimerState {
  return s.paused === null ? s : { ...s, endsAt: now + s.paused, paused: null };
}

/** The clock after `now`: a finished work phase becomes the break (when there is one), a finished break ends. */
export function tick(s: TimerState, def: { break_min: number }, now: number): TimerState {
  if (s.mode === "idle" || s.paused !== null || now < s.endsAt) return s;
  if (s.mode === "work" && def.break_min > 0) return { mode: "break", endsAt: s.endsAt + def.break_min * MIN, paused: null };
  return IDLE;
}

/** Milliseconds left in the phase; the whole work time when idle. */
export function left(s: TimerState, def: { work_min: number }, now: number): number {
  if (s.mode === "idle") return def.work_min * MIN;
  return s.paused ?? Math.max(0, s.endsAt - now);
}

/** `24:59`. */
export function mmss(ms: number): string {
  const total = Math.max(0, Math.ceil(ms / 1000));
  return `${String(Math.floor(total / 60)).padStart(2, "0")}:${String(total % 60).padStart(2, "0")}`;
}

/** The share of the phase that has passed, 0 to 1, for the bar. */
export function progress(s: TimerState, def: { work_min: number; break_min: number }, now: number): number {
  if (s.mode === "idle") return 0;
  const total = (s.mode === "work" ? def.work_min : def.break_min) * MIN;
  return total === 0 ? 0 : Math.min(1, Math.max(0, 1 - left(s, def, now) / total));
}

/** The word of the phase: the state is said, not only drawn. */
export function phaseWord(s: TimerState): string {
  if (s.mode === "idle") return "ready";
  const base = s.mode === "work" ? "working" : "break";
  return s.paused !== null ? `${base} · paused` : base;
}

// ---- hosts ---------------------------------------------------------------------------------------------------------

export type HostState = "idle" | "connected" | "wait" | "failed";

/** The state of a saved connection from the panes that run it: any running pane makes it connected. */
export function hostState(profile: string, conns: Iterable<PaneConn>): { state: HostState; note: string } {
  let wait: PaneConn | undefined;
  let failed = false;
  for (const c of conns) {
    if (c.profile !== profile) continue;
    if (c.state === "connected") return { state: "connected", note: "" };
    if (c.state === "reconnecting" || c.state === "connecting") wait = wait ?? c;
    else failed = true;
  }
  if (wait) return { state: "wait", note: wait.state === "reconnecting" ? `⟳ ${wait.attempt}/${wait.of}` : "connecting" };
  return failed ? { state: "failed", note: "failed" } : { state: "idle", note: "" };
}
