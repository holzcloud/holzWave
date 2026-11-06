// The tablet layout: the desktop window with the changes a finger needs (44 px controls, a 24 px divider area, no tear-off, the key row at the
// bottom, the height that follows the keyboard) and the phone and the desktop that stay as they were. An Android WebView, a real tablet, split
// screen, a hardware keyboard and a finger on a 24 px divider are NOT used: the platform is mocked (`app_info` answers `android`), the screen is
// 1280 by 800 and the touches are synthetic pointer events, in desktop Chromium and WebKit.
import assert from "node:assert/strict";
import { callsOf } from "./lib.mjs";
import { down, frames, move, open, run, up } from "./touch-lib.mjs";

// The tablet layout is a property of the Android shell; the web backend never reports `android`.
export const backends = ["tauri"];
export const backendNote = "the tablet layout belongs to the Android shell, which the web backend never is";

export default async function ({ engine, page, url }) {
  await tabletCase(page, url);
  await targetsCase(page, url);
  await dividerCase(page, url);
  await tearOffCase(page, url);
  await keyboardCase(page, url);
  await widthCase(page, url);
  await forcedCase(page, url, engine);
}

/** One workspace, three tabs, one tab of two panes side by side with one divider. */
const VIEW = {
  rev: 1,
  workspaces: [
    { name: "main", active: true, panes: 2, pane_ids: [1, 2] },
    { name: "infra", active: false, panes: 1, pane_ids: [3] },
  ],
  tabs: [
    { title: "shell", active: true, focus: 1, panes: 2, pane_ids: [1, 2] },
    { title: "notes", active: false, focus: 3, panes: 1, pane_ids: [3] },
    { title: "logs", active: false, focus: 4, panes: 1, pane_ids: [4] },
  ],
  focus: 1,
  panes: [
    [1, { x: 0, y: 0, w: 0.5, h: 1 }],
    [2, { x: 0.5, y: 0, w: 0.5, h: 1 }],
  ],
  dividers: [{ split: 10, axis: "Row", area: { x: 0, y: 0, w: 1, h: 1 }, ratio: 0.5 }],
  pane_names: [],
  windows: 1,
};

const TABLET = { width: 1280, height: 800, screen: [1280, 800] };

/** Open as an Android tablet and wait until both panes are started and the key row is drawn. */
async function openTablet(page, url, options = {}) {
  await open(page, url, { ...TABLET, view: VIEW, shown: 2, ...options });
  await page.waitForSelector("#keyrow:not([hidden])");
  await settle(page);
}

/** Wait until the stylesheet of the tablet is in and the page measured against it (a resize follows the sheet). */
async function settle(page) {
  await page.waitForFunction(() => {
    const root = document.documentElement;
    return !root.classList.contains("layout-tablet") || document.querySelector('link[href*="tablet.css"]')?.sheet;
  });
  await frames(page, 4);
  await page.waitForTimeout(100);
}

const rootHas = (page, cls) => page.evaluate((c) => document.documentElement.classList.contains(c), cls);
const sheetPresent = (page) => page.evaluate(() => document.querySelector('link[href*="tablet.css"]') !== null);
const clear = (page) => page.evaluate(() => window.__MOCK__.calls.splice(0));

/** What the page measured: the header strip, the metrics and the cells the shell was told for each pane. */
const measured = (page) =>
  page.evaluate(async () => {
    const { state } = await import(new URL("state.js", location.href).href);
    const canvas = document.getElementById("term").getBoundingClientRect();
    const told = new Map();
    // The shell is told a pane's size by `start_pane` and then by every `resize`; the last one of each pane is what it has.
    for (const c of window.__MOCK__.calls.filter((x) => x[0] === "start_pane" || x[0] === "resize")) told.set(c[1].pane, c[1]);
    return {
      header: state.headerPx,
      cellH: state.metrics.cellH,
      cellW: state.metrics.cellW,
      canvas: { w: canvas.width, h: canvas.height },
      panes: state.view.panes.map(([id, r]) => ({ id, unit: r, size: state.sizes.get(id), told: told.get(id) ?? null })),
    };
  });

