// The command palette (design.md section 6 rule 7): Cmd+K opens a fuzzy list of every registered action with its
// section and chord, the keyboard drives it, Enter runs the highlighted action, Escape gives the terminal its
// keyboard back, and while it is open no key reaches the program behind it. The browser stub plays the core.
import assert from "node:assert/strict";
import { callsOf } from "./lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await openCase(page, url);
  await filterCase(page, url);
  await keyboardCase(page, url);
  await escapeCase(page, url);
  await noLeakCase(page, url);
  await emptyCase(page, url);
  await pointerCase(page, url);
  await toggleCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** Open the page against the stub; the terminal input has the keyboard. */
async function open(page, url) {
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await page.evaluate(async () => {
    // The page's own registry, for synchronous predicates and for what the test registers late.
    window.__registry = await import(new URL("actions.js", location.href).href);
  });
  await page.evaluate(() => document.getElementById("term").focus());
  await page.waitForFunction(() => document.activeElement?.id === "ime");
}

const reset = (page) => page.evaluate(() => window.__MOCK__.reset());

/** The calls that would send something to the shell. */
const shellCalls = (page) =>
  page.evaluate(() => window.__MOCK__.calls.filter(([c]) => ["key", "text", "paste"].includes(c)).map(([c]) => c));

/** Open the palette with the shortcut and wait until its field has the keyboard. */
async function openPalette(page) {
  await page.keyboard.press("Meta+k");
  await page.waitForFunction(() => document.activeElement?.classList.contains("palette-input") === true);
}

const isOpen = (page) =>
  page.evaluate(() => {
    const b = document.querySelector(".palette-backdrop");
    return b !== null && !b.hidden;
  });

/** The rows as { title, section, chord, selected } in list order. */
const rows = (page) =>
  page.evaluate(() =>
    [...document.querySelectorAll(".palette-row")].map((r) => ({
      id: r.id,
      title: r.querySelector(".palette-title").textContent,
      section: r.querySelector(".palette-section").textContent,
      chord: r.querySelector(".palette-chord").textContent,
      selected: r.getAttribute("aria-selected") === "true",
    })),
  );

const activeTitle = async (page) => (await rows(page)).find((r) => r.selected)?.title;
const registeredTitles = (page) => page.evaluate(() => window.__registry.listActions().map((a) => a.title));

/** `needle` is a subsequence of `text`, ignoring case. */
function subsequence(needle, text) {
  let at = 0;
  for (const ch of text.toLowerCase()) if (ch === needle[at]) at++;
  return at === needle.length;
}

/** Cmd+K lists every registered action, in registration order, with section and chord; the field has the focus. */
async function openCase(page, url) {
  await open(page, url);
  assert.equal(await page.locator(".palette-backdrop").count(), 0, "no palette before the shortcut");
  await openPalette(page);

  const input = page.locator(".palette-input");
  assert.equal(await input.evaluate((n) => n.tagName), "INPUT");
  assert.equal(await input.inputValue(), "");
  assert.equal(await page.locator(".palette").getAttribute("role"), "dialog");
  assert.equal(await page.locator(".palette").getAttribute("aria-modal"), "true");
  assert.equal(await page.locator(".palette-list").getAttribute("role"), "listbox");
  assert.equal(await input.getAttribute("role"), "combobox");

  const all = await rows(page);
  const titles = await registeredTitles(page);
  assert.ok(titles.length >= 30, `the page registers its actions (${titles.length})`);
  assert.deepEqual(all.map((r) => r.title), titles, "every registered action, in registration order");
  assert.equal(await page.locator(".palette-row").first().getAttribute("role"), "option");

  const split = all.find((r) => r.title === "Split pane right");
  assert.equal(split?.section, "pane");
  assert.equal(split?.chord, "Cmd+D");
  const copy = all.find((r) => r.title === "Enter copy mode");
  assert.equal(copy?.section, "copy mode");
  assert.equal(copy?.chord, "Cmd+Shift+X");
  assert.equal(all.find((r) => r.title === "Open command palette")?.chord, "Cmd+K");

  assert.equal(all.filter((r) => r.selected).length, 1, "exactly one active row");
  assert.equal(all[0].selected, true, "the first row is active");
  assert.equal(await input.getAttribute("aria-activedescendant"), all[0].id);
  assert.ok(await page.locator(".palette").isVisible());
}

