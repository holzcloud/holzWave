// What every integration surface of the window shares: the tool list the core sent, and the one listener of the core's
// `integration` event. The blocks (integrate.ts) and the Context section (integratecontext.ts) both read them, so the window
// asks `integration_tools` once and hears the event once. The event carries no data; whoever hears it asks the core again.
import { call, listen } from "./bridge.js";
import { readTools } from "./integratestate.js";
import type { ToolEntry } from "./integratestate.js";

let tools: ToolEntry[] | null = null;
let toolsAsk: Promise<ToolEntry[]> | null = null;
const toolListeners = new Set<() => void>();

/** The tool entries the core listed, asked once; `force` asks again (the person set a path, or the core said they changed). */
export function loadTools(force = false): Promise<ToolEntry[]> {
  if (toolsAsk && !force) return toolsAsk;
  const ask = (async () => {
    try {
      tools = readTools(await call<unknown>("integration_tools"));
    } catch {
      // no answer is no list: a surface still draws what it has
      tools ??= [];
    }
    for (const notify of [...toolListeners]) notify();
    return tools ?? [];
  })();
  toolsAsk = ask;
  return ask;
}

/** The core answered with the tool entries again (a path was set or cleared). */
export function setTools(raw: unknown): void {
  const list = readTools(raw);
  if (list.length === 0) return;
  tools = list;
  toolsAsk = Promise.resolve(list);
  for (const notify of [...toolListeners]) notify();
}

/** The tool entries the core listed, in its order; empty before the list arrived. */
export function listedTools(): readonly ToolEntry[] {
  return tools ?? [];
}

/** The entry of a tool, or null before the list arrived or for a tool the core did not list. */
export function toolOf(id: string): ToolEntry | null {
  return tools?.find((t) => t.id === id) ?? null;
}

/** Call `notify` whenever the tool list changes; the answer removes it. */
export function onTools(notify: () => void): () => void {
  toolListeners.add(notify);
  return () => void toolListeners.delete(notify);
}

/** What an event says: it carries no data, the listener asks. `gap` is true when an event was missed. */
export interface Hint {
  kind: string;
  tool: string | null;
  pane: number | null;
  gap: boolean;
}
const hints = new Set<(h: Hint) => void>();
let listening = false;
let lastSeq = 0;

/** Listen for the event `integration` once for the window. It holds {seq, kind, tool, pane, code} and nothing else. */
function listenOnce(): void {
  if (listening) return;
  listening = true;
  listen<unknown>("integration", (payload) => {
    if (typeof payload !== "object" || payload === null) return;
    const o = payload as Record<string, unknown>;
    if (typeof o.kind !== "string" || typeof o.seq !== "number") return;
    const gap = lastSeq > 0 && o.seq !== lastSeq + 1;
    lastSeq = o.seq;
    if (o.kind === "tools" || gap) void loadTools(true);
    const hint: Hint = {
      kind: o.kind,
      tool: typeof o.tool === "string" ? o.tool : null,
      pane: typeof o.pane === "number" ? o.pane : null,
      gap,
    };
    for (const h of [...hints]) h(hint);
  }).catch(() => {
    listening = false;
  });
}

/** Hear every event of the core; the answer stops it. The window listens once however many surfaces ask. */
export function onHint(hear: (h: Hint) => void): () => void {
  hints.add(hear);
  listenOnce();
  return () => void hints.delete(hear);
}
