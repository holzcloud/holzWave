// The Local Network notice and the group of the Settings dialog (docs/design.md section 6.24), against the Tauri stub whose hook
// `__MOCK__.localNetwork` plays the shell. The sentences below are the design record's: the page's tables are held to them here and
// in the unit test. Nothing here can see the system question itself (it needs the app bundle started from Finder): that is the
// manual rows of the test plan.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { APP_DIR, callsOf } from "./lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000); // a dialog that never appears fails here, not at the runner's 30 s limit
  await noticeCase(page, url);
  await decideCase(page, url);
  await failedAllowCase(page, url);
  await notShownCase(page, url);
  await groupCase(page, url);
  await buttonsCase(page, url);
  await focusRefreshCase(page, url);
  await noGroupCase(page, url);
  await accessibilityCase(page, url);
  await sourceCase();
  assert.deepEqual(errors, [], "the page reported errors");
}

const golden = JSON.parse(readFileSync(join(APP_DIR, "tests/fixtures/protocol.json"), "utf8"));

const TITLE = "Allow local network access?";
const LEAD = "holzWave reaches computers on your local network:";
const ITEMS = [
  "SSH to machines on your network.",
  "Local AI servers on other computers, such as Ollama or LM Studio.",
  "Sync with a Nextcloud on your network.",
];
const NOTE =
  "macOS asks for your permission next. Programs you run in a pane, such as ssh, count as holzWave. You can change this any time in Settings.";

/** The state words of the design record: the word, the tone, the advice, the buttons after `Open System Settings`. */
const STATES = {
  allowed: { word: "allowed", tone: "ok", advice: "holzWave can reach computers on your local network.", extra: [] },
  denied: {
    word: "denied", tone: "warn",
    advice: "macOS blocks holzWave from your local network. Turn holzWave on in System Settings, then choose Check again.",
    extra: ["Check again"],
  },
  not_asked: {
    word: "not asked yet", tone: "plain", advice: "macOS has not been asked. Choose Allow and macOS shows its own question.",
    extra: ["Allow"],
  },
  unknown: {
    word: "not known", tone: "plain",
    advice: "holzWave could not tell. macOS may still be waiting for your answer. Choose Check again.", extra: ["Check again"],
  },
};
const ASKED = "Asked. Answer the macOS question, then choose Check again.";

/**
 * Open the page against the stub. Every case registers a complete init script (the platform, the whole view, every knob):
 * an earlier case's script stays and runs first, so a knob this one does not set would leak from the earlier one.
 */
async function open(page, url, { os = "macos", view = {}, afterAllow = "unknown", hold = false, query = "" } = {}) {
  const full = { supported: true, notice_due: false, state: "not_asked", ...view };
  await page.addInitScript(
    ([o, v, after, held, settings]) => {
      window.__MOCK__.on("app_info", () => ({ version: "0.0.1-alpha1", os: o, hostname: "woodbox" }));
      window.__MOCK__.on("get_settings", () => structuredClone(settings));
      const net = window.__MOCK__.localNetwork;
      net.set(v);
      net.afterAllow = after;
      net.openFails(null);
      net.answerFails(null);
      if (held) net.hold();
      else net.release();
    },
    [os, full, afterAllow, hold, golden.settings_view],
  );
  await page.goto(`${url}index.html?renderer=canvas${query}`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("window_ready").length >= 1);
}

const DUE = { notice_due: true, state: "not_asked" };
const notice = (page) => page.locator(".dlg.localnet");
const netCalls = (page, cmd) => callsOf(page, cmd);
const settle = (page) =>
  page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(() => setTimeout(done, 60)))));
const activeMatches = (page, selector) => page.evaluate((s) => document.activeElement?.matches(s) ?? false, selector);
const waitCalls = (page, cmd, n) =>
  page.waitForFunction(([c, count]) => window.__MOCK__.callsOf(c).length >= count, [cmd, n]);

