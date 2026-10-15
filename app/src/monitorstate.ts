// The system monitor block's data: the shape of the core's `monitor_sample` answer, read defensively, and the words and
// bars that show it. Pure functions, so node tests them.

export interface MonProc {
  pid: number;
  name: string;
  cpu: number;
  mem: number;
}

export interface MonSample {
  cpu: number;
  cores: number;
  mem_used: number;
  mem_total: number;
  net_rx: number;
  net_tx: number;
  uptime: number;
  procs: MonProc[];
}

const num = (v: unknown, max = Number.MAX_SAFE_INTEGER): number =>
  typeof v === "number" && Number.isFinite(v) && v >= 0 ? Math.min(v, max) : 0;

/** Read the answer of `monitor_sample`; `null` when it is not an object. A bad field is zero, a bad list is empty. */
export function readSample(raw: unknown): MonSample | null {
  if (typeof raw !== "object" || raw === null) return null;
  const o = raw as Record<string, unknown>;
  const list = Array.isArray(o["procs"]) ? (o["procs"] as unknown[]).slice(0, 32) : [];
  const procs: MonProc[] = [];
  for (const p of list) {
    if (typeof p !== "object" || p === null) continue;
    const q = p as Record<string, unknown>;
    procs.push({
      pid: num(q["pid"]),
      name: typeof q["name"] === "string" ? q["name"].slice(0, 40) : "",
      cpu: num(q["cpu"], 100 * 4096),
      mem: num(q["mem"]),
    });
  }
  return {
    cpu: num(o["cpu"], 100),
    cores: num(o["cores"], 4096),
    mem_used: num(o["mem_used"]),
    mem_total: num(o["mem_total"]),
    net_rx: num(o["net_rx"]),
    net_tx: num(o["net_tx"]),
    uptime: num(o["uptime"]),
    procs,
  };
}

export type Level = "ok" | "warn" | "danger";

/** How many of `segments` are lit for `percent`, and the level they are drawn in. A bar is never empty above zero. */
export function bar(percent: number, segments = 8): { on: number; level: Level } {
  const p = Math.max(0, Math.min(100, percent));
  const on = p === 0 ? 0 : Math.max(1, Math.round((p / 100) * segments));
  return { on, level: p > 90 ? "danger" : p > 70 ? "warn" : "ok" };
}

/** Bytes as `6.1G`, `412M`, `84K`, `0`. Binary units, one decimal under 10. */
export function bytes(n: number): string {
  const units = ["B", "K", "M", "G", "T"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  if (i === 0) return `${Math.round(v)}${units[0]}`;
  return `${v < 10 ? v.toFixed(1) : Math.round(v)}${units[i]}`;
}

/** `1.2 MB/s` for a rate in bytes per second. */
export function rate(n: number): string {
  const b = bytes(n);
  return `${b.slice(0, -1)} ${b.slice(-1)}B/s`.replace(" BB/s", " B/s");
}

/** `6.1/16G`: used and total memory, the unit of the total. */
export function memory(used: number, total: number): string {
  const t = bytes(total);
  const u = bytes(used);
  const unit = t.slice(-1);
  return u.endsWith(unit) ? `${u.slice(0, -1)}/${t}` : `${u}/${t}`;
}

/** `3 d 4 h`, `4 h 12 min`, `12 min`: how long the computer has been up. */
export function uptime(seconds: number): string {
  const d = Math.floor(seconds / 86400);
  const h = Math.floor((seconds % 86400) / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  return d > 0 ? `${d} d ${h} h` : h > 0 ? `${h} h ${m} min` : `${m} min`;
}
