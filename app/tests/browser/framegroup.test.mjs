// The group `Window frame` of the Settings dialog (docs/design.md section 6.33), against the Tauri stub whose hook
// `__MOCK__.windowFrame` plays the shell. The sentences below are the design record's: the page's table is held to them here and in
// the unit test. Nothing here can see a real Windows or Linux window: the group is run in Chromium and WebKit against a mocked shell
// only, and no change was ever applied to a real window frame.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { APP_DIR, callsOf, currentBackend } from "./lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000); // a dialog that never appears fails here, not at the runner's 30 s limit
  const requests = [];
  page.on("request", (r) => requests.push(r.url()));
  if (currentBackend() === "tauri") {
    await presenceCase(page, url);
    await absentCase(page, url, requests);
    await placementCase(page, url);
    await pressCase(page, url);
    await redrawCase(page, url);
    await pendingCase(page, url);
    await refusalCase(page, url);
    await inFlightCase(page, url);
    await keyboardCase(page, url);
    await readCountCase(page, url);
    await sourceCase();
  } else {
    await webCase(page, url);
  }
  assert.deepEqual(errors, [], "the page reported errors");
}

const golden = JSON.parse(readFileSync(join(APP_DIR, "tests/fixtures/protocol.json"), "utf8"));

/** The words of the design record. */
const LEGEND = "Window frame";
const ADVICE_FRAME = "Auto draws holzWave's own bar as the window bar. System keeps the system's title bar above holzWave's bar.";
const ADVICE_WAYLAND =
  "Under Wayland the own frame has not been tried; if the window cannot be moved or resized, choose System in this dialog (open it from the palette).";
const ADVICE_BUTTONS = "Auto follows the desktop: round on GNOME, flat on every other desktop.";
const APPLIES_LATER = "Applies after the next start.";
const FAILED = "The command failed.";

/** The view that applies, with a member changed. */
const VIEW = { applies: true, frame: "auto", buttons: "auto", pending: false };

/**
 * Open the page against the stub. Every case registers a complete init script (the platform, the whole view, every knob):
 * an earlier case's script stays and runs first, so a knob this one does not set would leak from the earlier one.
 */
async function open(page, url, opts = {}) {
  const {
    os = "linux", view = {}, started = { frame: "auto", buttons: "auto" }, statusFails = null, setFails = null, hold = false,
    shell = { applies: false, state: "unavailable" }, holdShell = false,
  } = opts;
  const full = { ...VIEW, ...view };
  await page.addInitScript(
    ([o, v, st, status, set, held, hs, heldShell, settings]) => {
      window.__MOCK__.on("app_info", () => ({ version: "0.0.1-alpha1", os: o, hostname: "woodbox" }));
      window.__MOCK__.on("get_settings", () => structuredClone(settings));
      const frame = window.__MOCK__.windowFrame;
      frame.set(v);
      frame.setStarted(st);
      frame.statusFails(status);
      frame.setFails(set);
      if (held) frame.hold();
      else frame.release();
      const host = window.__MOCK__.hostShell;
      host.set(hs);
      if (heldShell) host.hold();
      else host.release();
    },
    [os, full, started, statusFails, setFails, hold, shell, holdShell, golden.settings_view],
  );
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("window_ready").length >= 1);
}

const settings = (page) => page.getByRole("dialog", { name: "Settings" });
const group = (page) => page.locator("fieldset.set-fr");
const radio = (page, key, value) => page.locator(`input[name="set-${key}"][value="${value}"]`);
const radios = (page, key) => page.locator(`input[name="set-${key}"]`);
const calls = (page, cmd) => callsOf(page, cmd);
const settle = (page) =>
  page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(() => setTimeout(done, 60)))));

async function openSettings(page) {
  await page.keyboard.press("Meta+,");
  await settings(page).waitFor();
}

async function closeSettings(page) {
  await page.keyboard.press("Escape");
  await settings(page).waitFor({ state: "detached" });
}

