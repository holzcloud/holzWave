// The pinch on the phone and the tablet layouts, the device-local UI scale it keeps, and every other platform with its touch pointer unchanged.
// The gestures of one finger are in touch.test.mjs. No touch screen and no Android WebView is used:
// the platform is mocked (`app_info` answers `android`), the screen size is overridden, and the touches are synthetic `PointerEvent`s with
// `pointerType: "touch"` dispatched into the desktop engines. What only a device shows (the feel of a touch, palm rejection, Android's own
// selection menu fighting the page, the WebView's clipboard permission, a real pinch) is not covered here.
import assert from "node:assert/strict";
import { callsOf } from "./lib.mjs";
import { down, fontPx, frames, geometry, move, open, rootPx, run, selects, shown, stored, tap, up, visible, wait } from "./touch-lib.mjs";

// The touch layouts belong to the Android shell, which the web backend never is.
export const backends = ["tauri"];
export const backendNote = "the phone and tablet layouts belong to the Android shell, which the web backend never is";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await pinchCase(page, url);
  await tabletCase(page, url);
  await desktopCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

// ---- the pinch ---------------------------------------------------------------------------------------

const fullGrid = (page) =>
  page.evaluate(async () => {
    const { state } = await import(new URL("state.js", location.href).href);
    const { fitCells } = await import(new URL("geometry.js", location.href).href);
    const { cols, rows } = fitCells(state.metrics, { x: 0, y: 0, w: 1, h: 1 }, state.headerPx);
    return { cols, rows };
  });

async function pinchCase(page, url) {
  await open(page, url, { settings: true });
  await page.evaluate(() => localStorage.removeItem("hw.deviceScale"));
  assert.equal(await rootPx(page), "12px", "100 % at the start");
  assert.equal(await fontPx(page), 14);
  const before = await fullGrid(page);
  const g = await shown(page, 2);
  const mid = g.pt(10, 8);

  // Two fingers 100 px apart move to 125 px: UI scale 125 %.
  const left = [mid[0] - 50, mid[1]];
  const right = [mid[0] + 50, mid[1]];
  await run(page, [down(1, left), down(2, right), move(2, [right[0] + 25, right[1]], undefined, 20)]);
  await page.locator("#phone-top #touch-scale").waitFor({ state: "visible" });
  assert.equal(await page.locator("#phone-top #touch-scale").innerText(), "UI scale 125 %", "the label is in the top strip, in the status chip's place");
  assert.equal(await page.locator("#touch-scale").getAttribute("role"), "status");
  assert.equal(await rootPx(page), "15px", "the interface follows");
  assert.equal(await fontPx(page), 18, "14 px x 125 % = 17.5, rounded to 18");
  await page.waitForFunction(() => window.__MOCK__.callsOf("resize").length >= 1);
  await frames(page);
  const after = await fullGrid(page);
  assert.ok(after.cols < before.cols && after.rows < before.rows, `the grid shrank: ${JSON.stringify(before)} to ${JSON.stringify(after)}`);
  const resized = (await callsOf(page, "resize")).at(-1);
  assert.deepEqual({ cols: resized.cols, rows: resized.rows }, after, "the pty was told the grid of the bigger text");
  assert.equal(await stored(page), "125", "kept on this device");
  assert.deepEqual(await callsOf(page, "set_setting"), [], "and never sent to the core, so the sync cannot carry it");
  assert.equal(await page.evaluate(() => window.__MOCK__.callsOf("wheel").length + window.__MOCK__.callsOf("select").length), 0, "a pinch scrolls and selects nothing");

  // The label stays after the fingers lift (800 ms) and then goes.
  await run(page, [up(2, [right[0] + 25, right[1]]), up(1, left)]);
  await page.waitForTimeout(450);
  assert.equal(await page.locator("#touch-scale").count(), 1, "still there 450 ms after the lift");
  await page.waitForFunction(() => document.getElementById("touch-scale") === null, undefined, { timeout: 1500 });
  assert.equal(await page.locator("#phone-chip").innerText(), "", "the chip's place is empty again");
  assert.equal(await rootPx(page), "15px", "the scale stays");

  // Steps of 5, held to 75 and 200.
  await run(page, [down(1, left), down(2, right), move(2, [right[0] + 400, right[1]], undefined, 20), up(2, [right[0] + 400, right[1]]), up(1, left)]);
  assert.equal(await stored(page), "200", "held to 200");
  await run(page, [down(1, [mid[0] - 150, mid[1]]), down(2, [mid[0] + 150, mid[1]]), move(2, [mid[0] - 140, mid[1]], undefined, 20), up(2, [mid[0] - 140, mid[1]]), up(1, [mid[0] - 150, mid[1]])]);
  assert.equal(await stored(page), "75", "held to 75");
  await page.waitForTimeout(700);

  // A reload with the key present starts at the pinched value, garbage in the key is ignored.
  await page.evaluate(() => localStorage.setItem("hw.deviceScale", "125"));
  await page.reload();
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 1);
  assert.equal(await rootPx(page), "15px", "a reload starts at 125 %");
  assert.equal(await fontPx(page), 18);
  assert.equal(await stored(page), "125");
  for (const junk of ["garbage", "130.5", "9".repeat(500)]) {
    await page.evaluate((v) => localStorage.setItem("hw.deviceScale", v), junk);
    await page.reload();
    await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 1);
    assert.equal(await rootPx(page), "12px", `${junk.slice(0, 12)}: ignored`);
    assert.equal(await fontPx(page), 14);
  }

  // Changing the UI scale in the preferences on this device removes the key; the shared value applies.
  await page.evaluate(() => localStorage.setItem("hw.deviceScale", "125"));
  await page.reload();
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 1);
  assert.equal(await rootPx(page), "15px");
  await page.evaluate(async () => (await import(new URL("actions.js", location.href).href)).runAction("settings.open"));
  const field = page.getByRole("dialog", { name: "Settings" }).getByLabel("UI scale", { exact: true });
  await field.fill("150");
  await field.press("Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("set_setting").length >= 1);
  assert.deepEqual(await callsOf(page, "set_setting"), [{ key: "ui_scale", value: 150 }]);
  assert.equal(await stored(page), null, "the pinch value is removed");
  await page.waitForFunction(() => getComputedStyle(document.documentElement).fontSize === "18px");
  assert.equal(await fontPx(page), 21, "the shared 150 % applies here: 14 x 150 %");
  await page.keyboard.press("Escape");
}

