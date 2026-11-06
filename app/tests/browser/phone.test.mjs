// The phone layout: the layout rule in the page, one pane per screen, the strips and the sheet, and the desktop that stays as it was.
// An Android WebView is not used: the platform is mocked (`app_info` answers `android`) and the screen size is overridden, in the
// desktop engines. What a real phone shows (insets, touch, the system back gesture) is not covered here.
import assert from "node:assert/strict";
import { callsOf } from "./lib.mjs";

// The layout switch is a property of the Android shell; the web backend never reports `android`.
export const backends = ["tauri"];
export const backendNote = "the phone and tablet layouts belong to the Android shell, which the web backend never is";

export default async function ({ engine, page, url }) {
  await phoneCase(page, url);
  await sheetCase(page, url);
  await classCase(page, url);
  await desktopCase(page, url);
  await markupCase(page, url);
  await cuesCase(page, url, engine);
}

/** One workspace and one tab of three panes side by side, the second focused; a second tab and a second workspace to tap. */
const VIEW = {
  rev: 1,
  workspaces: [
    { name: "main", active: true, panes: 4, pane_ids: [1, 2, 3, 4] },
    { name: "infra", active: false, panes: 1, pane_ids: [5] },
  ],
  tabs: [
    { title: "shell", active: true, focus: 2, panes: 3, pane_ids: [1, 2, 3] },
    { title: "notes", active: false, focus: 4, panes: 1, pane_ids: [4] },
  ],
  focus: 2,
  panes: [
    [1, { x: 0, y: 0, w: 1 / 3, h: 1 }],
    [2, { x: 1 / 3, y: 0, w: 1 / 3, h: 1 }],
    [3, { x: 2 / 3, y: 0, w: 1 / 3, h: 1 }],
  ],
  dividers: [
    { split: 10, axis: "Row", area: { x: 0, y: 0, w: 1, h: 1 }, ratio: 1 / 3 },
    { split: 11, axis: "Row", area: { x: 1 / 3, y: 0, w: 2 / 3, h: 1 }, ratio: 0.5 },
  ],
  pane_names: [[3, "logs"]],
  windows: 1,
};

/**
 * Open the page as a device would: the platform, the viewport and the size of the screen. Every case registers its own init scripts
 * (the ones of an earlier case run first and are overridden by the later ones), and the layout commands answer like the core:
 * a focus, a tab or a workspace that changes is the next view.
 */
async function open(page, url, { os = "android", width = 390, height = 844, screen = [390, 844], view = VIEW } = {}) {
  await page.setViewportSize({ width, height });
  await page.addInitScript(([w, h]) => {
    for (const [key, value] of [["width", w], ["height", h], ["availWidth", w], ["availHeight", h]]) {
      Object.defineProperty(window.screen, key, { get: () => value, configurable: true });
    }
  }, screen);
  await page.addInitScript(
    ([info, v]) => {
      const mock = window.__MOCK__;
      mock.on("app_info", () => info);
      mock.setView(v);
      // The core's answers: the next view, as the layout event would carry it.
      mock.on("focus_pane", (a) => {
        const next = mock.view();
        next.focus = a.pane;
        next.tabs.find((t) => t.active).focus = a.pane;
        next.rev += 1;
        next.panes = next.panes.filter(([id]) => id !== a.pane).concat(next.panes.filter(([id]) => id === a.pane));
        mock.setView(next);
        return mock.view();
      });
    },
    [{ version: "0.0.1-alpha1", os, hostname: "pixel" }, view],
  );
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 1);
}

const pageState = (page) =>
  page.evaluate(async () => {
    const { state } = await import(new URL("state.js", location.href).href);
    return { focus: state.view?.focus, panes: state.view?.panes.map(([id]) => id), dividers: state.view?.dividers.length };
  });

/** The cells the core must be told for one pane that fills the stage, computed from the page's own metrics and rules. */
const fullGrid = (page) =>
  page.evaluate(async () => {
    const { state } = await import(new URL("state.js", location.href).href);
    const { fitCells } = await import(new URL("geometry.js", location.href).href);
    const { cols, rows } = fitCells(state.metrics, { x: 0, y: 0, w: 1, h: 1 }, state.headerPx);
    return { cols, rows };
  });

const rootHas = (page, cls) => page.evaluate((c) => document.documentElement.classList.contains(c), cls);
const present = (page, selector) => page.evaluate((s) => document.querySelector(s) !== null, selector);
const texts = (page, selector) => page.locator(selector).allTextContents();