/** Open the Settings dialog with the group present. */
async function groupOn(page, url, opts = {}) {
  await open(page, url, opts);
  await openSettings(page);
  await group(page).waitFor();
}

/** The group as the page drew it. */
const snapshot = (page) =>
  page.evaluate(() => {
    const g = document.querySelector("fieldset.set-fr");
    const rows = [...g.querySelectorAll(".set-fr-row")].map((row) => {
      const inputs = [...row.querySelectorAll("input")];
      return {
        name: row.querySelector(".set-fr-name").textContent,
        meta: row.querySelector(".set-fr-meta").textContent,
        legend: row.querySelector("legend").textContent,
        labels: inputs.map((i) => i.closest("label").textContent),
        selected: inputs.filter((i) => i.checked).map((i) => i.value),
        names: [...new Set(inputs.map((i) => i.name))],
        advice: row.querySelector(".set-fr-advice").textContent,
        describedBy: document.getElementById(row.getAttribute("aria-describedby"))?.textContent ?? null,
      };
    });
    const sets = [...document.querySelectorAll(".dlg.settings .set-group")];
    const line = g.querySelector(".set-fr-line");
    return {
      role: g.getAttribute("role"),
      label: document.getElementById(g.getAttribute("aria-labelledby"))?.textContent,
      rows,
      legends: sets.map((s) => s.querySelector("legend")?.textContent),
      index: sets.indexOf(g),
      beforeActions: (g.compareDocumentPosition(document.querySelector(".dlg.settings .dlg-actions")) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0,
      line: line.textContent,
      lineRole: line.getAttribute("role"),
      lineChildren: line.children.length,
      controls: g.querySelectorAll("button, a, [href], select, input:not([type=radio])").length,
      radios: g.querySelectorAll("input[type=radio]").length,
    };
  });

/** Presence on Windows and on Linux: the rows, the words, the advice in the spec's order, the selection from the view. */
async function presenceCase(page, url) {
  // Windows: the row `Frame` only, one advice sentence, no Wayland sentence.
  await groupOn(page, url, { os: "windows", view: { frame: "own", buttons: "round" } });
  let shape = await snapshot(page);
  assert.equal(shape.role, "group");
  assert.equal(shape.label, LEGEND);
  assert.equal(await page.getByRole("group", { name: LEGEND, exact: true }).count(), 1, "windows: the group has its accessible name");
  assert.equal(await page.getByRole("group", { name: "Frame Windows and Linux", exact: true }).count(), 1, "windows: the legend and the meta name the row");
  assert.equal(shape.rows.length, 1, "windows: one row");
  assert.equal(shape.radios, 3, "windows: three radios");
  assert.deepEqual(shape.rows[0], {
    name: "Frame", meta: "Windows and Linux", legend: "Frame Windows and Linux", labels: ["Auto", "Own", "System"], selected: ["own"],
    names: ["set-frame"], advice: ADVICE_FRAME, describedBy: ADVICE_FRAME,
  });
  assert.ok(!shape.rows[0].advice.includes("Wayland"), "windows: no Wayland sentence");
  assert.equal(await page.getByText("Buttons", { exact: true }).count(), 0, "windows: no Buttons row");
  assert.equal(await settings(page).getByText(ADVICE_BUTTONS).count(), 0);
  assert.deepEqual(shape.legends.slice(-2), ["Accessibility", LEGEND], "windows: after Accessibility");
  assert.equal(shape.index, shape.legends.length - 1);
  assert.equal(shape.beforeActions, true, "windows: before the buttons of the dialog");
  assert.equal(shape.line, "");
  assert.equal(shape.lineRole, "status");
  assert.equal(shape.lineChildren, 0);
  assert.equal(shape.controls, 0, "no button, link or other control in the group");
  assert.equal((await calls(page, "window_frame_status")).length, 1, "windows: one status read");
  assert.deepEqual(await calls(page, "window_frame_set"), [], "windows: nothing sent");

  // Linux: both rows, the three advice sentences in the spec's order.
  await closeSettings(page);
  await groupOn(page, url, { os: "linux", view: { frame: "system", buttons: "flat" } });
  shape = await snapshot(page);
  assert.equal(shape.radios, 6, "linux: six radios");
  assert.deepEqual(shape.rows, [
    {
      name: "Frame", meta: "Windows and Linux", legend: "Frame Windows and Linux", labels: ["Auto", "Own", "System"], selected: ["system"],
      names: ["set-frame"], advice: `${ADVICE_FRAME} ${ADVICE_WAYLAND}`, describedBy: `${ADVICE_FRAME} ${ADVICE_WAYLAND}`,
    },
    {
      name: "Buttons", meta: "Linux", legend: "Buttons Linux", labels: ["Auto", "Flat", "Round"], selected: ["flat"],
      names: ["set-buttons"], advice: ADVICE_BUTTONS, describedBy: ADVICE_BUTTONS,
    },
  ]);
  assert.deepEqual(shape.legends.slice(-2), ["Accessibility", LEGEND], "linux: after Accessibility (no Host shell here)");
  assert.equal(shape.beforeActions, true);
  assert.equal(await page.getByRole("group", { name: "Buttons Linux", exact: true }).count(), 1);
  assert.equal(await group(page).getByRole("radio", { name: "Round", exact: true }).count(), 1);
  await closeSettings(page);
  assert.equal(await group(page).count(), 0, "the group left with the dialog");

  // The golden views: every word of the shell selects the radio of the same word.
  for (const sample of golden.answers.window_frame.filter((s) => s.applies)) {
    await groupOn(page, url, { os: "linux", view: sample });
    shape = await snapshot(page);
    assert.deepEqual(shape.rows.map((r) => r.selected), [[sample.frame], [sample.buttons]], JSON.stringify(sample));
    assert.equal(shape.line, sample.pending ? APPLIES_LATER : "", JSON.stringify(sample));
    await closeSettings(page);
  }

  // A word that is in no table reads as `auto`, as text, never as markup.
  await groupOn(page, url, { os: "linux", view: { frame: "<b>own</b>", buttons: 7 } });
  shape = await snapshot(page);
  assert.deepEqual(shape.rows.map((r) => r.selected), [["auto"], ["auto"]]);
  assert.equal(await page.locator("fieldset.set-fr b").count(), 0);
  await closeSettings(page);
}

/** Anywhere else, and when the shell says no or fails, the group is not in the document at all and nothing is fetched. */
async function absentCase(page, url, requests) {
  const quiet = async (why, expectStatus) => {
    await openSettings(page);
    if (expectStatus) await page.waitForFunction(() => window.__MOCK__.callsOf("window_frame_status").length >= 1);
    await settle(page);
    assert.equal(await group(page).count(), 0, `${why}: no group`);
    assert.equal(await settings(page).getByText(LEGEND, { exact: true }).count(), 0, `${why}: no legend`);
    assert.equal(await page.locator(".dlg.settings .set-fr-row, .dlg.settings .set-fr-radio").count(), 0, `${why}: nothing of it`);
    assert.deepEqual(await calls(page, "window_frame_set"), [], `${why}: no window_frame_set`);
    assert.equal(requests.filter((u) => u.endsWith("/framegroup.css")).length, 0, `${why}: no framegroup.css request`);
    assert.equal(await settings(page).count(), 1, `${why}: the dialog itself is fine`);
  };
  for (const os of ["macos", "android", "web", ""]) {
    requests.length = 0;
    await open(page, url, { os, view: { applies: true } });
    await quiet(JSON.stringify(os), false);
    assert.deepEqual(await calls(page, "window_frame_status"), [], `${JSON.stringify(os)}: no window_frame_status`);
  }
  for (const os of ["windows", "linux"]) {
    requests.length = 0;
    await open(page, url, { os, view: { applies: false } });
    await quiet(`${os}, applies false`, true);

    requests.length = 0;
    await open(page, url, { os, statusFails: "no frame here", view: { applies: true } });
    await quiet(`${os}, a failed status call`, true);

    requests.length = 0;
    await open(page, url, { os, view: { applies: "yes" } });
    await quiet(`${os}, an answer that is not a view`, true);
  }
}

/** The place on Linux is the same whichever answer of the shell arrives first. */
async function placementCase(page, url) {
  const host = { applies: true, state: "off" };
  for (const order of ["frame first", "host shell first"]) {
    await open(page, url, { os: "linux", shell: host, hold: true, holdShell: true, view: { frame: "own" } });
    await openSettings(page);
    await page.waitForFunction(() => window.__MOCK__.callsOf("window_frame_status").length >= 1);
    await page.waitForFunction(() => window.__MOCK__.callsOf("host_shell_status").length >= 1);
    assert.equal(await group(page).count(), 0, `${order}: nothing before an answer`);
    const release = (which) => page.evaluate((w) => window.__MOCK__[w].release(), which);
    const waitFor = (selector) => page.locator(selector).waitFor();
    if (order === "frame first") {
      await release("windowFrame");
      await waitFor("fieldset.set-fr");
      await release("hostShell");
      await waitFor("fieldset.set-hs");
    } else {
      await release("hostShell");
      await waitFor("fieldset.set-hs");
      await release("windowFrame");
      await waitFor("fieldset.set-fr");
    }
    const shape = await snapshot(page);
    assert.deepEqual(shape.legends.slice(-3), ["Accessibility", "Host shell", LEGEND], `${order}: the order of the groups`);
    assert.equal(shape.beforeActions, true, `${order}: before the buttons`);
    await closeSettings(page);
  }
  // Windows: after Accessibility, before the buttons; the host shell group is not asked.
  await groupOn(page, url, { os: "windows", shell: host });
  const shape = await snapshot(page);
  assert.deepEqual(shape.legends.slice(-2), ["Accessibility", LEGEND]);
  assert.equal(shape.beforeActions, true);
  assert.deepEqual(await calls(page, "host_shell_status"), [], "windows: no host shell question");
  await closeSettings(page);
}

/** A press sends one command with both words; on Windows the buttons word is the unchanged one. */
async function pressCase(page, url) {
  await groupOn(page, url, { os: "windows", view: { frame: "auto", buttons: "round" } });
  await radio(page, "frame", "own").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("window_frame_set").length >= 1);
  assert.deepEqual(await calls(page, "window_frame_set"), [{ frame: "own", buttons: "round" }], "windows: the unchanged buttons word");
  await page.waitForFunction(() => document.querySelector('input[name="set-frame"]:checked')?.value === "own");
  await settle(page);
  assert.equal((await calls(page, "window_frame_set")).length, 1, "one command for one press");
  await closeSettings(page);

  await groupOn(page, url, { os: "linux", view: { frame: "own", buttons: "auto" } });
  await radio(page, "buttons", "round").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("window_frame_set").length >= 1);
  assert.deepEqual(await calls(page, "window_frame_set"), [{ frame: "own", buttons: "round" }], "linux: both words");
  await page.waitForFunction(() => document.querySelector('input[name="set-buttons"]:checked')?.value === "round");
  await radio(page, "frame", "system").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("window_frame_set").length >= 2);
  assert.deepEqual(await calls(page, "window_frame_set"), [
    { frame: "own", buttons: "round" },
    { frame: "system", buttons: "round" },
  ]);
  // The choice is a fact of the shell: opening the dialog again reads it.
  await page.waitForFunction(() => document.querySelector('input[name="set-frame"]:checked')?.value === "system");
  await closeSettings(page);
  await openSettings(page);
  await group(page).waitFor();
  assert.deepEqual((await snapshot(page)).rows.map((r) => r.selected), [["system"], ["round"]]);
  await closeSettings(page);
}

