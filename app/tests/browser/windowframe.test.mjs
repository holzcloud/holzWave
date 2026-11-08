// The own window frame (docs/design.md section 6.33): the shell tells the page, through one global written before the page runs,
// that the window has no system title bar, and the page draws the three window buttons in the bar's title row: minimise,
// maximise (the restore drawing while the window is maximised) and close. Everything else (no facts, a browser, hostile
// facts) draws nothing. The Tauri global here is a stub and the window is a variable of the test: no window was ever started
// on Windows for this test, so it shows what the page draws and sends, not what a real window does.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { APP_DIR, callsOf, currentBackend } from "./lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  if (currentBackend() === "tauri") {
    await buttonsCase(page, url);
    await sizeCase(page, url);
    await pressCase(page, url);
    await followCase(page, url);
    await failureCase(page, url);
    await staleCase(page, url);
    await fullscreenCase(page, url);
    await stateCase(page, url);
    await contrastCase(page, url);
    await accessibilityCase(page, url);
    await dragCase(page, url);
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
const BUTTON = (word) => `#bar .bar-window button[data-button="${word}"]`;
const WORDS = ["minimize", "maximize", "close"];

/**
 * Load the page with `facts` as the global (`undefined`: no global at all) and the platform `os` answered by `app_info`. The
 * window is a variable of the page, `window.__WIN`: `maximized` and `fullscreen` are what the window answers, and `max` and
 * `full` are the functions behind the two commands, which a test replaces to make an answer slow, odd or a failure. The init
 * scripts of an earlier load stay registered on the page and run first, so every load sets or deletes the global.
 */
async function open(page, url, { facts, os = "windows" }) {
  await page.addInitScript(
    ([value, name, present]) => {
      window.__MOCK__.on("app_info", () => ({ version: "0.0.1-alpha1", os: name, hostname: "woodbox" }));
      window.__WIN = {
        maximized: false,
        fullscreen: false,
        odd: null,
        max: () => {
          // `odd` makes the answer a failure or a value that is no boolean, by name.
          const odd = window.__WIN.odd;
          if (odd === "rejection") throw new Error("no window");
          const answers = { string: "false", number: 0, nothing: undefined, null: null, object: { maximized: false } };
          if (odd !== null) return answers[odd];
          return window.__WIN.maximized;
        },
        full: () => window.__WIN.fullscreen,
      };
      window.__MOCK__.on("plugin:window|is_maximized", () => window.__WIN.max());
      window.__MOCK__.on("plugin:window|is_fullscreen", () => window.__WIN.full());
      if (present) window.__HW_FRAME__ = value;
      else delete window.__HW_FRAME__;
    },
    [facts === undefined ? null : facts, os, facts !== undefined],
  );
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("window_ready").length >= 1);
}

/** Open a Windows window with the facts of the shell and wait until the first state read is done. */
async function openWindows(page, url) {
  await open(page, url, { facts: WINDOWS });
  await page.waitForFunction(() => window.__MOCK__.callsOf("plugin:window|is_maximized").length >= 1);
  await page.waitForSelector(BUTTON("close"));
  await settle(page);
}

/** Two animation frames and the end of every running transition: what the page drew by now is what it will draw. */
const settle = (page) =>
  page.evaluate(async () => {
    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    await Promise.all(document.getAnimations().map((a) => a.finished.catch(() => {})));
  });

/** Every call to a command of the window plugin so far, by name. */
const windowCalls = (page) =>
  page.evaluate(() => window.__MOCK__.calls.map(([name]) => name).filter((name) => name.startsWith("plugin:window|")));

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

/** The glyph word and the name of the maximise button, as the page shows them. */
const maximizeLook = (page) =>
  page.evaluate((selector) => {
    const button = document.querySelector(selector);
    return {
      glyph: button.querySelector(".wf-glyph").dataset.glyph,
      title: button.title,
      label: button.getAttribute("aria-label"),
      group: document.querySelector("#bar .bar-window").hasAttribute("data-maximized"),
    };
  }, BUTTON("maximize"));

/** Dispatch the `resize` event of the page (a window that was maximised or restored) `n` times in a row. */
const resize = (page, n = 1) =>
  page.evaluate((times) => {
    for (let i = 0; i < times; i += 1) window.dispatchEvent(new Event("resize"));
  }, n);