/** The notice opens once at the first start with the sentences of the record, `Allow` first. */
async function noticeCase(page, url) {
  await open(page, url, { view: DUE });
  await notice(page).waitFor();
  assert.equal(await notice(page).count(), 1);
  const d = await page.evaluate(() => {
    const box = document.querySelector(".dlg.localnet");
    const label = box.getAttribute("aria-labelledby");
    return {
      role: box.getAttribute("role"),
      modal: box.getAttribute("aria-modal"),
      title: document.getElementById(label)?.textContent ?? null,
      lead: box.querySelector(".localnet-lead")?.textContent ?? null,
      items: [...box.querySelectorAll(".localnet-list li")].map((n) => n.textContent),
      note: box.querySelector(".localnet-note")?.textContent ?? null,
      status: box.querySelector(".localnet-status")?.textContent ?? null,
      statusRole: box.querySelector(".localnet-status")?.getAttribute("role") ?? null,
      buttons: [...box.querySelectorAll("button")].map((b) => b.textContent),
      radius: getComputedStyle(box).borderRadius,
      width: box.getBoundingClientRect().width,
      styles: box.querySelectorAll("[style]").length + (box.hasAttribute("style") ? 1 : 0),
    };
  });
  assert.deepEqual(
    { ...d, width: undefined, radius: undefined },
    {
      role: "dialog", modal: "true", title: TITLE, lead: LEAD, items: ITEMS, note: NOTE, status: "", statusRole: "status",
      buttons: ["Not now", "Allow"], radius: undefined, width: undefined, styles: 0,
    },
  );
  assert.equal(d.radius, "8px", "a floating layer has the 8 px radius of the design");
  assert.ok(d.width <= 30 * 12 + 1, `the box is 30 rem wide at the most: ${d.width}`);
  assert.equal(await activeMatches(page, ".localnet-allow"), true, "Allow has the focus first");
  assert.equal((await netCalls(page, "local_network_status")).length, 1, "the state was asked once, at start");
  assert.deepEqual(await netCalls(page, "local_network_answer"), [], "nothing is answered until a person decides");

  // Tab moves between the two buttons and stays in the dialog.
  await page.keyboard.press("Tab");
  assert.equal(await activeMatches(page, ".localnet-notnow"), true);
  await page.keyboard.press("Tab");
  assert.equal(await activeMatches(page, ".localnet-allow"), true);
  await page.keyboard.press("Shift+Tab");
  assert.equal(await activeMatches(page, ".localnet-notnow"), true);
  await page.keyboard.press("Shift+Tab");
  assert.equal(await activeMatches(page, ".localnet-allow"), true);

  // The keyboard belongs to the notice: no key reaches the terminal and no shortcut runs.
  await page.keyboard.press("a");
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("Meta+d");
  assert.deepEqual(await callsOf(page, "key"), [], "keys do not reach the terminal while the notice is open");
  assert.deepEqual(await callsOf(page, "text"), []);
  assert.deepEqual(await callsOf(page, "split"), [], "nor do shortcuts");
  assert.equal(await notice(page).count(), 1);

  // Enter on Allow: one answer with allow true, the notice goes, the terminal input has the keyboard again.
  await page.keyboard.press("Enter");
  await notice(page).waitFor({ state: "detached" });
  assert.deepEqual(await netCalls(page, "local_network_answer"), [{ allow: true }]);
  await page.waitForFunction(() => document.activeElement?.id === "ime");
}

/** `Not now`, Escape and a click outside each send `allow: false` once and close; Space and Enter press the focused button. */
async function decideCase(page, url) {
  const ways = {
    "a click on Not now": (p) => p.locator(".localnet-notnow").click(),
    "Escape": (p) => p.keyboard.press("Escape"),
    "a click outside": (p) => p.mouse.click(4, 4),
    "Enter on Not now": async (p) => {
      await p.keyboard.press("Tab");
      await p.keyboard.press("Enter");
    },
    "Space on Not now": async (p) => {
      await p.keyboard.press("Tab");
      await p.keyboard.press(" ");
    },
  };
  for (const [name, decline] of Object.entries(ways)) {
    await open(page, url, { view: DUE });
    await notice(page).waitFor();
    await decline(page);
    await notice(page).waitFor({ state: "detached" });
    assert.deepEqual(await netCalls(page, "local_network_answer"), [{ allow: false }], `${name}: one decline`);
    await page.waitForFunction(() => document.activeElement?.id === "ime");
    await settle(page);
    assert.equal(await notice(page).count(), 0, `${name}: nothing asks again by itself`);
    assert.deepEqual(await netCalls(page, "local_network_answer"), [{ allow: false }], `${name}: still one`);
  }

  // A click on the box itself is not outside.
  await open(page, url, { view: DUE });
  await notice(page).waitFor();
  await page.locator(".localnet-lead").click();
  await settle(page);
  assert.equal(await notice(page).count(), 1);
  assert.deepEqual(await netCalls(page, "local_network_answer"), []);

  // The mouse on Allow does what Enter does.
  await page.locator(".localnet-allow").click();
  await notice(page).waitFor({ state: "detached" });
  assert.deepEqual(await netCalls(page, "local_network_answer"), [{ allow: true }]);
}

