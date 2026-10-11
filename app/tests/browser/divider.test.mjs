// DESK-16: dragging a divider follows the pointer for the whole gesture. Before the fix every set_ratio response
// rebuilt the overlay, which destroyed the element that held the pointer capture: 20 moves gave 1 set_ratio.
// The overlay is a keyed set of elements (dividers, pane headers, selection boxes): a render that changes nothing
// replaces nothing.
import assert from "node:assert/strict";
import { callsOf, emit } from "./lib.mjs";

const MOVES = 20;
const STEP_PX = 10;
const PAD = 6; // CSS px between a pane's edge and its grid (geometry.ts)
const ROW_PX = 18; // one cell row: ceil(14 px font * 1.25)
const HEADER = 20; // CSS px of a pane's header strip at the base font size (state.headerPx); the grid starts below it

/** Resolve after the next animation frame, so the page gets its one frame between two pointer moves. */
const frame = (page) => page.evaluate(() => new Promise((done) => requestAnimationFrame(() => done())));

/** Let responses of the stub settle: two frames and a short pause. */
async function settle(page) {
  await frame(page);
  await frame(page);
  await page.waitForTimeout(50);
}

const open = async (page, url) => {
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
};

export default async function ({ page, url }) {
  await dragCase(page, url);
  await rerenderCase(page, url);
  await selectionCase(page, url);
}

async function dragCase(page, url) {
  await open(page, url);
  await page.evaluate(() => {
    document.querySelector(".divider").dataset.tag = "original";
  });

  const stage = await page.evaluate(() => {
    const r = document.getElementById("term").getBoundingClientRect();
    return { left: r.left, width: r.width };
  });
  const box = await page.locator(".divider").boundingBox();
  assert.ok(box, "the divider has no box");
  const x0 = box.x + box.width / 2;
  const y = box.y + box.height / 2;
  const expected = (x0 + MOVES * STEP_PX - stage.left) / stage.width;

  await page.mouse.move(x0, y);
  await page.mouse.down();
  for (let i = 1; i <= MOVES; i++) {
    await page.mouse.move(x0 + i * STEP_PX, y);
    await frame(page);
  }
  await page.mouse.up();
  await settle(page);

  assert.equal(
    await page.locator('.divider[data-tag="original"]').count(),
    1,
    "the divider element was recreated during the drag",
  );
  assert.equal(await page.locator(".divider").count(), 1, "exactly one divider element");

  const sent = await callsOf(page, "set_ratio");
  assert.ok(sent.length >= 2, `only ${sent.length} set_ratio call(s) for ${MOVES} moves: the drag stopped early`);
  assert.ok(sent.length <= MOVES, `${sent.length} set_ratio calls for ${MOVES} moves: not one per animation frame`);

  const last = sent[sent.length - 1].ratio;
  assert.ok(
    Math.abs(last - expected) <= 0.03,
    `the last ratio is ${last}, the pointer ended at ${expected.toFixed(3)}`,
  );

  const after = await page.locator(".divider").boundingBox();
  assert.ok(after, "the divider vanished");
  assert.ok(
    Math.abs(after.x + after.width / 2 - (stage.left + expected * stage.width)) <= 3,
    `the divider sits at ${after.x + after.width / 2}, the pointer ended at ${stage.left + expected * stage.width}`,
  );

  const total = sent.length;
  for (let i = 1; i <= 5; i++) {
    await page.mouse.move(x0 + (MOVES + i) * STEP_PX, y);
    await frame(page);
  }
  await settle(page);
  assert.equal((await callsOf(page, "set_ratio")).length, total, "set_ratio was sent after the pointer was released");
}

/** Tag every overlay element that matches `selector` with its class and position in the DOM order. */
const tagAll = (page, selector, prefix) =>
  page.evaluate(
    ([sel, pre]) => {
      document.querySelectorAll(`#overlay ${sel}`).forEach((n, i) => {
        n.dataset.tag = `${pre}${i}`;
      });
    },
    [selector, prefix],
  );

/** The tags of the matching overlay elements in DOM order; an element without a tag shows as "". */
const tagsOf = (page, selector) =>
  page.evaluate(
    (sel) => [...document.querySelectorAll(`#overlay ${sel}`)].map((n) => n.dataset.tag ?? ""),
    selector,
  );