/** The radios are redrawn from the answer, not from the press; an answer that is no view or does not apply is handled. */
async function redrawCase(page, url) {
  await groupOn(page, url, { os: "linux" });
  await page.evaluate(() => {
    window.__MOCK__.on("window_frame_set", () => ({ applies: true, frame: "system", buttons: "flat", pending: true }));
  });
  await radio(page, "frame", "own").click();
  await page.waitForFunction(() => document.querySelector('input[name="set-frame"]:checked')?.value === "system");
  let shape = await snapshot(page);
  assert.deepEqual(shape.rows.map((r) => r.selected), [["system"], ["flat"]], "the shell's answer is drawn");
  assert.equal(shape.line, APPLIES_LATER);

  // An answer that is not a view: the radios go back to the last answer and the plain sentence is said.
  await page.evaluate(() => window.__MOCK__.on("window_frame_set", () => 5));
  await radio(page, "frame", "own").click();
  await page.waitForFunction(() => document.querySelector(".set-fr-line")?.textContent === "The command failed.");
  shape = await snapshot(page);
  assert.deepEqual(shape.rows.map((r) => r.selected), [["system"], ["flat"]], "the radios are back");

  // An answer that says the group does not apply: the group leaves.
  await page.evaluate(() => window.__MOCK__.on("window_frame_set", () => ({ applies: false, frame: "auto", buttons: "auto", pending: false })));
  await radio(page, "frame", "own").click();
  await page.waitForFunction(() => document.querySelector("fieldset.set-fr") === null);
  await closeSettings(page);
}

