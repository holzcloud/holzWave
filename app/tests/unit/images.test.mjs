// The picture store on the compiled output in dist/: what the page does with placements and with the pixels the core
// sends. The store takes its transport and its bitmap maker as parameters, so these tests drive it with fakes: every
// `image_data` call is a promise the test answers (or fails) by hand, in the order it likes, and a bitmap is a small
// object that counts its `close()` calls. No DOM, no clock.
import test from "node:test";
import assert from "node:assert/strict";
import { CHUNK_BYTES, HEADER_BYTES, ImageStore, MAX_IN_FLIGHT, MIB, placementRect, wantedOf } from "../../dist/images.js";
import { MAX_PLACEMENTS, applyFrame, usablePlacement, usablePlacements } from "../../dist/frames.js";

/** Let every promise chain that can run, run. */
const flush = () => new Promise((done) => setTimeout(done, 0));

/** A store with fake transport and fake bitmaps. `calls` are the `image_data` requests, oldest first, each answerable. */
function rig(budget = 64 * MIB) {
  const calls = [];
  const bitmaps = [];
  const knobs = { budget, ready: 0 };
  const store = new ImageStore({
    fetchChunk: (pane, image, generation, offset) =>
      new Promise((resolve, reject) => calls.push({ pane, image, generation, offset, resolve, reject })),
    makeBitmap: async (width, height, rgba) => {
      const bitmap = { width, height, rgba, closed: 0, close() { this.closed++; } };
      bitmaps.push(bitmap);
      return bitmap;
    },
    ready: () => knobs.ready++,
    budget: () => knobs.budget,
  });
  return { store, calls, bitmaps, knobs };
}

/** `w * h * 4` bytes that differ from place to place, so a piece in the wrong place is noticed. */
const pixels = (w, h, seed = 1) => Uint8ClampedArray.from({ length: w * h * 4 }, (_, i) => (i * 7 + (i >> 8) * 13 + seed) & 255);

/** The raw answer for `offset`: the 16-byte header (big endian) and the next piece. `over` puts a lie into the header. */
function reply(w, h, rgba, offset, over = {}) {
  const total = w * h * 4;
  const from = Math.min(offset, total);
  const body = rgba.subarray(from, Math.min(from + CHUNK_BYTES, total));
  const out = new Uint8Array(HEADER_BYTES + body.length);
  const view = new DataView(out.buffer);
  view.setUint32(0, over.w ?? w);
  view.setUint32(4, over.h ?? h);
  view.setUint32(8, over.total ?? total);
  view.setUint32(12, over.offset ?? from);
  out.set(body, HEADER_BYTES);
  return out.buffer;
}

const respond = async (call, raw) => {
  call.resolve(raw);
  await flush();
};

const want = (image, generation = 1) => ({ image, generation });

/** A placement as the core sends it, with the fields that matter and the rest at their quiet values. */
const place = (image, over = {}) => {
  const p = {
    generation: 1, layer: 2, z: 0, col: 0, row: 0, cols: 1, rows: 1, dx: 0, dy: 0, w: 8, h: 16, sx: 0, sy: 0, sw: 2, sh: 1, ...over,
  };
  return [image, p.generation, p.layer, p.z, p.col, p.row, p.cols, p.rows, p.dx, p.dy, p.w, p.h, p.sx, p.sy, p.sw, p.sh];
};

const GEO = { cellW: 8, cellH: 16, dpr: 1, cols: 20, rows: 4 };

// ---- placements in the mirror -----------------------------------------------------------------------

const MODES = { alt: false, bracketed_paste: false, mouse: false };
const msg = (seq, over = {}) => ({
  pane: 1, seq, full: false, cols: 4, rows: 2, fg: 0xffffff, bg: 0, cursor: null, scrollbar: [0, 2, 2], selection: [],
  matches: [], search: null, copy_cursor: null, modes: MODES, changed: [], ...over,
});

