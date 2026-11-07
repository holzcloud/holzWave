// The file hand-over on the page (docs/design.md section 6.6, rule File hand-over). An Android WebView, the system's file dialog and
// a real provider are not used: the platform is mocked (`app_info` answers `android`) and the two commands are played by the Tauri
// stub in the desktop engines. What a real device shows (the real document dialog, a drive or a USB stick that reports no size or a
// wrong one, a very large file on a phone) is not covered here.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { APP_DIR, DIST_DIR, callsOf } from "./lib.mjs";

// The hand-over belongs to the Android shell; the web backend never reports `android`.
export const backends = ["tauri"];
export const backendNote = "the file hand-over belongs to the Android shell, which the web backend never is";

const { STRINGS, messageFor } = await import(pathToFileURL(join(DIST_DIR, "handoverstate.js")).href);
const golden = JSON.parse(readFileSync(join(APP_DIR, "tests/fixtures/protocol.json"), "utf8"));
const spec = readFileSync(join(APP_DIR, "../docs/design.md"), "utf8");

export default async function ({ engine, page, url, errors }) {
  page.setDefaultTimeout(5000);
  for (const [name, run] of [
    ["entries", entriesCase],
    ["copy in", copyInCase],
    ["copy out", copyOutCase],
    ["hostile text", hostileCase],
    ["words", wordsCase],
    ["other platforms", otherPlatformsCase],
  ]) {
    try {
      await run(page, url);
    } catch (e) {
      // The case is named, and the line of the test that failed is kept, because the runner prints one line.
      const where = String(e?.stack ?? "").split("\n").find((l) => l.includes("handover.test.mjs"))?.trim() ?? "";
      throw new Error(`${name}: ${e?.message ?? e} ${where}`);
    }
  }
  assert.deepEqual(errors, [], `the page reported errors (${engine})`);
}

const VIEW = {
  rev: 1,
  workspaces: [{ name: "main", active: true, panes: 1, pane_ids: [1] }],
  tabs: [{ title: "shell", active: true, focus: 1, panes: 1, pane_ids: [1] }],
  focus: 1,
  panes: [[1, { x: 0, y: 0, w: 1, h: 1 }]],
  dividers: [],
  pane_names: [],
  windows: 1,
};

/** The answers of the host, as the page reads them. */
const DONE = { kind: "done", name: "report.pdf", size: 1288490, shown: "~/handover/report.pdf", renamed: false };
const TAKEN = { kind: "done", name: "report.pdf", size: 1288490, shown: "~/handover/report-2.pdf", renamed: true };
const OUT = { kind: "done", name: "report.pdf", size: 1288490, shown: "~/report.pdf", renamed: false };
const BIG = { kind: "too_large", name: "report.pdf", size: 612000000 };

/**
 * Open the page as a device would. `answers` are the host's answers to `handover_in` and `handover_out`, played in turn (the last one
 * stays); `hold` makes the first call of a command wait until `window.__release()` is called, so a second tap can be tried meanwhile.
 */
async function open(page, url, { os = "android", width = 390, height = 844, screen = [390, 844], hold = false } = {}) {
  await page.setViewportSize({ width, height });
  await page.addInitScript(([w, h]) => {
    for (const [key, value] of [["width", w], ["height", h], ["availWidth", w], ["availHeight", h]]) {
      Object.defineProperty(window.screen, key, { get: () => value, configurable: true });
    }
  }, screen);
  await page.addInitScript(
    ([info, shown, settings, holding]) => {
      const mock = window.__MOCK__;
      mock.on("app_info", () => info);
      mock.setView(shown);
      mock.on("get_settings", () => structuredClone(settings));
      // The vault is its own page module and is not under test here: a host with no vault shows nothing.
      mock.on("vault_status", () => ({ state: "none", protection: "plain", has_screen_lock: false, strongbox: false, can_protect: false, notice: "none", reason: null }));
      mock.on("vault_hidden", () => null);
      window.__ANSWERS = { handover_in: [], handover_out: [] };
      window.__WAIT = null;
      window.__release = () => window.__WAIT?.();
      for (const cmd of ["handover_in", "handover_out"]) {
        let first = true;
        mock.on(cmd, async () => {
          if (holding && first) {
            first = false;
            await new Promise((resolve) => {
              window.__WAIT = resolve;
            });
          }
          const queue = window.__ANSWERS[cmd];
          const next = queue.length > 1 ? queue.shift() : queue[0];
          if (next && next.__throw) throw next.__throw;
          return next === undefined ? null : structuredClone(next);
        });
      }
    },
    [{ version: "0.0.1-alpha1", os, hostname: "pixel" }, VIEW, golden.settings_view, hold],
  );
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 1);
  await page.waitForFunction(() => window.__MOCK__.callsOf("window_ready").length >= 1);
  await page.evaluate(async () => {
    window.__registry = await import(new URL("actions.js", location.href).href);
  });
  if (os === "android") await page.waitForFunction(() => window.__registry.listActions().some((a) => a.id === "files.copy_in"));
}

