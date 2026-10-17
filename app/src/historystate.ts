// The command history on the page: the shape of the core's `history_search` answer, and the words and pieces that show
// a hit. Pure, so node tests them. Every command comes from a shell and goes onto the screen as text only.

export interface Hit {
  cmd: string;
  cwd: string;
  host: string;
  /** Seconds since the epoch of the last use. */
  last: number;
  count: number;
  exit: number | null;
  /** Indices of the matched characters of `cmd`. */
  marks: number[];
}

const MAX_HITS = 200;

/** Read the answer of `history_search`; what is not well formed is dropped. */
export function readHits(raw: unknown): Hit[] {
  if (!Array.isArray(raw)) return [];
  const out: Hit[] = [];
  for (const item of raw.slice(0, MAX_HITS)) {
    if (typeof item !== "object" || item === null) continue;
    const o = item as Record<string, unknown>;
    if (typeof o["cmd"] !== "string" || o["cmd"] === "") continue;
    const marks = Array.isArray(o["marks"]) ? (o["marks"] as unknown[]).filter((m): m is number => Number.isInteger(m) && (m as number) >= 0) : [];
    out.push({
      cmd: o["cmd"].slice(0, 4096),
      cwd: typeof o["cwd"] === "string" ? o["cwd"].slice(0, 4096) : "",
      host: typeof o["host"] === "string" ? o["host"].slice(0, 128) : "",
      last: typeof o["last"] === "number" && Number.isFinite(o["last"]) ? o["last"] : 0,
      count: typeof o["count"] === "number" && o["count"] > 0 ? Math.floor(o["count"]) : 1,
      exit: typeof o["exit"] === "number" && Number.isInteger(o["exit"]) ? o["exit"] : null,
      marks,
    });
  }
  return out;
}

/** The pane of a `history-open` event: the shell of that pane asked for the history. Only an object that holds a non-negative
 * safe integer as `pane` is read; anything else is `null`, so a malformed event opens nothing. Other members are ignored. */
export function readOpenPane(raw: unknown): number | null {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return null;
  const pane = (raw as Record<string, unknown>)["pane"];
  return typeof pane === "number" && Number.isSafeInteger(pane) && pane >= 0 ? pane : null;
}

/** The command cut into runs: `[text, matched]`, so the page can wrap the matched runs. Indices count characters. */
export function pieces(cmd: string, marks: readonly number[]): [string, boolean][] {
  const chars = Array.from(cmd);
  const on = new Set(marks);
  const out: [string, boolean][] = [];
  chars.forEach((c, i) => {
    const hit = on.has(i);
    const last = out[out.length - 1];
    if (last && last[1] === hit) last[0] += c;
    else out.push([c, hit]);
  });
  return out;
}

/** `2 h ago`, `3 d ago`, `now`: how long ago `last` was, for the time `now` (both in seconds). */
export function ago(last: number, now: number): string {
  const s = Math.max(0, now - last);
  if (s < 60) return "now";
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  if (s < 7 * 86400) return `${Math.floor(s / 86400)} d ago`;
  if (s < 60 * 86400) return `${Math.floor(s / (7 * 86400))} w ago`;
  return `${Math.floor(s / (30 * 86400))} mo ago`;
}

/** The directory with the home folder as `~`, when `home` is known. */
export function shortDir(cwd: string, home: string): string {
  if (home !== "" && (cwd === home || cwd.startsWith(`${home}/`))) return `~${cwd.slice(home.length)}`;
  return cwd;
}

/** The line under a command: directory, age, how often, and a failure said in words. */
export function meta(h: Hit, now: number, home = ""): string {
  const parts = [shortDir(h.cwd, home), ago(h.last, now)].filter((p) => p !== "");
  if (h.count > 1) parts.push(`×${h.count}`);
  if (h.exit !== null && h.exit !== 0) parts.push(`exit ${h.exit}`);
  return parts.join(" · ");
}
