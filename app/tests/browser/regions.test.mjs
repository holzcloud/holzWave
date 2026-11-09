// The keyboard reaches the window bar (docs/design.md section 6.1): F6 and Shift+F6 move it between the terminal and the bar, the
// palette entry `Focus the bar` goes to the bar from anywhere, and while a control of the bar has the focus the control keeps Tab,
// Shift+Tab, Enter and Space and the terminal is sent nothing. In the terminal's own input Tab stays the terminal's, a text field
// of the page keeps its own keys, Escape from the bar gives the keyboard back, and a program on the alternate screen keeps F6.
// Every case reads the commands the page sent to the stub: the keys of the terminal are `key` commands. The press of a key is a
// real one (Playwright's keyboard), so the engine's own default (Tab moves, Enter presses) is what is checked.
import assert from "node:assert/strict";
import { callsOf, currentBackend } from "./lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await f6Case(page, url);
  await walkCase(page, url);
  await pressCase(page, url);
  await terminalTabCase(page, url);
  await fieldCase(page, url);
  await escapeCase(page, url);
  await paletteCase(page, url);
  await alternateScreenCase(page, url);
  // The window buttons are the shell's facts for a desktop window: the web backend (a page in a browser) has none.
  if (currentBackend() === "tauri") await windowButtonsCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** Open the page against the stub with the terminal input holding the keyboard. */
async function open(page, url, { os = null } = {}) {
  if (os !== null) {
    await page.addInitScript((name) => window.__MOCK__.on("app_info", () => ({ version: "0.0.1-alpha1", os: name, hostname: "woodbox" })), os);
  }
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await page.evaluate(() => document.getElementById("term").focus());
  await page.waitForFunction(() => document.activeElement?.id === "ime");
  await page.evaluate(() => window.__MOCK__.reset());
}

/** What has the keyboard: `ime` for the terminal input, `bar` for a control of the bar, else the tag and id of the element. */
const where = (page) =>
  page.evaluate(() => {
    const a = document.activeElement;
    if (a === null || a === document.body) return "body";
    if (a.id === "ime") return "ime";
    if (a.closest("#bar")) return "bar";
    return `${a.tagName.toLowerCase()}#${a.id}`;
  });

/** The element that has the keyboard, described for a message and for a comparison. */
const focused = (page) =>
  page.evaluate(() => {
    const a = document.activeElement;
    return a === null ? "" : `${a.tagName.toLowerCase()}.${a.className}|${a.dataset.button ?? ""}|${a.getAttribute("aria-label") ?? a.textContent}`;
  });

/** The commands that would send something to the shell. */
const shellCalls = (page) =>
  page.evaluate(() => window.__MOCK__.calls.filter(([c]) => ["key", "text", "paste"].includes(c)).map(([c, a]) => (c === "key" ? `key ${a.code}` : c)));

/** The controls of the bar that the keyboard reaches, counted the way the page counts them. */
const controlCount = (page) =>
  page.evaluate(
    () =>
      [...document.querySelectorAll("#bar button, #bar a[href], #bar [role='button'], #bar [tabindex]")].filter(
        (n) => n.tabIndex >= 0 && !n.disabled && n.getClientRects().length > 0,
      ).length,
  );

/** F6 takes the keyboard from the terminal to the bar and back; Shift+F6 does the same and lands on the last control. */
async function f6Case(page, url) {
  await open(page, url);
  assert.equal(await where(page), "ime");
  await page.keyboard.press("F6");
  assert.equal(await where(page), "bar", "F6 from the terminal moves the keyboard to the bar");
  const first = await page.evaluate(() => document.activeElement === [...document.querySelectorAll("#bar button")][0]);
  assert.equal(first, true, "it lands on the first control of the bar");
  assert.deepEqual(await shellCalls(page), [], "F6 is not sent to the terminal");
  await page.keyboard.press("F6");
  assert.equal(await where(page), "ime", "F6 from the bar gives the keyboard back to the terminal");
  assert.deepEqual(await shellCalls(page), [], "and still nothing was sent");

  await page.keyboard.press("Shift+F6");
  assert.equal(await where(page), "bar", "Shift+F6 from the terminal moves the keyboard to the bar");
  const last = await page.evaluate(() => {
    const all = [...document.querySelectorAll("#bar button")].filter((n) => n.getClientRects().length > 0 && !n.disabled);
    return document.activeElement === all[all.length - 1];
  });
  assert.equal(last, true, "it lands on the last control");
  await page.keyboard.press("Shift+F6");
  assert.equal(await where(page), "ime", "Shift+F6 from the bar gives the keyboard back");
  assert.deepEqual(await shellCalls(page), [], "no F6 reached the terminal");

  // A held key is one press: the action is not repeatable, so the repeats are consumed and the keyboard stays in the bar.
  await page.keyboard.press("F6");
  for (let i = 0; i < 3; i += 1) await page.keyboard.down("F6");
  await page.keyboard.up("F6");
  assert.equal(await where(page), "ime", "the first press of the held key went back, the repeats did nothing");
  assert.deepEqual(await shellCalls(page), [], "no F6 reached the terminal");
}