/** Every control of the strips and the sheet that is drawn, with its size. */
const targets = (page) =>
  page.evaluate(() =>
    [...document.querySelectorAll("#phone-top button, #phone-nav button, #phone-sheet button, [role=button], [role=tab], [role=option]")].map(
      (n) => {
        const r = n.getBoundingClientRect();
        return { name: n.className || n.tagName, width: r.width, height: r.height };
      },
    ),
  );

async function phoneCase(page, url) {
  await open(page, url);
  assert.ok(await rootHas(page, "layout-phone"), "an Android window of 390 px is the phone layout");
  assert.ok(!(await rootHas(page, "layout-tablet")), "and not the tablet layout");
  assert.equal(await page.locator("#bar").isVisible(), false, "the desktop bar is not drawn on a phone");
  assert.equal(await page.locator("#status").isVisible(), false, "the desktop status line is not drawn on a phone");

  // One pane fills the screen: the core's three panes become the focused one at the full rectangle, with no dividers.
  assert.deepEqual(await pageState(page), { focus: 2, panes: [2], dividers: 0 }, "the page sees the focused pane alone");
  assert.equal(await page.locator(".divider").count(), 0, "no divider is drawn");
  assert.equal(await page.locator("#overlay .panehead").count(), 1, "one pane header");
  const box = await page.evaluate(() => {
    const rect = (id) => document.getElementById(id).getBoundingClientRect().toJSON();
    return { term: rect("term"), stage: rect("stage"), top: rect("phone-top"), nav: rect("phone-nav"), win: [innerWidth, innerHeight] };
  });
  for (const key of ["x", "y", "width", "height"]) {
    assert.ok(Math.abs(box.term[key] - box.stage[key]) <= 1, `the canvas fills the stage (${key}: ${box.term[key]} against ${box.stage[key]})`);
  }
  assert.ok(Math.abs(box.top.bottom - box.stage.top) <= 1 && Math.abs(box.stage.bottom - box.nav.top) <= 1, "top strip, stage, bottom bar in a column");
  assert.ok(Math.abs(box.nav.bottom - box.win[1]) <= 1 && box.stage.width >= box.win[0] - 1, "the page fills the window");

  // The pty is told the full grid for the shown pane; the panes that are not shown are not touched.
  const grid = await fullGrid(page);
  const started = await callsOf(page, "start_pane");
  assert.deepEqual(started.map((a) => a.pane), [2], "only the shown pane was started");
  assert.deepEqual({ cols: started[0].cols, rows: started[0].rows }, grid, "the full-size grid");
  assert.ok(grid.cols > 20 && grid.rows > 20, `a grid that fills a phone: ${JSON.stringify(grid)}`);
  assert.equal((await callsOf(page, "close_pane")).length, 0, "no pane was closed");

  // The strip: the workspace, the tab, and `2/3` for a tab of three panes.
  assert.deepEqual(await texts(page, "#phone-top .phone-ws"), ["main"]);
  assert.deepEqual(await texts(page, "#phone-top .phone-tab"), ["1 shell"]);
  assert.deepEqual(await texts(page, "#phone-top .phone-count"), ["2/3"]);
  assert.equal(await page.locator("#phone-chip").count(), 1, "the place of a status chip exists");
  assert.equal((await texts(page, "#phone-chip"))[0], "", "and is empty");
  // The bottom bar: the tabs of the shown workspace, numbered, the shown one marked by more than colour.
  assert.deepEqual(await texts(page, "#phone-nav .phone-tab-btn"), ["1 shell", "2 notes"]);
  const nav = await page.evaluate(() =>
    [...document.querySelectorAll("#phone-nav .phone-tab-btn")].map((n) => ({
      current: n.getAttribute("aria-current"),
      deco: getComputedStyle(n).textDecorationLine,
      top: getComputedStyle(n).borderTopStyle,
    })),
  );
  assert.equal(nav[0].current, "page");
  assert.equal(nav[1].current, null);
  assert.ok(nav[0].deco.includes("underline") && !nav[1].deco.includes("underline"), "the shown tab is underlined");

  // A height-only change that the keyboard makes shrinks the stage and asks the pty for the smaller full grid.
  const before = grid.rows;
  await page.setViewportSize({ width: 390, height: 500 });
  await page.waitForFunction((n) => window.__MOCK__.callsOf("resize").length >= 1 && n > 0, before);
  const resized = (await callsOf(page, "resize")).at(-1);
  const smaller = await fullGrid(page);
  assert.deepEqual({ cols: resized.cols, rows: resized.rows }, smaller, "the resize carries the full grid of the smaller stage");
  assert.ok(smaller.rows < before && smaller.cols === grid.cols, "fewer rows, the same columns");
}