/** A refused Allow keeps the notice open and says the core's reason; the next Allow can still work. */
async function failedAllowCase(page, url) {
  await open(page, url, { view: DUE });
  await notice(page).waitFor();
  await page.evaluate(() => window.__MOCK__.localNetwork.answerFails("cannot save local_network.json: no space left"));
  await page.locator(".localnet-allow").click();
  await waitCalls(page, "local_network_answer", 1);
  await page.waitForFunction(() => document.querySelector(".localnet-status")?.textContent !== "");
  assert.equal(await notice(page).count(), 1, "the notice stays");
  assert.equal(await page.locator(".localnet-status").textContent(), "cannot save local_network.json: no space left");
  assert.equal(await activeMatches(page, ".localnet-allow"), true, "the keyboard stays in the notice");
  await page.evaluate(() => window.__MOCK__.localNetwork.answerFails(null));
  await page.locator(".localnet-allow").click();
  await notice(page).waitFor({ state: "detached" });
  assert.deepEqual(await netCalls(page, "local_network_answer"), [{ allow: true }, { allow: true }]);

  // A decline closes whatever the answer says.
  await open(page, url, { view: DUE });
  await notice(page).waitFor();
  await page.evaluate(() => window.__MOCK__.localNetwork.answerFails("cannot save local_network.json: no space left"));
  await page.keyboard.press("Escape");
  await notice(page).waitFor({ state: "detached" });
  assert.deepEqual(await netCalls(page, "local_network_answer"), [{ allow: false }]);
}

/** No notice on Windows and Linux, with `notice_due` false, with `supported` false, in another window. */
async function notShownCase(page, url) {
  for (const os of ["windows", "linux", ""]) {
    await open(page, url, { os, view: DUE });
    await settle(page);
    assert.equal(await notice(page).count(), 0, `${JSON.stringify(os)}: no notice`);
    for (const cmd of ["local_network_status", "local_network_answer", "local_network_open_settings"]) {
      assert.deepEqual(await netCalls(page, cmd), [], `${JSON.stringify(os)}: the page does not even call ${cmd}`);
    }
  }
  await open(page, url, { view: { notice_due: false } });
  await settle(page);
  assert.equal(await notice(page).count(), 0, "notice_due false");
  await open(page, url, { view: { supported: false, notice_due: true } });
  await settle(page);
  assert.equal(await notice(page).count(), 0, "supported false");
  assert.deepEqual(await netCalls(page, "local_network_answer"), []);
  // A window other than the first never shows it, and does not ask.
  await open(page, url, { view: DUE, query: "&window=window-2" });
  await settle(page);
  assert.equal(await page.evaluate(() => window.__MOCK__.label), "window-2");
  assert.equal(await notice(page).count(), 0, "a torn-off window never shows the notice");
  assert.deepEqual(await netCalls(page, "local_network_status"), []);
}

const settings = (page) => page.getByRole("dialog", { name: "Settings" });
const group = (page) => page.locator("fieldset.set-net");
const row = (page) => page.locator(".set-net-row");
const text = (page, selector) => page.locator(selector).first().textContent();

async function openSettings(page) {
  await page.keyboard.press("Meta+,");
  await settings(page).waitFor();
}

async function closeSettings(page) {
  await page.keyboard.press("Escape");
  await settings(page).waitFor({ state: "detached" });
}

/** Open the Settings dialog on a Mac whose shell reports `state` and wait for the state word. */
async function groupOn(page, url, state, extra = {}) {
  await open(page, url, { view: { state }, ...extra });
  await openSettings(page);
  await page.waitForFunction(() => document.querySelector(".set-net-state")?.textContent !== "");
}