/** Tab and Shift+Tab walk through every control of the bar, and nothing they do reaches the terminal. */
async function walkCase(page, url) {
  await open(page, url);
  const count = await controlCount(page);
  assert.ok(count >= 4, `the bar has controls to walk through (${count})`);
  await page.keyboard.press("F6");
  const seen = new Set([await focused(page)]);
  let steps = 0;
  while (steps < count + 2 && (await where(page)) === "bar") {
    await page.keyboard.press("Tab");
    steps += 1;
    if ((await where(page)) === "bar") seen.add(await focused(page));
  }
  assert.equal(steps, count, `Tab walks the controls one by one and leaves the bar after the last (${steps} presses for ${count} controls)`);
  assert.equal(seen.size, count, `Tab reached every control of the bar once (${seen.size} of ${count})`);
  assert.equal(await where(page), "ime", "past the last control the keyboard is the terminal's again, not lost");
  assert.deepEqual(await shellCalls(page), [], "no Tab reached the terminal");

  // Shift+Tab walks back: from the second control to the first, and past the first it gives the keyboard to the terminal.
  await page.keyboard.press("F6");
  const firstControl = await focused(page);
  await page.keyboard.press("Tab");
  const second = await focused(page);
  assert.notEqual(second, firstControl, "Tab moved on");
  await page.keyboard.press("Shift+Tab");
  assert.equal(await focused(page), firstControl, "Shift+Tab moved back to the first control");
  await page.keyboard.press("Shift+Tab");
  assert.equal(await where(page), "ime", "Shift+Tab past the first control gives the keyboard to the terminal");
  assert.deepEqual(await shellCalls(page), [], "no Shift+Tab reached the terminal");
}

/** Enter and Space press the focused button, and neither is sent to the terminal; a pressed button hands the keyboard back. */
async function pressCase(page, url) {
  await open(page, url);
  const plus = (title) => page.evaluate((t) => { document.querySelector(`#bar button[title="${t}"]`).focus(); }, title);

  await plus("New tab");
  assert.equal(await where(page), "bar");
  await page.keyboard.press("Enter");
  assert.equal((await callsOf(page, "new_tab")).length, 1, "Enter pressed the focused button");
  assert.deepEqual(await shellCalls(page), [], "Enter on a bar button is not sent to the terminal");
  assert.equal(await where(page), "ime", "the button gave the keyboard back to the terminal, as a click does");

  await plus("New workspace");
  await page.keyboard.press("Space");
  assert.equal((await callsOf(page, "new_workspace")).length, 1, "Space pressed the focused button");
  assert.deepEqual(await shellCalls(page), [], "Space on a bar button is not sent to the terminal");
  assert.equal(await where(page), "ime");

  // The arrows, Enter and Space are not the terminal's while a control has the focus either: nothing is sent and the key keeps its
  // default. Tab is the one the page takes itself (it walks the bar), and it sends nothing as well.
  await plus("New tab");
  const prevented = await page.evaluate(() => {
    const out = [];
    for (const key of ["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Tab", "Enter", " "]) {
      const e = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, composed: true });
      document.activeElement.dispatchEvent(e);
      out.push(e.defaultPrevented);
    }
    return out;
  });
  assert.deepEqual(prevented, [false, false, false, false, true, false, false], "the page prevents nothing of what a control keeps but Tab, which it walks");
  assert.deepEqual(await shellCalls(page), [], "and sends nothing");

  // A key that is none of these still goes to the terminal, as before: a letter typed while a bar button has the focus.
  await page.keyboard.press("x");
  assert.deepEqual(await shellCalls(page), ["key KeyX", "key KeyX"], "a letter is still the terminal's");
}