async function tabletCase(page, url) {
  await openTablet(page, url);
  assert.ok(await rootHas(page, "layout-tablet"), "an Android window of 1280 px on a screen of 800 px is the tablet layout");
  assert.ok(!(await rootHas(page, "layout-phone")));
  assert.ok(await sheetPresent(page), "the tablet's stylesheet is loaded");
  assert.equal(await page.locator("#phone-top").count(), 0, "no phone strip");
  assert.equal(await page.locator("#phone-nav").count(), 0, "no phone bar");
  assert.equal(await page.locator("#bar").isVisible(), true, "the desktop bar is drawn");
  assert.equal(await page.locator("#bar .bar-workspaces").count(), 1, "with the workspace row");
  assert.equal(await page.locator("#bar .bar-tabs").count(), 1, "and the tab row");
  assert.equal(await page.locator("#bar .tab-item").count(), 3, "three tabs");
  assert.equal(await page.locator("#overlay .panehead").count(), 2, "both panes have a header");

  // The core's rectangles are not transformed: both panes are drawn at the half of the stage, each with its own grid.
  const m = await measured(page);
  assert.deepEqual(m.panes.map((p) => p.id), [1, 2]);
  assert.deepEqual(m.panes.map((p) => p.unit), [VIEW.panes[0][1], VIEW.panes[1][1]], "the page holds the core's rectangles as they came");
  for (const p of m.panes) {
    assert.ok(p.told, `pane ${p.id} was told its size`);
    assert.ok(p.told.cols < m.canvas.w / m.cellW / 1.5, `pane ${p.id} is told the grid of half the stage, not of the whole (${p.told.cols} columns)`);
  }
  assert.notEqual(m.panes[0].told.pane, m.panes[1].told.pane);
}

async function targetsCase(page, url) {
  await openTablet(page, url);
  const sizes = await page.evaluate(() =>
    [...document.querySelectorAll("#bar button, #bar [role=button], #bar [role=tab], #overlay .panehead button, #overlay .panehead [role=button]")]
      .filter((n) => n.getBoundingClientRect().width > 0)
      .map((n) => {
        const r = n.getBoundingClientRect();
        return { name: `${n.tagName}.${n.className}`, w: Math.round(r.width * 100) / 100, h: Math.round(r.height * 100) / 100 };
      }),
  );
  assert.ok(sizes.length >= 12, `the bar and both headers have controls (${sizes.length})`);
  for (const s of sizes) assert.ok(s.w >= 44 - 0.01 && s.h >= 44 - 0.01, `${s.name} is ${s.w} by ${s.h}, not 44 by 44`);
  assert.ok(sizes.some((s) => s.name.includes("tab")), "tab squares are among them");
  assert.ok(sizes.some((s) => s.name.includes("ws-chip")), "workspace pills are among them");
  assert.ok(sizes.some((s) => s.name.includes("close")), "close marks are among them");
  assert.equal(await page.locator("#overlay .panehead .ph-close").count(), 2, "each header has its close mark");

  // The header strip is 44 px and the grid starts at its bottom edge: the shell was told the rows that fit below it.
  const heads = await page.evaluate(() => [...document.querySelectorAll("#overlay .panehead")].map((n) => n.getBoundingClientRect().height));
  assert.deepEqual(heads, [44, 44], "both strips are 44 px tall");
  const m = await measured(page);
  assert.equal(m.header, 44);
  for (const p of m.panes) {
    const height = p.unit.h * m.canvas.h;
    const rows = Math.max(1, Math.floor((height - m.header - 6) / m.cellH));
    assert.equal(p.told.rows, rows, `pane ${p.id}: the shell was told ${p.told.rows} rows, the cells below a 44 px strip are ${rows}`);
    assert.equal(p.size.rows, rows, "and the page keeps the same size");
  }
  // The bar has grown to hold its targets: the stage starts right under it.
  const bar = await page.evaluate(() => ({ bar: document.getElementById("bar").getBoundingClientRect().toJSON(), stage: document.getElementById("stage").getBoundingClientRect().toJSON() }));
  assert.ok(bar.bar.height >= 90, `the bar is two rows of 44 px (${bar.bar.height})`);
  assert.ok(Math.abs(bar.bar.bottom - bar.stage.top) <= 1, "the stage starts under the bar");
}

