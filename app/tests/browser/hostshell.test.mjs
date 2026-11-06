// The group `Host shell` of the Settings dialog (docs/design.md section 6.32), against the Tauri stub whose hook
// `__MOCK__.hostShell` plays the shell. The sentences below are the design record's: the page's table is held to them here and in
// the unit test. Nothing here can see a real Flatpak: the group is run in Chromium and WebKit against a mocked shell only.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { APP_DIR, callsOf, currentBackend } from "./lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000); // a dialog that never appears fails here, not at the runner's 30 s limit
  if (currentBackend() === "tauri") {
    await statesCase(page, url);
    await absentCase(page, url);
    await toggleCase(page, url);
    await refusalCase(page, url);
    await lateCase(page, url);
    await accessibilityCase(page, url);
    await sourceCase();
  } else {
    await webCase(page, url);
  }
  assert.deepEqual(errors, [], "the page reported errors");
}

const golden = JSON.parse(readFileSync(join(APP_DIR, "tests/fixtures/protocol.json"), "utf8"));

/** The state words of the design record: the word, the tone, the mark, the box and the advice. */
const STATES = {
  off: {
    word: "off", tone: "plain", mark: "", checked: false, disabled: false,
    advice: "New panes start inside the Flatpak sandbox. Your own tools and the files outside it are not reachable from them.",
  },
  on: {
    word: "on", tone: "warn", mark: "!", checked: true, disabled: false,
    advice: "New panes start on this computer through flatpak-spawn, with your rights and outside the sandbox. Panes that are already open keep running where they are.",
  },
  unavailable: {
    word: "not available", tone: "plain", mark: "?", checked: false, disabled: true,
    advice: "This Flatpak was started without the permission to run commands on the host (org.freedesktop.Flatpak), so panes can only start in the sandbox. See the Flatpak page of the documentation.",
  },
};
const NAME = "Run panes on the host";

/**
 * Open the page against the stub. Every case registers a complete init script (the platform, the whole view, every knob):
 * an earlier case's script stays and runs first, so a knob this one does not set would leak from the earlier one.
 */
async function open(page, url, { os = "linux", view = {}, statusFails = null, setFails = null, hold = false } = {}) {
  const full = { applies: true, state: "off", ...view };
  await page.addInitScript(
    ([o, v, status, set, held, settings]) => {
      window.__MOCK__.on("app_info", () => ({ version: "0.0.1-alpha1", os: o, hostname: "woodbox" }));
      window.__MOCK__.on("get_settings", () => structuredClone(settings));
      const shell = window.__MOCK__.hostShell;
      shell.set(v);
      shell.statusFails(status);
      shell.setFails(set);
      if (held) shell.hold();
      else shell.release();
    },
    [os, full, statusFails, setFails, hold, golden.settings_view],
  );
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("window_ready").length >= 1);
}

const settings = (page) => page.getByRole("dialog", { name: "Settings" });
const group = (page) => page.locator("fieldset.set-hs");
const box = (page) => page.getByRole("checkbox", { name: NAME });
const text = (page, selector) => page.locator(selector).first().textContent();
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

/** Open the Settings dialog on Linux in the desktop app whose shell reports `state` and wait for the group. */
async function groupOn(page, url, state, extra = {}) {
  await open(page, url, { view: { state }, ...extra });
  await openSettings(page);
  await group(page).waitFor();
}

