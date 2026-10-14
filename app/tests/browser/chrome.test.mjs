// The Terminal-modern chrome (D-01): the window bar, the status line and the design tokens, rendered from the
// core's state. Structure, state cues and tokens are asserted here; how it looks goes to the human checklist.
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { APP_DIR, callsOf, emit } from "./lib.mjs";

export default async function ({ page, url }) {
  await barCase(page, url);
  await titleCase(page, url);
  await renameCase(page, url);
  await platformCase(page, url);
  await statusCase(page, url);
  await rendererCase(page, url);
  await hostileCase(page, url);
  await tokenCase(page, url);
  await headerCase(page, url);
  await headerStateCase(page, url);
  await headerTextCase(page, url);
  await gridCase(page, url);
  await scaleCase(page, url);
  await clockCase(page, url); // last: it installs a fake clock that stays on the page
}

/** Two workspaces, three tabs and one pane; every tab names the pane that has its focus. */
const BAR_VIEW = {
  workspaces: [{ name: "alpha", active: true }, { name: "beta", active: false }],
  tabs: [
    { title: "one", active: true, focus: 1 },
    { title: "two", active: false, focus: 2 },
    { title: "three", active: false, focus: 3 },
  ],
  focus: 1,
  panes: [[1, { x: 0, y: 0, w: 1, h: 1 }]],
  dividers: [],
};

const INFO = { version: "0.0.1-alpha1", os: "linux", hostname: "woodbox" };

const PAD = 6; // CSS px between a pane's edge and its grid (geometry.ts)
const ROW_PX = 18; // one cell row: ceil(14 px font * 1.25)
const HEADER = 20; // CSS px of a pane's header strip at the base font size
const ROSE = "rgb(255, 143, 184)";

/** The canvas box in page pixels. */
const canvasBox = (page) =>
  page.evaluate(() => {
    const r = document.getElementById("term").getBoundingClientRect();
    return { left: r.left, top: r.top, width: r.width, height: r.height };
  });

/** Everything a test needs to know about the pane header strips, in DOM order. */
const heads = (page) =>
  page.evaluate(() =>
    [...document.querySelectorAll("#overlay .panehead")].map((n) => {
      const r = n.getBoundingClientRect();
      const s = getComputedStyle(n);
      const dot = n.querySelector(".ph-dot");
      return {
        pane: n.getAttribute("data-pane"),
        left: r.left, top: r.top, width: r.width, height: r.height,
        focused: n.classList.contains("focused"),
        line: n.classList.contains("focus-line"),
        exited: n.classList.contains("exited"),
        lineColour: s.borderTopColor,
        lineWidth: s.borderTopWidth,
        events: s.pointerEvents,
        title: n.querySelector(".ph-title")?.textContent,
        ctx: n.querySelector(".ph-ctx")?.textContent,
        state: dot?.getAttribute("data-state"),
        dotFill: dot ? getComputedStyle(dot).backgroundColor : null,
        dotMark: dot ? getComputedStyle(dot, "::after").content : null,
      };
    }),
  );

/**
 * Open the page against the stub. `info` is what `app_info` answers (null: the stub's own default). Every case
 * registers one, because an init script of an earlier case would otherwise still answer.
 */
async function open(page, url, info = INFO) {
  await page.addInitScript((i) => window.__MOCK__.on("app_info", () => i), info);
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
}

/** Show `view` the way the core would announce a new layout. */
async function showView(page, view) {
  await page.evaluate((v) => window.__MOCK__.setView(v), view);
  await emit(page, "layout", view);
}

const texts = (page, selector) => page.locator(selector).allTextContents();

