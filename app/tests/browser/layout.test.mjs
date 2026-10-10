// The frontend's preserved behaviour after the main.ts split: shortcuts, plain keys, paste and the key handler
// stack. Every case drives the real page against the Tauri stub and reads the commands the page sent.
import assert from "node:assert/strict";
import { callsOf } from "./lib.mjs";

/** Open the app against the stub and wait until the layout is on screen and every pane was started. */
async function open(page, url) {
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await page.evaluate(() => document.getElementById("term").focus());
}

/** Every command the page sent so far, as [cmd, args], without the start-up ones. */
const sent = (page) =>
  page.evaluate(() => window.__MOCK__.calls.filter(([c]) => c !== "get_layout" && c !== "start_pane" && c !== "resize"));

/** Press `chord` and return the commands it caused, in order. */
async function press(page, chord) {
  const before = (await sent(page)).length;
  await page.keyboard.press(chord);
  return (await sent(page)).slice(before);
}

export default async function ({ page, url }) {
  await shortcutCase(page, url);
  await plainKeyCase(page, url);
  await handlerStackCase(page, url);
  await barCase(page, url);
  await pointerCase(page, url);
  await wheelCase(page, url);
  await copyCase(page, url);
  await resizeCase(page, url);
}

const PAD = 6; // CSS px between a pane's edge and its grid (geometry.ts)
const ROW_PX = 18; // one cell row: ceil(14 px font * 1.25)

/** Two workspaces, three tabs and one pane. */
const BAR_VIEW = {
  workspaces: [{ name: "alpha", active: true }, { name: "beta", active: false }],
  tabs: [{ title: "one", active: true }, { title: "two", active: false }, { title: "three", active: false }],
  focus: 1,
  panes: [[1, { x: 0, y: 0, w: 1, h: 1 }]],
  dividers: [],
};

/** Show `view` the way the core would announce a new layout. */
async function showView(page, view) {
  await page.evaluate((v) => window.__MOCK__.setView(v), view);
  await page.evaluate((v) => window.__MOCK__.emit("layout", v), view);
}

/** The page position of the canvas' top-left corner. */
const canvasOrigin = (page) =>
  page.evaluate(() => {
    const r = document.getElementById("term").getBoundingClientRect();
    return { left: r.left, top: r.top, width: r.width };
  });

async function barCase(page, url) {
  await open(page, url);
  await showView(page, BAR_VIEW);
  const labels = () => page.locator("#bar button").allTextContents();
  assert.deepEqual(await labels(), ["alpha", "beta", "+", "one", "two", "three", "+"], "the bar shows the names");
  assert.deepEqual(
    await page.locator("#bar button.active").allTextContents(),
    ["alpha", "one"],
    "the active workspace and tab are marked",
  );

  const buttons = page.locator("#bar button");
  await buttons.nth(4).click(); // tab "two"
  assert.deepEqual((await callsOf(page, "select_tab")).at(-1), { index: 1 });
  await buttons.nth(1).click(); // workspace "beta"
  assert.deepEqual((await callsOf(page, "select_workspace")).at(-1), { index: 1 });
  await page.locator("#bar button.plus").nth(0).click();
  assert.deepEqual((await callsOf(page, "new_workspace")).at(-1), { name: "ws 3" }, "the first plus adds a workspace");
  await page.locator("#bar button.plus").nth(1).click();
  assert.equal((await callsOf(page, "new_tab")).length, 1, "the second plus adds a tab");
  assert.equal(
    await page.evaluate(() => document.activeElement?.id),
    "ime",
    "a bar button gives the keyboard back to the terminal input",
  );

  // Names are text, never markup (T-02-84).
  const hostile = structuredClone(BAR_VIEW);
  hostile.workspaces[0].name = '<img src=x onerror="window.__pwned=1"><b>bold</b>';
  hostile.tabs[0].title = "<script>window.__pwned=1</script>";
  await showView(page, hostile);
  assert.equal(await page.locator("#bar img, #bar b, #bar script").count(), 0, "a name was parsed as markup");
  assert.equal((await labels())[0], '<img src=x onerror="window.__pwned=1"><b>bold</b>');
  assert.equal(await page.evaluate(() => window.__pwned), undefined);
}