/** The status line says when the choice waits for the next start and is empty when it does not. */
async function pendingCase(page, url) {
  await groupOn(page, url, { os: "linux", started: { frame: "auto", buttons: "auto" } });
  assert.equal((await snapshot(page)).line, "", "nothing waits");
  await radio(page, "frame", "system").click();
  await page.waitForFunction(() => document.querySelector(".set-fr-line")?.textContent === "Applies after the next start.");
  const line = await page.locator(".set-fr-line").evaluate((n) => ({ role: n.getAttribute("role"), live: n.closest("[aria-hidden]") === null }));
  assert.equal(line.role, "status");
  assert.equal(line.live, true);
  // Set back to the pair the windows were built with: the line is empty again.
  await radio(page, "frame", "auto").click();
  await page.waitForFunction(() => document.querySelector(".set-fr-line")?.textContent === "");
  assert.equal(await page.locator(".set-fr-line").evaluate((n) => n.textContent), "");
  await closeSettings(page);

  // The windows were built with `own`: a stored `own` waits for nothing, a stored `auto` does.
  await groupOn(page, url, { os: "windows", started: { frame: "own", buttons: "auto" }, view: { frame: "own" } });
  assert.equal((await snapshot(page)).line, "");
  await radio(page, "frame", "auto").click();
  await page.waitForFunction(() => document.querySelector(".set-fr-line")?.textContent === "Applies after the next start.");
  await closeSettings(page);
}