test("a frame with images replaces the list, one without keeps it, an empty list clears it, a full frame sets it", () => {
  const a = place(1);
  const b = place(2, { col: 1 });
  let { mirror } = applyFrame(undefined, msg(1, { full: true, images: [a] }));
  assert.deepEqual(mirror.images, [a]);
  ({ mirror } = applyFrame(mirror, msg(2)));
  assert.deepEqual(mirror.images, [a], "a frame without the field keeps the list");
  ({ mirror } = applyFrame(mirror, msg(3, { images: [b] })));
  assert.deepEqual(mirror.images, [b], "a frame with the field replaces it");
  ({ mirror } = applyFrame(mirror, msg(4, { images: [] })));
  assert.deepEqual(mirror.images, [], "an empty list clears it");
  ({ mirror } = applyFrame(mirror, msg(5, { images: [a] })));
  ({ mirror } = applyFrame(mirror, msg(6, { full: true })));
  assert.deepEqual(mirror.images, [], "a full frame without the field has no pictures");
});

test("a placement that fails the check is skipped and at most 256 entries are read", () => {
  const sound = place(9);
  const bad = [
    place(1, { layer: 3 }), place(1, { layer: -1 }), place(1, { generation: 0 }), place(1, { generation: 1.5 }),
    place(1, { w: 0 }), place(1, { h: 0 }), place(1, { sw: 0 }), place(1, { sw: 16385 }), place(1, { sx: -1 }),
    place(1, { col: 2 ** 40 }), place(1, { z: 2 ** 40 }), place(-1), place(2 ** 32), sound.slice(0, 15), [...sound, 1],
    sound.map((n, i) => (i === 3 ? "0" : n)), sound.map((n, i) => (i === 3 ? Number.NaN : n)),
    sound.map((n, i) => (i === 3 ? Infinity : n)), null, undefined, "x", 7, {},
  ];
  assert.deepEqual(usablePlacements([...bad, sound]), [sound]);
  for (const entry of bad) assert.equal(usablePlacement(entry), null);
  const many = Array.from({ length: MAX_PLACEMENTS + 50 }, (_, i) => place(i + 1));
  assert.equal(usablePlacements(many).length, MAX_PLACEMENTS, "entries past the 256th are not even looked at");
  assert.deepEqual(usablePlacements("not a list"), []);
  const { mirror } = applyFrame(undefined, msg(1, { full: true, images: [...bad, sound] }));
  assert.deepEqual(mirror.images, [sound], "the mirror holds only what passed");
});

test("the rectangle is the cell position and the offsets scaled from the cell size told to the cell size now", () => {
  // Told 8 x 16 device pixels, now 10 x 20 CSS pixels at dpr 1: everything the core measured in pixels grows by 1.25.
  const g = { told: { cellW: 8, cellH: 16 }, cellW: 10, cellH: 20, dpr: 1, cols: 20, rows: 4 };
  const rect = placementRect(place(1, { col: 2, row: 1, dx: 4, dy: 8, w: 16, h: 16 }), g);
  assert.deepEqual(rect, { x: 2 * 10 + 5, y: 1 * 20 + 10, w: 20, h: 20 });
  // Told 16 x 32 device pixels at dpr 2 and the cell is still 8 x 16 CSS pixels: no scaling, device pixels become CSS pixels.
  const hi = { told: { cellW: 16, cellH: 32 }, cellW: 8, cellH: 16, dpr: 2, cols: 20, rows: 4 };
  assert.deepEqual(placementRect(place(1, { col: 1, dx: 4, w: 32, h: 32 }), hi), { x: 8 + 2, y: 0, w: 16, h: 16 });
  // Without a size told the size now is used.
  assert.deepEqual(placementRect(place(1, { col: 3, w: 8, h: 16 }), GEO), { x: 24, y: 0, w: 8, h: 16 });
});

test("a placement wholly outside the grid has no rectangle and is not wanted", () => {
  assert.equal(placementRect(place(1, { col: 20 }), GEO), null, "starts at the last column's right edge");
  assert.equal(placementRect(place(1, { row: 4 }), GEO), null);
  assert.equal(placementRect(place(1, { col: -1, w: 8 }), GEO), null, "ends at the left edge");
  assert.ok(placementRect(place(1, { col: -1, w: 9 }), GEO), "one pixel inside is visible");
  assert.ok(placementRect(place(1, { col: 19, w: 100 }), GEO), "reaching past the edge is clipped by the renderer, not here");
  const list = [place(1), place(2, { col: 30 }), place(3, { row: -5 })];
  assert.deepEqual(wantedOf(list, GEO), [{ image: 1, generation: 1 }]);
});