async function sheetCase(page, url) {
  await open(page, url);
  assert.equal(await page.locator("#phone-sheet").isVisible(), false, "the sheet starts closed");
  await page.locator("#phone-top .phone-title").click();
  assert.equal(await page.locator("#phone-sheet").isVisible(), true, "a tap on the strip's name opens the sheet");
  assert.equal(await page.locator("#phone-sheet").getAttribute("aria-modal"), "true");

  // The lists: workspace pills, the tabs of the workspace and the panes of the shown tab.
  assert.deepEqual(await texts(page, "#phone-sheet .phone-pill:not(.phone-pill-plus)"), ["main", "infra"]);
  assert.deepEqual(await texts(page, "#phone-sheet .phone-list[aria-label='Tabs'] .phone-row-name"), ["1 shell", "2 notes"]);
  const panes = await page.evaluate(() =>
    [...document.querySelectorAll("#phone-sheet .phone-pane")].map((n) => ({
      name: n.querySelector(".phone-row-name").textContent,
      current: n.getAttribute("aria-current"),
      dot: n.querySelector(".ph-dot").getAttribute("data-state"),
      deco: getComputedStyle(n.querySelector(".phone-row-name")).textDecorationLine,
      bar: getComputedStyle(n).borderLeftStyle,
      barWidth: parseFloat(getComputedStyle(n).borderLeftWidth),
    })),
  );
  assert.deepEqual(panes.map((p) => p.name), ["Pane 1", "Pane 2", "logs"], "a name the user gave wins, else `Pane N`");
  assert.deepEqual(panes.map((p) => p.current), [null, "true", null], "only the shown pane is marked");
  assert.deepEqual(panes.map((p) => p.dot), [null, "focused", null], "the dot is filled for the shown pane");
  assert.ok(panes[1].deco.includes("underline") && !panes[0].deco.includes("underline"), "the shown pane's name is underlined");
  assert.ok(panes[1].bar !== "none" && panes[1].barWidth >= 2, "and has a bar on its left");

  // Every control is at least 44 by 44 CSS px: the strips, the bottom bar and everything in the sheet.
  const all = await targets(page);
  assert.ok(all.length >= 14, `the controls were found (${all.length})`);
  for (const t of all) assert.ok(t.width >= 43.99 && t.height >= 43.99, `${t.name} is ${t.width} by ${t.height}`);

  // A tap on a pane row focuses that pane, closes the sheet and the stage shows it.
  await page.locator("#phone-sheet .phone-pane").nth(2).click();
  assert.deepEqual((await callsOf(page, "focus_pane")).at(-1), { pane: 3 });
  assert.equal(await page.locator("#phone-sheet").isVisible(), false, "a pane row closes the sheet");
  await page.waitForFunction(() => document.querySelector("#phone-top .phone-count")?.textContent === "3/3");
  assert.deepEqual(await pageState(page), { focus: 3, panes: [3], dividers: 0 }, "the stage shows the new focus");
  const started = (await callsOf(page, "start_pane")).map((a) => a.pane);
  assert.deepEqual(started, [2, 3], "the pane that comes into view is started at the full grid");
  const grid = await fullGrid(page);
  const last = (await callsOf(page, "start_pane")).at(-1);
  assert.deepEqual({ cols: last.cols, rows: last.rows }, grid);
  assert.equal((await callsOf(page, "close_pane")).length, 0, "the pane that left the screen was not closed");

  // A bottom tab, a workspace pill, the plus of the bar and the plus of the pills send the same commands as the desktop bar.
  await page.locator("#phone-nav .phone-tab-btn").nth(1).click();
  assert.deepEqual((await callsOf(page, "select_tab")).at(-1), { index: 1 });
  await page.locator("#phone-nav .phone-plus").click();
  assert.equal((await callsOf(page, "new_tab")).length, 1);
  await page.locator("#phone-top .phone-title").click();
  await page.locator("#phone-sheet .phone-pill:not(.phone-pill-plus)").nth(1).click();
  assert.deepEqual((await callsOf(page, "select_workspace")).at(-1), { index: 1 });
  assert.equal(await page.locator("#phone-sheet").isVisible(), true, "a pill keeps the sheet open for the tab to pick");
  await page.locator("#phone-sheet .phone-pill-plus").click();
  assert.deepEqual((await callsOf(page, "new_workspace")).at(-1), { name: "ws 3" });
  await page.locator("#phone-sheet .phone-list").first().locator(".phone-row").nth(1).click();
  assert.deepEqual((await callsOf(page, "select_tab")).at(-1), { index: 1 });
  assert.equal(await page.locator("#phone-sheet").isVisible(), false, "a tab row closes the sheet");

  // Escape closes the sheet and gives the focus back to the control that opened it; a tap outside closes it too.
  const opener = page.locator("#phone-nav .phone-opener");
  await opener.click();
  assert.equal(await page.locator("#phone-sheet").isVisible(), true);
  assert.equal(await page.evaluate(() => document.getElementById("app").hasAttribute("inert")), true, "the page behind the sheet is inert");
  await page.keyboard.press("Escape");
  assert.equal(await page.locator("#phone-sheet").isVisible(), false, "Escape closes the sheet");
  assert.equal(await page.evaluate(() => document.activeElement?.className), "phone-opener", "the focus is back on the opener");
  assert.equal(await page.evaluate(() => document.getElementById("app").hasAttribute("inert")), false);
  assert.equal((await callsOf(page, "key")).length, 0, "the Escape was not sent to the terminal");
  await page.locator("#phone-top .phone-title").click();
  await page.mouse.click(195, 20); // the scrim above the sheet
  assert.equal(await page.locator("#phone-sheet").isVisible(), false, "a tap outside closes the sheet");
  assert.equal(await page.evaluate(() => document.activeElement?.className), "phone-title");
}