async function barCase(page, url) {
  await open(page, url);
  await showView(page, BAR_VIEW);

  assert.deepEqual(await texts(page, "#bar .ws-chip"), ["alpha", "beta"], "one chip per workspace");
  assert.deepEqual(await texts(page, "#bar .tab"), ["1 one", "2 two", "3 three"], "numbered tabs: number, space, title");
  assert.deepEqual(await texts(page, "#bar .ws-chip.active"), ["alpha"], "the active workspace is marked");
  assert.deepEqual(await texts(page, "#bar .tab.active"), ["1 one"], "the active tab is marked");

  const current = await page.evaluate(() => ({
    tabs: [...document.querySelectorAll("#bar .tab")].map((n) => n.getAttribute("aria-current")),
    chips: [...document.querySelectorAll("#bar .ws-chip")].map((n) => n.getAttribute("aria-current")),
    // The underline is a text decoration, so the marker is there without any colour.
    deco: [...document.querySelectorAll("#bar .tab")].map((n) => getComputedStyle(n).textDecorationLine),
    chipDeco: [...document.querySelectorAll("#bar .ws-chip")].map((n) => getComputedStyle(n).textDecorationLine),
  }));
  assert.deepEqual(current.tabs, ["page", null, null], "only the active tab is aria-current page");
  assert.deepEqual(current.chips, ["true", null], "only the active workspace is aria-current");
  assert.ok(current.deco[0].includes("underline"), `the active tab has no underline: ${current.deco[0]}`);
  assert.ok(!current.deco[1].includes("underline") && !current.deco[2].includes("underline"), "an inactive tab is underlined");
  assert.ok(current.chipDeco[0].includes("underline"), "the active workspace chip has no underline");
  assert.ok(!current.chipDeco[1].includes("underline"), "an inactive workspace chip is underlined");

  // Buttons send the command with their own index and give the keyboard back to the terminal input.
  await page.locator("#bar .tab").nth(1).click();
  assert.deepEqual((await callsOf(page, "select_tab")).at(-1), { index: 1 });
  await page.locator("#bar .ws-chip").nth(1).click();
  assert.deepEqual((await callsOf(page, "select_workspace")).at(-1), { index: 1 });
  await page.locator("#bar button.plus").nth(0).click();
  assert.deepEqual((await callsOf(page, "new_workspace")).at(-1), { name: "ws 3" }, "the first plus adds a workspace");
  await page.locator("#bar button.plus").nth(1).click();
  assert.equal((await callsOf(page, "new_tab")).length, 1, "the second plus adds a tab");
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime", "a bar button gives the keyboard back");

  // The same element answers a later click with the index it has after the layout changed.
  const fewer = { ...BAR_VIEW, tabs: BAR_VIEW.tabs.slice(0, 2) };
  await showView(page, fewer);
  assert.deepEqual(await texts(page, "#bar .tab"), ["1 one", "2 two"], "a tab that left the layout is gone");
  await page.locator("#bar .tab").nth(1).click();
  assert.deepEqual((await callsOf(page, "select_tab")).at(-1), { index: 1 });

  // The bar's background drags the window; the buttons do not.
  const drag = await page.evaluate(() => ({
    bar: document.getElementById("bar").hasAttribute("data-tauri-drag-region"),
    buttons: document.querySelectorAll("#bar button[data-tauri-drag-region]").length,
    fill: document.querySelectorAll("#bar .bar-fill[data-tauri-drag-region]").length,
  }));
  assert.equal(drag.bar, true, "the bar is a drag region");
  assert.equal(drag.fill, 1, "the free space of the bar is a drag region");
  assert.equal(drag.buttons, 0, "a button must not be a drag region");

  // The right side: host name and clock in the mono label style.
  assert.deepEqual(await texts(page, "#bar .host"), ["woodbox"]);
  const clock = await texts(page, "#bar .clock");
  assert.equal(clock.length, 1);
  assert.match(clock[0], /^\d{2}:\d{2}$/, "the clock is HH:MM");
  const label = await page.evaluate(() => {
    const s = getComputedStyle(document.querySelector("#bar .clock"));
    return { transform: s.textTransform, spacing: s.letterSpacing, family: s.fontFamily };
  });
  assert.equal(label.transform, "uppercase", "labels are uppercase");
  assert.notEqual(label.spacing, "normal", "labels are tracked");
  assert.match(label.family, /JetBrains Mono/);
}

async function titleCase(page, url) {
  await open(page, url);
  // A tab still called `shell` shows the title its focus pane's program set; a tab the user named keeps its name.
  const auto = { ...BAR_VIEW, tabs: BAR_VIEW.tabs.map((t, i) => (i === 2 ? t : { ...t, title: "shell" })) };
  await showView(page, auto);
  await emit(page, "title", { pane: 1, title: "vim README.md" });
  assert.deepEqual(await texts(page, "#bar .tab"), ["1 vim README.md", "2 shell", "3 three"]);
  await emit(page, "title", { pane: 2, title: "htop" });
  assert.deepEqual(await texts(page, "#bar .tab"), ["1 vim README.md", "2 htop", "3 three"]);
  await emit(page, "title", { pane: 3, title: "top" });
  assert.deepEqual(await texts(page, "#bar .tab"), ["1 vim README.md", "2 htop", "3 three"], "a named tab keeps its name");
  // A pane that exited forgets its title.
  await emit(page, "pane-exit", { pane: 1, code: 0 });
  assert.deepEqual(await texts(page, "#bar .tab"), ["1 shell", "2 htop", "3 three"]);
}