async function rerenderCase(page, url) {
  await open(page, url);
  assert.equal(await page.locator("#overlay .panehead.focus-line").count(), 1, "two panes: one focus line");
  assert.equal(await page.locator("#overlay .focus").count(), 0, "the focus box of the page is gone");
  await tagAll(page, ".divider", "divider");
  await tagAll(page, ".panehead", "head");

  for (let i = 0; i < 3; i++) {
    await emit(page, "layout", await page.evaluate(() => window.__MOCK__.view()));
  }
  assert.deepEqual(await tagsOf(page, ".divider"), ["divider0"], "a render of the same view replaced the divider");
  assert.deepEqual(await tagsOf(page, ".panehead"), ["head0", "head1"], "a render of the same view replaced a header");

  // One pane: no focus line and no divider.
  const single = {
    workspaces: [{ name: "main", active: true }],
    tabs: [{ title: "shell", active: true }],
    focus: 1,
    panes: [[1, { x: 0, y: 0, w: 1, h: 1 }]],
    dividers: [],
  };
  await page.evaluate((v) => window.__MOCK__.setView(v), single);
  await emit(page, "layout", single);
  assert.equal(await page.locator("#overlay .panehead").count(), 1, "a single pane has its header");
  assert.equal(await page.locator("#overlay .panehead.focus-line").count(), 0, "a single pane has no focus line");
  assert.equal(await page.locator("#overlay .divider").count(), 0, "a vanished split removes its divider");

  // Two panes again: the elements come back.
  await page.evaluate(() => window.__MOCK__.reset());
  await emit(page, "layout", await page.evaluate(() => window.__MOCK__.view()));
  assert.equal(await page.locator("#overlay .panehead.focus-line").count(), 1, "the focus line returns with the second pane");
  assert.equal(await page.locator("#overlay .divider").count(), 1, "the divider returns with its split");
}

async function selectionCase(page, url) {
  await open(page, url);
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await tagAll(page, ".panehead", "head");
  await tagAll(page, ".divider", "divider");
  const canvas = await page.evaluate(() => {
    const r = document.getElementById("term").getBoundingClientRect();
    return { left: r.left, top: r.top, width: r.width };
  });
  // The selection is the core's: the boxes follow the span lists of the focused pane's frames.
  const msg = (seq, selection, full = false) => ({
    pane: 2, seq, full, cols: 20, rows: 6, fg: 0xffffff, bg: 0x14091a, cursor: null, scrollbar: [0, 6, 6],
    selection, matches: [], search: null, copy_cursor: null,
    modes: { alt: false, bracketed_paste: false, mouse: false }, changed: [],
  });
  const row = (y) => [y, 0, 9];

  await emit(page, "frame", msg(1, [row(0), row(1), row(2)], true));
  assert.equal(await page.locator("#overlay .sel").count(), 3, "rows 0 to 2 are selected: three boxes");
  await tagAll(page, ".sel", "row");

  await emit(page, "frame", msg(2, [row(0), row(1), row(2), row(3)]));
  assert.deepEqual(
    await tagsOf(page, ".sel"),
    ["row0", "row1", "row2", ""],
    "growing the selection recreated the boxes of the rows that were already there",
  );

  await emit(page, "frame", msg(3, [row(0), row(1)]));
  assert.deepEqual(
    await tagsOf(page, ".sel"),
    ["row0", "row1"],
    "shrinking the selection recreated the boxes of the rows that stay",
  );

  // A gesture on the pane changes no element: the answer to it is a frame, not a local selection.
  const x = canvas.left + canvas.width / 2 + PAD + 20;
  const rowY = (r) => canvas.top + HEADER + r * ROW_PX + ROW_PX / 2;
  await page.mouse.move(x, rowY(0));
  await page.mouse.down();
  await page.mouse.move(x + 120, rowY(2));
  await frame(page);
  await page.mouse.up();
  await emit(page, "layout", await page.evaluate(() => window.__MOCK__.view()));
  await settle(page);
  assert.deepEqual(await tagsOf(page, ".sel"), ["row0", "row1"], "a render of the same view replaced a selection box");
  assert.deepEqual(await tagsOf(page, ".panehead"), ["head0", "head1"], "the gesture replaced a header");
  assert.deepEqual(await tagsOf(page, ".divider"), ["divider0"], "the selection gesture replaced the divider");

  // The spans go: so do the boxes.
  await emit(page, "frame", msg(4, []));
  assert.equal(await page.locator("#overlay .sel").count(), 0, "the boxes stay after the spans are gone");
}
