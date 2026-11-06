// Touch gestures on the phone and tablet layouts: one finger scrolls, a long press selects a word with handles and a menu, two fingers
// pinch the UI scale of this device, and every other platform and pointer stays as it was. No touch screen and no Android WebView is used:
// the platform is mocked (`app_info` answers `android`), the screen size is overridden, and the touches are synthetic `PointerEvent`s with
// `pointerType: "touch"` dispatched into the desktop engines. What only a device shows (the feel of a touch, palm rejection, Android's own
// selection menu fighting the page, the WebView's clipboard permission, a real pinch) is not covered here.
import assert from "node:assert/strict";
import { callsOf, emit } from "./lib.mjs";
import { MODES, box, down, forget, frame, frames, logWheel, mark, move, open, run, selects, shown, sum, swipe, tap, up, visible, wait, wheels } from "./touch-lib.mjs";

// The touch layouts belong to the Android shell, which the web backend never is.
export const backends = ["tauri"];
export const backendNote = "the phone and tablet layouts belong to the Android shell, which the web backend never is";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await phoneCase(page, url);
  await reducedCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

// ---- the phone ---------------------------------------------------------------------------------------

async function phoneCase(page, url) {
  await open(page, url);
  assert.equal(await page.locator('link[href$="touch.css"]').count(), 1, "the touch styles are loaded on a phone");
  assert.equal(await page.locator(".touch-layer").count(), 0, "no handle and no menu until a long press");
  await logWheel(page);
  const g = await shown(page, 2);
  const stage = await box(page, "#stage");

  // 1. A short touch focuses the pane (it has the focus already) and begins nothing; a selection that is there is cleared.
  await tap(page, g.pt(5, 5));
  await frames(page);
  assert.deepEqual(await selects(page), [], "a tap sends no select begin");
  assert.equal((await callsOf(page, "focus_pane")).length, 0, "the pane had the focus: no focus_pane");
  await emit(page, "frame", frame(2, g.cols, g.rows, [[3, 2, 6]]));
  await tap(page, g.pt(5, 5));
  await frames(page);
  assert.deepEqual(await selects(page), [{ pane: 2, kind: "clear" }], "a tap with a selection present clears it");
  await emit(page, "frame", frame(2, g.cols, g.rows));
  await forget(page);

  // 2. One finger down by three cell heights scrolls three rows towards older output, selects nothing, and a fling follows the lift.
  const start = g.pt(5, 4);
  const to = [start[0], start[1] + 3.2 * g.ch];
  const swiped = await run(page, [...swipe(start, to, 8, 12), mark("lift"), up(1, to), mark("lifted")]);
  const lifted = swiped.lifted;
  const drag = (await wheels(page)).slice(0, lifted.wheel);
  assert.equal(sum(drag), -3, `three rows of history in total (${JSON.stringify(drag.map((w) => w.rows))})`);
  assert.ok(drag.every((w) => w.pane === 2 && w.mods === 0), "pane 2, no modifiers");
  assert.ok(drag.length <= 8, "the moves are batched per animation frame");
  assert.deepEqual(await selects(page), [], "a drag never starts a text selection");
  await page.waitForTimeout(750);
  const all = await wheels(page);
  const fling = all.slice(lifted.wheel);
  assert.ok(fling.length >= 1, "a fling follows the lift");
  assert.ok(fling.every((w) => w.rows < 0), "in the same direction");
  assert.ok(Math.max(...fling.map((w) => w.t)) - swiped.lift.t <= 700, "and stops within 700 ms");
  const early = Math.abs(sum(fling.filter((w) => w.t - swiped.lift.t < 250)));
  const rest = Math.abs(sum(fling.filter((w) => w.t - swiped.lift.t >= 250)));
  assert.ok(early >= rest, `the fling decays: ${early} rows in the first 250 ms, ${rest} after`);
  const settled = (await wheels(page)).length;
  await page.waitForTimeout(250);
  assert.equal((await wheels(page)).length, settled, "nothing more after it stopped");
  // Upwards is newer output.
  await forget(page);
  const back = await run(page, [...swipe(to, start, 8, 12), mark("lift"), up(1, start), mark("lifted")]);
  assert.equal(sum((await wheels(page)).slice(0, back.lifted.wheel)), 3, "a finger moving up scrolls towards newer output");
  await page.waitForTimeout(650);

  // 2b. A new touch stops a fling at once.
  await forget(page);
  const stopAt = g.pt(2, 2);
  const quick = await run(page, [
    ...swipe(start, [start[0], start[1] + 6 * g.ch], 6, 10),
    up(1, [start[0], start[1] + 6 * g.ch]),
    wait(40),
    down(1, stopAt),
    mark("stop"),
    wait(60),
    up(1, stopAt),
  ]);
  await page.waitForTimeout(500);
  const late = (await wheels(page)).slice(quick.stop.wheel).filter((w) => w.t > quick.stop.t + 20);
  assert.deepEqual(late, [], "no wheel call after the finger came down again");
  await forget(page);

  // 2c. A program that tracks the mouse still gets `wheel`, and no `mouse` press from a drag; a tap is its click.
  await emit(page, "frame", frame(2, g.cols, g.rows, [], { modes: { ...MODES, mouse: true } }));
  await frames(page);
  const tracked = await run(page, [...swipe(start, to, 8, 12), up(1, to), mark("lifted")]);
  assert.equal(sum((await wheels(page)).slice(0, tracked.lifted.wheel)), -3, "the same wheel input as on the desktop");
  assert.equal((await callsOf(page, "mouse")).length, 0, "a drag is not a press for the program");
  await page.waitForTimeout(650);
  await forget(page);
  await tap(page, g.pt(5, 5));
  await frames(page);
  const clicks = await callsOf(page, "mouse");
  assert.deepEqual(clicks.map((c) => [c.action, c.button]), [["press", "left"], ["release", "left"]], "a tap is the program's click");
  await run(page, [down(1, g.pt(4, 2)), wait(560)]);
  assert.deepEqual(await selects(page), [], "a long press on a program's pane selects nothing");
  await run(page, [up(1, g.pt(4, 2))]);
  await emit(page, "frame", frame(2, g.cols, g.rows));
  await forget(page);

  // 3. A move of 12 px before 500 ms is a scroll, not a selection.
  const p = g.pt(4, 2);
  await run(page, [down(1, p), move(1, [p[0], p[1] + 12], undefined, 20), wait(560), up(1, [p[0], p[1] + 12])]);
  assert.deepEqual(await selects(page), [], "a move over the slop before half a second is no long press");
  await page.waitForTimeout(300);
  await forget(page);

  // 3b. Holding 500 ms without moving selects the word under the finger; handles and the menu follow the core's spans.
  await page.evaluate(() => window.__MOCK__.on("selection_text", () => "abc"));
  await run(page, [down(1, p), wait(470)]);
  assert.deepEqual(await selects(page), [], "not yet at 470 ms");
  await run(page, [wait(80)]);
  assert.deepEqual(await selects(page), [{ pane: 2, kind: "begin", x: 4, y: 2, mode: "word" }], "at 500 ms the word under the finger");
  await emit(page, "frame", frame(2, g.cols, g.rows, [[2, 4, 9]]));
  await page.locator(".touch-handle").first().waitFor({ state: "visible" });
  assert.equal(await visible(page, ".touch-handle"), 2, "two handles");
  assert.equal(await visible(page, ".touch-menu"), 1, "and the menu");
  for (const s of [".touch-handle[data-role=start]", ".touch-handle[data-role=end]", ".touch-button:nth-child(1)", ".touch-button:nth-child(2)"]) {
    const b = await box(page, s);
    assert.ok(b.w >= 43.99 && b.h >= 43.99, `${s} is ${b.w} by ${b.h}`);
  }
  assert.deepEqual(await page.locator(".touch-button").allTextContents(), ["Copy", "Paste"], "the spec's two words");
  const start1 = await box(page, ".touch-handle[data-role=start]");
  const end1 = await box(page, ".touch-handle[data-role=end]");
  assert.ok(Math.abs(start1.x + start1.w / 2 - (g.left + 4 * g.cw)) <= 1, "the start handle is centred on the left edge of the first cell");
  assert.ok(Math.abs(end1.x + end1.w / 2 - (g.left + 10 * g.cw)) <= 1, "the end handle is centred on the right edge of the last cell");
  const menu1 = await box(page, ".touch-menu");
  assert.ok(menu1.bottom <= start1.y + 1 || menu1.y >= start1.y, "the menu is above the selection or below the handles");
  await run(page, [up(1, p)]);
  assert.equal(await visible(page, ".touch-handle"), 2, "the selection stays after the finger lifts");

  // 4. The end handle moves the end: an `extend` to the cell it names. Two cells to the right is two cells further.
  await forget(page);
  const tip = (b) => [b.x + b.w / 2, b.y + 18];
  const e0 = tip(end1);
  await run(page, [down(1, e0, ".touch-handle[data-role=end]"), move(1, [e0[0] + 2 * g.cw, e0[1]], ".touch-handle[data-role=end]", 20), up(1, [e0[0] + 2 * g.cw, e0[1]], ".touch-handle[data-role=end]")]);
  await frames(page);
  assert.deepEqual(await selects(page), [{ pane: 2, kind: "extend", x: 11, y: 2 }], "the end handle only extends");
  assert.equal((await wheels(page)).length, 0, "and a drag that starts on a handle never scrolls");
  await emit(page, "frame", frame(2, g.cols, g.rows, [[2, 4, 11]]));
  await frames(page);

  // The start handle: the core's anchor is the cell of the last `begin`, so the fixed end becomes the anchor, then the finger extends.
  await forget(page);
  const s0 = tip(await box(page, ".touch-handle[data-role=start]"));
  await run(page, [down(1, s0, ".touch-handle[data-role=start]"), move(1, [s0[0] - 2 * g.cw, s0[1]], ".touch-handle[data-role=start]", 20), up(1, [s0[0] - 2 * g.cw, s0[1]], ".touch-handle[data-role=start]")]);
  await frames(page);
  assert.deepEqual(await selects(page), [
    { pane: 2, kind: "begin", x: 11, y: 2, mode: "cell" },
    { pane: 2, kind: "extend", x: 2, y: 2 },
  ], "the start handle begins at the end cell and extends to the finger");
  await emit(page, "frame", frame(2, g.cols, g.rows, [[2, 2, 11]]));
  await frames(page);

  // Crossing: the end handle dragged to the left of the start swaps the roles.
  await forget(page);
  await page.evaluate(() => document.querySelector(".touch-handle[data-role=end]").setAttribute("data-grabbed", "yes"));
  const e1 = tip(await box(page, ".touch-handle[data-role=end]"));
  await run(page, [down(1, e1, "[data-grabbed]"), move(1, [e1[0] - 11 * g.cw, e1[1]], "[data-grabbed]", 20)]);
  await frames(page);
  const crossed = await selects(page);
  assert.deepEqual(crossed.at(-1), { pane: 2, kind: "extend", x: 0, y: 2 }, "the finger is left of the start cell now");
  await emit(page, "frame", frame(2, g.cols, g.rows, [[2, 0, 2]]));
  await frames(page);
  assert.equal(await page.locator("[data-grabbed]").getAttribute("data-role"), "start", "the handle under the finger is the start now");
  await run(page, [up(1, [e1[0] - 11 * g.cw, e1[1]], "[data-grabbed]")]);
  assert.equal(await visible(page, ".touch-handle"), 2);

  // Handles stay inside the stage, whatever the selection.
  await emit(page, "frame", frame(2, g.cols, g.rows, [[0, 0, g.cols - 1], [g.rows - 1, 0, g.cols - 1]]));
  await frames(page);
  for (const s of [".touch-handle[data-role=start]", ".touch-handle[data-role=end]", ".touch-menu"]) {
    const b = await box(page, s);
    assert.ok(b.x >= stage.x - 0.5 && b.y >= stage.y - 0.5 && b.right <= stage.right + 0.5 && b.bottom <= stage.bottom + 0.5, `${s} stays inside the stage: ${JSON.stringify(b)}`);
  }

  // 5. Copy: the desktop's copy path (the cached text) and the selection is cleared, the handles and the menu go.
  await emit(page, "frame", frame(2, g.cols, g.rows, [[2, 4, 9]]));
  await page.evaluate(() => {
    window.__exec = [];
    document.execCommand = (cmd) => {
      const a = document.activeElement;
      window.__exec.push([cmd, a && a.tagName, a && a.value]);
      return true;
    };
  });
  await page.waitForFunction(() => window.__MOCK__.callsOf("selection_text").length >= 1);
  await frames(page);
  await forget(page);
  await page.locator(".touch-button", { hasText: "Copy" }).click();
  assert.deepEqual(await page.evaluate(() => window.__exec), [["copy", "TEXTAREA", "abc"]], "the text of the core's selection went to the clipboard path");
  assert.deepEqual(await selects(page), [{ pane: 2, kind: "clear" }], "and the selection was cleared");
  await emit(page, "frame", frame(2, g.cols, g.rows));
  await frames(page);
  assert.equal(await page.locator(".touch-handle, .touch-menu").count(), 0, "handles and menu are gone");

  // Paste: the desktop's paste, with its confirmation for a multi-line text the program did not ask for.
  await page.evaluate(() => {
    window.__MOCK__.on("clipboard_text", () => "one\ntwo\nthree");
    window.__MOCK__.on("paste", (a) => (a.allow_unsafe ? "written" : "rejected"));
  });
  await run(page, [down(1, p), wait(560), up(1, p)]);
  await emit(page, "frame", frame(2, g.cols, g.rows, [[2, 4, 9]]));
  await page.locator(".touch-button", { hasText: "Paste" }).click();
  await page.locator('[role="dialog"]:visible').waitFor({ state: "visible" });
  assert.deepEqual(await callsOf(page, "paste"), [{ pane: 2, text: "one\ntwo\nthree", allow_unsafe: false }], "the same paste command, not a second path");
  assert.match(await page.locator('[role="dialog"]:visible').innerText(), /3 lines|three|one/, "the confirmation shows what would be pasted");
  await page.keyboard.press("Escape");
  await page.locator('[role="dialog"]:visible').waitFor({ state: "detached" });
  assert.equal((await callsOf(page, "paste")).length, 1, "a refused paste sends nothing more");
  await emit(page, "frame", frame(2, g.cols, g.rows));
  await frames(page);
  assert.equal(await page.locator(".touch-handle, .touch-menu").count(), 0);

  // A tap elsewhere clears the selection and the handles.
  await run(page, [down(1, p), wait(560), up(1, p)]);
  await emit(page, "frame", frame(2, g.cols, g.rows, [[2, 4, 9]]));
  await page.locator(".touch-handle").first().waitFor({ state: "visible" });
  await forget(page);
  await tap(page, g.pt(1, 8));
  await frames(page);
  assert.deepEqual(await selects(page), [{ pane: 2, kind: "clear" }], "a tap elsewhere clears");
  assert.equal(await page.locator(".touch-handle, .touch-menu").count(), 0, "and the handles go");
  await emit(page, "frame", frame(2, g.cols, g.rows));

  // 9. Hostile text: the selection's text never becomes an element, and the menu says only its two words.
  await page.evaluate(() => {
    window.__MOCK__.on("selection_text", () => '<img src=x onerror="window.__pwned=1"><b>bold</b>');
  });
  await run(page, [down(1, p), wait(560), up(1, p)]);
  await emit(page, "frame", frame(2, g.cols, g.rows, [[2, 4, 9]]));
  await page.locator(".touch-menu").waitFor({ state: "visible" });
  await page.waitForFunction(() => window.__MOCK__.callsOf("selection_text").length >= 1);
  await frames(page);
  assert.equal(await page.locator(".touch-menu img, .touch-menu b, .touch-layer img, #overlay img").count(), 0, "no element from the text");
  assert.equal(await page.evaluate(() => window.__pwned), undefined, "no handler ran");
  assert.equal(await page.locator(".touch-menu").innerText().then((t) => t.replace(/\s+/g, " ").trim()), "Copy Paste");
  await tap(page, g.pt(1, 8));
  await emit(page, "frame", frame(2, g.cols, g.rows));

  // 7. A mouse pointer on the same page selects with a drag, as before.
  await forget(page);
  const a = g.pt(1, 1);
  const b = g.pt(7, 1);
  await page.mouse.move(a[0], a[1]);
  await page.mouse.down();
  await page.mouse.move(b[0], b[1], { steps: 4 });
  await page.mouse.up();
  await frames(page);
  const ops = await selects(page);
  assert.deepEqual(ops[0], { pane: 2, kind: "begin", x: 1, y: 1, mode: "cell" }, "a mouse press begins a selection");
  assert.ok(ops.some((o) => o.kind === "extend" && o.x === 7), "and the drag extends it");
  assert.equal((await wheels(page)).length, 0);
}

// ---- reduced motion ----------------------------------------------------------------------------------------

async function reducedCase(page, url) {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await open(page, url);
  await logWheel(page);
  const g = await shown(page, 2);
  const start = g.pt(5, 4);
  const to = [start[0], start[1] + 3.2 * g.ch];
  const swiped = await run(page, [...swipe(start, to, 8, 12), up(1, to), mark("lifted")]);
  await page.waitForTimeout(700);
  const all = await wheels(page);
  assert.equal(sum(all), -3, "the drag scrolls as usual");
  assert.equal(all.length, swiped.lifted.wheel, "and there is no inertia with reduced motion");
  await page.emulateMedia({ reducedMotion: "no-preference" });
}