/**
 * DESK-32: a double click on a tab, a workspace chip or a pane header opens the name editor over it; Enter sends the
 * name once, Escape sends nothing, the field takes no key for the terminal, and a name is only ever shown as text.
 */
async function renameCase(page, url) {
  await open(page, url);
  await showView(page, { ...BAR_VIEW, pane_names: [[1, "<b>api</b>"]] });
  const sentCalls = (cmd) => callsOf(page, cmd);
  const field = page.locator("input.rename-input");

  await page.locator("#bar .tab").nth(1).dblclick();
  assert.equal(await field.count(), 1, "the editor opens over the tab");
  assert.equal(await field.inputValue(), "two", "it holds the current name");
  // The text is selected when the editor opens, so typing replaces it.
  await page.keyboard.type("docs");
  assert.deepEqual(await sentCalls("key"), [], "typing in the editor sends no key to the terminal");
  assert.deepEqual(await sentCalls("text"), [], "and no text");
  await page.keyboard.press("Enter");
  assert.equal(await field.count(), 0, "Enter closes the editor");
  assert.deepEqual(await sentCalls("rename_tab"), [{ index: 1, name: "docs" }], "one rename_tab with the index and the text");

  await page.locator("#bar .ws-chip").nth(1).dblclick();
  await page.keyboard.type("holzWave");
  await page.keyboard.press("Escape");
  assert.equal(await field.count(), 0, "Escape closes the editor");
  assert.deepEqual(await sentCalls("rename_workspace"), [], "Escape sends nothing");
  await page.locator("#bar .ws-chip").nth(1).dblclick();
  await page.keyboard.type("holzWave");
  await page.keyboard.press("Enter");
  assert.deepEqual(await sentCalls("rename_workspace"), [{ index: 1, name: "holzWave" }]);

  // A pane header shows the name first as text, and a double click on it renames the pane.
  const head = page.locator(".panehead .ph-title");
  assert.equal(await head.textContent(), "<b>api</b>", "a name is text, never markup");
  assert.equal(await page.locator(".panehead b").count(), 0);
  await head.dblclick();
  assert.equal(await field.inputValue(), "<b>api</b>");
  await page.keyboard.press("Backspace");
  await page.keyboard.press("Enter");
  assert.deepEqual(await sentCalls("rename_pane"), [{ pane: 1, name: "" }], "an empty name asks for the automatic title");

}

async function clockCase(page, url) {
  // Time is fixed before the page starts: 10:15:40 local, so thirty seconds later the minute has changed.
  await page.clock.install({ time: new Date(2026, 0, 5, 10, 15, 40) });
  await open(page, url);
  assert.deepEqual(await texts(page, "#bar .clock"), ["10:15"]);
  await page.clock.fastForward(30_000);
  assert.deepEqual(await texts(page, "#bar .clock"), ["10:16"], "the clock updates every 30 seconds");
  await page.clock.fastForward(30_000);
  assert.deepEqual(await texts(page, "#bar .clock"), ["10:16"]);
  await page.clock.fastForward(60_000);
  assert.deepEqual(await texts(page, "#bar .clock"), ["10:17"]);
}

async function platformCase(page, url) {
  const padding = () => page.evaluate(() => parseFloat(getComputedStyle(document.getElementById("bar")).paddingLeft));
  const platform = () => page.evaluate(() => document.documentElement.className);

  await open(page, url, { ...INFO, os: "macos" });
  assert.match(await platform(), /\bos-macos\b/, "the macOS class is on the root");
  assert.ok((await padding()) >= 64, `the bar clears the traffic lights: ${await padding()} px of padding`);

  await open(page, url, { ...INFO, os: "linux" });
  assert.doesNotMatch(await platform(), /os-macos/);
  assert.ok((await padding()) < 24, `no room for traffic lights outside macOS: ${await padding()} px`);

  await open(page, url, { ...INFO, os: "windows" });
  assert.match(await platform(), /\bos-windows\b/, "the Windows class is on the root");
  assert.doesNotMatch(await platform(), /os-macos/);
  assert.ok((await padding()) < 24, `no room for traffic lights outside macOS: ${await padding()} px`);

  await frameCase(page, url);

  // The stub's own answer (null): every field is an empty string and the page still starts.
  await open(page, url, null);
  assert.doesNotMatch(await platform(), /os-macos/);
  assert.deepEqual(await texts(page, "#bar .host"), [""]);
  assert.deepEqual(await texts(page, "#status .seg.version"), [""]);
  assert.equal((await callsOf(page, "app_info")).length, 1, "app_info is asked once at boot");
}

