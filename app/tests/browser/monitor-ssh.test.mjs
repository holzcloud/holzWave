// The system monitor of another computer (design.md section 6.5, 'On another computer'): the header says the connection's
// name, one line above the bars says in words whether the pane is connecting, has no answer or the host is not Linux,
// the last numbers stay, dimmed, while the host is silent, every word of the core or the host is text, and the palette
// opens one for each SSH connection. The browser stub plays the core: `answers` is the queue of what `monitor_sample`
// does, one item a call (a reading, `{ fail: text }` or `{ hold: true }`), and the last item repeats.
import assert from "node:assert/strict";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(6000);
  await page.addInitScript(setup, [VIEW, [TALOS, BOX]]);
  await liveCase(page, url);
  await connectingCase(page, url);
  await noAnswerCase(page, url);
  await unsupportedCase(page, url);
  await hostileCase(page, url);
  await entryCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

export const VIEW = {
  workspaces: [{ name: "main", active: true, panes: 2 }],
  tabs: [{ title: "sys", active: true, focus: 1, panes: 2 }],
  focus: 1,
  panes: [[1, { x: 0, y: 0, w: 0.5, h: 1 }], [2, { x: 0.5, y: 0, w: 0.5, h: 1 }]],
  dividers: [{ split: 10, axis: "Row", area: { x: 0, y: 0, w: 1, h: 1 }, ratio: 0.5 }],
  pane_names: [],
  windows: 1,
};

const connection = (id, name, kind) => ({
  id, name, kind, host: "h", port: 22, user: "u", auth: "key", key_path: "k", jump: null,
  startup: "", reconnect_attempts: 5, reconnect_delay_secs: 3, shell: "", has_secret: false,
});
export const TALOS = connection("talos", "Talos", "ssh");
export const BOX = connection("box", "Box", "wsl");

export const LIVE = {
  state: "live", cpu: 38.2, cores: 8, mem_used: 6.1 * 1024 ** 3, mem_total: 16 * 1024 ** 3, net_rx: 1.2 * 1024 * 1024,
  net_tx: 84 * 1024, uptime: 5400, procs: [{ pid: 4121, name: "node server", cpu: 12.4, mem: 300 * 1024 ** 2 }],
};

/** Runs before the page's own scripts. The case is in the hash of the address: `{ blocks, answers, label }`. */
export function setup([view, connections]) {
  const config = JSON.parse(decodeURIComponent(location.hash.slice(1) || "%7B%7D"));
  window.__MOCK__.setView(view);
  window.__MOCK__.on("connections_list", () => connections);
  window.__MOCK__.on("pane_blocks", () => config.blocks ?? []);
  window.__MOCK__.on("block_open", () => window.__MOCK__.view());
  const answers = config.answers ?? [];
  let n = 0;
  window.__release = null;
  window.__MOCK__.on("monitor_sample", () => {
    const item = answers[Math.min(n, answers.length - 1)];
    n += 1;
    if (item && item.hold) return new Promise((resolve) => { window.__release = resolve; });
    if (item && item.fail !== undefined) throw item.fail;
    return structuredClone(item);
  });
}

/** A monitor pane (pane 2) of the saved connection `talos`, named `label`, whose samples go as `answers`. */
export async function open(page, url, answers, label = "Talos", source = "talos") {
  const config = { blocks: [{ pane: 2, kind: "monitor", source, path: "", source_label: label }], answers };
  // Another hash on the same address is no new document: leave first, so the page and the stub start fresh.
  await page.goto("about:blank");
  await page.goto(`${url}index.html?renderer=canvas#${encodeURIComponent(JSON.stringify(config))}`);
  await page.waitForSelector(".block-monitor .mon-row");
}

const note = (page) => page.locator(".block-monitor .mon-note");
const values = (page) => page.locator(".block-monitor .mon-value").allTextContents();

async function liveCase(page, url) {
  await open(page, url, [LIVE]);
  await page.waitForFunction(() => document.querySelector(".block-monitor .mon-value")?.textContent === "38%");
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.callsOf("monitor_sample")[0]), { pane: 2 }, "the page names a pane and nothing else");
  assert.equal(await page.locator('.panehead[data-pane="2"] .ph-title').textContent(), "system");
  assert.equal(await page.locator('.panehead[data-pane="2"] .ph-ctx').textContent(), "Talos · 1 s");
  const rows = await page.locator(".block-monitor .mon-row").evaluateAll((n) => n.map((r) => Array.from(r.children).map((c) => c.textContent).filter(Boolean).join(" ")));
  assert.deepEqual(rows, ["CPU 38%", "MEM 6.1/16G", "NET ↓ 1.2 MB/s ↑ 84 KB/s", "UP 1 h 30 min · 8 cores"]);
  assert.equal(await note(page).isHidden(), true, "no note while the host answers");
  assert.equal(await note(page).textContent(), "");
  assert.equal(await page.locator(".block-monitor .mon").getAttribute("data-stale"), null);
}

async function connectingCase(page, url) {
  await open(page, url, [{ hold: true }, LIVE]);
  await page.waitForFunction(() => window.__release !== null);
  assert.equal(await note(page).textContent(), "Connecting to Talos …");
  assert.equal(await note(page).getAttribute("data-state"), "connecting");
  assert.equal(await note(page).getAttribute("data-tone"), "plain");
  assert.equal(await note(page).getAttribute("role"), "status");
  assert.deepEqual((await values(page)).filter(Boolean), [], "no numbers before the first answer");
  await page.evaluate((reading) => window.__release(reading), LIVE);
  await page.waitForFunction(() => document.querySelector(".block-monitor .mon-note")?.hidden === true);
  assert.equal(await note(page).textContent(), "");
  assert.equal(await note(page).getAttribute("data-state"), null);
  assert.equal((await values(page))[0], "38%");
}