const answer = (page, cmd, ...list) => page.evaluate(([c, l]) => (window.__ANSWERS[c] = l), [cmd, list]);
const calls = (page, cmd) => callsOf(page, cmd);
const present = (page, selector) => page.evaluate((s) => document.querySelector(s) !== null, selector);
const lineText = (page) => page.evaluate(() => document.querySelector("#handover-top .handover-line .handover-text")?.textContent ?? null);
const run = (page, id) => page.evaluate((i) => window.__registry.runAction(i), id);

/** Open the phone's sheet and press the row `label`. */
async function pressRow(page, label) {
  await page.locator(".phone-title").click();
  await page.waitForSelector("#phone-sheet:not([hidden])");
  await page.locator("#phone-sheet .phone-row", { hasText: label }).click();
  await page.waitForSelector("#phone-sheet", { state: "hidden" });
}

async function waitLine(page, text) {
  try {
    await page.waitForFunction((t) => document.querySelector("#handover-top .handover-line .handover-text")?.textContent === t, text);
  } catch (e) {
    throw new Error(`the line is ${JSON.stringify(await lineText(page))}, not ${JSON.stringify(text)}`, { cause: e });
  }
}

async function entriesCase(page, url) {
  // The phone: two palette entries in the group Files, two rows of the sheet in a group Files, each a 44 px target.
  await open(page, url);
  assert.equal(await page.evaluate(() => [...document.querySelectorAll('link[rel="stylesheet"]')].some((l) => l.href.endsWith("handover.css"))), true);
  const titles = await page.evaluate(() => window.__registry.listAvailableActions().filter((a) => /^files\.copy_(in|out)$/.test(a.id)).map((a) => a.title));
  assert.deepEqual(titles, [STRINGS.copyIn, STRINGS.copyOut]);
  await run(page, "palette.open");
  await page.waitForSelector(".palette-backdrop.open");
  await page.locator(".palette-input").fill("copy a file");
  const rows = await page.locator(".palette-row").evaluateAll((all) => all.map((r) => [r.querySelector(".palette-title").textContent, r.querySelector(".palette-section").textContent]));
  assert.deepEqual(rows, [[STRINGS.copyIn, "files"], [STRINGS.copyOut, "files"]], "two entries, in the group Files (the group of the file viewer's entry)");
  await page.keyboard.press("Escape");

  await page.locator(".phone-title").click();
  await page.waitForSelector("#phone-sheet:not([hidden])");
  const caps = await page.locator("#phone-sheet .phone-cap").allTextContents();
  assert.ok(caps.includes(STRINGS.files), `the sheet has the caption Files (${caps.join(" | ")})`);
  const sheetRows = page.locator("#phone-sheet .phone-list[aria-label='Files'] .phone-row");
  assert.deepEqual(await sheetRows.allTextContents(), [STRINGS.copyIn, STRINGS.copyOut]);
  for (const box of await sheetRows.evaluateAll((all) => all.map((b) => b.getBoundingClientRect().toJSON()))) {
    assert.ok(box.width >= 44 && box.height >= 44, `a row of the sheet is ${box.width} by ${box.height}`);
  }
  // No folder, no tree, no mirror: the only controls are the two rows, and no file input of any kind exists.
  assert.equal(await page.locator("input[type=file], [webkitdirectory], [directory]").count(), 0);
  const words = await page.evaluate(() => document.body.innerText);
  for (const word of ["Mirror", "Browse", "All files", "Choose folder", "Sync folder"]) assert.ok(!words.includes(word), `the page says ${word}`);
  await page.keyboard.press("Escape");

  // A tablet has the palette entries and no sheet.
  await open(page, url, { width: 1000, height: 700, screen: [1000, 700] });
  assert.equal(await present(page, "#phone-sheet"), false, "no sheet on a tablet");
  assert.equal(await page.evaluate(() => window.__registry.listAvailableActions().filter((a) => /^files\.copy_(in|out)$/.test(a.id)).length), 2);
}

