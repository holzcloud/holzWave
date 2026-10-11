// The Terminal-modern chrome (D-01): the window bar, the status line and the design tokens, rendered from the
// core's state. Structure, state cues and tokens are asserted here; how it looks goes to the human checklist.
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { APP_DIR, callsOf, emit } from "./lib.mjs";

export default async function ({ page, url }) {
  await barCase(page, url);
  await titleCase(page, url);
  await platformCase(page, url);
  await statusCase(page, url);
  await rendererCase(page, url);
  await hostileCase(page, url);
  await tokenCase(page, url);
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
  await showView(page, BAR_VIEW);
  // A title the tab's focus pane set replaces the tab's own title; the other tabs keep theirs.
  await emit(page, "title", { pane: 1, title: "vim README.md" });
  assert.deepEqual(await texts(page, "#bar .tab"), ["1 vim README.md", "2 two", "3 three"]);
  await emit(page, "title", { pane: 2, title: "htop" });
  assert.deepEqual(await texts(page, "#bar .tab"), ["1 vim README.md", "2 htop", "3 three"]);
  // A pane that exited forgets its title.
  await emit(page, "pane-exit", { pane: 1, code: 0 });
  assert.deepEqual(await texts(page, "#bar .tab"), ["1 one", "2 htop", "3 three"]);
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

  // The stub's own answer (null): every field is an empty string and the page still starts.
  await open(page, url, null);
  assert.doesNotMatch(await platform(), /os-macos/);
  assert.deepEqual(await texts(page, "#bar .host"), [""]);
  assert.deepEqual(await texts(page, "#status .seg.version"), [""]);
  assert.equal((await callsOf(page, "app_info")).length, 1, "app_info is asked once at boot");
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
  assert.deepEqual(order, ["ws", "ws", "panes", "renderer", "encoding", "version"]);

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
