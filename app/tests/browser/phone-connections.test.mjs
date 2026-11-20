// The way to the Connections view on a phone (docs/design.md 6.6, the groups of the sheet, and 6.34, "Phone and tablet"): a phone
// with a touchscreen only has no palette, so the sheet has a group `Connections` with one row `Connections…` that opens the view.
// The platform is mocked (`app_info` answers `android`) and the window is sized like a phone in the desktop engines; nothing here
// was run on a real phone, with a real on-screen keyboard or against a real server.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { APP_DIR } from "./lib.mjs";
import { open as openDevice } from "./touch-lib.mjs";

// The phone layout is the Android shell's; the web backend never reports `android`.
export const backends = ["tauri"];
export const backendNote = "the phone layout belongs to the Android shell, which the web backend never is";

const spec = readFileSync(join(APP_DIR, "../docs/design.md"), "utf8");

export default async function ({ engine, page, url, errors }) {
  page.setDefaultTimeout(5000);
  for (const [name, run] of [
    ["the row", rowCase],
    ["a tap opens the view", tapCase],
    ["the order of the groups", orderCase],
    ["no row elsewhere", elsewhereCase],
    ["words", wordsCase],
    ["cues", cuesCase],
  ]) {
    try {
      await run(page, url, engine);
    } catch (e) {
      // The case is named, and the line of the test that failed is kept, because the runner prints one line.
      const where = String(e?.stack ?? "").split("\n").find((l) => l.includes("phone-connections.test.mjs"))?.trim() ?? "";
      throw new Error(`${name}: ${e?.message ?? e} ${where}`);
    }
  }
  assert.deepEqual(errors, [], `the page reported errors (${engine})`);
}

/** Open the page as a phone (or as `os` at another size) and wait until the actions that add rows to the sheet exist. */
async function open(page, url, opts = {}) {
  const os = opts.os ?? "android";
  await openDevice(page, url, opts);
  await page.evaluate(async () => {
    window.__registry = await import(new URL("actions.js", location.href).href);
    window.__conns = await import(new URL("connections.js", location.href).href);
  });
  await page.waitForFunction(() => window.__registry.listActions().some((a) => a.id === "connections.open"));
  if (os === "android") await page.waitForFunction(() => window.__registry.listActions().some((a) => a.id === "files.copy_in"));
}

/** The words the page keeps beside the action: the caption of the group and the title of the row and of the palette entry. */
const words = (page) => page.evaluate(() => ({ group: window.__conns.CONNECTIONS_GROUP, title: window.__conns.CONNECTIONS_TITLE }));

async function openSheet(page) {
  await page.locator("#phone-top .phone-title").click();
  await page.waitForSelector("#phone-sheet:not([hidden])");
}

const captions = (page) => page.locator("#phone-sheet .phone-cap").allTextContents();
const row = (page, title) => page.locator("#phone-sheet .phone-list[aria-label='Connections'] .phone-row", { hasText: title });