/** A refusal restores the radios and says why as a bounded plain line; a failure with no reason says the fixed sentence. */
async function refusalCase(page, url) {
  const hostile = "<img src=x onerror=boom()> no\u0007 permission\u0000\nhere";
  await groupOn(page, url, { os: "linux", view: { frame: "own", buttons: "flat" }, setFails: hostile });
  await radio(page, "frame", "system").click();
  await page.waitForFunction(() => document.querySelector(".set-fr-line")?.textContent !== "");
  let shape = await snapshot(page);
  assert.deepEqual(shape.rows.map((r) => r.selected), [["own"], ["flat"]], "the radios go back to what the shell holds");
  assert.equal(shape.line, "<img src=x onerror=boom()> no  permission  here", "plain text, control characters as blanks");
  assert.equal(shape.lineRole, "status");
  assert.equal(shape.lineChildren, 0);
  assert.equal(await page.locator(".set-fr img").count(), 0);
  assert.equal(await page.locator('.set-fr input:disabled').count(), 0, "the radios can be pressed again");

  // A very long reason is shown bounded.
  await page.evaluate(() => window.__MOCK__.windowFrame.setFails("x".repeat(5000)));
  await radio(page, "buttons", "round").click();
  await page.waitForFunction(() => document.querySelector(".set-fr-line")?.textContent.startsWith("xxx"));
  const long = await page.locator(".set-fr-line").textContent();
  assert.ok(long.length <= 240 && long.length > 100, `bounded: ${long.length}`);
  assert.deepEqual((await snapshot(page)).rows.map((r) => r.selected), [["own"], ["flat"]]);

  // Markup stays markup.
  await page.evaluate(() => window.__MOCK__.windowFrame.setFails("<b>bold</b><script>boom()</script>"));
  await radio(page, "frame", "system").click();
  await page.waitForFunction(() => document.querySelector(".set-fr-line")?.textContent.startsWith("<b>"));
  assert.equal(await page.locator(".set-fr-line").textContent(), "<b>bold</b><script>boom()</script>");
  assert.equal(await page.locator(".set-fr-line *").count(), 0);

  // A rejection that is not a string says the fixed sentence; a blank string does too.
  for (const failure of [{ code: 7 }, 42, "   ", ""]) {
    await page.evaluate((f) => {
      window.__MOCK__.windowFrame.setFails(f === "" ? " " : f);
      document.querySelector(".set-fr-line").textContent = "";
    }, failure);
    await radio(page, "frame", "system").click();
    await page.waitForFunction(() => document.querySelector(".set-fr-line")?.textContent !== "");
    assert.equal(await page.locator(".set-fr-line").textContent(), FAILED, `a failure of ${JSON.stringify(failure)}`);
  }
  // A later press that works clears the line.
  await page.evaluate(() => window.__MOCK__.windowFrame.setFails(null));
  await radio(page, "frame", "system").click();
  await page.waitForFunction(() => document.querySelector('input[name="set-frame"]:checked')?.value === "system");
  shape = await snapshot(page);
  assert.equal(shape.line, APPLIES_LATER);
  await closeSettings(page);
}