/** The group sits after Accessibility with its legend, name and meta; every state has its word, tone, advice and buttons. */
async function groupCase(page, url) {
  for (const [state, want] of Object.entries(STATES)) {
    await groupOn(page, url, state);
    assert.equal(await group(page).count(), 1, state);
    const shape = await page.evaluate(() => {
      const sets = [...document.querySelectorAll(".dlg.settings .set-group")];
      const mine = document.querySelector("fieldset.set-net");
      return {
        legends: sets.map((s) => s.querySelector("legend")?.textContent),
        index: sets.indexOf(mine),
        role: mine.getAttribute("role"),
        label: document.getElementById(mine.getAttribute("aria-labelledby"))?.textContent,
        beforeActions: (mine.compareDocumentPosition(document.querySelector(".dlg.settings .dlg-actions")) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0,
      };
    });
    assert.deepEqual(shape.legends.slice(-2), ["Accessibility", "Local network"], `${state}: after Accessibility`);
    assert.equal(shape.index, shape.legends.length - 1);
    assert.equal(shape.role, "group");
    assert.equal(shape.label, "Local network");
    assert.equal(shape.beforeActions, true, `${state}: before the buttons of the dialog`);
    assert.equal(await text(page, ".set-net-name"), "Access");
    assert.equal(await text(page, ".set-net-meta"), "Privacy & Security");
    const r = row(page);
    assert.equal(await r.getAttribute("data-state"), state);
    assert.equal(await r.getAttribute("data-tone"), want.tone, state);
    assert.equal(await r.getAttribute("tabindex"), "-1");
    assert.equal(await text(page, ".set-net-state"), want.word, state);
    assert.equal(await text(page, ".set-net-advice"), want.advice, state);
    assert.deepEqual(await page.locator(".set-net-row button").allTextContents(), ["Open System Settings", ...want.extra], state);

    // The second cue besides the colour: the word always, and the underline and the mark for what needs attention.
    const cues = await page.locator(".set-net-state").evaluate((n) => ({
      mark: getComputedStyle(n, "::before").content, line: getComputedStyle(n).textDecorationLine,
    }));
    assert.equal(cues.line, state === "denied" ? "underline" : "none", `${state}: underline`);
    if (state === "denied") assert.match(cues.mark, /!/);
    else if (state === "unknown") assert.match(cues.mark, /\?/);
    else assert.ok(cues.mark === "none" || cues.mark === "normal", `${state}: no mark (${cues.mark})`);
    assert.equal(await page.locator("fieldset.set-net [style]").count(), 0, "no inline style");
    // The state was read once when the dialog opened (plus the one of the start).
    assert.equal((await netCalls(page, "local_network_status")).length, 2, state);
    assert.deepEqual(await netCalls(page, "local_network_answer"), [], `${state}: opening the dialog asks nothing`);
  }

  // Until the first answer the group says `Checking…` and shows no state word.
  await open(page, url, { view: { state: "allowed" }, hold: true });
  await openSettings(page);
  await group(page).waitFor();
  assert.equal(await text(page, ".set-net-line"), "Checking…");
  assert.equal(await page.locator(".set-net-line").getAttribute("role"), "status");
  assert.equal(await text(page, ".set-net-state"), "");
  assert.deepEqual(await page.locator(".set-net-row button").allTextContents(), ["Open System Settings"]);
  await page.evaluate(() => window.__MOCK__.localNetwork.release());
  await page.waitForFunction(() => document.querySelector(".set-net-state")?.textContent === "allowed");
  assert.equal(await text(page, ".set-net-line"), "");
}

/** The three buttons: what each sends, what the group line says, and where the keyboard goes. */
async function buttonsCase(page, url) {
  // Open System Settings: always there, one command, a refusal said in words.
  await groupOn(page, url, "allowed");
  await page.locator(".set-net-open").click();
  await waitCalls(page, "local_network_open_settings", 1);
  assert.equal((await netCalls(page, "local_network_open_settings")).length, 1);
  assert.equal(await text(page, ".set-net-line"), "");
  await page.evaluate(() => window.__MOCK__.localNetwork.openFails("cannot open System Settings: no such program"));
  await page.locator(".set-net-open").click();
  await page.waitForFunction(() => document.querySelector(".set-net-line")?.textContent === "cannot open System Settings: no such program");
  assert.equal((await netCalls(page, "local_network_open_settings")).length, 2);
  assert.equal(await group(page).count(), 1, "the group stays");

  // Allow (state not asked yet): one answer with allow true, the line says what the record says, the state is the shell's.
  await groupOn(page, url, "not_asked", { afterAllow: "unknown" });
  await page.locator(".set-net-allow").focus();
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => document.querySelector(".set-net-line")?.textContent?.startsWith("Asked."));
  assert.deepEqual(await netCalls(page, "local_network_answer"), [{ allow: true }]);
  assert.equal(await text(page, ".set-net-line"), ASKED);
  assert.equal(await row(page).getAttribute("data-state"), "unknown");
  assert.equal(await text(page, ".set-net-state"), "not known");
  assert.deepEqual(await page.locator(".set-net-row button").allTextContents(), ["Open System Settings", "Check again"]);
  assert.equal(await activeMatches(page, ".set-net-row"), true, "Allow went away: the keyboard moved to the row");

  // Check again: one more read of the state; the line says Checked.; the button goes when the state turns allowed.
  const before = (await netCalls(page, "local_network_status")).length;
  await page.evaluate(() => window.__MOCK__.localNetwork.set({ state: "denied" }));
  await page.locator(".set-net-check").focus();
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => document.querySelector(".set-net-line")?.textContent === "Checked.");
  assert.equal((await netCalls(page, "local_network_status")).length, before + 1, "exactly one more read");
  assert.equal(await row(page).getAttribute("data-state"), "denied");
  assert.equal(await activeMatches(page, ".set-net-check"), true, "the button that was pressed keeps the keyboard");
  await page.evaluate(() => window.__MOCK__.localNetwork.set({ state: "allowed" }));
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => document.querySelector(".set-net-state")?.textContent === "allowed");
  assert.equal(await page.locator(".set-net-check").count(), 0);
  assert.equal(await text(page, ".set-net-line"), "Checked.");
  assert.equal(await activeMatches(page, ".set-net-row"), true, "the button went away: the keyboard is on the row");
  assert.deepEqual(await netCalls(page, "local_network_answer"), [{ allow: true }], "a read never answers the question");

  // Tab reaches the buttons from the dialog and stays inside it.
  await groupOn(page, url, "denied");
  await page.locator("#set-theme-0").focus();
  for (let i = 0; i < 40 && !(await activeMatches(page, ".set-net-open")); i++) await page.keyboard.press("Tab");
  assert.equal(await activeMatches(page, ".set-net-open"), true, "Tab reaches Open System Settings");
  await page.keyboard.press("Tab");
  assert.equal(await activeMatches(page, ".set-net-check"), true);
}