async function copyInCase(page, url) {
  await open(page, url);
  await answer(page, "handover_in", DONE);
  await pressRow(page, STRINGS.copyIn);
  await waitLine(page, "Copied report.pdf (1.2 MB) to ~/handover/report.pdf");
  assert.equal((await calls(page, "handover_in")).length, 1);
  assert.equal(await page.locator("#handover-top .handover-mark").count(), 0, "a plain line has no mark");
  const box = await page.locator("#handover-top .handover-line").evaluate((n) => n.getBoundingClientRect().toJSON());
  assert.ok(box.height >= 44, `the line is ${box.height} high`);

  // A name that was taken: the chosen name and the place with the number.
  await answer(page, "handover_in", TAKEN);
  await run(page, "files.copy_in");
  await waitLine(page, "Copied report.pdf (1.2 MB) to ~/handover/report-2.pdf");

  // Too large: the refusal, with a mark and an underline besides its colour.
  await answer(page, "handover_in", BIG);
  await run(page, "files.copy_in");
  await waitLine(page, "report.pdf is 612 MB; the limit is 256 MB");
  assert.equal(await page.locator("#handover-top .handover-mark").textContent(), "!");
  assert.equal(await page.locator("#handover-top .handover-line").evaluate((n) => getComputedStyle(n).textDecorationLine), "underline");
  await answer(page, "handover_in", { ...BIG, size: null });
  await run(page, "files.copy_in");
  await waitLine(page, "report.pdf is larger than 256 MB");

  // A dialog that is left says nothing, and the line before goes.
  await answer(page, "handover_in", { kind: "cancelled" });
  await run(page, "files.copy_in");
  await page.waitForFunction(() => window.__MOCK__.callsOf("handover_in").length === 5);
  await page.waitForTimeout(100);
  assert.equal(await lineText(page), null, "nothing is said for a cancelled dialog");

  // Any other failure, and an answer of a kind the page does not know, are the one plain line.
  for (const bad of [{ kind: "failed" }, { kind: "unavailable" }, { kind: "exploded", text: "/data/user/0/secret" }, "text", { __throw: "ipc closed" }]) {
    await answer(page, "handover_in", bad);
    await run(page, "files.copy_in");
    await waitLine(page, "Could not copy the file");
    assert.equal(await page.locator("#handover-top .handover-mark").textContent(), "!");
    await page.locator("#handover-top .handover-line").click(); // a tap takes the line away
    await page.waitForFunction(() => document.querySelector("#handover-top .handover-line") === null);
  }

  // A second tap while the first call is pending sends no second call.
  await open(page, url, { hold: true });
  await answer(page, "handover_in", DONE);
  await run(page, "files.copy_in");
  await page.waitForFunction(() => window.__MOCK__.callsOf("handover_in").length === 1);
  await run(page, "files.copy_in");
  await run(page, "files.copy_in");
  await page.waitForTimeout(100);
  assert.equal((await calls(page, "handover_in")).length, 1, "taps while the dialog is open send nothing");
  await page.evaluate(() => window.__release());
  await waitLine(page, "Copied report.pdf (1.2 MB) to ~/handover/report.pdf");
  await run(page, "files.copy_in");
  await page.waitForFunction(() => window.__MOCK__.callsOf("handover_in").length === 2);
}