/** The three buttons of the table in the table's order, with their names, glyphs and the facts classes. */
async function buttonsCase(page, url) {
  await openWindows(page, url);
  assert.deepEqual(
    await page.evaluate(() => [...document.querySelectorAll("#bar .bar-window > *")].map((n) => [n.tagName, n.className, n.dataset.button])),
    WORDS.map((word) => ["BUTTON", "wf-btn", word]),
    "three buttons, in the order minimise, maximise, close, and nothing else in the group",
  );
  for (const [word, name] of [["minimize", "Minimize"], ["maximize", "Maximize"], ["close", "Close"]]) {
    const button = page.locator(BUTTON(word));
    assert.equal(await button.getAttribute("title"), name, `${word}: the title`);
    assert.equal(await button.getAttribute("aria-label"), name, `${word}: the accessible name`);
    assert.equal(await button.getAttribute("type"), "button");
    assert.equal(await page.locator("#bar .bar-window").getByRole("button", { name, exact: true }).count(), 1, `${name} is found by its role and name`);
    const glyph = await button.locator(".wf-glyph").evaluate((n) => [n.tagName, n.getAttribute("aria-hidden"), n.dataset.glyph, n.textContent]);
    assert.deepEqual(glyph, ["SPAN", "true", word, ""], `${word}: a drawn glyph that is hidden from the accessibility tree and holds no text`);
  }
  const root = await page.evaluate(() => [...document.documentElement.classList]);
  for (const name of ["frame-own", "frame-flat", "frame-right"]) assert.ok(root.includes(name), `root class ${name}`);
  assert.deepEqual(await maximizeLook(page), { glyph: "maximize", title: "Maximize", label: "Maximize", group: false });

  // Where the group sits: in the title row, flush with the right edge of the window, after the host name and the clock.
  const shape = await page.evaluate(() => {
    const bar = document.getElementById("bar");
    const group = bar.querySelector(".bar-window");
    const g = group.getBoundingClientRect();
    const barRect = bar.getBoundingClientRect();
    return {
      inside: group.parentElement === bar,
      flush: Math.abs(g.right - window.innerWidth),
      top: g.top - barRect.top,
      bottom: g.bottom,
      tabsTop: bar.querySelector(".bar-tabs").getBoundingClientRect().top,
      infoRight: bar.querySelector(".bar-info").getBoundingClientRect().right,
      left: g.left,
    };
  });
  assert.ok(shape.inside, "the group is a child of the bar");
  assert.ok(shape.flush < 0.6, `flush with the window edge, off by ${shape.flush}`);
  assert.ok(shape.top < 0.6, "in the title row, at the top of the bar");
  assert.ok(shape.bottom <= shape.tabsTop + 0.5, "the group stays in the title row");
  assert.ok(shape.infoRight <= shape.left + 0.5, "the host name and the clock end before the buttons");
}

/** 46 by 32 px at the base scale, never below 28 by 28 px, and in rem so the UI scale moves them. */
async function sizeCase(page, url) {
  await openWindows(page, url);
  const sizes = () =>
    page.evaluate(() => ({
      rem: parseFloat(getComputedStyle(document.documentElement).fontSize),
      buttons: [...document.querySelectorAll("#bar .bar-window .wf-btn")].map((n) => {
        const r = n.getBoundingClientRect();
        return [r.width, r.height];
      }),
      glyphs: [...document.querySelectorAll("#bar .bar-window .wf-glyph")].map((n) => {
        const r = n.getBoundingClientRect();
        return [r.width, r.height];
      }),
    }));
  const near = (a, b, what) => assert.ok(Math.abs(a - b) < 0.6, `${what}: ${a} is not ${b}`);

  const base = await sizes();
  assert.equal(base.rem, 12);
  for (const [w, h] of base.buttons) {
    near(w, 46, "width at 100 %");
    near(h, 32, "height at 100 %");
  }
  for (const [w, h] of base.glyphs) {
    near(w, 10, "glyph width");
    near(h, 10, "glyph height");
  }

  // 75 %, the smallest the Settings dialog allows: the floor of 28 px holds. 200 %, the largest: the sizes follow the scale.
  for (const [pixels, scale] of [[9, 75], [24, 200]]) {
    await page.evaluate((size) => (document.documentElement.style.fontSize = `${size}px`), pixels);
    await settle(page);
    const now = await sizes();
    assert.equal(now.rem, pixels);
    for (const [w, h] of now.buttons) {
      assert.ok(w >= 28 - 0.01 && h >= 28 - 0.01, `${scale} %: no hit box below 28 by 28 px: ${w} by ${h}`);
      near(w, Math.max(28, (46 * pixels) / 12), `${scale} %: width`);
      near(h, Math.max(28, (32 * pixels) / 12), `${scale} %: height`);
    }
  }
}