// ---- hostile pieces -----------------------------------------------------------------------------------

/** A header that lies fails the picture: no bitmap, no further request; the same generation is not retried, a new one is. */
async function refused(raw, why) {
  const t = rig();
  t.store.want(1, [want(5)]);
  assert.equal(t.calls.length, 1, why);
  await respond(t.calls[0], raw);
  assert.equal(t.bitmaps.length, 0, `${why}: no bitmap`);
  assert.equal(t.calls.length, 1, `${why}: no further request`);
  assert.deepEqual(t.store.stats(1), { held: 0, bytes: 0, jobs: 0 }, why);
  t.store.want(1, [want(5)]);
  assert.equal(t.calls.length, 1, `${why}: the same generation is not asked again`);
  t.store.want(1, [want(5, 2)]);
  assert.equal(t.calls.length, 2, `${why}: a new generation is`);
  assert.equal(t.calls[1].generation, 2);
}

test("a header with width 0 or a side above 16384 fails the picture", async () => {
  const px = pixels(2, 1);
  await refused(reply(2, 1, px, 0, { w: 0, total: 0 }), "width 0");
  await refused(reply(2, 1, px, 0, { h: 0, total: 0 }), "height 0");
  await refused(reply(2, 1, px, 0, { w: 16385, h: 1, total: 16385 * 4 }), "width 16385");
  await refused(reply(2, 1, px, 0, { w: 1, h: 16385, total: 16385 * 4 }), "height 16385");
});

test("a total that is not width times height times 4 fails the picture", async () => {
  const px = pixels(2, 1);
  await refused(reply(2, 1, px, 0, { total: 12 }), "total 12");
  await refused(reply(2, 1, px, 0, { total: 4 }), "total 4");
  await refused(reply(2, 1, px, 0, { total: 0 }), "total 0");
});

test("an offset other than the one asked, a piece longer than the rest and a piece shorter than the header fail the picture", async () => {
  const px = pixels(2, 1);
  await refused(reply(2, 1, px, 0, { offset: 4 }), "offset 4 for 0");
  const long = new Uint8Array(HEADER_BYTES + 12);
  long.set(new Uint8Array(reply(2, 1, px, 0)).subarray(0, HEADER_BYTES));
  await refused(long.buffer, "12 bytes for a total of 8");
  await refused(new Uint8Array(10).buffer, "10 bytes: no header");
  await refused(new ArrayBuffer(0), "no bytes");
  await refused(reply(2, 1, px, 0).slice(0, HEADER_BYTES + 4), "4 bytes for a total of 8");
  await refused("text", "not bytes");
  await refused(null, "nothing");
});

test("a request that fails fails the picture without a retry, and no text of the core is kept", async () => {
  const t = rig();
  t.store.want(1, [want(5)]);
  t.calls[0].reject("no such image");
  await flush();
  assert.equal(t.bitmaps.length, 0);
  t.store.want(1, [want(5)]);
  assert.equal(t.calls.length, 1, "not asked again");
  assert.equal(JSON.stringify(t.store.stats(1)).includes("no such"), false);
});

test("a later piece that does not match fails the picture and no more pieces are asked for", async () => {
  const t = rig();
  const [w, h] = [1024, 512]; // 2 MiB: four pieces
  const px = pixels(w, h);
  t.store.want(1, [want(5)]);
  await respond(t.calls[0], reply(w, h, px, 0));
  assert.deepEqual(t.calls.map((c) => c.offset), [0, CHUNK_BYTES, 2 * CHUNK_BYTES], "two pieces in flight after the header");
  await respond(t.calls[1], reply(w, h, px, CHUNK_BYTES, { offset: 0 }));
  assert.equal(t.calls.length, 3, "the picture failed: the fourth piece is never asked for");
  await respond(t.calls[2], reply(w, h, px, 2 * CHUNK_BYTES));
  assert.equal(t.bitmaps.length, 0, "the piece that arrived after the failure is discarded");
  assert.equal(t.calls.length, 3);
});