async function copyOutCase(page, url) {
  await open(page, url);
  await answer(page, "handover_out", OUT);
  await pressRow(page, STRINGS.copyOut);
  // The question has the spec's label, as its placeholder and its name, in a box of the palette's shape.
  await page.waitForSelector(".handover-ask");
  const input = page.locator(".handover-ask input");
  assert.equal(await input.getAttribute("placeholder"), "File to copy out, from the shell folder");
  assert.equal(await input.getAttribute("aria-label"), "File to copy out, from the shell folder");
  assert.equal(await page.evaluate(() => document.activeElement?.className.includes("handover-input")), true, "the field has the keyboard");
  assert.equal((await calls(page, "handover_out")).length, 0, "nothing is sent before an answer");
  const field = await input.evaluate((n) => n.getBoundingClientRect().toJSON());
  assert.ok(field.height >= 44, `the field is ${field.height} high`);

  // An empty answer, and one of spaces, send nothing and the box stays.
  await page.keyboard.press("Enter");
  await input.fill("   ");
  await page.keyboard.press("Enter");
  assert.equal((await calls(page, "handover_out")).length, 0);
  assert.equal(await page.locator(".handover-ask").count(), 1, "the box stays");

  // The typed text is sent exactly as it was typed.
  await input.fill("notes/report.pdf");
  await page.keyboard.press("Enter");
  await waitLine(page, "Copied report.pdf (1.2 MB) out");
  assert.deepEqual(await calls(page, "handover_out"), [{ path: "notes/report.pdf" }]);
  assert.equal(await page.locator(".handover-ask").count(), 0, "the box is gone");

  // Escape leaves without a call; so does a tap outside.
  await run(page, "files.copy_out");
  await page.waitForSelector(".handover-ask");
  await page.locator(".handover-ask input").fill("report.pdf");
  await page.keyboard.press("Escape");
  assert.equal(await page.locator(".handover-ask").count(), 0);
  await run(page, "files.copy_out");
  await page.waitForSelector(".handover-ask");
  await page.mouse.click(5, 800);
  assert.equal(await page.locator(".handover-ask").count(), 0);
  assert.equal((await calls(page, "handover_out")).length, 1, "Escape and a tap outside call nothing");

  // A second tap while the box is open opens no second box; the typed text with spaces at its ends is sent as it is.
  await run(page, "files.copy_out");
  await run(page, "files.copy_out");
  await page.waitForSelector(".handover-ask");
  assert.equal(await page.locator(".handover-ask").count(), 1);
  await page.locator(".handover-ask input").fill(" a b.txt ");
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("handover_out").length === 2);
  assert.deepEqual((await calls(page, "handover_out"))[1], { path: " a b.txt " });

  // The refusals and the failure of a copy out say what a copy in says.
  await answer(page, "handover_out", BIG);
  await run(page, "files.copy_out");
  await page.waitForSelector(".handover-ask");
  await page.locator(".handover-ask input").fill("big.iso");
  await page.keyboard.press("Enter");
  await waitLine(page, "report.pdf is 612 MB; the limit is 256 MB");
  await answer(page, "handover_out", { kind: "failed" });
  await run(page, "files.copy_out");
  await page.waitForSelector(".handover-ask");
  await page.locator(".handover-ask input").fill("../x");
  await page.keyboard.press("Enter");
  await waitLine(page, "Could not copy the file");
  await answer(page, "handover_out", { kind: "cancelled" });
  await run(page, "files.copy_out");
  await page.waitForSelector(".handover-ask");
  await page.locator(".handover-ask input").fill("x");
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("handover_out").length === 5);
  await page.waitForTimeout(100);
  assert.equal(await lineText(page), null, "a copy that starts clears the line of the one before, and a left dialog says nothing");
}

