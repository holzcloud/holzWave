// Selection through the core (DESK-18): gestures send begin, extend and clear in viewport cells, the highlight is
// drawn from the span list of the focused pane's frames, the text for `copy` comes from a cache that follows the
// spans, and pointer handlers can be stacked in front of the gesture.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { callsOf, emit } from "./lib.mjs";

const PAD = 6; // CSS px between a pane's edge and its grid (geometry.ts)
const BG = 0x14091a;
const MODES = { alt: false, bracketed_paste: false, mouse: false };
const golden = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));

/** A frame message for `pane`: 20 x 4 cells, `selection` spans and the rows in `changed`. */
const frame = (pane, seq, selection = [], over = {}) => ({
  pane, seq, full: false, cols: 20, rows: 4, fg: 0xf6efe6, bg: BG, cursor: null, scrollbar: [0, 4, 4],
  selection, matches: [], search: null, copy_cursor: null, modes: MODES, changed: [], ...over,
});
const fullFrame = (pane, seq, selection = []) => frame(pane, seq, selection, { full: true });

/** Open the page against the stub with a 20 x 4 mirror for both panes; returns the geometry helpers. */
async function open(page, url) {
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await page.evaluate(() => {
    document.getElementById("term").addEventListener("pointerdown", (e) => { window.__pid = e.pointerId; }, true);
    document.getElementById("term").focus();
  });
  const start = (await callsOf(page, "start_pane"))[0];
  const c = await page.evaluate(() => {
    const r = document.getElementById("term").getBoundingClientRect();
    return { left: r.left, top: r.top, width: r.width };
  });
  const cw = start.cell_w; // dpr 1
  const ch = start.cell_h;
  // Pane 1 is the left half, pane 2 (focused) the right half of the stub's default layout.
  const origin = (pane) => c.left + (pane === 1 ? 0 : c.width / 2) + PAD;
  const geo = {
    cw, ch,
    /** The page point at the centre of cell (cx, cy) of `pane`. */
    pt: (pane, cx, cy) => [origin(pane) + (cx + 0.5) * cw, c.top + PAD + (cy + 0.5) * ch],
    /** The box of cell span (x0..x1, y) of `pane`. */
    box: (pane, y, x0, x1) => ({
      x: origin(pane) + x0 * cw, y: c.top + PAD + y * ch, w: (x1 - x0 + 1) * cw, h: ch,
    }),
  };
  await emit(page, "frame", fullFrame(1, 1));
  await emit(page, "frame", fullFrame(2, 1));
  return geo;
}

/** Everything the page asked of the core about selection and focus, in order. */
const selectionCalls = (page) =>
  page.evaluate(() =>
    window.__MOCK__.calls
      .filter(([cmd]) => cmd === "select" || cmd === "focus_pane" || cmd === "selection_text")
      .map(([cmd, args]) => [cmd, args]),
  );

/** The `select` ops sent so far (pane and op). */
const selects = async (page) => (await callsOf(page, "select")).map((a) => ({ pane: a.pane, ...a.op }));

/** Two animation frames: whatever was coalesced into one has been sent. */
const frames = (page, n = 2) =>
  page.evaluate(async (count) => {
    for (let i = 0; i < count; i++) await new Promise((done) => requestAnimationFrame(() => done()));
  }, n);

const reset = (page) => page.evaluate(() => window.__MOCK__.reset());

/** Forget the calls and let the multi-click window of the click before run out. */
async function fresh(page) {
  await reset(page);
  await page.waitForTimeout(450);
}

