// DESK-20 / DESK-29: dragging a pane by its header and a tab in the strip sends one move command with ids and an edge,
// shows an outline while it goes, and sends nothing on Escape, on a plain click or on a drop on nothing. The terminal
// never hears of it.
import assert from "node:assert/strict";
import { callsOf, emit } from "./lib.mjs";

const VIEW = {
  workspaces: [{ name: "alpha", active: true }, { name: "beta", active: false }],
  tabs: [
    { title: "one", active: true, focus: 1 },
    { title: "two", active: false, focus: 3 },
    { title: "three", active: false, focus: 4 },
  ],
  focus: 1,
  panes: [[1, { x: 0, y: 0, w: 0.5, h: 1 }], [2, { x: 0.5, y: 0, w: 0.5, h: 1 }]],
  dividers: [{ split: 10, axis: "Row", area: { x: 0, y: 0, w: 1, h: 1 }, ratio: 0.5 }],
};

async function open(page, url) {
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await page.evaluate((v) => window.__MOCK__.setView(v), VIEW);
  await emit(page, "layout", VIEW);
  await page.waitForSelector("#bar .tab:nth-child(3)");
}

/** A pane's grid area on the window: the canvas box and the unit rectangle of the pane. */
async function paneBox(page, unit) {
  const c = await page.locator("canvas").first().boundingBox();
  return { x: c.x + unit.x * c.width, y: c.y + unit.y * c.height, w: unit.w * c.width, h: unit.h * c.height };
}

async function drag(page, from, to, { release = true, steps = 6 } = {}) {
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.mouse.move(to.x, to.y, { steps });
  if (release) await page.mouse.up();
}

const sent = async (page, cmd) => callsOf(page, cmd);

export default async function ({ page, url }) {
  await open(page, url);
  const right = await paneBox(page, VIEW.panes[1][1]);
  const left = await paneBox(page, VIEW.panes[0][1]);
  const headerPoint = async (box) => {
    // The right end of the header strip is the canvas's (the title is a node of the page): both start a drag.
    return { x: box.x + box.w - 12, y: box.y + 8 };
  };

  // A pane header dragged to the bottom edge of the other pane: one move_pane, edge bottom.
  await drag(page, await headerPoint(left), { x: right.x + right.w / 2, y: right.y + right.h - 10 }, { release: false });
  assert.equal(await page.locator(".drop-outline").count(), 1, "an outline shows where the pane would go");
  const o = await page.locator(".drop-outline").boundingBox();
  assert.ok(Math.abs(o.y - (right.y + right.h / 2)) < 2 && Math.abs(o.height - right.h / 2) < 2, `the lower half of the target: ${JSON.stringify({ o, right })}`);
  await page.mouse.up();
  assert.equal(await page.locator(".drop-outline").count(), 0, "the outline is gone after the drop");
  assert.deepEqual(await sent(page, "move_pane"), [{ pane: 1, target: 2, edge: "bottom" }]);

  // The title is a node of the page: pressing on it drags too. The middle of the target swaps.
  await page.evaluate(() => window.__MOCK__.reset());
  await page.evaluate((v) => window.__MOCK__.setView(v), VIEW);
  await drag(page, { x: (await page.locator('.panehead[data-pane="1"] .ph-title').boundingBox()).x + 4, y: left.y + 8 }, { x: right.x + right.w / 2, y: right.y + right.h / 2 });
  assert.deepEqual(await sent(page, "move_pane"), [{ pane: 1, target: 2, edge: "center" }]);

  // Escape cancels, a drop on the pane itself and a plain click change nothing.
  await page.evaluate(() => window.__MOCK__.reset());
  await page.evaluate((v) => window.__MOCK__.setView(v), VIEW);
  await drag(page, await headerPoint(left), { x: right.x + 40, y: right.y + right.h / 2 }, { release: false });
  await page.keyboard.press("Escape");
  await page.mouse.up();
  assert.equal(await page.locator(".drop-outline").count(), 0);
  await drag(page, await headerPoint(left), { x: left.x + 100, y: left.y + 100 });
  await page.mouse.click(right.x + right.w - 12, right.y + 8);
  assert.deepEqual(await sent(page, "move_pane"), [], "Escape, a drop on itself and a click send no move");

  // A pane onto another tab moves into it, next to that tab's focus pane.
  await drag(page, await headerPoint(left), await page.locator("#bar .tab").nth(1).boundingBox().then((b) => ({ x: b.x + b.width / 2, y: b.y + b.height / 2 })));
  assert.deepEqual(await sent(page, "move_pane"), [{ pane: 1, target: 3, edge: "right" }]);

  // Tabs: to another place in the strip, and onto a workspace chip.
  const centre = (b) => ({ x: b.x + b.width / 2, y: b.y + b.height / 2 });
  await page.evaluate(() => window.__MOCK__.reset());
  await page.evaluate((v) => window.__MOCK__.setView(v), VIEW);
  await drag(page, centre(await page.locator("#bar .tab").nth(0).boundingBox()), centre(await page.locator("#bar .tab").nth(2).boundingBox()));
  assert.deepEqual(await sent(page, "move_tab"), [{ from: 0, to: 2 }]);
  await drag(page, centre(await page.locator("#bar .tab").nth(0).boundingBox()), centre(await page.locator("#bar .ws-chip").nth(1).boundingBox()));
  assert.deepEqual(await sent(page, "move_tab_to_workspace"), [{ tab: 0, workspace: 1 }]);
  assert.deepEqual(await sent(page, "select_tab"), [], "the click that ends a drag does not select a tab");

  // None of it reaches the program.
  for (const cmd of ["key", "text", "mouse", "select", "paste"]) {
    assert.deepEqual(await sent(page, cmd), [], `${cmd} is not sent while dragging`);
  }
}