/** The group sits after Accessibility and before the buttons; every state has its word, tone, mark, box and advice. */
async function statesCase(page, url) {
  for (const [state, want] of Object.entries(STATES)) {
    await groupOn(page, url, state);
    assert.equal(await group(page).count(), 1, state);
    const shape = await page.evaluate(() => {
      const sets = [...document.querySelectorAll(".dlg.settings .set-group")];
      const mine = document.querySelector("fieldset.set-hs");
      return {
        legends: sets.map((s) => s.querySelector("legend")?.textContent),
        index: sets.indexOf(mine),
        role: mine.getAttribute("role"),
        label: document.getElementById(mine.getAttribute("aria-labelledby"))?.textContent,
        beforeActions: (mine.compareDocumentPosition(document.querySelector(".dlg.settings .dlg-actions")) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0,
        inputs: mine.querySelectorAll("input").length,
        buttons: mine.querySelectorAll("button, a, [href]").length,
        statusRole: mine.querySelector(".set-hs-line")?.getAttribute("role"),
      };
    });
    assert.deepEqual(shape.legends.slice(-2), ["Accessibility", "Host shell"], `${state}: after Accessibility`);
    assert.equal(shape.index, shape.legends.length - 1);
    assert.equal(shape.role, "group");
    assert.equal(shape.label, "Host shell");
    assert.equal(shape.beforeActions, true, `${state}: before the buttons of the dialog`);
    assert.equal(shape.inputs, 1, `${state}: one checkbox`);
    assert.equal(shape.buttons, 0, `${state}: no button and no link in the group`);
    assert.equal(shape.statusRole, "status");
    assert.equal(await text(page, ".set-hs-name"), NAME);
    assert.equal(await text(page, ".set-hs-meta"), "Flatpak");
    const r = page.locator(".set-hs-row");
    assert.equal(await r.getAttribute("data-state"), state);
    assert.equal(await r.getAttribute("data-tone"), want.tone, state);
    assert.equal(await r.getAttribute("tabindex"), "-1");
    assert.equal(await text(page, ".set-hs-word"), want.word, state);
    assert.equal(await text(page, ".set-hs-mark"), want.mark, state);
    assert.equal(await text(page, ".set-hs-advice"), want.advice, state);
    assert.equal(await box(page).isChecked(), want.checked, `${state}: the box`);
    assert.equal(await box(page).isDisabled(), want.disabled, `${state}: the box can be pressed`);

    // The second cue besides the colour: the word always, and the mark, the underline and the dashed box for what needs attention.
    const cues = await page.evaluate(() => {
      const word = document.querySelector(".set-hs-word");
      const input = document.querySelector(".set-hs-name input");
      return {
        word: word.textContent,
        underline: getComputedStyle(word).textDecorationLine,
        markHidden: document.querySelector(".set-hs-mark").getAttribute("aria-hidden"),
        boxBorder: getComputedStyle(input).borderTopStyle,
      };
    });
    assert.equal(cues.word, want.word, `${state}: the word is text`);
    assert.equal(cues.underline, want.tone === "warn" ? "underline" : "none", `${state}: the underline`);
    assert.equal(cues.markHidden, "true", `${state}: the mark is not read twice`);
    if (state === "unavailable") assert.equal(cues.boxBorder, "dashed", "not available: a dashed box");

    // The state was read once, when the dialog opened, and nothing was sent.
    assert.equal((await calls(page, "host_shell_status")).length, 1, `${state}: one read`);
    assert.deepEqual(await calls(page, "host_shell_set"), [], `${state}: nothing sent`);
    await settle(page);
    assert.equal((await calls(page, "host_shell_status")).length, 1, `${state}: no timer, no focus read`);
    await closeSettings(page);
    assert.equal(await group(page).count(), 0, "the group left with the dialog");
  }
}

/** Anywhere else, and when the shell says no or fails, the group is not in the document at all. */
async function absentCase(page, url) {
  for (const os of ["macos", "windows", ""]) {
    await open(page, url, { os, view: { applies: true, state: "on" } });
    await openSettings(page);
    await settle(page);
    assert.equal(await group(page).count(), 0, `${JSON.stringify(os)}: no group`);
    assert.equal(await settings(page).getByText("Host shell").count(), 0);
    assert.deepEqual(await calls(page, "host_shell_status"), [], `${JSON.stringify(os)}: no host_shell_status`);
    assert.deepEqual(await calls(page, "host_shell_set"), []);
  }
  // Linux that is not a Flatpak: the shell says the group does not apply.
  await open(page, url, { view: { applies: false, state: "unavailable" } });
  await openSettings(page);
  await page.waitForFunction(() => window.__MOCK__.callsOf("host_shell_status").length >= 1);
  await settle(page);
  assert.equal(await group(page).count(), 0, "applies false: no group");
  assert.equal(await page.locator(".dlg.settings .set-hs-row, .dlg.settings .set-hs-name").count(), 0, "applies false: nothing of it");
  assert.equal(await settings(page).count(), 1, "the dialog itself is fine");
  // The shell fails: nothing is drawn and nothing is said.
  await open(page, url, { statusFails: "not a flatpak desktop", view: { applies: true, state: "on" } });
  await openSettings(page);
  await page.waitForFunction(() => window.__MOCK__.callsOf("host_shell_status").length >= 1);
  await settle(page);
  assert.equal(await group(page).count(), 0, "a failed read: no group");
  // A word the page does not know reads as the safe one (and an answer that is no view draws nothing).
  await open(page, url, { view: { applies: true, state: "<b>on</b>" } });
  await openSettings(page);
  await group(page).waitFor();
  assert.equal(await text(page, ".set-hs-word"), "not available");
  assert.equal(await box(page).isDisabled(), true);
  assert.equal(await page.locator(".set-hs-row b").count(), 0);
  await open(page, url, { view: { applies: "yes" } });
  await openSettings(page);
  await page.waitForFunction(() => window.__MOCK__.callsOf("host_shell_status").length >= 1);
  await settle(page);
  assert.equal(await group(page).count(), 0, "not a view: no group");
}

/** Space on the focused box sends the choice and the group redraws from the answer. */
async function toggleCase(page, url) {
  await groupOn(page, url, "off");
  await box(page).focus();
  assert.equal(await box(page).evaluate((n) => document.activeElement === n), true);
  await page.keyboard.press("Space");
  await page.waitForFunction(() => window.__MOCK__.callsOf("host_shell_set").length >= 1);
  await page.waitForFunction(() => document.querySelector(".set-hs-word")?.textContent === "on");
  assert.deepEqual(await calls(page, "host_shell_set"), [{ enabled: true }]);
  assert.equal(await box(page).isChecked(), true);
  assert.equal(await text(page, ".set-hs-mark"), "!");
  assert.equal(await text(page, ".set-hs-advice"), STATES.on.advice);
  assert.equal(await page.locator(".set-hs-row").getAttribute("data-tone"), "warn");
  assert.equal(await text(page, ".set-hs-line"), "");
  assert.equal(await box(page).evaluate((n) => document.activeElement === n), true, "the keyboard stays on the box");

  await page.keyboard.press("Space");
  await page.waitForFunction(() => document.querySelector(".set-hs-word")?.textContent === "off");
  assert.deepEqual(await calls(page, "host_shell_set"), [{ enabled: true }, { enabled: false }]);
  assert.equal(await box(page).isChecked(), false);
  assert.equal(await text(page, ".set-hs-mark"), "");
  assert.equal(await text(page, ".set-hs-advice"), STATES.off.advice);
  // The choice is a fact of the shell: opening the dialog again reads it.
  await page.keyboard.press("Space");
  await page.waitForFunction(() => document.querySelector(".set-hs-word")?.textContent === "on");
  await closeSettings(page);
  await openSettings(page);
  await group(page).waitFor();
  await page.waitForFunction(() => document.querySelector(".set-hs-word")?.textContent === "on");
  assert.equal(await box(page).isChecked(), true);
  assert.equal((await calls(page, "host_shell_status")).length, 2, "one read per opening of the dialog");
  // A box that cannot be pressed sends nothing.
  await closeSettings(page);
  await groupOn(page, url, "unavailable");
  await box(page).focus({ timeout: 500 }).catch(() => {});
  await page.keyboard.press("Space");
  await settle(page);
  assert.deepEqual(await calls(page, "host_shell_set"), [], "unavailable: nothing is sent");
}

/** A refusal restores the box and says why as bounded plain text; the next press works. */
async function refusalCase(page, url) {
  const hostile = "<img src=x onerror=boom()> no\u0007 permission\u0000\nhere";
  await groupOn(page, url, "off", { setFails: hostile });
  await box(page).focus();
  await page.keyboard.press("Space");
  await page.waitForFunction(() => document.querySelector(".set-hs-line")?.textContent !== "");
  assert.equal(await box(page).isChecked(), false, "the box goes back to what the shell holds");
  assert.equal(await text(page, ".set-hs-word"), "off");
  const line = await page.evaluate(() => {
    const n = document.querySelector(".set-hs-line");
    return { text: n.textContent, role: n.getAttribute("role"), children: n.children.length, imgs: document.querySelectorAll(".set-hs img").length };
  });
  assert.equal(line.text, "<img src=x onerror=boom()> no  permission  here", "the reason is plain text with the control characters as blanks");
  assert.equal(line.role, "status");
  assert.equal(line.children, 0);
  assert.equal(line.imgs, 0);
  assert.equal(await box(page).evaluate((n) => document.activeElement === n), true, "the keyboard stays on the box");

  // A very long reason is shown bounded.
  await groupOn(page, url, "off", { setFails: "x".repeat(5000) });
  await box(page).focus();
  await page.keyboard.press("Space");
  await page.waitForFunction(() => document.querySelector(".set-hs-line")?.textContent !== "");
  const long = await text(page, ".set-hs-line");
  assert.ok(long.length <= 240 && long.length > 100, `bounded: ${long.length}`);
  assert.equal(await box(page).isChecked(), false);

  // A refusal with nothing to say shows the plain sentence, and a later press that works clears the line.
  await page.evaluate(() => window.__MOCK__.hostShell.setFails("   "));
  await page.keyboard.press("Space");
  await page.waitForFunction(() => document.querySelector(".set-hs-line")?.textContent === "The command failed.");
  await page.evaluate(() => window.__MOCK__.hostShell.setFails(null));
  await page.keyboard.press("Space");
  await page.waitForFunction(() => document.querySelector(".set-hs-word")?.textContent === "on");
  assert.equal(await text(page, ".set-hs-line"), "");
  assert.equal(await box(page).isChecked(), true);
}

/** The shell answers after the dialog is closed: the group never appears. */
async function lateCase(page, url) {
  await open(page, url, { hold: true });
  await openSettings(page);
  await page.waitForFunction(() => window.__MOCK__.callsOf("host_shell_status").length >= 1);
  assert.equal(await group(page).count(), 0, "nothing before the shell answers");
  await closeSettings(page);
  await page.evaluate(() => window.__MOCK__.hostShell.release());
  await settle(page);
  assert.equal(await group(page).count(), 0, "a late answer draws nothing");
  assert.equal(await page.locator(".set-hs-row").count(), 0);
}

/** Keyboard ring, sizes, higher contrast and forced colours keep the group readable. */
async function accessibilityCase(page, url) {
  await groupOn(page, url, "on");
  const label = await box(page).evaluate((n) => ({
    name: n.labels?.[0]?.textContent ?? null,
    group: n.closest("fieldset")?.getAttribute("aria-labelledby") ?? null,
  }));
  assert.equal(label.name, NAME, "the box has a label");
  assert.equal(label.group, "set-hs-legend");
  assert.equal(await page.getByRole("group", { name: "Host shell" }).count(), 1);

  // Sizes: the row and the label are at least 28 px high, the root is 12 px.
  const sizes = await page.evaluate(() => ({
    root: parseFloat(getComputedStyle(document.documentElement).fontSize),
    row: document.querySelector(".set-hs-row").getBoundingClientRect().height,
    name: document.querySelector(".set-hs-name").getBoundingClientRect().height,
    animation: getComputedStyle(document.querySelector(".set-hs-row")).animationName,
    transition: getComputedStyle(document.querySelector(".set-hs-row")).transitionDuration,
  }));
  assert.equal(sizes.root, 12);
  assert.ok(sizes.row >= 28, `the row is at least 28 px high: ${sizes.row}`);
  assert.ok(sizes.name >= 28, `the target is at least 28 px high: ${sizes.name}`);
  assert.equal(sizes.animation, "none", "nothing moves");
  assert.equal(sizes.transition, "0s", "nothing moves");

  // The keyboard ring is 2 px on the box and on the row.
  await box(page).focus();
  await page.keyboard.press("Shift+Tab");
  await page.keyboard.press("Tab");
  const ring = await box(page).evaluate((n) => {
    const s = getComputedStyle(n);
    return { focused: document.activeElement === n, style: s.outlineStyle, width: s.outlineWidth };
  });
  assert.equal(ring.focused, true);
  assert.equal(ring.style, "solid");
  assert.equal(ring.width, "2px");
  await page.locator(".set-hs-row").evaluate((n) => n.focus());
  const rowRing = await page.locator(".set-hs-row").evaluate((n) => {
    const s = getComputedStyle(n);
    return { style: s.outlineStyle, width: s.outlineWidth };
  });
  assert.equal(rowRing.width, "2px", "the row has the ring too");

  await page.evaluate(() => document.documentElement.classList.add("contrast-high"));
  const high = await page.evaluate(() => ({
    border: getComputedStyle(document.querySelector("fieldset.set-hs")).borderTopStyle,
    line: getComputedStyle(document.querySelector(".set-hs-word")).textDecorationLine,
    word: document.querySelector(".set-hs-word").textContent,
    mark: document.querySelector(".set-hs-mark").textContent,
  }));
  assert.equal(high.border, "solid");
  assert.equal(high.line, "underline");
  assert.equal(high.word, "on");
  assert.equal(high.mark, "!");
  await page.evaluate(() => document.documentElement.classList.remove("contrast-high"));

  await page.emulateMedia({ forcedColors: "active" });
  try {
    const forced = await page.evaluate(() => {
      const r = document.querySelector(".set-hs-row");
      const s = getComputedStyle(r);
      return {
        outline: s.outlineStyle, width: s.outlineWidth,
        border: getComputedStyle(document.querySelector("fieldset.set-hs")).borderTopStyle,
        word: document.querySelector(".set-hs-word").textContent,
        mark: document.querySelector(".set-hs-mark").textContent,
        underline: getComputedStyle(document.querySelector(".set-hs-word")).textDecorationLine,
      };
    });
    assert.equal(forced.outline, "solid", "the row has a system-colour outline");
    assert.equal(forced.width, "2px");
    assert.equal(forced.border, "solid");
    assert.equal(forced.word, "on");
    assert.equal(forced.mark, "!");
    assert.equal(forced.underline, "underline");
  } finally {
    await page.emulateMedia({ forcedColors: null });
  }

  // The dashed box of `not available` stays dashed under forced colours.
  await closeSettings(page);
  await groupOn(page, url, "unavailable");
  await page.emulateMedia({ forcedColors: "active" });
  try {
    const dashed = await page.locator(".set-hs-name input").evaluate((n) => getComputedStyle(n).borderTopStyle);
    assert.equal(dashed, "dashed");
    assert.equal(await text(page, ".set-hs-word"), "not available");
    assert.equal(await text(page, ".set-hs-mark"), "?");
  } finally {
    await page.emulateMedia({ forcedColors: null });
  }
}

/** The browser build has no such group, whatever the platform says, and never asks the shell. */
async function webCase(page, url) {
  await open(page, url, { os: "linux", view: { applies: true, state: "on" } });
  await openSettings(page);
  await settle(page);
  assert.equal(await group(page).count(), 0, "web: no group");
  assert.equal(await settings(page).getByText("Host shell").count(), 0);
  assert.deepEqual(await calls(page, "host_shell_status"), [], "web: no host_shell_status");
  assert.deepEqual(await calls(page, "host_shell_set"), [], "web: no host_shell_set");
}

/** The shipped sources: no markup built from a string, no colour literal, no pixel font size, no outside control. */
async function sourceCase() {
  const dist = join(APP_DIR, "dist");
  for (const name of ["hostshell.js", "hostshellstate.js"]) {
    const code = readFileSync(join(dist, name), "utf8");
    assert.doesNotMatch(code, /cssText|innerHTML|outerHTML|insertAdjacentHTML|setAttribute\(\s*["']style/, name);
    assert.doesNotMatch(code, /override|flatseal|href|open_url|reveal_path/i, `${name}: no control for a tool outside the app`);
  }
  const css = readFileSync(join(dist, "hostshell.css"), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
  const colours = css.match(/#[0-9a-fA-F]{3,8}\b|\b(?:rgb|rgba|hsl|hsla)\(/g) ?? [];
  assert.deepEqual(colours, [], `hostshell.css hard-codes colours: ${colours.join(", ")}`);
  assert.deepEqual(css.match(/font-size:\s*\d+(?:\.\d+)?px/g) ?? [], [], "hostshell.css has a pixel font size");
  assert.doesNotMatch(css, /gradient|blur|box-shadow|animation|transition/, "hostshell.css is flat and still");
}
