// Pictures in a pane (the Kitty graphics protocol): the page draws what a program put in the grid. A frame carries
// placements, sixteen numbers each; the pixels are pulled, never pushed: for each placement whose (image, generation)
// the page does not hold, it asks the core for the bytes with `image_data`, piece by piece, checks every header
// against what it asked, assembles straight RGBA and makes one bitmap of it. Nothing the core sends is trusted: the
// sizes are checked before anything is allocated, a piece that does not match the request fails its generation, and
// the bitmaps a pane keeps never exceed the person's own memory setting.
//
// The store is a class that takes its transport and its bitmap maker as parameters, so node tests it with fakes (this
// file has no DOM and no bridge at import). The page's one store is `images`, wired to `image_data` and
// `createImageBitmap` below. Nothing here logs sizes or bytes, and no text of the core is ever shown.
import { motionReduced, onPreferenceChange } from "./a11y.js";
import type { ImageDraw } from "./frame.js";
import type { PlacementMsg } from "./frames.js";
import { MAX_IMAGE_SIDE } from "./frames.js";
import { DEFAULT_IMAGE_MEMORY_MIB } from "./settings-model.js";
import { state } from "./state.js";

// ---- the numbers -----------------------------------------------------------------------------------

/** The header of an `image_data` answer: width, height, total length and offset of the first byte, four big-endian u32. */
export const HEADER_BYTES = 16;
/** The most bytes of pixels one answer carries; the core cuts at the same number. */
export const CHUNK_BYTES = 512 * 1024;
/** The most `image_data` requests in flight for one pane. */
export const MAX_IN_FLIGHT = 2;
/** The bytes in one MiB of the `image_memory_mib` setting. */
export const MIB = 1024 * 1024;
/** How many failed (image, generation) pairs a pane remembers, so that a failed one is not asked for again. */
const MAX_FAILED = 256;

/**
 * Why a picture failed, which decides what asks for it again. `refused`: the picture itself is not usable (a header that
 * lies, sizes that do not add up, a piece of the wrong length, no bitmap could be made, larger than the whole budget):
 * never asked for again for that generation, only a change of the budget gives it another try. `fit`: it does not fit
 * next to the pictures in use. `transport`: the pull itself failed. The last two can go away.
 */
type FailKind = "refused" | "fit" | "transport";

/** A failed (image, generation) pair and why. */
interface Failure {
  generation: number;
  kind: FailKind;
}

// ---- placements to rectangles -------------------------------------------------------------------------

/** What the rectangle of a placement is measured against: the cell size the core was told and the one now. */
export interface PlaceGeometry {
  /** The cell size in device pixels that the page last told the core for this pane; absent means the size now. */
  told?: { cellW: number; cellH: number };
  /** The cell size now, CSS pixels. */
  cellW: number;
  cellH: number;
  dpr: number;
  /** The grid of the pane's frame, in cells. */
  cols: number;
  rows: number;
}

/**
 * Where a placement goes, in CSS pixels relative to the grid's top-left corner, or null when it cannot be drawn: it is
 * not a finite size, or it lies wholly outside the grid's box (`cols` by `rows` cells), which also means it is not worth
 * asking for. The core measures in device pixels of the cell size it was told, so the offsets and the size are scaled
 * by (cell size now) / (cell size told) before the device pixels become CSS pixels; the column and the row are whole
 * cells and use the cell size now. A font or zoom change between a placement and the next frame therefore moves the
 * picture with the cells instead of tearing it.
 */
export function placementRect(p: PlacementMsg, g: PlaceGeometry): { x: number; y: number; w: number; h: number } | null {
  const [, , , , col, row, , , dx, dy, pxW, pxH] = p;
  const sx = g.told && g.told.cellW > 0 ? (g.cellW * g.dpr) / g.told.cellW : 1;
  const sy = g.told && g.told.cellH > 0 ? (g.cellH * g.dpr) / g.told.cellH : 1;
  const x = col * g.cellW + (dx * sx) / g.dpr;
  const y = row * g.cellH + (dy * sy) / g.dpr;
  const w = (pxW * sx) / g.dpr;
  const h = (pxH * sy) / g.dpr;
  if (![x, y, w, h].every(Number.isFinite) || w <= 0 || h <= 0) return null;
  if (x >= g.cols * g.cellW || y >= g.rows * g.cellH || x + w <= 0 || y + h <= 0) return null;
  return { x, y, w, h };
}