/** The radios are disabled while a command is in flight and enabled after it; the keyboard stays where it was. */
async function inFlightCase(page, url) {
  await groupOn(page, url, { os: "linux" });
  await page.evaluate(() => {
    window.__MOCK__.on("window_frame_set", (args) => new Promise((go) => {
      window.__answer = () => go({ applies: true, frame: args.frame, buttons: args.buttons, pending: true });
    }));
  });
  await radios(page, "frame").nth(1).focus();
  await page.keyboard.press("Space");
  await page.waitForFunction(() => window.__MOCK__.callsOf("window_frame_set").length >= 1);
  assert.equal(await page.locator("fieldset.set-fr input:disabled").count(), 5, "five radios are disabled while the call is held; the one the keyboard is on is not");
  assert.equal(await radios(page, "frame").nth(1).isDisabled(), false, "the radio with the keyboard keeps it");
  assert.equal(await group(page).getAttribute("aria-busy"), "true");
  // A second press while it is held sends nothing.
  await radios(page, "frame").nth(1).evaluate((n) => n.dispatchEvent(new Event("change", { bubbles: true })));
  await settle(page);
  assert.equal((await calls(page, "window_frame_set")).length, 1, "one command while one is in flight");
  await page.evaluate(() => window.__answer());
  await page.waitForFunction(() => document.querySelectorAll("fieldset.set-fr input:disabled").length === 0);
  assert.equal(await group(page).getAttribute("aria-busy"), null);
  assert.equal(await radios(page, "frame").nth(1).evaluate((n) => document.activeElement === n), true, "the keyboard is back on the radio");
  assert.equal(await page.locator(".set-fr-line").textContent(), APPLIES_LATER);
  await closeSettings(page);
}