test("a second piece that changes the sizes fails the picture", async () => {
  const t = rig();
  const [w, h] = [768, 512];
  const px = pixels(w, h);
  t.store.want(1, [want(5)]);
  await respond(t.calls[0], reply(w, h, px, 0));
  await respond(t.calls[1], reply(w, h, px, CHUNK_BYTES, { w: 512, h: 768 }));
  await respond(t.calls[2], reply(w, h, px, 2 * CHUNK_BYTES));
  assert.equal(t.bitmaps.length, 0);
});

// ---- chunks ----------------------------------------------------------------------------------------------

test("an image of 1.5 MiB arrives in three pieces, asked at 0, 512 KiB and 1 MiB, joined in order whatever the arrival order", async () => {
  const t = rig();
  const [w, h] = [768, 512];
  const px = pixels(w, h, 3);
  t.store.want(1, [want(5)]);
  assert.equal(t.calls.length, 1, "the header comes first");
  await respond(t.calls[0], reply(w, h, px, 0));
  assert.deepEqual(t.calls.map((c) => [c.pane, c.image, c.generation, c.offset]), [
    [1, 5, 1, 0], [1, 5, 1, CHUNK_BYTES], [1, 5, 1, 2 * CHUNK_BYTES],
  ]);
  await respond(t.calls[2], reply(w, h, px, 2 * CHUNK_BYTES)); // the last piece first
  assert.equal(t.bitmaps.length, 0, "not complete yet");
  await respond(t.calls[1], reply(w, h, px, CHUNK_BYTES));
  assert.equal(t.bitmaps.length, 1, "exactly one bitmap");
  assert.equal(t.bitmaps[0].width, w);
  assert.equal(t.bitmaps[0].height, h);
  assert.equal(Buffer.compare(Buffer.from(t.bitmaps[0].rgba), Buffer.from(px)), 0, "the pieces are in their places");
  assert.equal(t.knobs.ready, 1, "the scene is told once");
  assert.deepEqual(t.store.stats(1), { held: 1, bytes: w * h * 4, jobs: 0 });
});

test("at most two requests are in flight for one pane, and a pane has its own two", async () => {
  const t = rig();
  t.store.want(1, [want(1), want(2), want(3), want(4)]);
  assert.equal(t.calls.length, MAX_IN_FLIGHT, "two requests, not four");
  t.store.want(2, [want(1), want(2), want(3)]);
  assert.equal(t.calls.length, 2 * MAX_IN_FLIGHT, "the other pane has its own limit");
  const px = pixels(2, 1);
  await respond(t.calls[0], reply(2, 1, px, 0));
  assert.equal(t.calls.filter((c) => c.pane === 1).length, 3, "an answer frees a place for the next request of that pane");
  assert.equal(t.bitmaps.length, 1);
});

test("a picture dropped from the list before its next piece was asked stops asking, and a late answer makes no bitmap", async () => {
  const t = rig();
  const [w, h] = [1024, 512];
  const px = pixels(w, h);
  t.store.want(1, [want(5)]);
  await respond(t.calls[0], reply(w, h, px, 0));
  assert.equal(t.calls.length, 3);
  t.store.want(1, []); // scrolled away: not wanted any more
  await respond(t.calls[1], reply(w, h, px, CHUNK_BYTES));
  await respond(t.calls[2], reply(w, h, px, 2 * CHUNK_BYTES));
  assert.equal(t.calls.length, 3, "the fourth piece is never asked for");
  assert.equal(t.bitmaps.length, 0, "the answers that arrived after the drop are discarded");
  assert.equal(t.store.has(1), false, "and the pane holds nothing");
});