async function rowCase(page, url, engine) {
  await open(page, url);
  const w = await words(page);
  assert.deepEqual(w, { group: "Connections", title: "Connections…" });
  await openSheet(page);
  const caps = await captions(page);
  assert.ok(caps.includes(w.group), `the sheet has the caption Connections (${caps.join(" | ")})`);
  const rows = page.locator("#phone-sheet .phone-list[aria-label='Connections'] .phone-row");
  assert.deepEqual(await rows.allTextContents(), [w.title], "one row in the group");
  assert.equal(await rows.evaluate((n) => n.tagName), "BUTTON", "the row is a real button");
  assert.equal(await rows.getAttribute("type"), "button");
  const box = await rows.evaluate((n) => {
    const r = n.getBoundingClientRect();
    const name = n.querySelector(".phone-row-name");
    return { w: r.width, h: r.height, fits: name.scrollWidth <= name.clientWidth };
  });
  assert.ok(box.h >= 44 && box.w >= 44, `the row is ${box.w} by ${box.h}`);
  assert.ok(box.fits, "the row is wide enough for its word");
  // The caption has the label style of the other captions: the class, upper case and the tracking.
  const cap = page.locator("#phone-sheet .phone-cap", { hasText: w.group });
  assert.equal(await cap.evaluate((n) => n.classList.contains("label")), true, "the caption has the label class");
  const style = await cap.evaluate((n) => ({ transform: getComputedStyle(n).textTransform, spacing: getComputedStyle(n).letterSpacing }));
  assert.equal(style.transform, "uppercase");
  assert.notEqual(style.spacing, "normal", "the caption is tracked");

  // The keyboard reaches the row in the order of the sheet on the screen, and the row shows the 2 px ring.
  const option = engine === "webkit" && process.platform === "darwin"; // WebKit on macOS visits buttons with Option+Tab only
  const seen = [];
  for (let i = 0; i < 20; i++) {
    const at = await page.evaluate(() => {
      const n = document.activeElement;
      return { text: n?.textContent ?? "", y: n?.getBoundingClientRect().top ?? -1, inSheet: !!n?.closest("#phone-sheet") };
    });
    assert.ok(at.inSheet, "the focus stays inside the sheet");
    seen.push(at);
    if (at.text === w.title) break;
    await page.keyboard.press(option ? "Alt+Tab" : "Tab");
  }
  assert.equal(seen.at(-1).text, w.title, `Tab reaches the row (${seen.map((s) => s.text).join(" | ")})`);
  for (let i = 1; i < seen.length; i++) assert.ok(seen[i].y >= seen[i - 1].y - 1, `Tab goes down the sheet (${seen.map((s) => `${s.text}@${s.y}`).join(" | ")})`);
  const ring = await row(page, w.title).evaluate((n) => ({ width: getComputedStyle(n).outlineWidth, style: getComputedStyle(n).outlineStyle }));
  assert.deepEqual(ring, { width: "2px", style: "solid" }, "the row has the 2 px focus ring");
  // Enter on the row opens the view as a tap does.
  await page.keyboard.press("Enter");
  await page.waitForSelector("#phone-sheet", { state: "hidden" });
  await page.waitForSelector(".dlg.conns");
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.conns", { state: "detached" });
}

async function tapCase(page, url) {
  await open(page, url);
  const w = await words(page);
  await openSheet(page);
  await row(page, w.title).click();
  await page.waitForSelector("#phone-sheet", { state: "hidden" });
  // The view is the full-screen sheet of 6.34, with the server row first.
  await page.waitForSelector(".dlg.conns");
  await page.waitForSelector(".conns .cx-srv");
  assert.equal(await page.evaluate(() => document.activeElement?.classList.contains("phone-title")), false, "the opener gets nothing back");
  const view = await page.locator(".dlg.conns").evaluate((n) => n.getBoundingClientRect().toJSON());
  assert.ok(Math.abs(view.width - 390) <= 1 && view.left <= 1, `the view is the full width of the phone: ${JSON.stringify(view)}`);
  const server = await page.locator(".conns .cx-srv").evaluate((n) => n.getBoundingClientRect().height);
  assert.ok(server >= 44, `the server row is ${server} high`);
  assert.equal(await page.locator(".conns .cx-srv-state").textContent(), "not signed in");
  assert.ok((await page.locator(".conns .cx-srv-acts button").allTextContents()).includes("Sign in…"), "the row offers Sign in…");
  assert.equal(await page.locator(".dlg.conns").count(), 1, "one view");
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.conns", { state: "detached" });
}

/** The captions of the groups in the sheet, from the first added group on. */
async function addedGroups(page) {
  const caps = await captions(page);
  return caps.slice(caps.findIndex((c) => c.startsWith("Panes")) + 1);
}