/** A press sends exactly its own window command; the maximise press asks the window again; the page never keeps the keyboard. */
async function pressCase(page, url) {
  await openWindows(page, url);
  const initial = await windowCalls(page);
  assert.deepEqual(initial.sort(), ["plugin:window|is_fullscreen", "plugin:window|is_maximized"], "the state is read once on load, nothing is sent");

  const delta = async (word) => {
    const before = (await windowCalls(page)).length;
    await page.locator(BUTTON(word)).click();
    await settle(page);
    return (await windowCalls(page)).slice(before);
  };

  assert.deepEqual(await delta("minimize"), ["plugin:window|minimize"], "minimise sends minimise and asks nothing");
  assert.deepEqual(
    (await delta("maximize")).sort(),
    ["plugin:window|is_fullscreen", "plugin:window|is_maximized", "plugin:window|toggle_maximize"].sort(),
    "maximise sends toggle_maximize and then asks the state again",
  );
  assert.deepEqual(await delta("close"), ["plugin:window|close"], "close sends close and asks nothing");

  // The command goes out before the question: the window has moved by the time it is asked.
  const order = await windowCalls(page);
  assert.ok(order.indexOf("plugin:window|toggle_maximize") < order.lastIndexOf("plugin:window|is_maximized"));

  // A double click on a button is two presses of that button and nothing more.
  const before = (await windowCalls(page)).length;
  await page.locator(BUTTON("minimize")).dblclick();
  await settle(page);
  assert.deepEqual((await windowCalls(page)).slice(before), ["plugin:window|minimize", "plugin:window|minimize"]);

  for (const forbidden of ["destroy", "hide", "set_decorations", "start_resize_dragging", "start_dragging", "set_fullscreen", "maximize", "unmaximize"]) {
    assert.equal((await callsOf(page, `plugin:window|${forbidden}`)).length, 0, forbidden);
  }
  assert.equal(
    await page.evaluate(() => window.__MOCK__.listening("tauri://close-requested")),
    0,
    "the page never listens to the close request: a listener would keep the window open",
  );
  assert.equal(await page.evaluate(() => document.activeElement?.closest(".bar-window") === null), true, "a button does not keep the keyboard");

  // A refused command is swallowed: the page goes on.
  await page.evaluate(() => window.__MOCK__.on("plugin:window|minimize", () => { throw new Error("refused"); }));
  await page.locator(BUTTON("minimize")).click();
  await settle(page);
  assert.equal(await page.locator(BUTTON("minimize")).count(), 1);
}

