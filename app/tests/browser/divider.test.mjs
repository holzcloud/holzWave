// DESK-16: dragging a divider follows the pointer for the whole gesture. Before the fix every set_ratio response
// rebuilt the overlay, which destroyed the element that held the pointer capture: 20 moves gave 1 set_ratio.
import assert from "node:assert/strict";
import { callsOf } from "./lib.mjs";

const MOVES = 20;
const STEP_PX = 10;

/** Resolve after the next animation frame, so the page gets its one frame between two pointer moves. */
const frame = (page) => page.evaluate(() => new Promise((done) => requestAnimationFrame(() => done())));

/** Let responses of the stub settle: two frames and a short pause. */
async function settle(page) {
  await frame(page);
  await frame(page);
  await page.waitForTimeout(50);
}

export default async function ({ page, url }) {
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
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