/** Arrow keys move between the radios of a row, the page's ring shows, and the legends name the rows. */
async function keyboardCase(page, url) {
  await groupOn(page, url, { os: "linux", view: { frame: "auto", buttons: "auto" } });
  await radios(page, "frame").first().focus();
  await page.keyboard.press("ArrowRight");
  await page.waitForFunction(() => window.__MOCK__.callsOf("window_frame_set").length >= 1);
  assert.deepEqual(await calls(page, "window_frame_set"), [{ frame: "own", buttons: "auto" }]);
  await page.waitForFunction(() => document.querySelectorAll("fieldset.set-fr input:disabled").length === 0);
  const kept = await radios(page, "frame").nth(1).evaluate((n) => ({ focused: document.activeElement === n, checked: n.checked }));
  assert.equal(kept.focused, true, "the keyboard is on the radio that was chosen");
  assert.equal(kept.checked, true);
  // The ring is the page's 2 px rule. (WebKit does not match :focus-visible after an arrow key inside a radio group, for any
  // choice row of the dialog, so the ring is read after Tab brings the keyboard back to the radio, as the host shell test does.)
  await page.keyboard.press("Tab");
  await page.keyboard.press("Shift+Tab");
  const ring = await radios(page, "frame").nth(1).evaluate((n) => {
    const s = getComputedStyle(n);
    return { focused: document.activeElement === n, style: s.outlineStyle, width: s.outlineWidth };
  });
  assert.equal(ring.focused, true, "Shift+Tab is back on the chosen radio");
  assert.equal(ring.style, "solid", "the ring is drawn on the radio the keyboard is on");
  assert.equal(ring.width, "2px", "the page's 2 px ring");
  await page.keyboard.press("ArrowRight");
  await page.waitForFunction(() => window.__MOCK__.callsOf("window_frame_set").length >= 2);
  await page.waitForFunction(() => document.querySelector('input[name="set-frame"]:checked')?.value === "system");
  await page.waitForFunction(() => document.querySelectorAll("fieldset.set-fr input:disabled").length === 0);
  await page.keyboard.press("ArrowLeft");
  await page.waitForFunction(() => window.__MOCK__.callsOf("window_frame_set").length >= 3);
  assert.deepEqual((await calls(page, "window_frame_set")).map((c) => c.frame), ["own", "system", "own"]);
  // The keys of a row stay in that row: the second row is another tab stop.
  await page.waitForFunction(() => document.querySelectorAll("fieldset.set-fr input:disabled").length === 0);
  await page.keyboard.press("Tab");
  assert.equal(await page.evaluate(() => document.activeElement?.getAttribute("name")), "set-buttons", "Tab leaves the row");

  // Sizes: a choice is at least 28 px high, the root is 12 px, nothing moves.
  const sizes = await page.evaluate(() => ({
    root: parseFloat(getComputedStyle(document.documentElement).fontSize),
    option: document.querySelector(".set-fr-row .set-option").getBoundingClientRect().height,
    animation: getComputedStyle(document.querySelector(".set-fr")).animationName,
    transition: getComputedStyle(document.querySelector(".set-fr")).transitionDuration,
    slot: getComputedStyle(document.querySelector(".set-fr-slot")).display,
  }));
  assert.equal(sizes.root, 12);
  assert.ok(sizes.option >= 28, `the target is at least 28 px high: ${sizes.option}`);
  assert.equal(sizes.animation, "none");
  assert.equal(sizes.transition, "0s");
  assert.equal(sizes.slot, "contents");

  // Higher contrast and forced colours keep the group and its words.
  await page.evaluate(() => document.documentElement.classList.add("contrast-high"));
  assert.equal(await group(page).evaluate((n) => getComputedStyle(n).borderTopStyle), "solid", "contrast-high: the group keeps its border");
  await page.evaluate(() => document.documentElement.classList.remove("contrast-high"));
  await page.emulateMedia({ forcedColors: "active" });
  try {
    assert.equal(await group(page).evaluate((n) => getComputedStyle(n).borderTopStyle), "solid", "forced colours: the group keeps its border");
    assert.equal(await page.locator(".set-fr-advice").first().textContent(), `${ADVICE_FRAME} ${ADVICE_WAYLAND}`);
  } finally {
    await page.emulateMedia({ forcedColors: null });
  }
  await closeSettings(page);
}

