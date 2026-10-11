// The command palette (design.md section 6 rule 7): Cmd+K opens a fuzzy list of every registered action with its
// section and chord, the keyboard drives it, Enter runs the highlighted action, Escape gives the terminal its
// keyboard back, and while it is open no key reaches the program behind it. It is a floating layer: an 8 px radius,
// the only shadow of the page, a height of at most 60 % of the window, and 160 ms transitions only when motion is
// allowed. The browser stub plays the core.
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
  await radiusAndShadowCase(page, url);
  await sizeAndScrollCase(page, url);
  await lateRegistrationCase(page, url);
  await motionCase(page, url);
  await contrastCase(page, url);
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

// ---- the floating-layer rules ---------------------------------------------------------------------

/** Register `count` actions the page itself does not have, so the list is longer than the dialog. */
const registerMany = (page, count) =>
  page.evaluate((n) => {
    for (let i = 1; i <= n; i++) {
      const id = `extra.item_${i}`;
      if (!window.__registry.listActions().some((a) => a.id === id)) {
        window.__registry.registerAction({ id, title: `Extra action ${i}`, run: () => {} });
      }
    }
  }, count);

/** The 8 px radius, and the only box-shadow of the whole document, held by the dialog. */
async function radiusAndShadowCase(page, url) {
  await open(page, url);
  await openPalette(page);
  const look = await page.evaluate(() => {
    const dialog = document.querySelector(".palette");
    const shadowed = [...document.querySelectorAll("*")]
      .filter((n) => getComputedStyle(n).boxShadow !== "none")
      .map((n) => (n.id ? `#${n.id}` : n.tagName.toLowerCase()) + (n.className ? `.${String(n.className).split(" ").join(".")}` : ""));
    const radii = ["borderTopLeftRadius", "borderTopRightRadius", "borderBottomLeftRadius", "borderBottomRightRadius"].map(
      (k) => getComputedStyle(dialog)[k],
    );
    return { shadowed, radii };
  });
  assert.deepEqual(look.radii, ["8px", "8px", "8px", "8px"], "the dialog has an 8 px radius");
  assert.deepEqual(look.shadowed, ["div.palette"], "the dialog is the only element with a shadow");
  await page.keyboard.press("Escape");
  // A hidden element still reports its computed style, so only what is laid out counts.
  const closed = await page.evaluate(
    () =>
      [...document.querySelectorAll("*")].filter(
        (n) => n.getClientRects().length > 0 && getComputedStyle(n).boxShadow !== "none",
      ).length,
  );
  assert.equal(closed, 0, "no element is drawn with a shadow while the palette is closed");
}

/** A long list scrolls inside a dialog of at most 60 % of the window, and the keyboard keeps the active row in view. */
async function sizeAndScrollCase(page, url) {
  await open(page, url);
  await registerMany(page, 80);
  await openPalette(page);

  const size = await page.evaluate(() => {
    const dialog = document.querySelector(".palette").getBoundingClientRect();
    const list = document.querySelector(".palette-list");
    return { h: dialog.height, win: innerHeight, scroll: list.scrollHeight, client: list.clientHeight, overflow: getComputedStyle(list).overflowY };
  });
  assert.ok(size.h <= size.win * 0.6 + 0.5, `the dialog is ${size.h} px high in a window of ${size.win} px`);
  assert.ok(size.scroll > size.client, "the list is longer than its box");
  assert.match(size.overflow, /auto|scroll/);

  const geometry = () =>
    page.evaluate(() => {
      const list = document.querySelector(".palette-list").getBoundingClientRect();
      const row = document.querySelector('.palette-row[aria-selected="true"]').getBoundingClientRect();
      return { top: row.top - list.top, bottom: list.bottom - row.bottom, scrollTop: document.querySelector(".palette-list").scrollTop };
    });
  const visible = (g) => g.top >= -0.5 && g.bottom >= -0.5;

  // Down through the whole list: every step leaves the active row inside the box and the list has scrolled.
  const total = (await rows(page)).length;
  for (let i = 0; i < total - 1; i++) {
    await page.keyboard.press("ArrowDown");
    if (i % 9 === 0 || i === total - 2) assert.ok(visible(await geometry()), `row ${i + 1} is in view`);
  }
  const end = await geometry();
  assert.ok(end.scrollTop > 0, "the list scrolled");
  assert.ok(visible(end));
  assert.equal((await rows(page))[total - 1].selected, true, "the last row is active");

  // Up and wrap: from the first row Up goes to the last, Down comes back to the first and the list is at the top.
  await page.keyboard.press("ArrowDown");
  const first = await geometry();
  assert.equal(first.scrollTop, 0, "wrapping to the first row shows the top of the list");
  assert.ok(visible(first));
  await page.keyboard.press("ArrowUp");
  assert.ok(visible(await geometry()));
  assert.equal((await rows(page))[total - 1].selected, true);

  // A page moves several rows, stops at the ends and keeps the row in view.
  await page.keyboard.press("PageUp");
  const afterPage = (await rows(page)).findIndex((r) => r.selected);
  assert.ok(afterPage < total - 2 && afterPage > 0, `PageUp moved by a page (row ${afterPage} of ${total})`);
  assert.ok(visible(await geometry()));
  for (let i = 0; i < 40; i++) await page.keyboard.press("PageUp");
  assert.equal((await rows(page))[0].selected, true, "PageUp stops at the first row");
  assert.equal((await geometry()).scrollTop, 0);

  // Typing a query that selects row 0 again returns the list to its top.
  await page.keyboard.type("extra action 7");
  assert.equal((await geometry()).scrollTop, 0);
  assert.equal((await rows(page))[0].title, "Extra action 7");
  await page.keyboard.press("Escape");
}