/** The maximise button follows the window: on load, on a resize, after a press, and back. */
async function followCase(page, url) {
  await openWindows(page, url);
  assert.deepEqual(await maximizeLook(page), { glyph: "maximize", title: "Maximize", label: "Maximize", group: false });

  await page.evaluate(() => (window.__WIN.maximized = true));
  await resize(page);
  await page.waitForFunction((s) => document.querySelector(s).querySelector(".wf-glyph").dataset.glyph === "restore", BUTTON("maximize"));
  assert.deepEqual(await maximizeLook(page), { glyph: "restore", title: "Restore", label: "Restore", group: true });
  assert.equal(await page.locator("#bar .bar-window").getByRole("button", { name: "Restore", exact: true }).count(), 1);
  assert.equal(await page.locator("#bar .bar-window").getByRole("button", { name: "Maximize", exact: true }).count(), 0);
  assert.deepEqual(
    await page.evaluate(() => [...document.querySelectorAll("#bar .bar-window .wf-glyph")].map((n) => n.dataset.glyph)),
    ["minimize", "restore", "close"],
    "only the maximise button changes",
  );

  await page.evaluate(() => (window.__WIN.maximized = false));
  await resize(page);
  await page.waitForFunction((s) => document.querySelector(s).querySelector(".wf-glyph").dataset.glyph === "maximize", BUTTON("maximize"));
  assert.deepEqual(await maximizeLook(page), { glyph: "maximize", title: "Maximize", label: "Maximize", group: false });

  // A press asks again, with no resize event: the window has been maximised by the press.
  await page.evaluate(() => (window.__WIN.maximized = true));
  await page.locator(BUTTON("maximize")).click();
  await page.waitForFunction((s) => document.querySelector(s).querySelector(".wf-glyph").dataset.glyph === "restore", BUTTON("maximize"));
  assert.equal((await maximizeLook(page)).title, "Restore");

  // A burst of 20 resize events asks the window once.
  await settle(page);
  const asked = (await callsOf(page, "plugin:window|is_maximized")).length;
  await resize(page, 20);
  await page.waitForFunction((n) => window.__MOCK__.callsOf("plugin:window|is_maximized").length >= n, asked + 1);
  await settle(page);
  await page.waitForTimeout(100);
  assert.equal((await callsOf(page, "plugin:window|is_maximized")).length, asked + 1, "twenty events, one question");
  assert.equal((await callsOf(page, "plugin:window|is_fullscreen")).length, asked + 1, "and one for the other member");
  // The next burst is asked again: the throttle is not stuck.
  await resize(page, 5);
  await page.waitForFunction((n) => window.__MOCK__.callsOf("plugin:window|is_maximized").length >= n, asked + 2);
}

/** A failed or odd answer keeps the last state. */
async function failureCase(page, url) {
  await openWindows(page, url);
  await page.evaluate(() => (window.__WIN.maximized = true));
  await resize(page);
  await page.waitForFunction((s) => document.querySelector(s).querySelector(".wf-glyph").dataset.glyph === "restore", BUTTON("maximize"));

  for (const odd of ["rejection", "string", "number", "nothing", "null", "object"]) {
    const asked = (await callsOf(page, "plugin:window|is_maximized")).length;
    await page.evaluate((name) => (window.__WIN.odd = name), odd);
    await resize(page);
    await page.waitForFunction((n) => window.__MOCK__.callsOf("plugin:window|is_maximized").length >= n, asked + 1);
    await settle(page);
    assert.deepEqual(await maximizeLook(page), { glyph: "restore", title: "Restore", label: "Restore", group: true }, `${odd} keeps the last state`);
  }
  // The failed full screen read keeps the group too: it was drawn and it stays drawn.
  await page.evaluate(() => (window.__WIN.full = () => { throw new Error("no window"); }));
  await resize(page);
  await settle(page);
  assert.equal(await page.locator("#bar .bar-window").isVisible(), true);
}

/** Two reads overlap: the older answer, arriving last, does not overwrite the newer one. */
async function staleCase(page, url) {
  await openWindows(page, url);
  await page.evaluate(() => {
    let n = 0;
    window.__WIN.max = () => {
      n += 1;
      if (n === 1) return new Promise((resolve) => (window.__WIN.release = () => resolve(false)));
      return true;
    };
  });
  await resize(page);
  await page.waitForFunction(() => typeof window.__WIN.release === "function");
  await resize(page);
  await page.waitForFunction((s) => document.querySelector(s).querySelector(".wf-glyph").dataset.glyph === "restore", BUTTON("maximize"));
  await page.evaluate(() => window.__WIN.release());
  await settle(page);
  await page.waitForTimeout(100);
  assert.equal((await maximizeLook(page)).glyph, "restore", "the older answer was dropped");
}