async function orderCase(page, url) {
  // The file hand-over is loaded on its own after the start: make it add its rows first, before the Connections row exists.
  await page.addInitScript(() => {
    const mock = window.__MOCK__;
    mock.on("get_settings", async () => {
      const registry = await import(new URL("actions.js", location.href).href);
      while (!registry.listActions().some((a) => a.id === "files.copy_in")) await new Promise((done) => setTimeout(done, 10));
      window.__filesFirst = !registry.listActions().some((a) => a.id === "connections.open");
      return null;
    });
  });
  await open(page, url);
  assert.equal(await page.evaluate(() => window.__filesFirst), true, "the Files rows were added before the Connections row");
  await openSheet(page);
  assert.deepEqual((await captions(page)).slice(0, 3).map((c) => c.split(" ")[0]), ["Workspaces", "Tabs", "Panes"]);
  assert.deepEqual(await addedGroups(page), ["Connections", "Files"], "Connections comes before Files");
  assert.equal(await page.locator("#phone-sheet .phone-list[aria-label='Files'] .phone-row").count(), 2, "Files keeps its two rows");
  await page.keyboard.press("Escape");

  // The other way round: the hand-over is fetched only after the Connections row exists.
  const held = async (route) => {
    await page.waitForFunction(() => window.__registry?.listActions().some((a) => a.id === "connections.open"), null, { polling: 20 }).catch(() => {});
    await route.continue();
  };
  await page.route("**/handover.js", held);
  await page.addInitScript(() => window.__MOCK__.on("get_settings", null));
  await page.addInitScript(() => {
    void import(new URL("actions.js", location.href).href).then((m) => (window.__registry = m));
  });
  await open(page, url);
  await page.unroute("**/handover.js", held);
  await openSheet(page);
  assert.deepEqual(await addedGroups(page), ["Connections", "Files"], "Connections comes before Files when it was added first");

  // A group the order does not name comes after the named ones.
  await page.evaluate(async () => {
    const phone = await import(new URL("phone.js", location.href).href);
    phone.addSheetRow("Later", "A later row", () => {});
  });
  assert.deepEqual(await addedGroups(page), ["Connections", "Files", "Later"]);
  await page.keyboard.press("Escape");

  // The sheet made again after a change of the class holds each group once.
  await open(page, url, { screen: [1000, 700] });
  await page.setViewportSize({ width: 1000, height: 700 });
  await page.waitForFunction(() => document.documentElement.classList.contains("layout-tablet"));
  assert.equal(await page.locator("#phone-sheet").count(), 0, "no sheet on a tablet");
  await page.setViewportSize({ width: 390, height: 844 });
  await page.waitForFunction(() => document.documentElement.classList.contains("layout-phone"));
  await openSheet(page);
  assert.deepEqual(await addedGroups(page), ["Connections", "Files"], "each group once after the sheet was made again");
  assert.equal(await page.locator("#phone-sheet .phone-list[aria-label='Connections'] .phone-row").count(), 1);
  await page.keyboard.press("Escape");
}

async function elsewhereCase(page, url) {
  const entry = () => page.evaluate(() => window.__registry.listAvailableActions().find((a) => a.id === "connections.open")?.title ?? null);
  // A tablet has the palette and no sheet; the entry is the same.
  await open(page, url, { width: 1000, height: 700, screen: [1000, 700], shown: 3 });
  assert.equal(await page.evaluate(() => document.documentElement.classList.contains("layout-tablet")), true);
  assert.equal(await page.locator("#phone-sheet").count(), 0, "tablet: no sheet, so no row");
  assert.equal(await page.getByText("Connections…", { exact: true }).count(), 0, "tablet: no row anywhere");
  assert.equal(await entry(), "Connections…", "tablet: the palette entry is unchanged");
  // A desktop and a browser window, even one as narrow as a phone, draw no row.
  for (const os of ["macos", "web"]) {
    await open(page, url, { os, shown: 3 });
    assert.equal(await page.locator("#phone-sheet").count(), 0, `${os}: no sheet`);
    assert.equal(await page.getByText("Connections…", { exact: true }).count(), 0, `${os}: no row`);
    assert.equal(await entry(), "Connections…", `${os}: the palette entry is unchanged`);
  }
  // The phone keeps the palette entry too, for a hardware keyboard.
  await open(page, url);
  assert.equal(await entry(), "Connections…", "phone: the palette entry is unchanged");
}