/** The (image, generation) pairs the placements ask for, leaving out those wholly outside the grid. */
export function wantedOf(list: readonly PlacementMsg[], g: PlaceGeometry): Wanted[] {
  const out: Wanted[] = [];
  for (const p of list) if (placementRect(p, g)) out.push({ image: p[0], generation: p[1] });
  return out;
}

// ---- the store -----------------------------------------------------------------------------------------

/** What the store needs of a bitmap: its size and a way to let go of it. */
export interface PictureBitmap {
  readonly width: number;
  readonly height: number;
  close(): void;
}

/** Where the store gets pixels and bitmaps from; the page's own are below. */
export interface StoreDeps<B extends PictureBitmap> {
  /** One `image_data` call. The raw answer is whatever the transport gave: a buffer, a view or an array of bytes. */
  fetchChunk(pane: number, image: number, generation: number, offset: number): Promise<unknown>;
  /** One bitmap from straight RGBA (`width * height * 4` bytes). */
  makeBitmap(width: number, height: number, rgba: Uint8ClampedArray<ArrayBuffer>): Promise<B>;
  /** A bitmap arrived: the scene draws again. */
  ready(): void;
  /** The picture memory of one pane in bytes, read at each `want`; 0 means pictures are off. */
  budget(): number;
}

/** A picture a placement asks for. */
export interface Wanted {
  image: number;
  generation: number;
}

/** One picture being pulled. */
interface Job {
  image: number;
  generation: number;
  /** False once the job ended or was dropped: its answers are discarded and it asks for nothing. */
  live: boolean;
  /** The header is not known until the first answer: that answer is the only request until then. */
  headAsked: boolean;
  width: number;
  height: number;
  total: number;
  buf: Uint8ClampedArray<ArrayBuffer> | null;
  /** The offset of the next piece to ask for; the pieces are `CHUNK_BYTES` apart. */
  next: number;
  /** Bytes received so far. */
  filled: number;
  /** The bytes this job holds against the pane's budget from its header until it ends. */
  reserved: number;
}

interface Held<B> {
  generation: number;
  bitmap: B;
  bytes: number;
  /** When the bitmap was last drawn, on the store's own counter: the smaller, the longer ago. */
  used: number;
}

interface PaneImages<B> {
  /** The pane's id. */
  pane: number;
  /** The decoded pictures by image id. */
  held: Map<number, Held<B>>;
  jobs: Map<number, Job>;
  /** The generation each placed picture should have now. */
  wanted: Map<number, number>;
  /** The image that failed, by image id: its generation is not asked for again until an event clears the mark. */
  failed: Map<number, Failure>;
  inFlight: number;
  /** The bytes the jobs have reserved for pictures that are being assembled. */
  reserved: number;
  /** The budget the last `want` saw: a change gives the pictures that failed another try. */
  budget: number;
}

/** What the store holds for a pane, for tests and the status line. */
export interface PaneStats {
  held: number;
  bytes: number;
  jobs: number;
}

/** The bytes of a raw answer: a buffer, a view or an array of numbers (as `pdfview.ts` reads one); null for anything else. */
function bytesOf(raw: unknown): Uint8Array | null {
  if (raw instanceof ArrayBuffer) return new Uint8Array(raw);
  if (ArrayBuffer.isView(raw)) return new Uint8Array(raw.buffer, raw.byteOffset, raw.byteLength);
  if (Array.isArray(raw) && raw.length <= HEADER_BYTES + CHUNK_BYTES) return Uint8Array.from(raw as number[]);
  return null;
}

export class ImageStore<B extends PictureBitmap> {
  private readonly panes = new Map<number, PaneImages<B>>();
  private tick = 0;
  /** The panes in which a bitmap was closed to make room since the marks were last looked at. */
  private readonly freed = new Set<number>();