/** A window focus reads once while the dialog is open (at most once a second); nothing polls and nothing reads when it is closed. */
async function focusRefreshCase(page, url) {
  await groupOn(page, url, "not_asked");
  await settle(page);
  const calls = async () => (await netCalls(page, "local_network_status")).length;
  const base = await calls();
  const focus = () => page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await focus();
  await waitCalls(page, "local_network_status", base + 1);
  await focus();
  await settle(page);
  assert.equal(await calls(), base + 1, "a second focus inside a second reads nothing");
  await page.waitForTimeout(1100);
  await page.evaluate(() => window.__MOCK__.localNetwork.set({ state: "allowed" }));
  await focus();
  await waitCalls(page, "local_network_status", base + 2);
  await page.waitForFunction(() => document.querySelector(".set-net-state")?.textContent === "allowed");
  assert.equal(await row(page).getAttribute("data-state"), "allowed", "the row follows the shell without a button");

  // Nothing polls.
  await page.waitForTimeout(1500);
  assert.equal(await calls(), base + 2, "no timer");

  // With the dialog closed the window focus reads nothing.
  await closeSettings(page);
  await page.waitForTimeout(1100);
  await focus();
  await settle(page);
  assert.equal(await calls(), base + 2, "closed: no read");
  assert.equal(await group(page).count(), 0, "the group left with the dialog");
}