/** In the terminal's own input Tab is a key for the terminal, and so are Enter and the arrows. */
async function terminalTabCase(page, url) {
  await open(page, url);
  await page.keyboard.press("Tab");
  assert.deepEqual(await shellCalls(page), ["key Tab", "key Tab"], "Tab in the terminal is sent (press and release)");
  assert.equal(await where(page), "ime", "and the keyboard stays in the terminal input");
  await page.evaluate(() => window.__MOCK__.reset());
  await page.keyboard.press("Shift+Tab");
  assert.deepEqual(await shellCalls(page), ["key Tab", "key Tab"], "Shift+Tab is the terminal's too");
  assert.equal(await where(page), "ime");
  await page.evaluate(() => window.__MOCK__.reset());
  await page.keyboard.press("Enter");
  await page.keyboard.press("ArrowLeft");
  assert.deepEqual(await shellCalls(page), ["key Enter", "key Enter", "key ArrowLeft", "key ArrowLeft"], "Enter and the arrows are sent");
  assert.equal(await where(page), "ime");
}

/** A text field of the page keeps its own keys, in the bar and out of it. */
async function fieldCase(page, url) {
  await open(page, url);
  await page.evaluate(() => {
    const input = document.createElement("input");
    input.id = "field";
    const area = document.createElement("textarea");
    area.id = "area";
    document.body.append(input, area);
    input.focus();
  });
  await page.keyboard.type("hi");
  assert.equal(await page.evaluate(() => document.getElementById("field").value), "hi", "typing reached the field");
  await page.keyboard.press("Enter");
  await page.keyboard.press("ArrowLeft");
  assert.deepEqual(await shellCalls(page), [], "nothing of a text field reaches the terminal");
  assert.equal(await where(page), "input#field", "the field keeps the keyboard");
  // F6 is a chord and works from a field as well: the keyboard goes to the bar, and a letter typed there is not the field's.
  await page.keyboard.press("F6");
  assert.equal(await where(page), "bar");
  await page.keyboard.press("Escape");
  assert.equal(await where(page), "ime", "Escape from the bar goes to the terminal, not back to the field");
  // Escape in the field itself is not taken by the page.
  await page.evaluate(() => document.getElementById("area").focus());
  await page.keyboard.press("Escape");
  assert.equal(await where(page), "textarea#area", "Escape in a field outside the bar leaves the field alone");
  assert.deepEqual(await shellCalls(page), [], "no key of the field reached the terminal");
}

/** Escape on a control of the bar gives the keyboard to the terminal and sends nothing. */
async function escapeCase(page, url) {
  await open(page, url);
  await page.keyboard.press("F6");
  await page.keyboard.press("Tab");
  await page.keyboard.press("Tab");
  assert.equal(await where(page), "bar");
  await page.keyboard.press("Escape");
  assert.equal(await where(page), "ime", "Escape returns the keyboard to the terminal");
  assert.deepEqual(await shellCalls(page), [], "Escape was not sent to the terminal");
  // From the terminal Escape is the terminal's, as before.
  await page.keyboard.press("Escape");
  assert.deepEqual(await shellCalls(page), ["key Escape", "key Escape"], "Escape in the terminal is sent");
}

/** The palette entry `Focus the bar` takes the keyboard to the bar, and the way back is F6 or Escape. */
async function paletteCase(page, url) {
  await open(page, url);
  await page.keyboard.press("Meta+k");
  await page.waitForFunction(() => document.activeElement?.classList.contains("palette-input") === true);
  await page.keyboard.type("Focus the bar");
  const titles = await page.evaluate(() => [...document.querySelectorAll(".palette-row .palette-title")].map((n) => n.textContent));
  assert.equal(titles[0], "Focus the bar", `the entry is the best match (${titles.join(", ")})`);
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => document.activeElement?.closest("#bar") !== null && document.activeElement?.closest("#bar") !== undefined);
  assert.equal(await where(page), "bar", "the entry moved the keyboard to the bar, after the palette closed and gave it back");
  assert.deepEqual(await shellCalls(page), [], "nothing was sent");
  await page.keyboard.press("F6");
  assert.equal(await where(page), "ime");
  assert.equal(
    await page.evaluate(() => document.querySelector(".palette-backdrop")?.hidden !== false),
    true,
    "the palette is closed",
  );
}