  constructor(private readonly deps: StoreDeps<B>) {}

  /** Whether the store holds or does anything for this pane. A pane without pictures costs nothing. */
  has(pane: number): boolean {
    return this.panes.has(pane);
  }

  stats(pane: number): PaneStats {
    const ps = this.panes.get(pane);
    if (!ps) return { held: 0, bytes: 0, jobs: 0 };
    let bytes = 0;
    for (const h of ps.held.values()) bytes += h.bytes;
    return { held: ps.held.size, bytes, jobs: ps.jobs.size };
  }

  /**
   * The pictures the pane's placements ask for now. Starts the pulls for those the store does not hold, drops those no
   * longer wanted (latest wins) and, when the budget is 0, forgets everything: pictures are off, nothing is asked for.
   */
  want(pane: number, items: readonly Wanted[]): void {
    const budget = this.deps.budget();
    if (!(budget > 0)) {
      this.forget(pane);
      return;
    }
    let ps = this.panes.get(pane);
    if (!ps) {
      if (items.length === 0) return;
      ps = { pane, held: new Map(), jobs: new Map(), wanted: new Map(), failed: new Map(), inFlight: 0, reserved: 0, budget };
      this.panes.set(pane, ps);
    }
    if (ps.budget !== budget) {
      ps.budget = budget;
      ps.failed.clear(); // a picture that did not fit may fit now
    }
    const wanted = new Map<number, number>();
    for (const { image, generation } of items) {
      const have = wanted.get(image);
      if (have === undefined || generation > have) wanted.set(image, generation);
    }
    ps.wanted = wanted;
    for (const [image, job] of ps.jobs) if (wanted.get(image) !== job.generation) this.drop(ps, image, job);
    this.trim(ps, budget);
    const others = this.clearFreed();
    this.plan(pane, ps);
    for (const o of others) if (o !== ps) this.plan(o.pane, o);
  }

  /**
   * Something that can make a failed pull succeed happened (a connection that opened, a reconnect): every mark that is
   * not a refusal is cleared in every pane and the pulls restart at once for the pictures the placements still ask for.
   * A picture the core refused for a property of its own is not asked for again.
   */
  retry(): void {
    for (const ps of [...this.panes.values()]) {
      if (this.clearMarks(ps) > 0) this.plan(ps.pane, ps);
    }
  }

  /**
   * The pictures to draw for a pane's placements: those whose bitmap the store holds, with their rectangles, in the
   * order the layers need (layer, then `z`, then image id). A bitmap that is held for an older generation of the
   * picture stands in until the new one has arrived, so an animation never blinks.
   */
  resolve(pane: number, list: readonly PlacementMsg[], g: PlaceGeometry): ImageDraw<B>[] {
    const ps = this.panes.get(pane);
    if (!ps || ps.held.size === 0 || !(this.deps.budget() > 0)) return [];
    const out: ImageDraw<B>[] = [];
    const now = ++this.tick;
    for (const p of list) {
      const held = ps.held.get(p[0]);
      if (!held) continue;
      const rect = placementRect(p, g);
      if (!rect) continue;
      const [, , layer, z, , , , , , , , , sx, sy, sw, sh] = p;
      if (sx + sw > held.bitmap.width || sy + sh > held.bitmap.height) continue;
      held.used = now;
      out.push({
        layer: layer as 0 | 1 | 2, z, id: p[0], bitmap: held.bitmap,
        dx: rect.x, dy: rect.y, dw: rect.w, dh: rect.h, sx, sy, sw, sh,
      });
    }
    if (out.length > 1) out.sort((a, b) => a.layer - b.layer || a.z - b.z || a.id - b.id);
    return out;
  }

  /** The pane is gone (or pictures are off): close every bitmap, cancel every pull and forget what failed. */
  forget(pane: number): void {
    const ps = this.panes.get(pane);
    if (!ps) return;
    for (const [image, job] of ps.jobs) this.drop(ps, image, job);
    for (const h of ps.held.values()) h.bitmap.close();
    ps.held.clear();
    ps.wanted.clear();
    ps.failed.clear();
    this.freed.delete(pane);
    this.tidy(pane, ps);
  }