/** Typing narrows the list to the actions the query is a subsequence of, best match first. */
async function filterCase(page, url) {
  await open(page, url);
  await openPalette(page);
  const total = (await rows(page)).length;
  await page.keyboard.type("split");

  const found = await rows(page);
  assert.ok(found.length >= 2 && found.length < total, `narrowed from ${total} to ${found.length}`);
  for (const r of found) assert.ok(subsequence("split", r.title), `"${r.title}" matches "split"`);
  assert.deepEqual(
    found.slice(0, 2).map((r) => r.title).sort(),
    ["Split pane down", "Split pane right"],
    "the two split actions rank first",
  );
  assert.equal(found[0].selected, true, "the best match is active");
  assert.equal(await page.locator(".palette-input").getAttribute("aria-activedescendant"), found[0].id);

  // Case does not matter and a longer query narrows further.
  await page.locator(".palette-input").fill("");
  await page.keyboard.type("SPLIT PANE R");
  assert.deepEqual((await rows(page)).map((r) => r.title)[0], "Split pane right");

  // A section is not part of the matched text: a query for a title word still finds it.
  await page.locator(".palette-input").fill("");
  await page.keyboard.type("tab");
  const tabs = (await rows(page)).map((r) => r.title);
  assert.ok(tabs.includes("New tab"));
  assert.ok(tabs.indexOf("New tab") < tabs.indexOf("Select tab 1") || !tabs.includes("Select tab 1"));
}

/** Up and Down move the active row (and wrap), Enter runs exactly the highlighted action and closes. */
async function keyboardCase(page, url) {
  await open(page, url);
  await openPalette(page);
  await reset(page);
  await page.keyboard.type("split");
  const found = await rows(page);

  await page.keyboard.press("ArrowDown");
  let now = await rows(page);
  assert.equal(now[1].selected, true, "ArrowDown moves to the second row");
  assert.equal(now.filter((r) => r.selected).length, 1);
  assert.equal(await page.locator(".palette-input").getAttribute("aria-activedescendant"), now[1].id);
  await page.keyboard.press("ArrowUp");
  assert.equal((await rows(page))[0].selected, true, "ArrowUp moves back");
  await page.keyboard.press("ArrowUp");
  now = await rows(page);
  assert.equal(now[now.length - 1].selected, true, "ArrowUp from the first row wraps to the last");
  await page.keyboard.press("ArrowDown");
  assert.equal((await rows(page))[0].selected, true, "ArrowDown from the last row wraps to the first");

  // Enter runs the highlighted one: the second split action, which is not the best match.
  await page.keyboard.press("ArrowDown");
  const picked = await activeTitle(page);
  assert.equal(picked, found[1].title);
  const expected = { "Split pane right": false, "Split pane down": true }[picked];
  assert.notEqual(expected, undefined, `"${picked}" is one of the split actions`);
  await page.keyboard.press("Enter");

  await page.waitForFunction(() => window.__MOCK__.callsOf("split").length >= 1);
  assert.deepEqual(await callsOf(page, "split"), [{ column: expected }], "the highlighted action ran, and only it");
  assert.equal(await isOpen(page), false, "Enter closes the palette");
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime", "the terminal input has the keyboard");
  assert.deepEqual(await shellCalls(page), [], "Enter and the arrows reached no program");
}

