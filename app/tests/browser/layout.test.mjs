// The frontend's preserved behaviour after the main.ts split: shortcuts, plain keys, paste and the key handler
// stack. Every case drives the real page against the Tauri stub and reads the commands the page sent.
import assert from "node:assert/strict";

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
}

async function shortcutCase(page, url) {
  await open(page, url);
  // The default stub view: one workspace, one tab, panes 1 and 2 with pane 2 focused.
  const cases = [
    ["Meta+d", [["split", { column: false }]]],
    ["Meta+Shift+D", [["split", { column: true }]]],
    ["Meta+t", [["new_tab", undefined]]],
    ["Meta+Shift+N", [["new_workspace", { name: "ws 2" }]]],
    ["Meta+Shift+W", [["close_pane", { pane: 2 }]]],
    ["Meta+]", [["cycle_focus", { step: 1 }]]],
    ["Meta+[", [["cycle_focus", { step: -1 }]]],
    ["Meta+1", [["select_tab", { index: 0 }]]],
    ["Meta+9", [["select_tab", { index: 8 }]]],
    ["Meta+Alt+2", [["select_workspace", { index: 1 }]]],
  ];
  for (const [chord, want] of cases) {
    assert.deepEqual(await press(page, chord), want, `${chord} sent the wrong commands`);
  }

  // A Cmd chord that nothing owns goes nowhere: not to a command and not to the shell.
  for (const chord of ["Meta+z", "Meta+w", "Meta+k"]) {
    assert.deepEqual(await press(page, chord), [], `${chord} must send nothing`);
  }
}

async function plainKeyCase(page, url) {
  await open(page, url);
  assert.deepEqual(await press(page, "a"), [["input", { pane: 2, data: "a" }]], "a plain key goes to the focused pane");
  assert.deepEqual(await press(page, "Enter"), [["input", { pane: 2, data: "\r" }]]);
  assert.deepEqual(await press(page, "ArrowUp"), [["input", { pane: 2, data: "\x1b[A" }]]);
  assert.deepEqual(await press(page, "Control+C"), [["input", { pane: 2, data: "\x03" }]]);
  assert.deepEqual(await press(page, "Alt+b"), [["input", { pane: 2, data: "\x1bb" }]]);

  // Paste goes to the focused pane as typed text.
  await page.evaluate(() => {
    const data = new DataTransfer();
    data.setData("text", "echo pasted");
    window.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data }));
  });
  const calls = await sent(page);
  assert.deepEqual(calls[calls.length - 1], ["input", { pane: 2, data: "echo pasted" }], "paste must reach the pane");
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
  assert.deepEqual(await press(page, "a"), [["input", { pane: 2, data: "a" }]], "popping must restore the shell");
  assert.deepEqual(await seen(), ["watch:d", "watch:a"]);

  // Removing twice is harmless and removes nothing else.
  await pop("grab");
  await pop("watch");
  assert.deepEqual(await press(page, "Meta+d"), [["split", { column: false }]]);
  assert.deepEqual(await seen(), [], "a removed handler was consulted");
}