/**
 * Windows and Linux keep the native title bar (owner decision of 2026-10-12): it sits above the page, so the page's
 * own bar needs no room for traffic lights and nothing else changes. Under every platform class the regions lie
 * where they lie under macOS apart from the bar's left padding: the same heights, in the same order, no overlap, and
 * the bar's free area still carries the drag attribute.
 */
async function frameCase(page, url) {
  const layout = () =>
    page.evaluate(() => {
      const box = (selector) => {
        const r = document.querySelector(selector).getBoundingClientRect();
        return { left: r.left, top: r.top, width: r.width, height: r.height };
      };
      const tabs = [...document.querySelectorAll("#bar .tab")].map((n) => n.getBoundingClientRect());
      return {
        bar: box("#bar"),
        stage: box("#stage"),
        status: box("#status"),
        tabTops: tabs.map((r) => r.top),
        tabHeights: tabs.map((r) => r.height),
        headHeights: [...document.querySelectorAll("#overlay .panehead")].map((n) => n.getBoundingClientRect().height),
        drag: [...document.querySelectorAll("#bar .bar-fill, #bar .bar-sep, #bar .bar-info")].map((n) =>
          n.hasAttribute("data-tauri-drag-region"),
        ),
        padding: parseFloat(getComputedStyle(document.getElementById("bar")).paddingLeft),
      };
    });

  const seen = {};
  for (const os of ["macos", "windows", "linux", null]) {
    await open(page, url, os === null ? { ...INFO, os: "" } : { ...INFO, os });
    await showView(page, BAR_VIEW);
    seen[os] = await layout();
    const l = seen[os];
    assert.ok(l.drag.length > 0 && l.drag.every(Boolean), `${os}: the bar's free area drags the window`);
    assert.equal(l.bar.top, 0, `${os}: the bar is the first strip of the page, directly under the native frame`);
    assert.equal(l.stage.top, l.bar.top + l.bar.height, `${os}: the stage starts where the bar ends`);
    assert.equal(l.status.top, l.stage.top + l.stage.height, `${os}: the status line starts where the stage ends`);
    assert.ok(l.headHeights.length > 0, `${os}: the pane headers are drawn`);
  }
  assert.equal(seen.macos.padding, 80, "macOS: the room for the traffic lights");
  for (const os of ["windows", "linux", null]) {
    const { padding: other, ...rest } = seen[os];
    const { padding: mac, ...same } = seen.macos;
    assert.equal(other, seen[null].padding, `${os}: the base padding of the bar`);
    assert.ok(other < mac, `${os}: less room than macOS`);
    // The bar's width is the same; its content starts further left, so only the left edges of the tabs move.
    assert.deepEqual(rest, same, `${os}: the regions lie where they lie under macOS`);
  }
}