test("a request for a dropped picture is not sent: latest wins", async () => {
  const t = rig();
  t.store.want(1, [want(1), want(2), want(3)]);
  assert.equal(t.calls.length, 2);
  t.store.want(1, [want(3)]); // 1 and 2 are gone, 3 is wanted
  const px = pixels(2, 1);
  await respond(t.calls[0], reply(2, 1, px, 0)); // the answer for the dropped picture 1
  assert.equal(t.bitmaps.length, 0, "a dropped picture's answer makes nothing");
  assert.ok(t.calls.some((c) => c.image === 3), "the wanted picture is asked for");
  assert.ok(!t.calls.some((c) => c.image === 2 && c !== t.calls[1]), "the dropped picture 2 is not asked for again");
});

test("a newer generation replaces the bitmap, which is closed, and the old one stands in until then", async () => {
  const t = rig();
  const px = pixels(2, 1);
  const px2 = pixels(2, 1, 9);
  t.store.want(1, [want(5, 1)]);
  await respond(t.calls[0], reply(2, 1, px, 0));
  const first = t.bitmaps[0];
  t.store.want(1, [want(5, 2)]);
  assert.equal(t.calls[1].generation, 2);
  const drawn = t.store.resolve(1, [place(5, { generation: 2 })], GEO);
  assert.equal(drawn.length, 1, "the old generation is drawn until the new one has arrived");
  assert.equal(drawn[0].bitmap, first);
  assert.equal(first.closed, 0);
  await respond(t.calls[1], reply(2, 1, px2, 0));
  assert.equal(first.closed, 1, "the replaced bitmap is closed");
  assert.equal(t.bitmaps.length, 2);
  assert.equal(t.store.resolve(1, [place(5, { generation: 2 })], GEO)[0].bitmap, t.bitmaps[1]);
  assert.equal(t.store.stats(1).held, 1);
});

// ---- the budget ---------------------------------------------------------------------------------------------

const KIB600 = [300, 512]; // 300 x 512 x 4 = 614 400 bytes
const KIB400 = [320, 320]; // 409 600 bytes

test("with a budget of 1 MiB two bitmaps of 600 KiB do not both stay: the one no placement uses is evicted and closed first", async () => {
  const t = rig(1 * MIB);
  const [w, h] = KIB600;
  t.store.want(1, [want(1)]);
  await respond(t.calls[0], reply(w, h, pixels(w, h), 0));
  await respond(t.calls[1], reply(w, h, pixels(w, h), CHUNK_BYTES));
  assert.equal(t.bitmaps.length, 1);
  t.store.want(1, [want(2)]); // picture 1 is no longer placed
  await respond(t.calls[2], reply(w, h, pixels(w, h, 5), 0));
  await respond(t.calls[3], reply(w, h, pixels(w, h, 5), CHUNK_BYTES));
  assert.equal(t.bitmaps.length, 2, "the second picture is fetched and made");
  assert.equal(t.bitmaps[0].closed, 1, "the unused one was evicted and closed");
  assert.equal(t.bitmaps[1].closed, 0);
  assert.deepEqual(t.store.stats(1), { held: 1, bytes: w * h * 4, jobs: 0 });
});

test("when both are placed the second is not fetched once its header says it does not fit, and the first stays drawn", async () => {
  const t = rig(1 * MIB);
  const [w, h] = KIB600;
  t.store.want(1, [want(1), want(2)]);
  assert.equal(t.calls.length, 2, "both headers are asked for");
  await respond(t.calls[0], reply(w, h, pixels(w, h), 0));
  const before = t.calls.length;
  await respond(t.calls[1], reply(w, h, pixels(w, h, 5), 0)); // picture 2's header: 600 + 600 KiB is over 1 MiB
  assert.ok(!t.calls.slice(before).some((c) => c.image === 2), "no piece of the second picture is asked for");
  const rest = t.calls.find((c) => c.image === 1 && c.offset === CHUNK_BYTES);
  await respond(rest, reply(w, h, pixels(w, h), CHUNK_BYTES));
  assert.equal(t.bitmaps.length, 1);
  assert.equal(t.bitmaps[0].closed, 0, "the first stays");
  const drawn = t.store.resolve(1, [place(1), place(2)], GEO);
  assert.deepEqual(drawn.map((d) => d.id), [1], "only the first is drawn");
  t.store.want(1, [want(1), want(2)]);
  assert.ok(!t.calls.slice(before).some((c) => c.image === 2), "and the second is not retried");
});