async function pointerCase(page, url) {
  await open(page, url);
  const c = await canvasOrigin(page);
  const y = c.top + PAD + ROW_PX / 2;
  // Pane 1 is the left half and pane 2 (focused) the right half of the default stub layout.
  const drag = async (fromX, toX) => {
    await page.mouse.move(fromX, y);
    await page.mouse.down();
    await page.mouse.move(toX, y, { steps: 4 });
    await page.mouse.up();
  };
  const kinds = (calls) => calls.map(([cmd, args]) => (cmd === "select" ? `select:${args.op.kind}` : cmd));

  const before = (await sent(page)).length;
  await drag(c.left + c.width / 2 + PAD + 4, c.left + c.width / 2 + PAD + 44);
  const dragged = (await sent(page)).slice(before);
  const dragKinds = kinds(dragged);
  assert.equal(dragKinds[0], "select:begin", "a drag begins a selection in the core");
  assert.ok(dragKinds.includes("select:extend"), "a drag extends it");
  assert.ok(dragKinds.every((k) => k.startsWith("select:")), `a drag in the focused pane sends only select: ${dragKinds}`);
  assert.ok(dragged.every(([, args]) => args.pane === 2), "the selection is in the pane under the pointer");
  assert.equal(await page.locator("#overlay .sel").count(), 0, "the page draws no selection of its own: the frames do");

  const mid = (await sent(page)).length;
  await page.mouse.click(c.left + PAD + 4, y); // a plain click is not a selection
  assert.deepEqual(
    kinds((await sent(page)).slice(mid)),
    ["focus_pane", "select:begin", "select:clear"],
    "a click in the other pane focuses it, begins and clears",
  );
  assert.deepEqual((await sent(page)).slice(mid)[0], ["focus_pane", { pane: 1 }]);

  const last = (await sent(page)).length;
  await drag(c.left + PAD + 4, c.left + PAD + 44);
  const extra = (await sent(page)).slice(last).filter(([cmd]) => cmd !== "focus_pane" && cmd !== "select");
  assert.deepEqual(extra, [], "a selection drag sends nothing but focus and select commands");
}

async function wheelCase(page, url) {
  await open(page, url);
  const c = await canvasOrigin(page);
  await page.mouse.move(c.left + c.width * 0.75, 200);
  await page.mouse.wheel(0, 120);
  await page.waitForFunction(() => window.__MOCK__.callsOf("wheel").length >= 1, undefined, { timeout: 5000 });
  const down = (await callsOf(page, "wheel")).at(-1);
  assert.equal(down.pane, 2, "the wheel goes to the pane under the pointer");
  assert.ok(down.rows > 0, `a downward wheel gave ${down.rows} rows`);

  await page.mouse.move(c.left + c.width * 0.25, 200);
  await page.mouse.wheel(0, -120);
  await page.waitForFunction(() => window.__MOCK__.callsOf("wheel").length >= 2, undefined, { timeout: 5000 });
  const up = (await callsOf(page, "wheel")).at(-1);
  assert.equal(up.pane, 1, "the wheel follows the pointer to the other pane");
  assert.ok(up.rows < 0, `an upward wheel gave ${up.rows} rows`);
}

async function copyCase(page, url) {
  await open(page, url);
  // The page copies through a hidden textarea and execCommand; record what it would copy.
  await page.evaluate(() => {
    window.__copied = null;
    document.execCommand = (cmd) => {
      window.__copied = [cmd, document.activeElement && document.activeElement.value];
      return true;
    };
    window.__MOCK__.on("selection_text", () => "hello world");
  });
  const row = (y, text) => ({ y, runs: [[text, text.length, 0xf6efe6, 0x14091a, 0]] });
  await page.evaluate((frame) => window.__MOCK__.emit("frame", frame), {
    pane: 2, seq: 1, full: true, cols: 60, rows: 4, fg: 0xf6efe6, bg: 0x14091a, cursor: null,
    scrollbar: [0, 4, 4], selection: [[0, 0, 10]], matches: [], search: null, copy_cursor: null,
    modes: { alt: false, bracketed_paste: false, mouse: false },
    changed: [row(0, "hello world"), row(1, "second row")],
  });
  await page.waitForFunction(() => window.__MOCK__.callsOf("selection_text").length >= 1);
  await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => done())));
  const before = (await sent(page)).length;
  await page.keyboard.press("Meta+c");
  assert.deepEqual(await page.evaluate(() => window.__copied), ["copy", "hello world"], "Cmd+C copies the selected text");
  const calls = (await sent(page)).slice(before);
  assert.deepEqual(calls, [], "Cmd+C must not reach the shell or the core");
}

async function resizeCase(page, url) {
  await open(page, url);
  const started = (await callsOf(page, "start_pane")).sort((a, b) => a.pane - b.pane);
  assert.equal(started.length, 2);
  await page.setViewportSize({ width: 700, height: 500 });
  await page.waitForFunction(() => window.__MOCK__.callsOf("resize").length >= 2, undefined, { timeout: 5000 });
  const resized = (await callsOf(page, "resize")).sort((a, b) => a.pane - b.pane);
  assert.deepEqual(resized.map((r) => r.pane), [1, 2], "every pane is told its new size once");
  for (const [i, r] of resized.entries()) {
    assert.ok(r.cols < started[i].cols, `pane ${r.pane} has ${r.cols} columns, it started with ${started[i].cols}`);
    assert.ok(r.rows < started[i].rows, `pane ${r.pane} has ${r.rows} rows, it started with ${started[i].rows}`);
  }
}