/** One status read per open of the dialog, none after it closes, and none by a timer or by focus. */
async function readCountCase(page, url) {
  await groupOn(page, url, { os: "linux" });
  assert.equal((await calls(page, "window_frame_status")).length, 1, "one read");
  await settle(page);
  await radios(page, "frame").first().focus();
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await settle(page);
  assert.equal((await calls(page, "window_frame_status")).length, 1, "no timer, no focus read");
  await closeSettings(page);
  await settle(page);
  assert.equal((await calls(page, "window_frame_status")).length, 1, "none after the dialog closes");
  await openSettings(page);
  await group(page).waitFor();
  assert.equal((await calls(page, "window_frame_status")).length, 2, "one read per opening");
  await closeSettings(page);

  // The shell answers after the dialog is closed: the group never appears.
  await open(page, url, { os: "linux", hold: true });
  await openSettings(page);
  await page.waitForFunction(() => window.__MOCK__.callsOf("window_frame_status").length >= 1);
  assert.equal(await group(page).count(), 0, "nothing before the shell answers");
  await closeSettings(page);
  await page.evaluate(() => window.__MOCK__.windowFrame.release());
  await settle(page);
  assert.equal(await group(page).count(), 0, "a late answer draws nothing");
}

/** The browser build has no such group, whatever the platform says, and never asks the shell. */
async function webCase(page, url) {
  for (const os of ["linux", "windows"]) {
    await open(page, url, { os, view: { applies: true } });
    await openSettings(page);
    await settle(page);
    assert.equal(await group(page).count(), 0, `web ${os}: no group`);
    assert.equal(await settings(page).getByText(LEGEND, { exact: true }).count(), 0);
    assert.deepEqual(await calls(page, "window_frame_status"), [], `web ${os}: no window_frame_status`);
    assert.deepEqual(await calls(page, "window_frame_set"), [], `web ${os}: no window_frame_set`);
    await closeSettings(page);
  }
}

/** The shipped sources: no markup built from a string, no colour literal, no pixel font size, no timer. */
async function sourceCase() {
  const dist = join(APP_DIR, "dist");
  for (const name of ["framegroup.js", "framegroupstate.js"]) {
    const code = readFileSync(join(dist, name), "utf8");
    assert.doesNotMatch(code, /cssText|innerHTML|outerHTML|insertAdjacentHTML|setAttribute\(\s*["']style/, name);
    assert.doesNotMatch(code, /setTimeout|setInterval|requestAnimationFrame|addEventListener\(\s*["'](?:focus|visibilitychange)/, `${name}: no timer, no polling`);
  }
  const css = readFileSync(join(dist, "framegroup.css"), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
  const colours = css.match(/#[0-9a-fA-F]{3,8}\b|\b(?:rgb|rgba|hsl|hsla)\(/g) ?? [];
  assert.deepEqual(colours, [], `framegroup.css hard-codes colours: ${colours.join(", ")}`);
  assert.deepEqual(css.match(/font-size:\s*\d+(?:\.\d+)?px/g) ?? [], [], "framegroup.css has a pixel font size");
  assert.doesNotMatch(css, /gradient|blur|box-shadow|animation|transition/, "framegroup.css is flat and still");
}