test("an image larger than the whole budget is not fetched after its header says so", async () => {
  const t = rig(1 * MIB);
  const [w, h] = [512, 512]; // exactly 1 MiB: fits
  const big = [600, 600]; // 1 440 000 bytes: over
  t.store.want(1, [want(1)]);
  await respond(t.calls[0], reply(big[0], big[1], pixels(big[0], big[1]), 0));
  assert.equal(t.calls.length, 1, "no piece after the header");
  assert.equal(t.bitmaps.length, 0);
  t.store.want(1, [want(2)]);
  await respond(t.calls[1], reply(w, h, pixels(w, h), 0));
  await respond(t.calls[2], reply(w, h, pixels(w, h), CHUNK_BYTES));
  assert.equal(t.bitmaps.length, 1, "an image that fills the budget exactly is taken");
});

test("a budget of 0 requests nothing, draws nothing and lets go of what is held", async () => {
  const t = rig();
  t.store.want(1, [want(1)]);
  await respond(t.calls[0], reply(2, 1, pixels(2, 1), 0));
  assert.equal(t.store.resolve(1, [place(1)], GEO).length, 1);
  t.knobs.budget = 0;
  t.store.want(1, [want(1), want(2)]);
  assert.equal(t.calls.length, 1, "nothing is asked for");
  assert.equal(t.bitmaps[0].closed, 1, "what was held is closed");
  assert.deepEqual(t.store.resolve(1, [place(1)], GEO), [], "and nothing is drawn");
  assert.equal(t.store.has(1), false);
  t.knobs.budget = 64 * MIB;
  t.store.want(1, [want(1)]);
  assert.equal(t.calls.length, 2, "with the budget back, the picture is fetched again");
});

test("the bitmap least recently drawn is evicted first", async () => {
  const t = rig(1 * MIB); // room for two of 400 KiB, not three
  const [w, h] = KIB400;
  const fetch = async (image, seed) => {
    const before = t.calls.length;
    t.store.want(1, [want(image)]);
    await respond(t.calls[before], reply(w, h, pixels(w, h, seed), 0));
    return t.bitmaps.length - 1;
  };
  const a = await fetch(1, 1);
  t.store.want(1, [want(1), want(2)]);
  await respond(t.calls[t.calls.length - 1], reply(w, h, pixels(w, h, 2), 0));
  const b = a + 1;
  assert.equal(t.bitmaps.length, 2);
  t.store.resolve(1, [place(2)], GEO); // picture 2 drawn most recently
  t.store.resolve(1, [place(1)], GEO); // then picture 1
  t.store.want(1, [want(3)]); // neither is placed now; room is needed for 3
  await respond(t.calls[t.calls.length - 1], reply(w, h, pixels(w, h, 3), 0));
  assert.equal(t.bitmaps[b].closed, 1, "picture 2 was drawn before picture 1: it goes");
  assert.equal(t.bitmaps[a].closed, 0, "picture 1 stays");
  assert.equal(t.bitmaps.length, 3);
});

test("a budget lowered below what is held trims at the next want, unused first", async () => {
  const t = rig();
  const [w, h] = KIB600;
  for (const image of [1, 2]) {
    t.store.want(1, [want(1), want(2)].slice(0, image));
    const open = t.calls.filter((c) => c.image === image && c.offset === 0).pop();
    await respond(open, reply(w, h, pixels(w, h, image), 0));
    const rest = t.calls.filter((c) => c.image === image && c.offset === CHUNK_BYTES).pop();
    await respond(rest, reply(w, h, pixels(w, h, image), CHUNK_BYTES));
  }
  assert.equal(t.bitmaps.length, 2);
  t.knobs.budget = 1 * MIB;
  t.store.want(1, [want(2)]);
  assert.equal(t.bitmaps[0].closed, 1, "the unused picture goes");
  assert.equal(t.bitmaps[1].closed, 0, "the placed one stays");
  assert.equal(t.store.stats(1).bytes, 614400);
});