/** One frame message for `pane` with the screen of a full-screen program when `alt` is true. */
const frame = (pane, alt) => ({
  pane, seq: 1, full: true, cols: 60, rows: 4, fg: 0xf6efe6, bg: 0x14091a,
  cursor: { x: 0, y: 0, style: "block", blink: false },
  scrollbar: [0, 4, 4], selection: [], matches: [], search: null, copy_cursor: null,
  modes: { alt, bracketed_paste: false, mouse: false }, changed: [],
});

/** A program on the alternate screen reads F6 itself (htop, mc); the palette entry still reaches the bar. */
async function alternateScreenCase(page, url) {
  await open(page, url);
  const focusPane = await page.evaluate(async () => (await window.__MOCK__.invoke("get_layout")).focus);
  await page.evaluate((f) => window.__MOCK__.emit("frame", f), frame(focusPane, true));
  await page.waitForTimeout(50);
  await page.keyboard.press("F6");
  assert.equal(await where(page), "ime", "F6 stays with the program on the alternate screen");
  assert.deepEqual(await shellCalls(page), ["key F6", "key F6"], "and it is sent to it");
  await page.evaluate(() => window.__MOCK__.reset());
  await page.keyboard.press("Meta+k");
  await page.waitForFunction(() => document.activeElement?.classList.contains("palette-input") === true);
  await page.keyboard.type("Focus the bar");
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => document.activeElement?.closest("#bar") instanceof Element);
  assert.equal(await where(page), "bar", "the palette entry reaches the bar over the program");
  await page.keyboard.press("F6");
  assert.equal(await where(page), "ime", "from the bar F6 is the page's again");
  assert.deepEqual(await shellCalls(page), [], "nothing reached the program");
  await page.evaluate((f) => window.__MOCK__.emit("frame", f), frame(focusPane, false));
  await page.waitForTimeout(50);
  await page.keyboard.press("F6");
  assert.equal(await where(page), "bar", "with the normal screen back, F6 is the page's");
}

/** The three window buttons of a Windows window are controls of the bar like the rest: Tab reaches each and Enter presses it. */
async function windowButtonsCase(page, url) {
  await page.addInitScript(() => {
    window.__HW_FRAME__ = { os: "windows", style: "flat", side: "right", buttons: ["minimize", "maximize", "close"] };
    window.__WIN = { maximized: false };
    window.__MOCK__.on("plugin:window|is_maximized", () => window.__WIN.maximized);
    window.__MOCK__.on("plugin:window|is_fullscreen", () => false);
  });
  await open(page, url, { os: "windows" });
  await page.waitForSelector('#bar .bar-window [data-button="close"]');
  await page.keyboard.press("Shift+F6");
  assert.equal(await page.evaluate(() => document.activeElement?.dataset.button), "close", "Shift+F6 lands on the close button, the last control");
  await page.keyboard.press("Shift+Tab");
  assert.equal(await page.evaluate(() => document.activeElement?.dataset.button), "maximize", "Shift+Tab reaches Maximize");
  await page.keyboard.press("Shift+Tab");
  assert.equal(await page.evaluate(() => document.activeElement?.dataset.button), "minimize", "and Minimize");
  await page.evaluate(() => window.__MOCK__.reset());
  await page.keyboard.press("Enter");
  const calls = await page.evaluate(() => window.__MOCK__.calls.map(([c]) => c).filter((c) => c.startsWith("plugin:window|")));
  assert.ok(calls.includes("plugin:window|minimize"), `Enter pressed Minimize (${calls.join(", ")})`);
  assert.deepEqual(await shellCalls(page), [], "no key of this reached the terminal");
}