async function wordsCase(page, url) {
  // The caption and the row read as the table "The words" of 6.34 says, and 6.6 names the group with the same words.
  const section = spec.slice(spec.indexOf("### 6.34 Server connection and sessions"), spec.indexOf("### 6.35 Devices and signing in to a server"));
  const table = section.slice(section.indexOf("**The words.**"));
  const line = table.split("\n").find((l) => l.includes("`Connections…`"));
  assert.ok(line, "the table of words of 6.34 has Connections…");
  await open(page, url);
  const w = await words(page);
  assert.ok(line.includes(`\`${w.group}\``) && line.includes(`\`${w.title}\``), `the line names both words: ${line}`);
  assert.match(line, /sheet \(6\.6\)/, "the line says where: the phone's sheet");
  const mobile = spec.slice(spec.indexOf("### 6.6 Mobile"), spec.indexOf("### 6.7 Platforms"));
  assert.ok(mobile.includes(`\`${w.group}\``) && mobile.includes(`\`${w.title}\``), "6.6 names the group and the row");
  await openSheet(page);
  assert.equal(await page.locator("#phone-sheet .phone-cap", { hasText: w.group }).textContent(), w.group);
  assert.equal(await row(page, w.title).textContent(), w.title);
  await page.keyboard.press("Escape");
}

async function cuesCase(page, url, engine) {
  await open(page, url);
  const w = await words(page);
  await openSheet(page);
  const look = () =>
    row(page, w.title).evaluate((n) => {
      const s = getComputedStyle(n);
      const bg = getComputedStyle(document.getElementById("phone-sheet")).backgroundColor;
      return { text: n.textContent, color: s.color, bg, border: s.borderLeftStyle, borderWidth: s.borderLeftWidth };
    });
  const plain = await look();
  assert.equal(plain.border, "solid", "the row has its left border, like the other rows");
  // contrast-high: the row keeps its words and a text colour apart from the sheet.
  await page.evaluate(() => document.documentElement.classList.add("contrast-high"));
  const high = await look();
  assert.equal(high.text, w.title);
  assert.notEqual(high.color, high.bg, "the text is not the colour of the sheet");
  assert.equal(high.border, "solid");
  await page.evaluate(() => document.documentElement.classList.remove("contrast-high"));
  await page.keyboard.press("Escape");

  // motion-reduce: the sheet does not slide, and the row still opens the view.
  await page.emulateMedia({ reducedMotion: "reduce" });
  await open(page, url);
  await openSheet(page);
  assert.equal(await page.evaluate(() => getComputedStyle(document.getElementById("phone-sheet")).animationName), "none");
  await row(page, w.title).click();
  await page.waitForSelector(".dlg.conns");
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.conns", { state: "detached" });
  await page.emulateMedia({ reducedMotion: "no-preference" });

  if (engine !== "chromium") {
    console.log(`NOT RUN phone-connections forced colours [${engine}]: this engine cannot emulate forced colours`);
    return;
  }
  // forced-colors: the row keeps its border and its words in system colours.
  await page.emulateMedia({ forcedColors: "active" });
  await open(page, url);
  await openSheet(page);
  const forced = await look();
  assert.equal(await page.evaluate(() => matchMedia("(forced-colors: active)").matches), true, "forced colours are emulated");
  assert.equal(forced.text, w.title);
  assert.equal(forced.border, "solid", `the row keeps its border: ${JSON.stringify(forced)}`);
  assert.notEqual(forced.color, forced.bg);
  await page.keyboard.press("Escape");
  await page.emulateMedia({ forcedColors: "none" });
}