/** Full screen: the group is not drawn, the rest of the bar is as before, and the group comes back. */
async function fullscreenCase(page, url) {
  await openWindows(page, url);
  const before = await boxes(page);
  await page.evaluate(() => (window.__WIN.fullscreen = true));
  await resize(page);
  await page.waitForFunction(() => document.querySelector("#bar .bar-window").hidden === true);
  assert.equal(await page.locator("#bar .bar-window").isVisible(), false, "the group is not drawn");
  assert.equal(await page.evaluate(() => getComputedStyle(document.querySelector("#bar .bar-window")).display), "none");
  assert.equal(await page.locator("#bar .bar-window .wf-btn").first().isVisible(), false, "no button is drawn or reachable");
  const during = await boxes(page);
  for (const key of ["#bar", "#bar .bar-workspaces", "#bar .bar-tabs", "#bar .tab"]) assert.deepEqual(during[key], before[key], `${key} keeps its box`);
  assert.equal(during["#bar .bar-info"][2], before["#bar .bar-info"][2], "the host name and the clock keep their width");
  assert.equal(await page.locator("#bar .bar-info").isVisible(), true, "the bar keeps its other content");
  const room = await page.evaluate(() => window.innerWidth - document.querySelector("#bar .bar-info").getBoundingClientRect().right);
  assert.ok(room < 9, `with no buttons the host name and the clock end at the bar's padding: ${room} px`);

  await page.evaluate(() => (window.__WIN.fullscreen = false));
  await resize(page);
  await page.waitForFunction(() => document.querySelector("#bar .bar-window").hidden === false);
  assert.equal(await page.locator("#bar .bar-window").isVisible(), true, "the group returns");
  await settle(page);
  assert.deepEqual(await boxes(page), before, "and the bar is as it was");
}

/** Resolve a token or a system colour to the `rgb(...)` string the engine computes for it, as text or as a fill. */
const resolver = (page) => (value, as = "color") =>
  page.evaluate(
    ([v, prop]) => {
      const probe = document.createElement("span");
      probe.style[prop] = v;
      document.body.append(probe);
      const resolved = getComputedStyle(probe)[prop];
      probe.remove();
      return resolved;
    },
    [value, as],
  );

const look = (page, selector) =>
  page.evaluate((s) => {
    const button = document.querySelector(s);
    const style = getComputedStyle(button);
    const glyph = getComputedStyle(button.querySelector(".wf-glyph"));
    return {
      background: style.backgroundColor,
      color: style.color,
      glyph: glyph.color,
      outline: [style.outlineStyle, style.outlineWidth, style.outlineOffset, style.outlineColor],
      transition: style.transitionDuration,
    };
  }, selector);

/** Rest, hover, the red close under the pointer on Windows, keyboard focus: each with its fill, tone and second cue. */
async function stateCase(page, url) {
  await page.emulateMedia({ forcedColors: null, reducedMotion: null, contrast: "no-preference" });
  await openWindows(page, url);
  const resolve = resolver(page);
  const dim = await resolve("var(--hw-dim)");
  const ink = await resolve("var(--hw-ink)");
  const wash = await resolve("var(--hw-rose-wash)", "backgroundColor");
  const danger = await resolve("var(--hw-danger)", "backgroundColor");
  const ground = await resolve("var(--hw-ground)");
  const clear = await resolve("transparent", "backgroundColor");

  // Rest: no fill, the dim tone, the drawn glyph.
  await page.mouse.move(500, 300);
  await settle(page);
  for (const word of WORDS) {
    const rest = await look(page, BUTTON(word));
    assert.equal(rest.background, clear, `${word}: no fill at rest`);
    assert.equal(rest.glyph, dim, `${word}: the glyph at rest is --hw-dim`);
    assert.equal(rest.outline[0], "none", `${word}: no ring without the keyboard`);
  }
  // The glyph is drawn: each drawing paints something that is not the button's fill (a stroke or a border).
  const painted = await page.evaluate(() =>
    [...document.querySelectorAll("#bar .bar-window .wf-glyph")].map((n) => {
      const parts = [getComputedStyle(n), getComputedStyle(n, "::before"), getComputedStyle(n, "::after")];
      return parts.some((s) => s.content !== "none" && s.display !== "none" && (s.backgroundColor !== "rgba(0, 0, 0, 0)" || s.borderTopWidth !== "0px" || s.borderRightWidth !== "0px"));
    }),
  );
  assert.deepEqual(painted, [true, true, true], "every glyph is drawn by the style sheet");

  // Hover: the wash and the stronger tone, on the minimise and on the maximise button; close under the pointer on Windows is red.
  for (const word of ["minimize", "maximize"]) {
    await page.hover(BUTTON(word));
    await settle(page);
    const hover = await look(page, BUTTON(word));
    assert.equal(hover.background, wash, `${word}: the wash under the pointer`);
    assert.equal(hover.glyph, ink, `${word}: the stronger tone under the pointer`);
  }
  await page.hover(BUTTON("close"));
  await settle(page);
  const red = await look(page, BUTTON("close"));
  assert.equal(red.background, danger, "close under the pointer: the danger fill");
  assert.equal(red.glyph, ground, "close under the pointer: the glyph in the ground colour");
  assert.notEqual(red.background, wash, "and not the ordinary wash");

  // Keyboard focus: the 2 px ring on the hit box. The ring is `:focus-visible`, which an engine grants to a script's focus() after
  // a key press. (The Tab key cannot walk the bar here: while a bar button has the focus, the page's key handler sends Tab to
  // the terminal, as it does for every button of the bar, so each button is focused by name.)
  await page.mouse.move(500, 300);
  await page.keyboard.press("Shift");
  for (const word of WORDS) {
    await page.evaluate((w) => document.querySelector(`#bar .bar-window [data-button="${w}"]`).focus(), word);
    await settle(page);
    assert.equal(await page.evaluate(() => document.activeElement?.dataset.button), word);
    const ring = await look(page, BUTTON(word));
    assert.deepEqual(ring.outline, ["solid", "2px", "-2px", ink], `${word}: the ring is 2 px solid --hw-ink, offset -2 px`);
  }
  await page.evaluate(() => document.activeElement.blur());
}