async function statusCase(page, url) {
  await open(page, url);
  await showView(page, BAR_VIEW);

  const place = await page.evaluate(() => ({
    after: document.getElementById("stage").nextElementSibling?.id,
    parent: document.getElementById("status")?.parentElement?.id,
  }));
  assert.deepEqual(place, { after: "status", parent: "app" }, "the status line is the strip after the stage");

  assert.deepEqual(await texts(page, "#status .seg.ws"), ["1 alpha", "2 beta"], "one segment per workspace");
  assert.deepEqual(await texts(page, "#status .seg.ws.active"), ["1 alpha"]);
  const current = await page.evaluate(() =>
    [...document.querySelectorAll("#status .seg.ws")].map((n) => ({
      current: n.getAttribute("aria-current"),
      weight: getComputedStyle(n).fontWeight,
      upper: getComputedStyle(n).textTransform,
    })),
  );
  assert.equal(current[0].current, "true", "the active segment is aria-current");
  assert.equal(current[1].current, null);
  assert.ok(Number(current[0].weight) > Number(current[1].weight), "the active segment is bolder: a cue without colour");
  assert.equal(current[0].upper, "uppercase");

  assert.deepEqual(await texts(page, "#status .seg.panes"), ["1 pane"]);
  assert.match((await texts(page, "#status .seg.renderer"))[0], /^canvas2d\b/);
  assert.deepEqual(await texts(page, "#status .seg.encoding"), ["UTF-8"]);
  assert.deepEqual(await texts(page, "#status .seg.version"), ["0.0.1-alpha1"]);

  // The order is the contract of the design: workspaces, panes, renderer, encoding, version.
  const order = await page.evaluate(() =>
    [...document.querySelectorAll("#status .seg")].map((n) => [...n.classList].filter((c) => !["seg", "label", "active"].includes(c))[0]),
  );
  // The mode segment (copy mode, plan 21) and the notice (a refused tear-off) sit after the workspaces; both are empty,
  // so not shown, outside copy mode and when nothing was refused.
  assert.deepEqual(order, ["ws", "ws", "mode", "notice", "panes", "renderer", "encoding", "version"]);
  assert.deepEqual(await texts(page, "#status .seg.mode"), [""], "no copy mode, no mode text");
  assert.deepEqual(await texts(page, "#status .seg.notice"), [""], "nothing refused, no notice");

  // A workspace that appears and one that goes: the segments follow the view.
  const three = { ...BAR_VIEW, workspaces: [...BAR_VIEW.workspaces, { name: "gamma", active: false }] };
  await showView(page, three);
  assert.deepEqual(await texts(page, "#status .seg.ws"), ["1 alpha", "2 beta", "3 gamma"]);
  await showView(page, { ...BAR_VIEW, workspaces: [{ name: "beta", active: true }] });
  assert.deepEqual(await texts(page, "#status .seg.ws"), ["1 beta"]);

  // Two panes: the count follows the layout.
  await page.evaluate(() => window.__MOCK__.reset());
  await emit(page, "layout", await page.evaluate(() => window.__MOCK__.view()));
  assert.deepEqual(await texts(page, "#status .seg.panes"), ["2 panes"]);
}

async function rendererCase(page, url) {
  await open(page, url);
  // Thirty draws make one statistics batch; every layout event schedules one draw.
  await page.evaluate(async () => {
    const view = window.__MOCK__.view();
    for (let i = 0; i < 34; i++) {
      window.__MOCK__.emit("layout", view);
      await new Promise((done) => requestAnimationFrame(() => done()));
    }
  });
  await page.waitForFunction(
    () => /^canvas2d \d+\.\d{2} ms$/.test(document.querySelector("#status .seg.renderer")?.textContent ?? ""),
    undefined,
    { timeout: 5000 },
  );
  assert.equal(await page.locator("#stats").count(), 0, "the transitional #stats text is gone");
}

async function hostileCase(page, url) {
  // Names, titles and the host name are text, never markup (T-02-68).
  const bad = '<img src=x onerror="window.__pwned=1"><b>bold</b>';
  await open(page, url, { version: "<i>v</i>", os: "linux", hostname: bad });
  const view = structuredClone(BAR_VIEW);
  view.workspaces[0].name = bad;
  view.tabs[0].title = "<script>window.__pwned=1</script>";
  view.tabs[1].title = "shell"; // a tab still called shell shows the title its program set
  await showView(page, view);
  await emit(page, "title", { pane: 2, title: bad });

  const nodes = await page.locator("#bar img, #bar b, #bar script, #bar i, #status img, #status b, #status i").count();
  assert.equal(nodes, 0, "a name was parsed as markup");
  assert.equal((await texts(page, "#bar .ws-chip"))[0], bad);
  assert.equal((await texts(page, "#bar .tab"))[0], "1 <script>window.__pwned=1</script>");
  assert.equal((await texts(page, "#bar .tab"))[1], `2 ${bad}`);
  assert.deepEqual(await texts(page, "#bar .host"), [bad]);
  assert.deepEqual(await texts(page, "#status .seg.version"), ["<i>v</i>"]);
  assert.equal(await page.evaluate(() => window.__pwned), undefined);
}