  // ---- the pulls ----------------------------------------------------------------------------------

  /** End a job without a result: its answers that are still on their way are discarded when they arrive. */
  private drop(ps: PaneImages<B>, image: number, job: Job): void {
    job.live = false;
    job.buf = null;
    ps.reserved -= job.reserved;
    job.reserved = 0;
    if (ps.jobs.get(image) === job) ps.jobs.delete(image);
  }

  /** End a job because its pixels cannot be used: the generation is not asked for again until an event clears the mark. */
  private fail(ps: PaneImages<B>, image: number, job: Job, kind: FailKind): void {
    this.drop(ps, image, job);
    ps.failed.set(image, { generation: job.generation, kind });
    while (ps.failed.size > MAX_FAILED) ps.failed.delete(ps.failed.keys().next().value as number);
  }

  /** Clear the marks of a pane that can go away (`fit` and `transport`); a refusal stays. How many were cleared. */
  private clearMarks(ps: PaneImages<B>): number {
    let n = 0;
    for (const [image, f] of ps.failed) {
      if (f.kind === "refused") continue;
      ps.failed.delete(image);
      n++;
    }
    return n;
  }

  /**
   * Memory was freed in the panes that closed a bitmap to make room: their marks that can go away are cleared. The
   * panes whose marks were cleared are returned, for their pulls to restart. A job that ends by failing frees nothing
   * and never gets here.
   */
  private clearFreed(): PaneImages<B>[] {
    const out: PaneImages<B>[] = [];
    for (const pane of this.freed) {
      const ps = this.panes.get(pane);
      if (ps && this.clearMarks(ps) > 0) out.push(ps);
    }
    this.freed.clear();
    return out;
  }

  /** Close a bitmap the pane lets go of to make room, and note that memory was freed. */
  private evict(ps: PaneImages<B>, image: number, h: Held<B>): void {
    h.bitmap.close();
    ps.held.delete(image);
    this.freed.add(ps.pane);
  }

  /** Start a pull for every picture the pane wants that it does not hold, pull or remember as failed, and send what may go out. */
  private plan(pane: number, ps: PaneImages<B>): void {
    for (const [image, generation] of ps.wanted) {
      if (ps.held.get(image)?.generation === generation) continue;
      if (ps.jobs.has(image) || ps.failed.get(image)?.generation === generation) continue;
      ps.jobs.set(image, {
        image, generation, live: true, headAsked: false, width: 0, height: 0, total: 0, buf: null, next: 0, filled: 0,
        reserved: 0,
      });
    }
    this.pump(pane, ps);
    this.tidy(pane, ps);
  }

  /** The pane's state is dropped when it holds nothing, waits for nothing and remembers no failure (`forget` clears those). */
  private tidy(pane: number, ps: PaneImages<B>): void {
    const idle = ps.held.size === 0 && ps.jobs.size === 0 && ps.inFlight === 0 && ps.failed.size === 0;
    if (idle && this.panes.get(pane) === ps) this.panes.delete(pane);
  }

  /** The bytes the pane holds against its budget: decoded bitmaps and the buffers being assembled. */
  private bytesIn(ps: PaneImages<B>): number {
    let n = ps.reserved;
    for (const h of ps.held.values()) n += h.bytes;
    return n;
  }

  /**
   * Evict bitmaps until `need` more bytes fit in `budget`: those no placement uses first, the least recently drawn
   * first. Every evicted bitmap is closed. False when it still does not fit.
   */
  private makeRoom(ps: PaneImages<B>, need: number, budget: number): boolean {
    let used = this.bytesIn(ps);
    if (used + need <= budget) return true;
    const unused = [...ps.held].filter(([image]) => !ps.wanted.has(image)).sort((a, b) => a[1].used - b[1].used);
    for (const [image, h] of unused) {
      this.evict(ps, image, h);
      used -= h.bytes;
      if (used + need <= budget) return true;
    }
    return false;
  }