/** Where the system has no such setting there is no group, and the page does not call the shell. */
async function noGroupCase(page, url) {
  for (const os of ["windows", "linux", ""]) {
    await open(page, url, { os, view: { state: "allowed" } });
    await openSettings(page);
    await settle(page);
    assert.equal(await group(page).count(), 0, `${JSON.stringify(os)}: no group`);
    assert.equal(await settings(page).getByText("Local network").count(), 0);
    for (const cmd of ["local_network_status", "local_network_answer", "local_network_open_settings"]) {
      assert.deepEqual(await netCalls(page, cmd), [], `${JSON.stringify(os)}: no ${cmd}`);
    }
  }
  // A Mac below macOS 15: the shell says unsupported, and the group removes itself.
  await open(page, url, { view: { supported: false, state: "not_asked" } });
  await openSettings(page);
  await page.waitForFunction(() => window.__MOCK__.callsOf("local_network_status").length >= 2);
  await settle(page);
  assert.equal(await group(page).count(), 0, "supported false: no group");
  assert.equal(await settings(page).count(), 1, "the dialog itself is fine");
}

/** Forced colours and higher contrast keep the border and the second cue; every button is at least 28 px high. */
async function accessibilityCase(page, url) {
  await groupOn(page, url, "denied");
  const sizes = await page.evaluate(() => {
    const root = parseFloat(getComputedStyle(document.documentElement).fontSize);
    return { root, heights: [...document.querySelectorAll(".set-net-row button")].map((b) => b.getBoundingClientRect().height) };
  });
  assert.equal(sizes.root, 12, "the base root font size");
  for (const h of sizes.heights) assert.ok(h >= 28, `a button is at least 28 px high: ${h}`);

  await page.evaluate(() => document.documentElement.classList.add("contrast-high"));
  const high = await page.evaluate(() => ({
    border: getComputedStyle(document.querySelector("fieldset.set-net")).borderTopStyle,
    mark: getComputedStyle(document.querySelector(".set-net-state"), "::before").content,
    line: getComputedStyle(document.querySelector(".set-net-state")).textDecorationLine,
    word: document.querySelector(".set-net-state").textContent,
  }));
  assert.equal(high.border, "solid");
  assert.match(high.mark, /!/);
  assert.equal(high.line, "underline");
  assert.equal(high.word, "denied");
  await page.evaluate(() => document.documentElement.classList.remove("contrast-high"));

  await page.emulateMedia({ forcedColors: "active" });
  try {
    const forced = await page.evaluate(() => {
      const r = document.querySelector(".set-net-row");
      const s = getComputedStyle(r);
      return {
        outline: s.outlineStyle, width: s.outlineWidth,
        border: getComputedStyle(document.querySelector("fieldset.set-net")).borderTopStyle,
        mark: getComputedStyle(document.querySelector(".set-net-state"), "::before").content,
        word: document.querySelector(".set-net-state").textContent,
      };
    });
    assert.equal(forced.outline, "solid", "the row has a system-colour outline");
    assert.equal(forced.width, "2px");
    assert.equal(forced.border, "solid");
    assert.match(forced.mark, /!/);
    assert.equal(forced.word, "denied");
  } finally {
    await page.emulateMedia({ forcedColors: null });
  }

  // The notice has the same sizes.
  await open(page, url, { view: DUE });
  await notice(page).waitFor();
  const buttons = await page.evaluate(() => [...document.querySelectorAll(".dlg.localnet button")].map((b) => b.getBoundingClientRect().height));
  assert.equal(buttons.length, 2);
  for (const h of buttons) assert.ok(h >= 28, `a button of the notice is at least 28 px high: ${h}`);
  const border = await page.evaluate(() => getComputedStyle(document.querySelector(".dlg.localnet")).borderTopStyle);
  assert.equal(border, "solid");
}

/** The shipped sources: no markup built from a string, no colour literal, no pixel font size. */
async function sourceCase() {
  const dist = join(APP_DIR, "dist");
  for (const name of ["localnet.js", "localnetstate.js"]) {
    const code = readFileSync(join(dist, name), "utf8");
    assert.doesNotMatch(code, /cssText|innerHTML|outerHTML|insertAdjacentHTML|setAttribute\(\s*["']style/, name);
  }
  const css = readFileSync(join(dist, "localnet.css"), "utf8");
  const colours = css.match(/#[0-9a-fA-F]{3,8}\b|\b(?:rgb|rgba|hsl|hsla)\(/g) ?? [];
  assert.deepEqual(colours, [], `localnet.css hard-codes colours: ${colours.join(", ")}`);
  assert.deepEqual(css.match(/font-size:\s*\d+(?:\.\d+)?px/g) ?? [], [], "localnet.css has a pixel font size");
}