/** One drag of a divider: the line and its touch area, then the touch and the mouse gesture. */
async function dividerCase(page, url) {
  await openTablet(page, url);
  const area = await page.evaluate(() => {
    const node = document.querySelector(".divider");
    const line = getComputedStyle(node, "::after");
    const r = node.getBoundingClientRect();
    const term = document.getElementById("term").getBoundingClientRect();
    return {
      box: r.toJSON(), term: term.toJSON(), width: parseFloat(line.width), touchAction: getComputedStyle(node).touchAction,
      lineLeft: parseFloat(line.left), bg: line.backgroundColor,
    };
  });
  assert.ok(Math.abs(area.box.width - 24) <= 0.01, `the divider's area is 24 px wide (${area.box.width})`);
  assert.ok(Math.abs(area.box.left + area.box.width / 2 - (area.term.left + area.term.width / 2)) <= 1, "centred on the line");
  assert.ok(Math.abs(area.box.height - area.term.height) <= 1, "and as long as the split");
  assert.equal(area.width, 1, "the line inside it is 1 px");
  assert.equal(area.touchAction, "none", "a finger on the divider does not scroll or zoom the page");

  const cx = area.box.left + area.box.width / 2;
  const cy = area.box.top + area.box.height / 2;
  // 10 px off the line: still inside the 24 px area, outside the desktop's 8 px.
  const grab = cx + 10;
  const dx = 120;
  const expected = (grab + dx - area.term.left) / area.term.width;

  await clear(page);
  await run(page, [
    down(5, [grab, cy], ".divider"),
    move(5, [grab + dx / 2, cy], ".divider", 20),
    move(5, [grab + dx, cy], ".divider", 20),
    up(5, [grab + dx, cy], ".divider"),
  ]);
  await frames(page, 3);
  const touch = await callsOf(page, "set_ratio");
  assert.ok(touch.length >= 1, "a touch on the area sends set_ratio");
  assert.equal(touch.at(-1).split, 10);
  assert.ok(Math.abs(touch.at(-1).ratio - expected) <= 0.02, `the ratio is ${touch.at(-1).ratio}, the finger ended at ${expected.toFixed(3)}`);
  for (const cmd of ["wheel", "select", "mouse"]) assert.equal((await callsOf(page, cmd)).length, 0, `no ${cmd} command for a divider drag`);
  assert.deepEqual(await page.evaluate(() => [window.scrollX, window.scrollY]), [0, 0], "the page did not scroll");

  // The same drag with the mouse gives the same command (the pane is back at its start: the mock keeps the ratio).
  await page.evaluate(() => window.__MOCK__.setView(JSON.parse(JSON.stringify(window.__MOCK__.view()))));
  const now = await page.locator(".divider").boundingBox();
  const mx = now.x + now.width / 2;
  const startRatio = (mx - area.term.left) / area.term.width;
  await clear(page);
  await page.mouse.move(mx + 10, cy);
  await page.mouse.down();
  await page.mouse.move(mx + 10 + dx / 2, cy, { steps: 4 });
  await page.mouse.move(mx + 10 + dx, cy, { steps: 4 });
  await page.mouse.up();
  await frames(page, 3);
  const mouse = await callsOf(page, "set_ratio");
  assert.ok(mouse.length >= 1, "a mouse on the area sends set_ratio as well");
  const mouseExpected = (mx + 10 + dx - area.term.left) / area.term.width;
  assert.ok(Math.abs(mouse.at(-1).ratio - mouseExpected) <= 0.02, `the mouse ratio ${mouse.at(-1).ratio} against ${mouseExpected.toFixed(3)} (start ${startRatio.toFixed(3)})`);
  assert.equal(mouse.at(-1).split, 10);
}

const run1 = (page, id) =>
  page.evaluate(async (i) => (await import(new URL("actions.js", location.href).href)).runAction(i), id);
const titles = (page) =>
  page.evaluate(async () => (await import(new URL("actions.js", location.href).href)).listActions().map((a) => a.title));