  /** A budget that shrank: bring the pane under it, unused bitmaps first, then the least recently drawn of the rest. */
  private trim(ps: PaneImages<B>, budget: number): void {
    if (this.bytesIn(ps) <= budget) return;
    if (this.makeRoom(ps, 0, budget)) return;
    const rest = [...ps.held].sort((a, b) => a[1].used - b[1].used);
    for (const [image, h] of rest) {
      this.evict(ps, image, h);
      if (this.bytesIn(ps) <= budget) return;
    }
    // Only buffers still being assembled remain: they were reserved under the old budget.
    for (const [image, job] of ps.jobs) {
      this.drop(ps, image, job);
      if (this.bytesIn(ps) <= budget) return;
    }
  }

  /** Send requests while fewer than `MAX_IN_FLIGHT` are out. A request for a job that is no longer wanted is never sent. */
  private pump(pane: number, ps: PaneImages<B>): void {
    while (ps.inFlight < MAX_IN_FLIGHT) {
      let sent = false;
      for (const job of ps.jobs.values()) {
        if (!job.live) continue;
        let offset: number;
        if (!job.headAsked) {
          job.headAsked = true;
          offset = 0;
        } else if (job.buf && job.next < job.total) {
          offset = job.next;
          job.next += CHUNK_BYTES;
        } else {
          continue;
        }
        ps.inFlight++;
        void this.request(pane, ps, job, offset);
        sent = true;
        break;
      }
      if (!sent) return;
    }
  }

  private async request(pane: number, ps: PaneImages<B>, job: Job, offset: number): Promise<void> {
    let raw: unknown;
    let failed = false;
    try {
      raw = await this.deps.fetchChunk(pane, job.image, job.generation, offset);
    } catch {
      failed = true; // the core's text is not shown or kept
    }
    ps.inFlight--;
    let kind: FailKind | null = null;
    if (job.live) kind = failed ? "transport" : this.accept(ps, job, offset, raw);
    // What this answer freed clears the marks first, so that the job that failed is marked after and stays marked.
    const others = this.clearFreed();
    if (kind !== null) this.fail(ps, job.image, job, kind);
    this.plan(pane, ps);
    for (const o of others) if (o !== ps) this.plan(o.pane, o);
  }

  /** Check one answer against what was asked and take its bytes; why it failed when anything does not match, else null. */
  private accept(ps: PaneImages<B>, job: Job, offset: number, raw: unknown): FailKind | null {
    const bytes = bytesOf(raw);
    if (!bytes || bytes.length < HEADER_BYTES) return "refused";
    const head = new DataView(bytes.buffer, bytes.byteOffset, HEADER_BYTES);
    const width = head.getUint32(0);
    const height = head.getUint32(4);
    const total = head.getUint32(8);
    if (head.getUint32(12) !== offset) return "refused";
    if (job.buf === null) {
      // The first answer: the sizes decide the allocation, so they are checked before it.
      if (width < 1 || height < 1 || width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE) return "refused";
      if (total !== width * height * 4) return "refused";
      // Larger than the whole budget: no event but a new budget can change that, so it is the picture's own fault.
      if (!(total <= this.deps.budget())) return "refused";
      if (!this.reserve(ps, job, total)) return "fit";
      job.width = width;
      job.height = height;
      job.total = total;
      job.buf = new Uint8ClampedArray(total);
      job.next = CHUNK_BYTES;
    } else if (width !== job.width || height !== job.height || total !== job.total) {
      return "refused";
    }
    const buf = job.buf;
    const body = bytes.subarray(HEADER_BYTES);
    // Every piece is exactly as long as the core cuts it: a shorter or a longer one is a lie about this request.
    if (body.length !== Math.min(CHUNK_BYTES, job.total - offset)) return "refused";
    buf.set(body, offset);
    job.filled += body.length;
    if (job.filled === job.total) this.finish(ps, job, buf);
    return null;
  }