/** WCAG contrast of two `rgb(r, g, b)` strings. */
function ratio(a, b) {
  const lum = (css) => {
    const [r, g, bl] = css.match(/\d+(\.\d+)?/g).slice(0, 3).map(Number).map((v) => {
      const s = v / 255;
      return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
    });
    return 0.2126 * r + 0.7152 * g + 0.0722 * bl;
  };
  const [hi, lo] = [lum(a), lum(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

/** The red close button keeps 4.5 to 1 between its glyph and its fill in the three built-in themes (their chrome tokens). */
async function contrastCase(page, url) {
  await page.emulateMedia({ forcedColors: null, reducedMotion: "reduce", contrast: "no-preference" });
  await openWindows(page, url);
  const themes = [
    ["aubergine", "#14091a", "#ec8272"],
    ["Schiefer", "#1b1c1f", "#ec8272"],
    ["Papier", "#fff3f8", "#b3261e"],
  ];
  for (const [name, ground, danger] of themes) {
    await page.evaluate(
      ([g, d]) => {
        document.documentElement.style.setProperty("--hw-ground", g);
        document.documentElement.style.setProperty("--hw-danger", d);
      },
      [ground, danger],
    );
    await page.hover(BUTTON("close"));
    await settle(page);
    const red = await look(page, BUTTON("close"));
    assert.ok(ratio(red.glyph, red.background) >= 4.5, `${name}: ${red.glyph} on ${red.background} is ${ratio(red.glyph, red.background).toFixed(2)} to 1`);
    await page.mouse.move(500, 300);
  }
  await page.emulateMedia({ reducedMotion: null });
}

/** Higher contrast, reduced motion and forced colours. */
async function accessibilityCase(page, url) {
  await page.emulateMedia({ forcedColors: null, reducedMotion: null, contrast: "no-preference" });
  await openWindows(page, url);
  const resolve = resolver(page);

  // Reduced motion removes the 160 ms transition; without it the hover colour fades.
  await page.evaluate(() => document.documentElement.classList.remove("motion-reduce"));
  assert.deepEqual((await look(page, BUTTON("maximize"))).transition.split(", "), ["0.16s", "0.16s"], "160 ms, for the fill and the glyph tone, without the setting");
  await page.evaluate(() => document.documentElement.classList.add("motion-reduce"));
  assert.deepEqual((await look(page, BUTTON("maximize"))).transition.split(", "), ["0s"], "none under motion-reduce");
  await page.evaluate(() => document.documentElement.classList.remove("motion-reduce"));

  // Higher contrast: the idle glyph takes the stronger tone, like the rest of the bar.
  await page.mouse.move(500, 300);
  const normal = await look(page, BUTTON("minimize"));
  await page.evaluate(() => document.documentElement.classList.add("contrast-high"));
  await settle(page);
  const high = await look(page, BUTTON("minimize"));
  assert.notEqual(high.glyph, normal.glyph, "the idle tone changes in higher contrast");
  assert.equal(high.glyph, await resolve("var(--hw-ink-2)"), "to the stronger tone");
  await page.evaluate(() => document.documentElement.classList.remove("contrast-high"));

  // Forced colours: the strokes are ButtonText, hover and focus are Highlight with a visible border, and the glyph never
  // vanishes into the fill.
  await page.emulateMedia({ forcedColors: "active" });
  await open(page, url, { facts: WINDOWS });
  await page.waitForSelector(BUTTON("close"));
  await settle(page);
  const text = await resolve("ButtonText");
  const highlight = await resolve("Highlight");
  for (const word of WORDS) {
    const rest = await look(page, BUTTON(word));
    assert.equal(rest.glyph, text, `${word}: the glyph is ButtonText under forced colours`);
    assert.notEqual(rest.glyph, rest.background, `${word}: the glyph is visible against the button`);
  }
  for (const word of WORDS) {
    await page.hover(BUTTON(word));
    await settle(page);
    const hover = await page.evaluate((s) => {
      const style = getComputedStyle(document.querySelector(s));
      return { color: style.color, border: [style.borderTopStyle, style.borderTopWidth, style.borderTopColor], background: style.backgroundColor };
    }, BUTTON(word));
    assert.equal(hover.color, highlight, `${word}: Highlight under the pointer`);
    assert.notEqual(hover.border[0], "none", `${word}: a visible border under the pointer`);
    assert.equal(hover.border[1], "1px");
    assert.equal(hover.border[2], highlight);
    assert.notEqual(hover.background, hover.color, `${word}: the glyph is not painted over itself`);
  }
  await page.mouse.move(500, 300);
  await page.keyboard.press("Shift");
  await page.evaluate(() => document.querySelector('#bar .bar-window [data-button="maximize"]').focus());
  await settle(page);
  const ring = await look(page, BUTTON("maximize"));
  assert.deepEqual(ring.outline.slice(0, 3), ["solid", "2px", "-2px"], "the ring stays 2 px under forced colours");
  assert.equal(ring.outline[3], highlight, "in Highlight");
  assert.equal(await page.evaluate(() => document.querySelectorAll("#bar .bar-window .wf-btn").length), 3);
  await page.emulateMedia({ forcedColors: null });
  // The forced-colours rules of the sheet name system colours only: no hex value and no theme token.
  const rules = await page.evaluate(() => {
    const out = [];
    for (const sheet of document.styleSheets) {
      let list;
      try {
        list = sheet.cssRules;
      } catch {
        continue;
      }
      if (!sheet.href?.endsWith("windowframe.css")) continue;
      for (const rule of list) {
        if (rule instanceof CSSMediaRule && /forced-colors/.test(rule.conditionText)) for (const inner of rule.cssRules) out.push(inner.cssText);
      }
    }
    return out;
  });
  assert.ok(rules.length >= 2, `the sheet has forced-colors rules (${rules.length})`);
  for (const text of rules) assert.doesNotMatch(text, /#[0-9a-fA-F]{3,8}\b|\b(?:rgb|rgba|hsl|hsla|color-mix)\(|var\(--hw-/, text);
}

/** The buttons and the group are not drag regions; the free space of the bar still is. */
async function dragCase(page, url) {
  await openWindows(page, url);
  const drag = await page.evaluate(() => ({
    group: document.querySelector("#bar .bar-window").hasAttribute("data-tauri-drag-region"),
    buttons: [...document.querySelectorAll("#bar .bar-window .wf-btn, #bar .bar-window .wf-glyph")].map((n) => n.hasAttribute("data-tauri-drag-region")),
    inside: document.querySelectorAll("#bar .bar-window [data-tauri-drag-region]").length,
    free: [...document.querySelectorAll("#bar .bar-fill, #bar .bar-info")].map((n) => n.hasAttribute("data-tauri-drag-region")),
  }));
  assert.equal(drag.group, false, "the group is not a drag region");
  assert.deepEqual(drag.buttons, [false, false, false, false, false, false], "no button and no glyph is");
  assert.equal(drag.inside, 0);
  assert.deepEqual(drag.free, [true, true], "the free space of the bar still drags the window");
}

/** No facts: nothing is drawn, nothing is asked, and the bar is the one of a platform that never heard of the frame. */
async function absentCase(page, url) {
  await open(page, url, { facts: undefined, os: "linux" });
  assert.equal(await page.locator("#bar .bar-window").count(), 0, "no group");
  assert.equal(await page.evaluate(() => document.documentElement.className.includes("frame-")), false, "no frame class");
  const linux = await boxes(page);
  await open(page, url, { facts: undefined, os: "windows" });
  assert.equal(await page.locator("#bar .bar-window").count(), 0, "no group on Windows without facts either");
  assert.deepEqual(await boxes(page), linux, "the bar is the same without facts");
  await resize(page);
  await settle(page);
  assert.deepEqual(await windowCalls(page), [], "nothing is asked of the window and nothing is sent");
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
    { ...WINDOWS, buttons: ["close"] },
    { ...WINDOWS, buttons: ["minimize", "maximize"] },
    { ...WINDOWS, buttons: ["close", "maximize", "minimize"] },
    { ...WINDOWS, buttons: ["close", "close"] },
    { ...WINDOWS, buttons: ["minimize", "maximize", "destroy"] },
    { ...WINDOWS, buttons: "close" },
    { ...WINDOWS, buttons: [1] },
    { ...WINDOWS, buttons: ["minimize", "maximize", "close", "close"] },
  ];
  for (const facts of hostile) {
    await open(page, url, { facts });
    assert.equal(await page.locator("#bar .bar-window").count(), 0, `nothing is drawn for ${JSON.stringify(facts)}`);
    assert.equal(await page.evaluate(() => document.documentElement.className.includes("frame-")), false, JSON.stringify(facts));
    assert.deepEqual(await windowCalls(page), [], `nothing is asked for ${JSON.stringify(facts)}`);
  }
  // Under macOS the shell never sends the facts, and the page draws nothing if they arrive anyway.
  await open(page, url, { facts: { ...WINDOWS, os: "macos" }, os: "macos" });
  assert.equal(await page.locator("#bar .bar-window").count(), 0);
}

/** A browser draws no window buttons: the facts are ignored there. */
async function webCase(page, url) {
  await open(page, url, { facts: WINDOWS });
  assert.equal(await page.locator("#bar .bar-window").count(), 0, "no group in a browser");
  assert.equal(await page.evaluate(() => document.documentElement.className.includes("frame-")), false);
  assert.deepEqual(await windowCalls(page), [], "and no call");
}

/** The shipped source holds no call that parses markup and none of the commands or events the page must never use. */
function sourceCase() {
  for (const file of ["src/windowframe.ts", "src/windowframestate.ts"]) {
    const source = readFileSync(join(APP_DIR, file), "utf8");
    const code = source.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "").replace(/\s\/\/ .*$/gm, "");
    assert.doesNotMatch(code, /innerHTML|outerHTML|insertAdjacentHTML|DOMParser|createContextualFragment|document\.write|cssText/, file);
    assert.doesNotMatch(
      code,
      /plugin:window\|(?:destroy|hide|set_decorations|set_always_on_top|set_fullscreen|maximize|unmaximize|start_dragging|start_resize_dragging|set_size|set_position|close_requested)|set_decorations|start_resize_dragging|close-requested|onCloseRequested|\blisten\(/,
      file,
    );
    const commands = [...code.matchAll(/plugin:window\|(\w+)/g)].map((m) => m[1]);
    for (const command of commands) assert.ok(["minimize", "toggle_maximize", "close", "is_maximized", "is_fullscreen"].includes(command), `${file}: ${command}`);
  }
  const page = readFileSync(join(APP_DIR, "src/windowframe.ts"), "utf8");
  for (const command of ["plugin:window|minimize", "plugin:window|toggle_maximize", "plugin:window|close"]) assert.ok(page.includes(command), command);
  assert.match(readFileSync(join(APP_DIR, "src/windowframestate.ts"), "utf8"), /plugin:window\|is_maximized[\s\S]*plugin:window\|is_fullscreen/);
  const sheet = readFileSync(join(APP_DIR, "src/windowframe.css"), "utf8");
  assert.doesNotMatch(sheet, /#[0-9a-fA-F]{3,8}\b|\b(?:rgb|rgba|hsl|hsla)\(/, "the style sheet holds no literal colour");
  assert.doesNotMatch(sheet, /gradient|blur|box-shadow|text-shadow/, "flat: no gradient, no blur, no shadow");
}