async function classCase(page, url) {
  // A tablet screen with a wide window: the tablet layout. The desktop bar stays, the phone's strips are not made.
  await open(page, url, { width: 1280, height: 800, screen: [1280, 800] });
  assert.ok(await rootHas(page, "layout-tablet"), "a 1280 px Android window on a 1280 by 800 screen is the tablet layout");
  assert.ok(!(await rootHas(page, "layout-phone")));
  assert.equal(await page.locator("#bar").isVisible(), true, "the desktop bar is drawn");
  assert.equal(await present(page, "#phone-top"), false, "and the phone's strip is not");
  assert.equal(await present(page, "#phone-nav"), false);
  assert.deepEqual((await pageState(page)).panes, [1, 2, 3], "the tablet keeps all the panes");

  // The window narrows (split screen): the phone layout, and widening it again takes the strips away.
  await page.setViewportSize({ width: 700, height: 800 });
  await page.waitForFunction(() => document.documentElement.classList.contains("layout-phone"));
  assert.ok(!(await rootHas(page, "layout-tablet")));
  assert.equal(await present(page, "#phone-top"), true);
  assert.equal(await page.locator("#bar").isVisible(), false);
  assert.deepEqual((await pageState(page)).panes, [2], "the view was cut to the shown pane when the class changed");
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.waitForFunction(() => document.documentElement.classList.contains("layout-tablet"));
  assert.equal(await present(page, "#phone-top"), false, "the strips are gone on the tablet");
  assert.equal(await present(page, "#phone-nav"), false);
  assert.equal(await present(page, "#phone-sheet"), false);
  assert.deepEqual((await pageState(page)).panes, [1, 2, 3], "and the panes are all there again");
  assert.equal(await page.locator("#bar").isVisible(), true);

  // A landscape phone: a wide window on a screen whose short side is a phone's stays a phone.
  await open(page, url, { width: 900, height: 400, screen: [900, 400] });
  assert.ok(await rootHas(page, "layout-phone"), "900 by 400 is a landscape phone");

  // The keyboard changes the height only: the class stays and the strips are the same elements.
  await open(page, url);
  await page.evaluate(() => {
    window.__strip = document.getElementById("phone-top");
    window.__nav = document.getElementById("phone-nav");
    window.__events = [];
    document.addEventListener("hw:layout", (e) => window.__events.push(e.detail.layout));
  });
  await page.setViewportSize({ width: 390, height: 500 });
  await page.waitForFunction(() => innerHeight === 500);
  await page.waitForTimeout(150);
  assert.ok(await rootHas(page, "layout-phone"), "a height-only change keeps the phone layout");
  assert.equal(
    await page.evaluate(() => document.getElementById("phone-top") === window.__strip && document.getElementById("phone-nav") === window.__nav),
    true,
    "the strips are not rebuilt",
  );
  assert.deepEqual(await page.evaluate(() => window.__events), [], "no layout event fired");
  await page.setViewportSize({ width: 390, height: 844 });
  await page.waitForFunction(() => innerHeight === 844);
  await page.waitForTimeout(150);
  assert.deepEqual(await page.evaluate(() => window.__events), [], "no layout event fired when the keyboard went again");
}