  /**
   * Make room for `total` bytes of a picture that is about to be allocated, and hold them against the budget; false
   * when it cannot fit. This runs on the header, before the allocation: a picture over the whole budget is never
   * fetched past its header, and one that does not fit next to the pictures in use is not fetched either (the ones
   * in use stay). Unused bitmaps go first; the older generation of the same picture, which stands in until the new
   * one arrives, goes last of all.
   */
  private reserve(ps: PaneImages<B>, job: Job, total: number): boolean {
    const budget = this.deps.budget();
    if (!(total <= budget)) return false;
    if (!this.makeRoom(ps, total, budget)) {
      const own = ps.held.get(job.image);
      if (!own) return false;
      this.evict(ps, job.image, own);
      if (!this.makeRoom(ps, total, budget)) return false;
    }
    ps.reserved += total;
    job.reserved = total;
    return true;
  }

  /** All pieces are in: make the bitmap, and install it unless the job was dropped meanwhile. */
  private finish(ps: PaneImages<B>, job: Job, buf: Uint8ClampedArray<ArrayBuffer>): void {
    job.buf = null;
    this.deps.makeBitmap(job.width, job.height, buf).then(
      (bitmap) => {
        if (!job.live) {
          bitmap.close();
          return;
        }
        this.drop(ps, job.image, job);
        ps.held.get(job.image)?.bitmap.close();
        ps.held.set(job.image, { generation: job.generation, bitmap, bytes: job.total, used: ++this.tick });
        this.deps.ready();
      },
      () => {
        if (job.live) this.fail(ps, job.image, job, "refused"); // no bitmap could be made of these pixels
      },
    );
  }
}

// ---- the page's store ----------------------------------------------------------------------------------

let redraw: () => void = () => {};
/** The scene's redraw: called when a bitmap arrived. */
export function onImageReady(fn: () => void): void {
  redraw = fn;
}

let bridge: Promise<typeof import("./bridge.js")> | null = null;
/** The bridge, loaded at the first request: this file stays importable where there is no window. */
const bridged = () => (bridge ??= import("./bridge.js"));

/**
 * The page's one store. Alpha: the engine's pixels are straight (not premultiplied) RGBA, which is what an `ImageData`
 * holds; the bitmap is made with the default options and the canvas blends it as straight alpha. `premultiplyAlpha:
 * "none"` is left out on purpose: WebKit then draws a half-transparent pixel as an opaque one (measured).
 */
export const images = new ImageStore<ImageBitmap>({
  fetchChunk: async (pane, image, generation, offset) =>
    (await bridged()).call<unknown>("image_data", { pane, image, generation, offset }),
  makeBitmap: (width, height, rgba) =>
    createImageBitmap(new ImageData(rgba, width, height)),
  ready: () => redraw(),
  budget: () => (state.settings?.settings.image_memory_mib ?? DEFAULT_IMAGE_MEMORY_MIB) * MIB,
});

// ---- the person's motion preference -----------------------------------------------------------------------

/**
 * Tell the core whether animated pictures may play: `enabled` is false exactly when motion is reduced (the setting, or
 * the system preference when the setting is `system`). It is sent once at start, on every change of that fact (a change
 * of a preference that leaves it as it was sends nothing) and after every reconnect to a server, whose worker may have
 * been restarted with the default. The core pauses and the page keeps no timer for pictures: a picture that was never
 * started shows its first frame, one that was running stays on the frame it shows. The switch is one per person in the
 * core, so two windows with different preferences share it and the last to report wins. A core that refuses the call
 * (an older build) leaves the page working. Call once, after the preferences were first resolved.
 */
export async function initImages(): Promise<void> {
  const { call, onConnection } = await bridged();
  let sent: boolean | null = null;
  const report = (again: boolean): void => {
    const enabled = !motionReduced();
    if (!again && sent === enabled) return;
    sent = enabled;
    void call("set_image_animation", { enabled }).catch(() => {});
  };
  report(false);
  onPreferenceChange(() => report(false));
  onConnection((info) => {
    if (info.state !== "open") return;
    images.retry(); // a pull that failed while the connection was down is asked for again, resumed or not
    if (info.resumed) report(true);
  });
}