const TEAR_OFF = ["Move pane to new window", "Move tab to new window"];
const TEAR_CALLS = ["tear_off_pane", "tear_off_tab", "release_outside", "drag_over", "drag_end"];
const tearCalls = async (page) => {
  let n = 0;
  for (const c of TEAR_CALLS) n += (await callsOf(page, c)).length;
  return n;
};

async function tearOffCase(page, url) {
  await openTablet(page, url);
  assert.ok(!(await titles(page)).some((t) => TEAR_OFF.includes(t)), "the two tear-off actions are not registered");
  assert.equal(await run1(page, "palette.open"), true);
  await page.waitForSelector(".palette-row");
  const listed = await page.evaluate(() => [...document.querySelectorAll(".palette-row .palette-title")].map((n) => n.textContent));
  assert.ok(listed.length > 3, "the palette lists the other actions");
  for (const t of TEAR_OFF) assert.ok(!listed.includes(t), `the palette does not list "${t}"`);
  await page.locator(".palette-input").fill("new window");
  assert.equal(await page.locator(".palette-row").count(), 0, "a search for the words finds neither entry");
  await page.keyboard.press("Escape");
  assert.equal(await run1(page, "pane.move_to_new_window"), false, "a stored shortcut for it runs nothing");
  assert.equal(await run1(page, "tab.move_to_new_window"), false);

  // A pane's header dragged out of the window and released: nothing is sent, nothing is said, the pane stays.
  await clear(page);
  const head = await page.evaluate(() => {
    const r = document.querySelectorAll("#overlay .panehead")[0].getBoundingClientRect();
    return { x: r.left + r.width - 150, y: r.top + 22 };
  });
  await page.mouse.move(head.x, head.y);
  await page.mouse.down();
  await page.mouse.move(head.x, head.y - 60, { steps: 4 });
  await page.mouse.move(-200, head.y, { steps: 6 });
  await page.waitForTimeout(150); // the hover poll runs every 100 ms or so
  await page.mouse.up();
  await frames(page, 3);
  assert.equal(await tearCalls(page), 0, "no tear-off, outline or release call went to the shell");
  assert.equal(await page.evaluate(() => document.querySelector("#status .notice")?.textContent ?? ""), "", "and nothing is said about it");
  assert.equal(await page.locator("#overlay .panehead").count(), 2, "both panes are still here");

  // A desktop at the same size has both entries and a divider of 8 px.
  await open(page, url, { ...TABLET, os: "macos", view: VIEW, shown: 2 });
  await frames(page, 4);
  const all = await titles(page);
  for (const t of TEAR_OFF) assert.ok(all.includes(t), `on a desktop "${t}" is registered`);
  assert.equal(await run1(page, "palette.open"), true);
  await page.waitForSelector(".palette-row");
  await page.locator(".palette-input").fill("new window");
  const found = await page.evaluate(() => [...document.querySelectorAll(".palette-row .palette-title")].map((n) => n.textContent));
  for (const t of TEAR_OFF) assert.ok(found.includes(t), `the palette lists "${t}" on a desktop`);
  await page.keyboard.press("Escape");
  const width = await page.locator(".divider").evaluate((n) => n.getBoundingClientRect().width);
  assert.equal(width, 8, "the desktop's divider is 8 px");
}