async function noAnswerCase(page, url) {
  await open(page, url, [LIVE, { fail: "the connection was lost" }, LIVE]);
  await page.waitForFunction(() => document.querySelector(".block-monitor .mon-note")?.textContent === "No answer from Talos: the connection was lost", null, { timeout: 4000 });
  assert.equal(await note(page).getAttribute("data-tone"), "warn");
  assert.equal(await note(page).getAttribute("data-state"), "no_answer");
  const look = await note(page).evaluate((n) => ({ line: getComputedStyle(n).textDecorationLine, mark: getComputedStyle(n, "::before").content }));
  assert.ok(look.line.includes("underline"), "a second cue: the underline");
  assert.ok(look.mark.includes("!"), `a third cue: the mark ${look.mark}`);
  // The numbers stay, dimmed.
  assert.equal((await values(page))[0], "38%");
  assert.equal(await page.locator(".block-monitor .mon").getAttribute("data-stale"), "true");
  const colours = await page.evaluate(() => {
    const colour = (selector) => getComputedStyle(document.querySelector(selector)).color;
    return [colour(".block-monitor .mon-value"), colour(".block-monitor .mon-label")];
  });
  assert.equal(colours[0], colours[1], "the dimmed number has the colour of dim text");
  // They return by themselves when the host answers again.
  await page.waitForFunction(() => document.querySelector(".block-monitor .mon-note")?.hidden === true, null, { timeout: 4000 });
  assert.equal(await page.locator(".block-monitor .mon").getAttribute("data-stale"), null);
  assert.equal((await values(page))[0], "38%");

  // A host that never answered has no numbers to keep.
  await open(page, url, [{ fail: "the host did not answer in time" }]);
  await page.waitForFunction(() => document.querySelector(".block-monitor .mon-note")?.hidden === false);
  assert.equal(await note(page).textContent(), "No answer from Talos: the host did not answer in time");
  assert.deepEqual((await values(page)).filter(Boolean), []);
  assert.equal(await page.locator(".block-monitor .mon").getAttribute("data-stale"), null, "nothing to dim");
}

async function unsupportedCase(page, url) {
  await open(page, url, [{ state: "unsupported", system: "Darwin" }, LIVE]);
  await page.waitForFunction(() => document.querySelector(".block-monitor .mon-note")?.textContent?.startsWith("Not supported"));
  assert.equal(await note(page).textContent(), "Not supported: Talos does not run Linux (it says Darwin).");
  assert.equal(await note(page).getAttribute("data-tone"), "warn");
  assert.equal(await note(page).getAttribute("data-state"), "unsupported");
  assert.deepEqual((await values(page)).filter(Boolean), [], "empty rows");
  assert.equal(await page.locator('.block-monitor .mon-bar i[data-on]:not([data-on=""])').count(), 0, "no lit segment");
  assert.deepEqual((await page.locator(".block-monitor .mon-name").allTextContents()).filter(Boolean), []);
  // A later live reading replaces it.
  await page.waitForFunction(() => document.querySelector(".block-monitor .mon-note")?.hidden === true, null, { timeout: 4000 });
  assert.equal((await values(page))[0], "38%");
}

async function hostileCase(page, url) {
  const label = "<img src=x onerror=window.__XSS=1>";
  await open(page, url, [{ fail: "<b>bold</b><script>window.__XSS=2</script>" }, { state: "unsupported", system: "<i>x</i>" }], label);
  await page.waitForFunction(() => document.querySelector(".block-monitor .mon-note")?.textContent?.startsWith("No answer"));
  assert.equal(await note(page).textContent(), `No answer from ${label}: <b>bold</b><script>window.__XSS=2</script>`);
  await page.waitForFunction(() => document.querySelector(".block-monitor .mon-note")?.textContent?.startsWith("Not supported"), null, { timeout: 4000 });
  assert.equal(await note(page).textContent(), `Not supported: ${label} does not run Linux (it says ixi).`);
  assert.equal(await page.locator(".block-monitor .mon-note > *").count(), 0, "the note holds text and no element");
  assert.equal(await page.locator(".block-monitor img, .block-monitor b, .block-monitor script").count(), 0);
  assert.equal(await page.locator('.panehead[data-pane="2"] .ph-ctx').textContent(), `${label} · 1 s`);
  assert.equal(await page.locator('.panehead[data-pane="2"] img').count(), 0);
  assert.equal(await page.evaluate(() => window.__XSS), undefined, "nothing ran");
}

async function entryCase(page, url) {
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("connections_list").length >= 1);
  await page.evaluate(async () => {
    window.__registry = await import(new URL("actions.js", location.href).href);
  });
  const titles = await page.evaluate(() => window.__registry.listActions().map((a) => a.title));
  assert.ok(titles.includes("System monitor: Talos (ssh)"), "each SSH connection has a monitor");
  assert.ok(!titles.some((t) => t.startsWith("System monitor: Box")), "a WSL connection has none");
  assert.ok(titles.includes("System monitor"), "the local one is still there");
  await page.evaluate(() => window.__registry.runAction("monitor.on.talos"));
  await page.waitForFunction(() => window.__MOCK__.callsOf("block_open").length === 1);
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.callsOf("block_open")), [{ kind: "monitor", source: "talos", path: "", placement: "tab" }]);
  await page.evaluate(() => window.__registry.runAction("monitor.open"));
  await page.waitForFunction(() => window.__MOCK__.callsOf("block_open").length === 2);
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.callsOf("block_open")[1]), { kind: "monitor", source: "local", path: "", placement: "tab" });
}