async function shortcutCase(page, url) {
  await open(page, url);
  // The default stub view: one workspace, one tab, panes 1 and 2 with pane 2 focused.
  const cases = [
    ["Meta+d", [["split", { column: false }]]],
    ["Meta+Shift+D", [["split", { column: true }]]],
    ["Meta+t", [["new_tab", undefined]]],
    ["Meta+Shift+N", [["new_workspace", { name: "ws 2" }]]],
    ["Meta+w", [["close_pane", { pane: 2 }]]],
    ["Meta+]", [["cycle_focus", { step: 1 }]]],
    ["Meta+[", [["cycle_focus", { step: -1 }]]],
    ["Meta+1", [["select_tab", { index: 0 }]]],
    ["Meta+9", [["select_tab", { index: 8 }]]],
    ["Meta+Alt+2", [["select_workspace", { index: 1 }]]],
  ];
  for (const [chord, want] of cases) {
    assert.deepEqual(await press(page, chord), want, `${chord} sent the wrong commands`);
  }

  // A Cmd chord that nothing owns goes nowhere: not to a command and not to the shell. Cmd+Shift+W is one of them
  // since Cmd+W closes the pane (D-14).
  for (const chord of ["Meta+z", "Meta+Shift+W", "Meta+k"]) {
    assert.deepEqual(await press(page, chord), [], `${chord} must send nothing`);
  }
}

async function plainKeyCase(page, url) {
  await open(page, url);
  // A key is a `key` command with the facts of the event (down, then up); the core builds the bytes.
  const key = (code, k, mods = 0) => [
    ["key", { pane: 2, code, key: k, mods, repeat: false, release: false, composing: false }],
    ["key", { pane: 2, code, key: k, mods, repeat: false, release: true, composing: false }],
  ];
  assert.deepEqual(await press(page, "a"), key("KeyA", "a"), "a plain key goes to the focused pane");
  assert.deepEqual(await press(page, "Enter"), key("Enter", "Enter"));
  assert.deepEqual(await press(page, "ArrowUp"), key("ArrowUp", "ArrowUp"));

  // Paste goes to the focused pane through the core, not as typed text.
  await page.evaluate(() => {
    const data = new DataTransfer();
    data.setData("text/plain", "echo pasted");
    window.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data }));
  });
  const calls = await sent(page);
  assert.deepEqual(
    calls[calls.length - 1],
    ["paste", { pane: 2, text: "echo pasted", allow_unsafe: false }],
    "paste must reach the pane",
  );
}

async function handlerStackCase(page, url) {
  await open(page, url);
  // The page's own module instance: the same URL resolves to the same module record.
  const install = (kind) =>
    page.evaluate(async (k) => {
      const mod = await import(new URL("keyboard.js", location.href).href);
      window.__seen ??= [];
      window.__pops ??= {};
      window.__pops[k.name] = mod.pushKeyHandler((e) => {
        // The modifier's own keydown comes first; only the key under test is recorded.
        if (e.key.length === 1) window.__seen.push(`${k.name}:${e.key}`);
        return k.consume;
      });
    }, kind);
  const pop = (name) => page.evaluate((n) => window.__pops[n](), name);
  const seen = () => page.evaluate(() => window.__seen.splice(0));

  // A handler that returns false lets the shortcut run.
  await install({ name: "watch", consume: false });
  assert.deepEqual(await press(page, "Meta+d"), [["split", { column: false }]], "a handler that passes must not block");
  assert.deepEqual(await seen(), ["watch:d"]);

  // A newer handler that consumes stops the shortcut, the older handler and the shell.
  await install({ name: "grab", consume: true });
  assert.deepEqual(await press(page, "Meta+d"), [], "a consuming handler must stop the shortcut");
  assert.deepEqual(await press(page, "a"), [], "a consuming handler must stop the key for the shell");
  assert.deepEqual(await seen(), ["grab:d", "grab:a"], "the older handler must not see a consumed key");

  // Popping it restores the shortcut and the older handler is consulted again.
  await pop("grab");
  assert.deepEqual(await press(page, "Meta+d"), [["split", { column: false }]], "popping must restore the shortcut");
  assert.deepEqual(
    (await press(page, "a")).map(([cmd, args]) => [cmd, args.pane, args.key, args.release]),
    [["key", 2, "a", false], ["key", 2, "a", true]],
    "popping must restore the shell",
  );
  assert.deepEqual(await seen(), ["watch:d", "watch:a"]);

  // Removing twice is harmless and removes nothing else.
  await pop("grab");
  await pop("watch");
  assert.deepEqual(await press(page, "Meta+d"), [["split", { column: false }]]);
  assert.deepEqual(await seen(), [], "a removed handler was consulted");
}
