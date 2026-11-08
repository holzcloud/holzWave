// The own window frame, first slice (docs/design.md section 6.33): the shell tells the page, through one global written before
// the page runs, that the window has no system title bar, and the page draws a close button in the bar that asks the window
// runtime to close the window it is in. Everything else (no facts, a browser, hostile facts) draws nothing. The Tauri global
// here is a stub: no window was ever started on Windows for this test, so it shows what the page draws and sends, not what a
// real window does.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { APP_DIR, callsOf, currentBackend } from "./lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  if (currentBackend() === "tauri") {
    await closeCase(page, url);
    await absentCase(page, url);
    await hostileCase(page, url);
  } else {
    await webCase(page, url);
  }
  sourceCase();
  assert.deepEqual(errors, [], "the page reported errors");
}

/** What the shell writes for Windows: the text of `windowframe.rs`, as the page receives it. */
const WINDOWS = { os: "windows", style: "flat", side: "right", buttons: ["minimize", "maximize", "close"] };
const GROUP = '#bar .bar-window button[data-button="close"]';

/**
 * Load the page with `facts` as the global (`undefined`: no global at all) and the platform `os` answered by `app_info`.
 * The init scripts of an earlier load stay registered on the page and run first, so every load sets or deletes the global.
 */
async function open(page, url, { facts, os = "windows" }) {
  await page.addInitScript(
    ([value, name, present]) => {
      window.__MOCK__.on("app_info", () => ({ version: "0.0.1-alpha1", os: name, hostname: "woodbox" }));
      if (present) window.__HW_FRAME__ = value;
      else delete window.__HW_FRAME__;
    },
    [facts === undefined ? null : facts, os, facts !== undefined],
  );
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("window_ready").length >= 1);
}

const boxes = (page) =>
  page.evaluate(() => {
    const rect = (node) => {
      const r = node.getBoundingClientRect();
      return [r.x, r.y, r.width, r.height].map((n) => Math.round(n * 100) / 100);
    };
    return Object.fromEntries(
      ["#bar", "#bar .bar-workspaces", "#bar .bar-fill", "#bar .bar-info", "#bar .bar-tabs", "#bar .tab"].map((s) => [
        s,
        rect(document.querySelector(s)),
      ]),
    );
  });

/** The facts of the shell: one close button in the right of the title row that closes the window it is in. */
async function closeCase(page, url) {
  await open(page, url, { facts: WINDOWS });
  assert.equal(await page.locator(GROUP).count(), 1, "exactly one close button");
  assert.equal(await page.locator("#bar .bar-window button").count(), 1, "and no other button yet");
  const button = page.locator("#bar .bar-window").getByRole("button", { name: "Close" });
  assert.equal(await button.count(), 1, "its accessible name is Close");
  assert.equal(await page.locator(GROUP).getAttribute("title"), "Close");
  assert.equal(await page.locator(GROUP).getAttribute("type"), "button");
  const root = await page.evaluate(() => [...document.documentElement.classList]);
  for (const name of ["frame-own", "frame-flat", "frame-right"]) assert.ok(root.includes(name), `root class ${name}`);

  // Where it sits: in the title row, flush with the right edge of the window, the size of the design, the free space still drags.
  const shape = await page.evaluate(() => {
    const bar = document.getElementById("bar");
    const group = bar.querySelector(".bar-window");
    const button = group.querySelector("button");
    const r = button.getBoundingClientRect();
    const barRect = bar.getBoundingClientRect();
    return {
      inside: group.parentElement === bar,
      groupDrags: group.hasAttribute("data-tauri-drag-region"),
      buttonDrags: button.hasAttribute("data-tauri-drag-region"),
      fillDrags: bar.querySelector(".bar-fill").hasAttribute("data-tauri-drag-region"),
      infoDrags: bar.querySelector(".bar-info").hasAttribute("data-tauri-drag-region"),
      flush: Math.abs(r.right - window.innerWidth),
      top: r.top - barRect.top,
      width: r.width,
      height: r.height,
      rem: parseFloat(getComputedStyle(document.documentElement).fontSize),
      tabsTop: bar.querySelector(".bar-tabs").getBoundingClientRect().top,
      buttonBottom: r.bottom,
      infoRight: bar.querySelector(".bar-info").getBoundingClientRect().right,
      left: r.left,
    };
  });
  assert.ok(shape.inside, "the group is a child of the bar");
  assert.equal(shape.groupDrags || shape.buttonDrags, false, "the buttons are not a drag region");
  assert.ok(shape.fillDrags && shape.infoDrags, "the free space of the bar still drags the window");
  assert.ok(shape.flush < 0.6, `flush with the window edge, off by ${shape.flush}`);
  assert.ok(shape.top < 0.6, "in the title row, at the top of the bar");
  assert.ok(Math.abs(shape.width - (46 * shape.rem) / 12) < 1 || shape.width === 28, `46 px wide at the base size: ${shape.width}`);
  assert.ok(shape.height >= 28 - 0.5, `at least 28 px high: ${shape.height}`);
  assert.ok(shape.buttonBottom <= shape.tabsTop + 0.5, "the button stays in the title row");
  assert.ok(shape.infoRight <= shape.left + 0.5, "the host name and the clock end before the buttons");

  // The click asks the runtime to close this window and nothing else.
  await button.click();
  assert.equal((await callsOf(page, "plugin:window|close")).length, 1, "one close call");
  for (const forbidden of ["plugin:window|destroy", "plugin:window|hide", "plugin:window|set_decorations"]) {
    assert.equal((await callsOf(page, forbidden)).length, 0, forbidden);
  }
  assert.equal(
    await page.evaluate(() => window.__MOCK__.listening("tauri://close-requested")),
    0,
    "the page never listens to the close request: a listener would keep the window open",
  );
  // The button does not keep the keyboard.
  assert.equal(await page.evaluate(() => document.activeElement?.closest(".bar-window") === null), true);
}