/** An action registered after boot, from outside the palette, is listed at the next open and runs. */
async function lateRegistrationCase(page, url) {
  await open(page, url);
  await openPalette(page);
  const before = (await rows(page)).length;
  await page.keyboard.press("Escape");

  await page.evaluate(() => {
    window.__lateRuns = 0;
    window.__registry.registerAction({ id: "late.arrival", title: "Late arrival", run: () => void window.__lateRuns++ });
  });
  await openPalette(page);
  const after = await rows(page);
  assert.equal(after.length, before + 1, "the late action is listed");
  assert.deepEqual(after[after.length - 1], { ...after[after.length - 1], title: "Late arrival", section: "late", chord: "" });

  await page.keyboard.type("late arr");
  assert.equal((await rows(page))[0].title, "Late arrival");
  await page.keyboard.press("Enter");
  assert.equal(await page.evaluate(() => window.__lateRuns), 1, "Enter ran it");
  assert.equal(await isOpen(page), false);

  // An action taken away is gone at the next open.
  await page.evaluate(() => window.__registry.unregisterAction("late.arrival"));
  await openPalette(page);
  assert.equal((await rows(page)).length, before);
  await page.keyboard.press("Escape");
}

/** The longest transition, in seconds, of the palette's own elements; `0` for none. */
const longestTransition = (page) =>
  page.evaluate(() => {
    const parse = (v) => v.split(",").map((t) => (t.trim().endsWith("ms") ? parseFloat(t) / 1000 : parseFloat(t) || 0));
    let longest = 0;
    let count = 0;
    for (const n of document.querySelectorAll(".palette-backdrop, .palette-backdrop *")) {
      count++;
      longest = Math.max(longest, ...parse(getComputedStyle(n).transitionDuration));
    }
    return { longest, count };
  });

const waitClass = (page, name, present) =>
  page.waitForFunction(([n, p]) => document.documentElement.classList.contains(n) === p, [name, present]);

/** 160 ms when motion is allowed; none when the OS or the setting asks for less, and the page follows live. */
async function motionCase(page, url) {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await open(page, url);
  await waitClass(page, "motion-reduce", false);
  await openPalette(page);
  const allowed = await longestTransition(page);
  assert.ok(allowed.count > 3, "the palette has elements");
  assert.equal(allowed.longest, 0.16, "opening uses 160 ms");
  await page.keyboard.press("Escape");

  // The OS asks for reduced motion while the page runs: the next opening has no transition.
  await page.emulateMedia({ reducedMotion: "reduce" });
  await waitClass(page, "motion-reduce", true);
  await openPalette(page);
  assert.equal((await longestTransition(page)).longest, 0, "no palette element has a transition under reduced motion");
  assert.equal(await page.locator(".palette").isVisible(), true, "and it is still shown");
  await page.keyboard.press("Escape");

  // Back to no preference: the 160 ms are back.
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await waitClass(page, "motion-reduce", false);
  await openPalette(page);
  assert.equal((await longestTransition(page)).longest, 0.16);
  await page.keyboard.press("Escape");
  await page.emulateMedia({ reducedMotion: null });
}

/** The colour a token resolves to, in the form getComputedStyle reports it. */
const token = (page, name) =>
  page.evaluate((n) => {
    const probe = document.createElement("span");
    probe.style.color = `var(${n})`;
    document.body.append(probe);
    const c = getComputedStyle(probe).color;
    probe.remove();
    return c;
  }, name);

/** Higher contrast swaps the hairline of the dialog to the stronger token, as it does for the chrome. */
async function contrastCase(page, url) {
  await page.emulateMedia({ contrast: "no-preference", reducedMotion: null, forcedColors: null });
  await open(page, url);
  await openPalette(page);
  const border = () => page.evaluate(() => getComputedStyle(document.querySelector(".palette")).borderTopColor);
  assert.equal(await border(), await token(page, "--hw-edge"), "the normal hairline");
  await page.keyboard.press("Escape");

  await page.emulateMedia({ contrast: "more" });
  await waitClass(page, "contrast-high", true);
  await openPalette(page);
  assert.equal(await border(), await token(page, "--hw-ink-3"), "the higher contrast hairline");
  await page.keyboard.press("Escape");
  await page.emulateMedia({ contrast: null });
}