async function desktopCase(page, url) {
  // The same window on a desktop platform is unchanged: neither class, the bar, no phone element.
  for (const os of ["macos", "web", "windows", "linux"]) {
    await open(page, url, { os });
    assert.ok(!(await rootHas(page, "layout-phone")) && !(await rootHas(page, "layout-tablet")), `${os}: no layout class`);
    assert.equal(await page.locator("#bar").isVisible(), true, `${os}: the desktop bar is drawn`);
    for (const id of ["phone-top", "phone-nav", "phone-sheet", "phone-scrim", "phone-chip"]) {
      assert.equal(await present(page, `#${id}`), false, `${os}: no #${id}`);
    }
    assert.equal(await page.evaluate(() => [...document.querySelectorAll('link[rel="stylesheet"]')].some((l) => l.href.endsWith("phone.css"))), false, `${os}: phone.css is not loaded`);
    assert.deepEqual((await pageState(page)).panes, [1, 2, 3], `${os}: every pane of the tab is drawn`);
    assert.equal(await page.locator(".divider").count(), 2, `${os}: the dividers are drawn`);
  }
}

async function markupCase(page, url) {
  // Names are text: markup in a workspace, tab or pane name makes no element and no script error.
  const hostile = '<img src=x onerror="window.__pwned=1">';
  const view = structuredClone(VIEW);
  view.workspaces[0].name = hostile;
  view.tabs[0].title = hostile;
  view.pane_names = [[1, hostile]];
  await open(page, url, { view });
  await page.locator("#phone-top .phone-title").click();
  assert.equal(await page.locator("#phone-top img, #phone-nav img, #phone-sheet img").count(), 0, "no element was made from a name");
  assert.deepEqual(await texts(page, "#phone-top .phone-ws"), [hostile]);
  assert.ok((await texts(page, "#phone-sheet .phone-pane .phone-row-name")).includes(hostile));
  assert.equal(await page.evaluate(() => window.__pwned), undefined, "no handler ran");
}

async function cuesCase(page, url, engine) {
  // Motion: the sheet slides in unless the person asked for less.
  await open(page, url);
  await page.locator("#phone-top .phone-title").click();
  const slide = () => page.evaluate(() => getComputedStyle(document.getElementById("phone-sheet")).animationName);
  assert.equal(await slide(), "phone-sheet-in", "with full motion the sheet slides in");
  await page.emulateMedia({ reducedMotion: "reduce" });
  await open(page, url);
  await page.locator("#phone-top .phone-title").click();
  assert.equal(await slide(), "none", "with reduced motion the sheet has no animation");
  assert.equal(await page.evaluate(() => document.documentElement.classList.contains("motion-reduce")), true);
  await page.emulateMedia({ reducedMotion: "no-preference" });

  // Forced colours: the underline and the bar are decorations and borders, so they stay when the browser replaces the colours.
  if (engine !== "chromium") {
    console.log(`NOT RUN phone forced colours [${engine}]: this engine cannot emulate forced colours`);
    return;
  }
  await page.emulateMedia({ forcedColors: "active" });
  await open(page, url);
  await page.locator("#phone-top .phone-title").click();
  const seen = await page.evaluate(() => {
    const tab = document.querySelector("#phone-nav .phone-tab-btn.active");
    const row = document.querySelector("#phone-sheet .phone-pane.active");
    return {
      forced: matchMedia("(forced-colors: active)").matches,
      tab: getComputedStyle(tab).textDecorationLine,
      tabLine: getComputedStyle(tab).borderTopStyle,
      name: getComputedStyle(row.querySelector(".phone-row-name")).textDecorationLine,
      bar: getComputedStyle(row).borderLeftStyle,
    };
  });
  assert.equal(seen.forced, true, "forced colours are emulated");
  assert.ok(seen.tab.includes("underline") && seen.tabLine !== "none", `the shown tab keeps its underline and line: ${JSON.stringify(seen)}`);
  assert.ok(seen.name.includes("underline") && seen.bar !== "none", `the shown pane row keeps its underline and bar: ${JSON.stringify(seen)}`);
  await page.emulateMedia({ forcedColors: "none" });
}