export default async function ({ page, url, errors }) {
  await gestureCase(page, url);
  await clickCountCase(page, url);
  await dragCase(page, url);
  await unfocusedCase(page, url);
  await highlightCase(page, url);
  await cacheCase(page, url);
  await copyEventCase(page, url);
  await copyActionCase(page, url);
  await handlerStackCase(page, url);
  await wheelCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** A plain click begins and clears; the payload keys and strings are the golden's. */
async function gestureCase(page, url) {
  const g = await open(page, url);
  const [x, y] = g.pt(2, 3, 2);
  await page.mouse.click(x, y);
  await frames(page);

  const ops = await selects(page);
  assert.deepEqual(ops, [
    { pane: 2, kind: "begin", x: 3, y: 2, mode: "cell" },
    { pane: 2, kind: "clear" },
  ], "a click in the focused pane begins a cell selection and clears it again");
  assert.deepEqual((await selectionCalls(page)).map(([cmd]) => cmd), ["select", "select"], "no focus_pane for the focused pane");

  // The shapes are the ones the Rust side pinned.
  const begin = golden.ops.select.find((o) => o.kind === "begin");
  const clear = golden.ops.select.find((o) => o.kind === "clear");
  const sent = (await callsOf(page, "select")).map((a) => a.op);
  assert.deepEqual(Object.keys(sent[0]).sort(), Object.keys(begin).sort(), "begin has the golden's keys");
  assert.deepEqual(Object.keys(sent[1]).sort(), Object.keys(clear).sort(), "clear has the golden's keys");
  assert.ok(golden.ops.select.some((o) => o.kind === "begin" && o.mode === sent[0].mode), "the mode is a golden string");
}

/** Double click selects a word, triple click a line; a slow or distant second click starts over. */
async function clickCountCase(page, url) {
  const g = await open(page, url);
  const [x, y] = g.pt(2, 5, 1);
  const kinds = async () => (await selects(page)).map((o) => (o.kind === "begin" ? `begin:${o.mode}` : o.kind));

  await page.mouse.click(x, y);
  await page.mouse.click(x, y);
  assert.deepEqual(await kinds(), ["begin:cell", "clear", "begin:word"], "the second click selects a word and keeps it");

  await page.mouse.click(x, y);
  assert.deepEqual(await kinds(), ["begin:cell", "clear", "begin:word", "begin:line"], "the third click selects a line");

  await fresh(page);
  await page.mouse.click(x, y);
  await page.waitForTimeout(450); // longer than the 400 ms window
  await page.mouse.click(x, y);
  assert.deepEqual(await kinds(), ["begin:cell", "clear", "begin:cell", "clear"], "a slow second click is a new single click");

  await fresh(page);
  await page.mouse.click(x, y);
  await page.mouse.click(x + 12, y); // farther than 4 px
  assert.deepEqual(await kinds(), ["begin:cell", "clear", "begin:cell", "clear"], "a distant second click is a new single click");

  await fresh(page);
  await page.mouse.click(x, y);
  await page.mouse.click(x + 3, y); // within 4 px, still inside one cell
  assert.deepEqual((await kinds()).filter((k) => k.startsWith("begin")), ["begin:cell", "begin:word"]);
}

/** A drag sends extends coalesced per frame, clamped to the grid; the release sends nothing further. */
async function dragCase(page, url) {
  const g = await open(page, url);
  const [x0, y0] = g.pt(2, 1, 1);
  await page.mouse.move(x0, y0);
  await page.mouse.down();
  const [x1, y1] = g.pt(2, 8, 3);
  await page.mouse.move(x1, y1, { steps: 12 });
  await frames(page);

  let ops = await selects(page);
  assert.deepEqual(ops[0], { pane: 2, kind: "begin", x: 1, y: 1, mode: "cell" });
  const extends_ = ops.filter((o) => o.kind === "extend");
  assert.ok(extends_.length >= 1 && extends_.length <= 12, `${extends_.length} extends for 12 moves`);
  assert.deepEqual(extends_.at(-1), { pane: 2, kind: "extend", x: 8, y: 3 }, "the last extend is the pointer's cell");
  for (let i = 1; i < extends_.length; i++) {
    const [a, b] = [extends_[i - 1], extends_[i]];
    assert.ok(a.x !== b.x || a.y !== b.y, "an extend repeated the cell before it");
  }

  // Thirty moves inside one task make one extend: the last cell.
  const before = (await selects(page)).length;
  const pts = Array.from({ length: 30 }, (_, i) => g.pt(2, 8 + (i % 5), 3 - (i % 3)));
  pts.push(g.pt(2, 12, 2));
  await page.evaluate(
    (points) => {
      const c = document.getElementById("term");
      for (const [x, y] of points) {
        c.dispatchEvent(new PointerEvent("pointermove", { clientX: x, clientY: y, pointerId: window.__pid, bubbles: true, buttons: 1 }));
      }
    },
    pts,
  );
  await frames(page);
  ops = (await selects(page)).slice(before);
  assert.deepEqual(ops, [{ pane: 2, kind: "extend", x: 12, y: 2 }], "31 moves in one task send one extend");

  // Far past the pane's grid: clamped to its last column and row (the frame is 20 x 4).
  await page.mouse.move(g.pt(2, 0, 0)[0] + 300, g.pt(2, 0, 0)[1] + 300);
  await frames(page);
  assert.deepEqual((await selects(page)).at(-1), { pane: 2, kind: "extend", x: 19, y: 3 }, "clamped to cols-1, rows-1");

  // Above and left of the grid: clamped to 0, 0.
  await page.mouse.move(g.pt(2, 0, 0)[0] - 40, g.pt(2, 0, 0)[1] - 30);
  await frames(page);
  assert.deepEqual((await selects(page)).at(-1), { pane: 2, kind: "extend", x: 0, y: 0 }, "clamped to 0, 0");

  const total = (await selects(page)).length;
  await page.mouse.up();
  await frames(page);
  await page.waitForTimeout(60);
  assert.equal((await selects(page)).length, total, "the release sends nothing further after a drag");
  await page.mouse.move(x0, y0);
  await frames(page);
  assert.equal((await selects(page)).length, total, "moving without the button sends nothing");
}

/** A click in the other pane focuses it, begins and clears there. */
async function unfocusedCase(page, url) {
  const g = await open(page, url);
  const [x, y] = g.pt(1, 2, 0);
  await page.mouse.click(x, y);
  await frames(page);
  const calls = await selectionCalls(page);
  assert.deepEqual(calls.map(([cmd]) => cmd).sort(), ["focus_pane", "select", "select"]);
  assert.deepEqual(calls.find(([cmd]) => cmd === "focus_pane")[1], { pane: 1 });
  assert.deepEqual(await selects(page), [
    { pane: 1, kind: "begin", x: 2, y: 0, mode: "cell" },
    { pane: 1, kind: "clear" },
  ]);
}

/** The highlight is one box per span of the focused pane, keyed and reused, and gone with the spans. */
async function highlightCase(page, url) {
  const g = await open(page, url);
  const boxes = () =>
    page.evaluate(() =>
      [...document.querySelectorAll("#overlay .sel")].map((n) => {
        const r = n.getBoundingClientRect();
        return { x: r.left, y: r.top, w: r.width, h: r.height, tag: n.dataset.tag ?? "" };
      }),
    );
  const near = (got, want, what) => {
    for (const k of ["x", "y", "w", "h"]) assert.ok(Math.abs(got[k] - want[k]) < 0.6, `${what}: ${k} ${got[k]} != ${want[k]}`);
  };

  assert.equal((await boxes()).length, 0, "no selection, no box");

  await emit(page, "frame", frame(2, 2, [[1, 0, 3]]));
  let b = await boxes();
  assert.equal(b.length, 1, "one span, one box");
  near(b[0], g.box(2, 1, 0, 3), "the box of [1, 0, 3]");

  await page.evaluate(() => document.querySelectorAll("#overlay .sel").forEach((n, i) => { n.dataset.tag = `row${i}`; }));
  await emit(page, "frame", frame(2, 3, [[1, 0, 3], [2, 0, 9]]));
  b = await boxes();
  assert.equal(b.length, 2, "two spans, two boxes");
  assert.equal(b.filter((x) => x.tag === "row0").length, 1, "the box of the first span was reused");
  near(b.find((x) => x.tag === "row0"), g.box(2, 1, 0, 3), "reused box");
  near(b.find((x) => x.tag === ""), g.box(2, 2, 0, 9), "the new box");

  // A frame that repeats the spans keeps the elements.
  await emit(page, "frame", frame(2, 4, [[1, 0, 3], [2, 0, 9]]));
  assert.equal((await boxes()).filter((x) => x.tag === "row0").length, 1, "an unchanged selection replaced a box");

  // Spans that are not on the grid draw nothing and throw nothing.
  await emit(page, "frame", frame(2, 5, [[99, 0, 3], [0, 5, 2], ["a", 0, 1], [1, 0, 9999]]));
  b = await boxes();
  assert.equal(b.length, 1, "only the clamped span is drawn");
  near(b[0], g.box(2, 1, 0, 19), "a span past the last column is clamped");

  // A selection in the unfocused pane is not drawn.
  await emit(page, "frame", frame(1, 2, [[0, 0, 4]]));
  assert.equal((await boxes()).length, 1, "pane 1 is not focused: its spans draw no box");

  await emit(page, "frame", frame(2, 6, []));
  assert.equal((await boxes()).length, 0, "the boxes are removed with the spans");
}

/** The text is asked for once per change of the spans and cached; an empty list clears the cache. */
async function cacheCase(page, url) {
  await open(page, url);
  await page.evaluate(() => window.__MOCK__.on("selection_text", () => "abc"));
  const asked = async () => (await callsOf(page, "selection_text")).length;
  const copyText = () =>
    page.evaluate(() => {
      const dt = new DataTransfer();
      const e = new ClipboardEvent("copy", { clipboardData: dt, cancelable: true, bubbles: true });
      document.getElementById("term").dispatchEvent(e);
      return { text: dt.getData("text/plain"), prevented: e.defaultPrevented };
    });
  const settle = async (want) => {
    const deadline = Date.now() + 2000;
    let got = await copyText();
    while (got.text !== want && Date.now() < deadline) {
      await frames(page, 1);
      got = await copyText();
    }
    return got;
  };

  assert.equal(await asked(), 0, "nothing selected, nothing asked");
  await emit(page, "frame", frame(2, 2, [[1, 0, 3]]));
  assert.deepEqual(await settle("abc"), { text: "abc", prevented: true }, "the answer is cached");
  assert.equal(await asked(), 1, "asked once for one selection");

  await emit(page, "frame", frame(2, 3, [[1, 0, 3]]));
  await emit(page, "frame", frame(2, 4, [[1, 0, 3]], { changed: [{ y: 3, runs: [["z", 1, 1, 0, 0]] }] }));
  await frames(page);
  assert.equal(await asked(), 1, "the same spans and rows outside them ask nothing");

  await emit(page, "frame", frame(2, 5, [[1, 0, 4]]));
  await frames(page);
  assert.equal(await asked(), 2, "other spans ask again");

  await emit(page, "frame", frame(2, 6, [[1, 0, 4]], { changed: [{ y: 1, runs: [["new", 3, 1, 0, 0]] }] }));
  await frames(page);
  assert.equal(await asked(), 3, "a changed row inside the selection asks again: the text may differ");

  await emit(page, "frame", frame(2, 7, []));
  assert.deepEqual(await copyText(), { text: "", prevented: false }, "an empty span list clears the cache");
  await emit(page, "frame", frame(2, 8, []));
  assert.equal(await asked(), 3, "nothing selected, nothing asked");

  // An answer that arrives after the selection is gone is dropped.
  await page.evaluate(() => window.__MOCK__.on("selection_text", () => new Promise((done) => setTimeout(() => done("late"), 150))));
  await emit(page, "frame", frame(2, 9, [[0, 0, 1]]));
  await emit(page, "frame", frame(2, 10, []));
  await page.waitForTimeout(300);
  assert.deepEqual(await copyText(), { text: "", prevented: false }, "a late answer must not refill the cache");
}

/** The `copy` event takes its text synchronously from the cache. */
async function copyEventCase(page, url) {
  await open(page, url);
  await page.evaluate(() => window.__MOCK__.on("selection_text", () => "line one\nline two"));
  await emit(page, "frame", frame(2, 2, [[1, 0, 3]]));
  await page.waitForFunction(() => window.__MOCK__.callsOf("selection_text").length >= 1);
  await frames(page);

  const result = await page.evaluate(() => {
    const dt = new DataTransfer();
    const e = new ClipboardEvent("copy", { clipboardData: dt, cancelable: true });
    document.dispatchEvent(e); // not bubbling: the listener must be in the capture phase of the document
    return { text: dt.getData("text/plain"), prevented: e.defaultPrevented };
  });
  assert.deepEqual(result, { text: "line one\nline two", prevented: true });

  // A text field of the page keeps its own copy.
  const field = await page.evaluate(() => {
    const input = document.createElement("input");
    document.body.append(input);
    const dt = new DataTransfer();
    const e = new ClipboardEvent("copy", { clipboardData: dt, cancelable: true, bubbles: true });
    input.dispatchEvent(e);
    input.remove();
    return { text: dt.getData("text/plain"), prevented: e.defaultPrevented };
  });
  assert.deepEqual(field, { text: "", prevented: false }, "a copy inside a text field is not taken over");
}

/** Cmd+C copies the cached text through a hidden textarea and execCommand; with no cache it asks the core. */
async function copyActionCase(page, url) {
  await open(page, url);
  await page.evaluate(() => {
    window.__exec = [];
    window.__written = [];
    document.execCommand = (cmd) => {
      const a = document.activeElement;
      window.__exec.push([cmd, a && a.tagName, a && a.value]);
      return true;
    };
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText: async (t) => { window.__written.push(t); } },
    });
    window.__MOCK__.on("selection_text", () => "abc");
  });
  await emit(page, "frame", frame(2, 2, [[1, 0, 3]]));
  await page.waitForFunction(() => window.__MOCK__.callsOf("selection_text").length >= 1);
  await frames(page);

  const before = (await page.evaluate(() => window.__MOCK__.calls.length));
  await page.keyboard.press("Meta+c");
  assert.deepEqual(await page.evaluate(() => window.__exec), [["copy", "TEXTAREA", "abc"]], "execCommand copied the cache");
  assert.equal(await page.evaluate(() => document.querySelectorAll("textarea").length), 0, "the textarea was removed");
  assert.equal(await page.evaluate(() => document.activeElement && document.activeElement.id), "term", "the canvas has the keyboard back");
  const shell = (await page.evaluate((n) => window.__MOCK__.calls.slice(n).map((c) => c[0]), before))
    .filter((c) => ["key", "text", "paste", "input"].includes(c));
  assert.deepEqual(shell, [], "Cmd+C must not reach the shell");

  // No cache: the core is asked for the text (a selection scrolled out of view has no spans) and nothing is
  // copied when it has none.
  await emit(page, "frame", frame(2, 3, []));
  await page.evaluate(() => {
    window.__exec.length = 0;
    window.__MOCK__.on("selection_text", () => null);
  });
  const asked = (await callsOf(page, "selection_text")).length;
  await page.keyboard.press("Meta+c");
  await page.waitForFunction((n) => window.__MOCK__.callsOf("selection_text").length > n, asked);
  await frames(page);
  assert.deepEqual(await page.evaluate(() => [window.__exec, window.__written]), [[], []], "no selection: nothing is copied");

  await page.evaluate(() => window.__MOCK__.on("selection_text", () => "from the core"));
  await page.keyboard.press("Meta+c");
  await page.waitForFunction(() => window.__written.length >= 1);
  assert.deepEqual(await page.evaluate(() => window.__written), ["from the core"]);
}