// ---- the tablet --------------------------------------------------------------------------------------

async function tabletCase(page, url) {
  await open(page, url, { width: 1280, height: 800, screen: [1280, 800], shown: 3 });
  assert.equal(await page.evaluate(() => document.documentElement.classList.contains("layout-tablet")), true);
  assert.equal(await page.locator('link[href$="touch.css"]').count(), 1);
  const one = await shown(page, 1);
  const three = await shown(page, 3);
  await shown(page, 2);

  // A tap on a pane without the focus focuses it and begins nothing.
  await tap(page, three.pt(4, 4));
  await frames(page);
  assert.deepEqual(await callsOf(page, "focus_pane"), [{ pane: 3 }], "focus_pane for the pane that did not have it");
  assert.deepEqual(await selects(page), []);

  // The label floats at the top of the stage (there is no top strip).
  const mid = one.pt(10, 8);
  await run(page, [down(1, [mid[0] - 50, mid[1]]), down(2, [mid[0] + 50, mid[1]]), move(2, [mid[0] + 75, mid[1]], undefined, 20)]);
  await page.locator("#stage > #touch-scale").waitFor({ state: "visible" });
  assert.equal(await page.locator("#stage > #touch-scale").innerText(), "UI scale 125 %");
  assert.equal(await page.locator("#phone-top").count(), 0, "a tablet has no top strip");
  assert.equal(await page.locator("#touch-scale").evaluate((n) => getComputedStyle(n).position), "absolute");
  await run(page, [up(2, [mid[0] + 75, mid[1]]), up(1, [mid[0] - 50, mid[1]])]);
  await page.waitForFunction(() => document.getElementById("touch-scale") === null, undefined, { timeout: 2000 });
  await page.evaluate(() => localStorage.removeItem("hw.deviceScale"));
}

// ---- the desktop and the browser, unchanged --------------------------------------------------------------

async function desktopCase(page, url) {
  for (const os of ["macos", "web", "windows", "linux"]) {
    await open(page, url, { os, shown: 3, view: undefined });
    assert.equal(await page.locator('link[href$="touch.css"]').count(), 0, `${os}: touch.css is not loaded`);
    const g = await shown(page, 2);
    // A touch pointer is handled as it is today: a press begins a selection, a drag does not scroll.
    await run(page, [down(1, g.pt(3, 2)), wait(40)]);
    await frames(page);
    assert.deepEqual(await selects(page), [{ pane: 2, kind: "begin", x: 3, y: 2, mode: "cell" }], `${os}: a touch press begins a selection`);
    await run(page, [move(1, g.pt(8, 2)), up(1, g.pt(8, 2))]);
    await frames(page);
    assert.ok((await selects(page)).some((o) => o.kind === "extend"), `${os}: a drag extends it`);
    assert.equal((await callsOf(page, "wheel")).length, 0, `${os}: nothing scrolled`);
    assert.equal(await page.locator(".touch-layer, .touch-handle, .touch-menu, #touch-scale").count(), 0, `${os}: no touch element`);
    assert.equal(await rootPx(page), "12px", `${os}: the root size is the stylesheet's`);
    assert.equal(await fontPx(page), 14, `${os}: the font is the setting`);
    assert.equal(await stored(page), null, `${os}: nothing stored`);
  }
}