// ---- order, forgetting, the cost of nothing -----------------------------------------------------------------

test("pictures are drawn by layer, then z, then image id; one the bitmap cannot supply is left out", async () => {
  const t = rig();
  for (const image of [1, 2, 3, 4]) {
    const before = t.calls.length;
    t.store.want(1, [1, 2, 3, 4].slice(0, image).map((i) => want(i)));
    await respond(t.calls[before], reply(2, 2, pixels(2, 2, image), 0));
  }
  assert.equal(t.bitmaps.length, 4);
  const list = [
    place(3, { layer: 2, z: 0 }), place(1, { layer: 1, z: 5 }), place(2, { layer: 0, z: 9 }), place(4, { layer: 1, z: -2 }),
    place(2, { layer: 2, z: 0, col: 1 }), place(1, { layer: 1, z: 5, col: 2, sw: 3 }), // 3 wide from a 2 wide bitmap
  ];
  const drawn = t.store.resolve(1, list, GEO);
  assert.deepEqual(drawn.map((d) => [d.layer, d.z, d.id]), [[0, 9, 2], [1, -2, 4], [1, 5, 1], [2, 0, 2], [2, 0, 3]]);
  assert.deepEqual([drawn[0].sx, drawn[0].sy, drawn[0].sw, drawn[0].sh], [0, 0, 2, 1]);
});

test("forgetting a pane closes all its bitmaps and cancels its queue", async () => {
  const t = rig();
  t.store.want(1, [want(1), want(2), want(3)]);
  await respond(t.calls[0], reply(2, 1, pixels(2, 1), 0));
  await respond(t.calls[1], reply(2, 1, pixels(2, 1), 0));
  assert.equal(t.bitmaps.length, 2);
  const before = t.calls.length;
  t.store.forget(1);
  assert.equal(t.bitmaps.every((b) => b.closed === 1), true, "every bitmap is closed");
  assert.deepEqual(t.store.stats(1), { held: 0, bytes: 0, jobs: 0 });
  for (const call of t.calls.slice(before - 1)) call.resolve(reply(2, 1, pixels(2, 1), 0)); // answers still on their way
  await flush();
  assert.equal(t.bitmaps.length, 2, "late answers make no bitmap");
  assert.equal(t.calls.length, before, "and nothing is asked for");
  assert.equal(t.store.has(1), false, "and once they are in the pane holds no state at all");
});

test("a bitmap that arrives for a picture dropped while it was being made is closed", async () => {
  let release;
  const slow = new ImageStore({
    fetchChunk: () => Promise.resolve(reply(2, 1, pixels(2, 1), 0)),
    makeBitmap: () => new Promise((done) => (release = () => done(made))),
    ready: () => {},
    budget: () => 64 * MIB,
  });
  const made = { width: 2, height: 1, closed: 0, close() { this.closed++; } };
  slow.want(1, [want(1)]);
  await flush();
  slow.forget(1);
  release();
  await flush();
  assert.equal(made.closed, 1, "a bitmap nobody wants is closed, not kept");
  assert.equal(slow.has(1), false);
});

test("a pane without placements costs nothing: the store is not touched and holds no state for it", () => {
  const t = rig();
  t.store.want(1, []);
  assert.equal(t.store.has(1), false);
  assert.equal(t.calls.length, 0);
  assert.deepEqual(t.store.resolve(1, [], GEO), []);
  t.store.forget(1);
  assert.equal(t.store.has(1), false);
});

test("the budget is read at each want: a change takes effect on the next frame", async () => {
  const t = rig(1 * MIB);
  t.store.want(1, [want(1)]);
  await respond(t.calls[0], reply(600, 600, pixels(600, 600), 0)); // 1 440 000 bytes: over 1 MiB
  assert.equal(t.bitmaps.length, 0);
  t.knobs.budget = 4 * MIB;
  t.store.want(1, [want(1)]);
  assert.equal(t.calls.length, 2, "the picture that did not fit is asked for again once the budget grew");
});