/** Handlers pushed on the pointer stack see the events first and can consume them. */
async function handlerStackCase(page, url) {
  const g = await open(page, url);
  const [x, y] = g.pt(2, 4, 1);
  await page.evaluate(async () => {
    const { pushPointerHandler } = await import(new URL("pointer.js", location.href).href);
    window.__seen = [];
    window.__consume = { top: true, low: false };
    window.__pops = {
      low: pushPointerHandler((e, hit) => { window.__seen.push(["low", e.type, hit && hit.pane]); return window.__consume.low; }),
      top: pushPointerHandler((e, hit) => { window.__seen.push(["top", e.type, hit && hit.pane]); return window.__consume.top; }),
    };
  });
  const seen = async (type) => (await page.evaluate(() => window.__seen)).filter((s) => s[1] === type);

  await page.mouse.click(x, y);
  assert.deepEqual(await seen("pointerdown"), [["top", "pointerdown", 2]], "only the top handler saw a consumed pointerdown");
  assert.deepEqual(await seen("pointerup"), [["top", "pointerup", 2]]);
  assert.deepEqual(await selects(page), [], "a consumed click starts no selection");

  // Not consumed: the next handler sees it and the gesture runs.
  await page.evaluate(() => { window.__consume.top = false; window.__seen.length = 0; });
  await page.mouse.click(x, y);
  assert.deepEqual((await seen("pointerdown")).map((s) => s[0]), ["top", "low"], "newest first");
  assert.deepEqual((await selects(page)).map((o) => o.kind), ["begin", "clear"]);

  // Popping removes exactly that push; popping twice does nothing.
  await page.evaluate(() => { window.__consume.top = true; window.__pops.top(); window.__pops.top(); window.__seen.length = 0; });
  await fresh(page);
  await page.mouse.click(x, y);
  assert.deepEqual((await seen("pointerdown")).map((s) => s[0]), ["low"], "the popped handler is gone, the other stays");
  assert.deepEqual((await selects(page)).map((o) => o.kind), ["begin", "clear"]);

  await page.evaluate(() => { window.__pops.low(); window.__seen.length = 0; });
  await fresh(page);
  await page.mouse.click(x, y);
  assert.deepEqual(await page.evaluate(() => window.__seen), [], "no handlers left");
  assert.deepEqual((await selects(page)).map((o) => o.kind), ["begin", "clear"]);
}

/** The wheel still scrolls the pane under the pointer. */
async function wheelCase(page, url) {
  const g = await open(page, url);
  await page.mouse.move(...g.pt(1, 3, 1));
  await page.mouse.wheel(0, 120);
  await page.waitForFunction(() => window.__MOCK__.callsOf("scroll").length >= 1, undefined, { timeout: 5000 });
  const down = (await callsOf(page, "scroll")).at(-1);
  assert.equal(down.pane, 1);
  assert.ok(down.rows > 0);
}