async function hostileCase(page, url) {
  const hostile = '<img src=x onerror="window.__pwned=1">';
  await open(page, url);
  for (const outcome of [
    { kind: "done", name: hostile, size: 10, shown: `~/handover/${hostile}`, renamed: false },
    { kind: "too_large", name: hostile, size: 612000000 },
  ]) {
    await answer(page, "handover_in", outcome);
    await run(page, "files.copy_in");
    await page.waitForSelector("#handover-top .handover-line");
    assert.equal(await page.locator("#handover-top img, #handover-top *[onerror]").count(), 0, "a name never becomes an element");
    assert.ok((await lineText(page)).includes(hostile), "it is shown as text");
    assert.equal(await page.evaluate(() => window.__pwned), undefined);
    await page.locator("#handover-top .handover-line").click();
  }
  // A name of 10 000 characters is cut to 255.
  const long = "x".repeat(10000);
  await answer(page, "handover_in", { kind: "done", name: long, size: 1, shown: `~/handover/${long}`, renamed: false });
  await run(page, "files.copy_in");
  await page.waitForSelector("#handover-top .handover-line");
  const shown = await lineText(page);
  assert.equal(shown.split("x").length - 1, 255 + 512 - "~/handover/".length, "the name is cut to 255 and the place to 512 characters");
  assert.ok(shown.length < 1200, `the line is ${shown.length} characters`);
  assert.equal(await page.evaluate(() => window.__pwned), undefined);
  assert.equal(await page.locator("img").count(), 0);
}

async function wordsCase(page, url) {
  // The rendered text equals the spec's: read from the spec's rule and from the page's own table.
  const start = spec.indexOf("**File hand-over.**");
  const rule = spec.slice(start, spec.indexOf("**Notification.**"));
  await open(page, url);
  const seen = [];
  for (const [id, cmd, outcome] of [
    ["files.copy_in", "handover_in", DONE],
    ["files.copy_in", "handover_in", TAKEN],
    ["files.copy_in", "handover_in", BIG],
    ["files.copy_in", "handover_in", { kind: "failed" }],
  ]) {
    await answer(page, cmd, outcome);
    const before = (await calls(page, cmd)).length;
    await run(page, id);
    await page.waitForFunction(([c, n]) => window.__MOCK__.callsOf(c).length === n + 1, [cmd, before]);
    await page.waitForFunction(() => document.querySelector("#handover-top .handover-line") !== null);
    await page.waitForTimeout(50);
    const text = await lineText(page);
    seen.push(text);
    assert.ok(rule.includes(`\`${text}\``), `the spec does not have \`${text}\``);
    await page.locator("#handover-top .handover-line").click();
  }
  assert.deepEqual(seen, [
    "Copied report.pdf (1.2 MB) to ~/handover/report.pdf",
    "Copied report.pdf (1.2 MB) to ~/handover/report-2.pdf",
    "report.pdf is 612 MB; the limit is 256 MB",
    "Could not copy the file",
  ]);
  assert.deepEqual(seen.slice(0, 3), [DONE, TAKEN, BIG].map((o) => messageFor(o, "in").text));
  assert.equal(messageFor(OUT, "out").text, "Copied report.pdf (1.2 MB) out");
  assert.ok(rule.includes("`Copied report.pdf (1.2 MB) out`"));
  for (const label of [STRINGS.copyIn, STRINGS.copyOut, STRINGS.askPath]) assert.ok(rule.includes(`\`${label}\``), label);
}

async function otherPlatformsCase(page, url) {
  for (const os of ["macos", "web", "windows", "linux"]) {
    await open(page, url, { os });
    await page.waitForTimeout(100);
    assert.equal(await page.evaluate(() => window.__registry.listActions().some((a) => /^files\.copy_(in|out)$/.test(a.id))), false, `${os}: no action`);
    assert.equal(await page.evaluate(() => window.__registry.listAvailableActions().some((a) => /shell folder/.test(a.title))), false, `${os}: no entry`);
    assert.equal(await present(page, "#phone-sheet"), false, `${os}: no sheet, so no row`);
    assert.equal(await present(page, "#handover-top"), false, `${os}: no place for a line`);
    assert.equal(
      await page.evaluate(() => [...document.querySelectorAll('link[rel="stylesheet"]')].some((l) => l.href.endsWith("handover.css"))),
      false,
      `${os}: handover.css is not loaded`,
    );
    assert.equal(
      await page.evaluate(() => performance.getEntriesByType("resource").some((r) => /\/handover(state)?\.(js|css)$/.test(r.name))),
      false,
      `${os}: the hand-over files are not fetched`,
    );
    assert.equal((await calls(page, "handover_in")).length + (await calls(page, "handover_out")).length, 0, `${os}: no call`);
  }
  // A phone-sized window of another platform still has none of it.
  await open(page, url, { os: "linux", width: 390, height: 844 });
  assert.equal(await present(page, "#phone-sheet"), false);
}