async function keyboardCase(page, url) {
  await openTablet(page, url);
  const where = await page.evaluate(() => ({
    row: document.getElementById("keyrow").getBoundingClientRect().toJSON(),
    stage: document.getElementById("stage").getBoundingClientRect().toJSON(),
    last: document.getElementById("app").lastElementChild.id,
    h: innerHeight,
  }));
  assert.equal(where.last, "keyrow", "the key row is the last element of the window");
  assert.ok(where.row.top >= where.stage.bottom - 1, "below the stage");
  assert.ok(where.row.bottom <= where.h + 1 && Math.abs(where.row.bottom - where.h) <= 1, "inside the viewport, at the bottom");

  // The on-screen keyboard takes height: the window follows the visual viewport and stays the tablet layout.
  const stageBefore = where.stage.height;
  await page.setViewportSize({ width: 1280, height: 500 });
  await page.waitForFunction(() => document.documentElement.style.getPropertyValue("--hw-vvh") === "500px");
  await settle(page);
  assert.ok(await rootHas(page, "layout-tablet"), "a change of height never changes the layout");
  const app = await page.evaluate(() => ({
    h: document.getElementById("app").getBoundingClientRect().height,
    stage: document.getElementById("stage").getBoundingClientRect().height,
    row: document.getElementById("keyrow").getBoundingClientRect().toJSON(),
  }));
  assert.ok(Math.abs(app.h - 500) <= 1, `the window is the visible 500 px (${app.h})`);
  assert.ok(app.stage < stageBefore - 250, "the stage is shorter by what the keyboard took");
  assert.ok(app.row.bottom <= 501, "the key row is still inside the visible part");

  // A key of a hardware keyboard hides the row; the rest of the layout stays.
  await page.keyboard.press("b");
  await page.waitForSelector("#keyrow[hidden]", { state: "attached" });
  assert.equal(await page.locator("#keyrow").isVisible(), false, "a hardware key hides the row");
  assert.ok(await rootHas(page, "layout-tablet"), "and the layout is the same");
  assert.equal(await page.locator(".divider").count(), 1);
}

async function widthCase(page, url) {
  // A window of 700 px on the same screen is the phone: the tablet's sheet is not in the page.
  await open(page, url, { width: 700, height: 800, screen: [1280, 800], view: VIEW });
  await page.waitForSelector("#keyrow:not([hidden])");
  await page.waitForTimeout(150);
  assert.ok(await rootHas(page, "layout-phone"), "700 px wide is the phone layout");
  assert.ok(!(await rootHas(page, "layout-tablet")));
  assert.equal(await sheetPresent(page), false, "tablet.css is not loaded");
  assert.equal(await page.locator("#phone-nav").count(), 1, "the phone bar is there");

  // A window that is a tablet and then narrows: the sheet goes away again and the pane header is 24 px.
  await openTablet(page, url);
  assert.ok(await sheetPresent(page));
  await page.setViewportSize({ width: 700, height: 800 });
  await page.waitForFunction(() => document.documentElement.classList.contains("layout-phone"));
  await page.waitForFunction(() => document.querySelector('link[href*="tablet.css"]') === null);
  await frames(page, 3);
  assert.equal((await measured(page)).header, 24, "the header strip is the desktop's 24 px again");
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.waitForFunction(() => document.documentElement.classList.contains("layout-tablet"));
  await settle(page);
  assert.equal((await measured(page)).header, 44, "and 44 px when the window is a tablet again");
  assert.equal(await page.locator(".divider").evaluate((n) => n.getBoundingClientRect().width), 24, "with a 24 px divider area");

  // A desktop at 1280 by 800 is a desktop: no class, no sheet, the numbers of today.
  await open(page, url, { ...TABLET, os: "macos", view: VIEW, shown: 2 });
  await page.waitForTimeout(150);
  assert.ok(!(await rootHas(page, "layout-tablet")) && !(await rootHas(page, "layout-phone")));
  assert.equal(await sheetPresent(page), false, "no tablet.css on a desktop");
  const m = await measured(page);
  assert.equal(m.header, 24, "the desktop's header strip");
  assert.equal(await page.locator(".divider").evaluate((n) => n.getBoundingClientRect().width), 8, "and divider");
  assert.equal(await page.locator("#keyrow").count(), 0, "and no key row");
}

async function forcedCase(page, url, engine) {
  if (engine !== "chromium") {
    console.log("  tablet: forced colours skipped on WebKit (it cannot emulate the media feature)");
    return;
  }
  await page.emulateMedia({ forcedColors: "active" });
  await openTablet(page, url);
  const line = await page.evaluate(() => {
    const s = getComputedStyle(document.querySelector(".divider"), "::after");
    return { border: s.borderLeftColor, width: s.borderLeftWidth, style: s.borderLeftStyle, bg: s.backgroundColor, w: s.width };
  });
  assert.equal(line.style, "solid", "the divider's line is a border in forced colours");
  assert.equal(line.width, "1px");
  assert.notEqual(line.border, "rgba(0, 0, 0, 0)", "and not transparent");
  assert.equal(line.w, "1px", "still 1 px wide");
  await page.emulateMedia({ forcedColors: "none" });
}