/** Every colour comes from a token that app.css defines once; the chrome styles hold none of their own. */
async function tokenCase(page, url) {
  const dist = (name) => readFileSync(join(APP_DIR, "dist", name), "utf8");
  const app = dist("app.css");
  const chrome = dist("chrome.css");

  const TOKENS = [
    "--hw-ground", "--hw-ground-2", "--hw-pane", "--hw-edge", "--hw-ink", "--hw-ink-2", "--hw-ink-3", "--hw-rose",
    "--hw-rose-deep", "--hw-rose-wash", "--hw-ok", "--hw-warn", "--hw-danger", "--hw-info",
  ];
  for (const token of TOKENS) {
    const defined = app.match(new RegExp(`(^|[;{\\s])${token}\\s*:`, "g")) ?? [];
    assert.equal(defined.length, 1, `${token} must be defined exactly once in app.css, found ${defined.length}`);
    assert.doesNotMatch(chrome, new RegExp(`(^|[;{\\s])${token}\\s*:`), `${token} is redefined in chrome.css`);
  }

  const colours = chrome.match(/#[0-9a-fA-F]{3,8}\b|\b(?:rgb|rgba|hsl|hsla|hwb|lab|lch|oklab|oklch|color-mix)\(/g) ?? [];
  assert.deepEqual(colours, [], `chrome.css hard-codes colours: ${colours.join(", ")}`);
  const named = chrome.match(/:\s*(?:[^;{}]*\s)?(?:white|black|red|green|blue|gray|grey|yellow|orange|pink|purple|cyan|magenta)\b/g) ?? [];
  assert.deepEqual(named, [], `chrome.css uses named colours: ${named.join(", ")}`);

  // The old token names have no user left.
  for (const file of readdirSync(join(APP_DIR, "dist")).filter((f) => f.endsWith(".css"))) {
    const old = dist(file).match(/var\(--(?:ground|bar|edge|ink|ink2|rose)\)/g) ?? [];
    assert.deepEqual(old, [], `${file} still uses the old token names`);
  }

  // The tokens reach the elements: the bar is the ground-2 colour (#1D0F26), the page the ground (#14091A).
  await open(page, url);
  const colour = await page.evaluate(() => ({
    bar: getComputedStyle(document.getElementById("bar")).backgroundColor,
    status: getComputedStyle(document.getElementById("status")).backgroundColor,
    body: getComputedStyle(document.body).backgroundColor,
  }));
  assert.equal(colour.bar, "rgb(29, 15, 38)");
  assert.equal(colour.status, "rgb(29, 15, 38)");
  assert.equal(colour.body, "rgb(20, 9, 26)");

  // No source file builds markup from a string.
  const src = join(APP_DIR, "src");
  for (const f of readdirSync(src).filter((n) => n.endsWith(".ts"))) {
    const text = readFileSync(join(src, f), "utf8");
    assert.doesNotMatch(text, /\.(?:inner|outer)HTML\s*=|insertAdjacentHTML|document\.write\(/, `${f} builds markup`);
  }
}

/** One header strip per visible pane, over the pane's top edge, with the focus line only on the focused one. */
async function headerCase(page, url) {
  await open(page, url);
  const c = await canvasBox(page);
  const strips = await heads(page);
  assert.deepEqual(strips.map((h) => h.pane), ["1", "2"], "one header per visible pane, in layout order");
  for (const h of strips) {
    assert.equal(h.height, HEADER, `pane ${h.pane}: the strip is as high as state.headerPx`);
    assert.equal(h.top, c.top, `pane ${h.pane}: the strip sits on the pane's top edge`);
    assert.equal(h.width, c.width / 2, `pane ${h.pane}: the strip is as wide as the pane`);
    assert.equal(h.events, "none", `pane ${h.pane}: the strip must not take pointer events`);
  }
  assert.equal(strips[0].left, c.left);
  assert.equal(strips[1].left, c.left + c.width / 2);

  // Clicks go through the strip to the terminal.
  const through = await page.evaluate(
    ([x, y]) => document.elementFromPoint(x, y)?.id,
    [c.left + c.width * 0.75, c.top + HEADER / 2],
  );
  assert.equal(through, "term", "a click on a header reaches the canvas");

  // Pane 2 has the focus: a rose line and a filled dot. Pane 1 has neither.
  assert.equal(strips[1].focused, true);
  assert.equal(strips[1].line, true, "the focused pane has the focus line while two panes are visible");
  assert.equal(strips[1].lineWidth, "1px");
  assert.equal(strips[1].lineColour, ROSE, "the focus line is rose");
  assert.equal(strips[0].focused, false);
  assert.equal(strips[0].line, false);
  assert.notEqual(strips[0].lineColour, ROSE, "an unfocused pane shows no rose");
  assert.deepEqual(strips.map((h) => h.state), ["idle", "focused"]);
  assert.notEqual(strips[1].dotFill, "rgba(0, 0, 0, 0)", "the focused dot is filled");
  assert.equal(strips[0].dotFill, "rgba(0, 0, 0, 0)", "an unfocused dot is hollow");

  // Rose is rationed: nothing else in a header is rose.
  const rose = await page.evaluate((rose) => {
    const out = [];
    for (const n of document.querySelectorAll("#overlay .panehead, #overlay .panehead *")) {
      const s = getComputedStyle(n);
      for (const p of ["color", "backgroundColor", "borderBottomColor", "borderLeftColor", "borderRightColor"]) {
        if (s[p] === rose) out.push(`${n.className}:${p}`);
      }
    }
    return out;
  }, ROSE);
  assert.deepEqual(rose, [], "rose appears in a header outside the focus line");

  // A single pane has no focus line (design.md section 5), but it is still the focused one.
  const single = { ...BAR_VIEW, focus: 1 };
  await showView(page, single);
  const one = await heads(page);
  assert.equal(one.length, 1);
  assert.equal(one[0].line, false, "no focus line with a single pane");
  assert.notEqual(one[0].lineColour, ROSE);
  assert.equal(one[0].state, "focused");
  assert.equal(one[0].width, c.width, "the single header spans the canvas");

  // The headers are keyed: a pane that leaves takes its header, one that comes gets one.
  await page.evaluate(() => window.__MOCK__.reset());
  await emit(page, "layout", await page.evaluate(() => window.__MOCK__.view()));
  assert.deepEqual((await heads(page)).map((h) => h.pane), ["1", "2"]);
}

/** State is never colour alone: the exited pane has its own dot shape and a label. */
async function headerStateCase(page, url) {
  await open(page, url);
  await emit(page, "pane-exit", { pane: 1, code: 0 });
  const [gone, live] = await heads(page);
  assert.equal(gone.state, "exited");
  assert.equal(gone.exited, true);
  assert.equal(gone.ctx, "exited", "the context slot says it");
  assert.equal(gone.dotFill, "rgba(0, 0, 0, 0)", "the exited dot is hollow");
  assert.notEqual(gone.dotMark, "none", "the exited dot carries a dash");
  assert.equal(live.dotMark, "none", "a live dot has no dash");
  assert.equal(live.state, "focused");
  assert.equal(live.exited, false);
}

/** Titles and working directories are text; long ones are cut by the style, never by the layout. */
async function headerTextCase(page, url) {
  await open(page, url);
  const [a, b] = await heads(page);
  assert.equal(a.title, "shell", "without a title event the header shows the tab's title");
  assert.equal(b.title, "shell");
  assert.equal(a.ctx, "", "no context before a working directory was reported");

  await emit(page, "title", { pane: 2, title: "vim README.md" });
  await emit(page, "cwd", { pane: 2, cwd: "/home/holz/src/app" });
  await emit(page, "cwd", { pane: 1, cwd: "/etc/nginx" });
  const [p1, p2] = await heads(page);
  assert.equal(p2.title, "vim README.md");
  assert.equal(p2.ctx, "~/src/app", "the home prefix is shown as ~");
  assert.equal(p1.title, "shell", "the other pane keeps the tab's title");
  assert.equal(p1.ctx, "/etc/nginx", "a path outside home is shown whole");

  // Hostile and huge strings (T-02-68, T-02-70).
  const bad = '<img src=x onerror="window.__pwned=1"><b>bold</b>';
  await emit(page, "title", { pane: 1, title: bad });
  await emit(page, "cwd", { pane: 1, cwd: `/srv/${bad}` });
  await emit(page, "title", { pane: 2, title: "t".repeat(4096) });
  await emit(page, "cwd", { pane: 2, cwd: `/home/holz/${"d/".repeat(2000)}` });
  assert.equal(await page.locator("#overlay img, #overlay b").count(), 0, "a title was parsed as markup");
  assert.equal(await page.evaluate(() => window.__pwned), undefined);
  const [h1, h2] = await heads(page);
  assert.equal(h1.title, bad);
  assert.equal(h1.ctx, `/srv/${bad}`);
  assert.equal(h2.height, HEADER, "a huge title does not make the strip grow");
  assert.equal(h2.width, (await canvasBox(page)).width / 2);

  const style = await page.evaluate(() => {
    const out = {};
    for (const part of ["ph-title", "ph-ctx"]) {
      const n = document.querySelectorAll(`#overlay .${part}`)[1];
      const s = getComputedStyle(n);
      out[part] = { overflow: s.overflow, wrap: s.whiteSpace, ellipsis: s.textOverflow, cut: n.scrollWidth > n.clientWidth };
    }
    return out;
  });
  for (const part of ["ph-title", "ph-ctx"]) {
    assert.deepEqual(
      [style[part].overflow, style[part].wrap, style[part].ellipsis, style[part].cut],
      ["hidden", "nowrap", "ellipsis", true],
      `${part}: cut with an ellipsis`,
    );
  }
}

/** The core is told grids that start below the header, and the pane's first row is drawn there. */
async function gridCase(page, url) {
  await open(page, url);
  const c = await canvasBox(page);
  const started = (await callsOf(page, "start_pane")).sort((a, b) => a.pane - b.pane);
  const rows = Math.floor((c.height - HEADER - PAD) / ROW_PX);
  assert.deepEqual(started.map((s) => s.rows), [rows, rows], "the grid is sized below the header");

  // Row 0 of pane 1 is drawn right under the header: a coloured run shows there and not above it.
  const RED = 0xff0000;
  await page.evaluate(
    ([red]) =>
      window.__MOCK__.emit("frame", {
        pane: 1, seq: 1, full: true, cols: 20, rows: 4, fg: 0xf6efe6, bg: 0x14091a, cursor: null,
        scrollbar: [0, 4, 4], selection: [], matches: [], search: null, copy_cursor: null,
        modes: { alt: false, bracketed_paste: false, mouse: false },
        changed: [{ y: 0, runs: [[" ".repeat(10), 10, 0xf6efe6, red, 0]] }],
      }),
    [RED],
  );
  const pixel = (x, y) =>
    page.evaluate(
      ([px, py]) => {
        const cv = document.getElementById("term");
        const [r, g, b] = cv.getContext("2d").getImageData(px, py, 1, 1).data;
        return (r << 16) | (g << 8) | b;
      },
      [x, y],
    );
  await page.waitForFunction(
    async ([x, y]) => {
      await new Promise((done) => requestAnimationFrame(() => done()));
      const [r, g, b] = document.getElementById("term").getContext("2d").getImageData(x, y, 1, 1).data;
      return r === 0xff && g === 0 && b === 0;
    },
    [PAD + 2, HEADER + 2],
    { timeout: 5000 },
  );
  assert.equal(await pixel(PAD + 2, HEADER + 2), RED, "the first row starts at the header's lower edge");
  assert.notEqual(await pixel(PAD + 2, HEADER - 2), RED, "nothing of the grid is drawn under the header");
}

/** The UI scale is the root font size: the header, the bar and the grid below the header follow it. */
async function scaleCase(page, url) {
  await open(page, url);
  const scaled = async (px) => {
    const before = (await callsOf(page, "resize")).length;
    await page.evaluate((size) => {
      document.documentElement.style.fontSize = `${size}px`;
      window.dispatchEvent(new Event("resize"));
    }, px);
    await page.waitForFunction((n) => window.__MOCK__.callsOf("resize").length >= n + 2, before, { timeout: 5000 });
    return page.evaluate(() => ({
      token: getComputedStyle(document.documentElement).getPropertyValue("--hw-header").trim(),
      head: document.querySelector("#overlay .panehead")?.getBoundingClientRect().height,
      bar: document.getElementById("bar").getBoundingClientRect().height,
      status: document.getElementById("status").getBoundingClientRect().height,
      canvas: document.getElementById("term").getBoundingClientRect().height,
      resizes: window.__MOCK__.callsOf("resize").slice(-2),
    }));
  };

  const big = await scaled(18); // UI scale 150 %
  assert.equal(big.token, "30px");
  assert.equal(big.head, 30, "the header follows the root font size");
  assert.equal(big.bar, 48, "the bar follows it too");
  assert.equal(big.status, 36);
  const rows = Math.floor((big.canvas - 30 - PAD) / ROW_PX);
  assert.deepEqual(big.resizes.map((r) => r.rows), [rows, rows], "the resize payload is the grid below the header");

  const odd = await scaled(13);
  assert.equal(odd.token, "22px", "20/12 of 13 px, snapped to a device pixel");
  assert.equal(odd.head, 22);
  const rows13 = Math.floor((odd.canvas - 22 - PAD) / ROW_PX);
  assert.deepEqual(odd.resizes.map((r) => r.rows), [rows13, rows13]);
}