/** No facts: nothing is drawn and the bar is the one of a platform that never heard of the frame. */
async function absentCase(page, url) {
  await open(page, url, { facts: undefined, os: "linux" });
  assert.equal(await page.locator("#bar .bar-window").count(), 0, "no group");
  assert.equal(await page.evaluate(() => document.documentElement.className.includes("frame-")), false, "no frame class");
  const linux = await boxes(page);
  await open(page, url, { facts: undefined, os: "windows" });
  assert.equal(await page.locator("#bar .bar-window").count(), 0, "no group on Windows without facts either");
  assert.deepEqual(await boxes(page), linux, "the bar is the same without facts");
  assert.equal((await callsOf(page, "plugin:window|close")).length, 0);
}

/** The global is untrusted input: anything but the exact shape is ignored, quietly. */
async function hostileCase(page, url) {
  const hostile = [
    "windows", 42, null, {}, [], [WINDOWS],
    { ...WINDOWS, os: "linux" },
    { ...WINDOWS, os: "macos" },
    { ...WINDOWS, style: "round" },
    { ...WINDOWS, side: "left" },
    { ...WINDOWS, extra: "x" },
    { os: "windows", style: "flat", side: "right" },
    { ...WINDOWS, buttons: [] },
    { ...WINDOWS, buttons: ["minimize", "maximize"] },
    { ...WINDOWS, buttons: ["close", "close"] },
    { ...WINDOWS, buttons: ["close", "destroy"] },
    { ...WINDOWS, buttons: "close" },
    { ...WINDOWS, buttons: [1] },
    { ...WINDOWS, buttons: ["minimize", "maximize", "close", "close"] },
  ];
  for (const facts of hostile) {
    await open(page, url, { facts });
    assert.equal(await page.locator("#bar .bar-window").count(), 0, `nothing is drawn for ${JSON.stringify(facts)}`);
    assert.equal(await page.evaluate(() => document.documentElement.className.includes("frame-")), false, JSON.stringify(facts));
  }
  assert.equal((await callsOf(page, "plugin:window|close")).length, 0);
}

/** A browser draws no window buttons: the facts are ignored there. */
async function webCase(page, url) {
  await open(page, url, { facts: WINDOWS });
  assert.equal(await page.locator("#bar .bar-window").count(), 0, "no group in a browser");
  assert.equal(await page.evaluate(() => document.documentElement.className.includes("frame-")), false);
  assert.equal((await callsOf(page, "plugin:window|close")).length, 0, "and no call");
}

/** The shipped source holds no call that parses markup and none of the commands or events the page must never use. */
function sourceCase() {
  const source = readFileSync(join(APP_DIR, "src/windowframe.ts"), "utf8");
  const code = source.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
  assert.doesNotMatch(code, /innerHTML|outerHTML|insertAdjacentHTML|DOMParser|createContextualFragment|document\.write|cssText/);
  assert.doesNotMatch(code, /plugin:window\|(?:destroy|hide|set_decorations|set_always_on_top)|set_decorations|close-requested|listen\(/);
  assert.match(code, /plugin:window\|close/);
}