/** Escape closes, gives the terminal its keyboard back, and a key then reaches the program again. */
async function escapeCase(page, url) {
  await open(page, url);
  await openPalette(page);
  await reset(page);
  await page.keyboard.type("spl");
  await page.keyboard.press("Escape");
  assert.equal(await isOpen(page), false, "Escape closes");
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime", "focus is back in the terminal input");
  assert.deepEqual(await callsOf(page, "split"), [], "Escape runs nothing");
  assert.deepEqual(await shellCalls(page), [], "nothing typed in the palette reached the program");

  await page.keyboard.press("x");
  await page.waitForFunction(() => window.__MOCK__.callsOf("key").length >= 1);
  assert.equal((await callsOf(page, "key"))[0].key, "x", "the terminal has its keys back");

  // Opened again, the palette starts empty with every action listed.
  await openPalette(page);
  assert.equal(await page.locator(".palette-input").inputValue(), "");
  assert.equal((await rows(page)).length, (await registeredTitles(page)).length);
  await page.keyboard.press("Escape");
}

/** While the palette is open no key becomes a command or program input, not even a shortcut that is not its own. */
async function noLeakCase(page, url) {
  await open(page, url);
  await openPalette(page);
  await reset(page);
  await page.keyboard.type("hello world");
  for (const k of ["Backspace", "ArrowLeft", "ArrowRight", "Tab", "Shift+Tab", "Control+c", "Control+d", "F5", "Meta+w", "Meta+t", "Meta+d", "Meta+Shift+n"]) {
    await page.keyboard.press(k);
  }
  assert.deepEqual(await shellCalls(page), [], "no key, text or paste command");
  for (const cmd of ["close_pane", "new_tab", "split", "new_workspace"]) {
    assert.deepEqual(await callsOf(page, cmd), [], `${cmd} was not run by a shortcut behind the palette`);
  }
  assert.equal(await isOpen(page), true, "the palette is still open");
  assert.ok(
    await page.evaluate(() => document.activeElement?.classList.contains("palette-input") === true),
    "Tab keeps the focus in the field",
  );
  await page.keyboard.press("Escape");
}

/** A query nothing matches shows a message, no rows, and Enter runs nothing. */
async function emptyCase(page, url) {
  await open(page, url);
  await openPalette(page);
  await reset(page);
  await page.keyboard.type("zzzzqq");
  assert.equal((await rows(page)).length, 0);
  const message = await page.locator(".palette-empty").textContent();
  assert.match(message, /zzzzqq/, "the message names the query");
  assert.ok(await page.locator(".palette-empty").isVisible());
  assert.equal(await page.locator(".palette-input").getAttribute("aria-activedescendant"), null);
  await page.keyboard.press("Enter");
  await page.keyboard.press("ArrowDown");
  assert.equal(await isOpen(page), true, "Enter on nothing keeps the palette open");
  assert.equal((await page.evaluate(() => window.__MOCK__.calls.length)), 0, "nothing was sent");

  // The query is never markup.
  await page.locator(".palette-input").fill("<img src=x onerror=window.__pwned=1>");
  assert.equal(await page.evaluate(() => window.__pwned), undefined);
  assert.equal(await page.locator(".palette-empty img").count(), 0);
  await page.keyboard.press("Escape");
}

/** A click on a row runs it; a click outside the dialog closes without running anything. */
async function pointerCase(page, url) {
  await open(page, url);
  await openPalette(page);
  await reset(page);
  await page.locator(".palette-row").filter({ has: page.getByText("New tab", { exact: true }) }).click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("new_tab").length >= 1);
  assert.equal((await callsOf(page, "new_tab")).length, 1);
  assert.equal(await isOpen(page), false);
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime");

  await openPalette(page);
  await reset(page);
  await page.mouse.click(5, 5); // the backdrop, outside the dialog
  assert.equal(await isOpen(page), false, "a click outside closes");
  assert.equal(await page.evaluate(() => window.__MOCK__.calls.length), 0, "and runs nothing");
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime");
}

/** The shortcut that opened the palette also closes it. */
async function toggleCase(page, url) {
  await open(page, url);
  await openPalette(page);
  await page.keyboard.press("Meta+k");
  assert.equal(await isOpen(page), false, "Cmd+K again closes");
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime");
}
