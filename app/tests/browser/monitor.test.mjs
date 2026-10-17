// The system monitor block (design.md section 6.5): bars with numbers, rates, the busiest processes, one sample a second.
import assert from "node:assert/strict";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  const view = {
    workspaces: [{ name: "main", active: true, panes: 2 }],
    tabs: [{ title: "sys", active: true, focus: 1, panes: 2 }],
    focus: 1,
    panes: [[1, { x: 0, y: 0, w: 0.5, h: 1 }], [2, { x: 0.5, y: 0, w: 0.5, h: 1 }]],
    dividers: [{ split: 10, axis: "Row", area: { x: 0, y: 0, w: 1, h: 1 }, ratio: 0.5 }],
    pane_names: [],
    windows: 1,
  };
  const sample = { cpu: 38.2, cores: 8, mem_used: 6.1 * 1024 ** 3, mem_total: 16 * 1024 ** 3, net_rx: 1.2 * 1024 * 1024, net_tx: 84 * 1024, uptime: 5400,
    procs: [{ pid: 4121, name: "holzwave", cpu: 12.4, mem: 300 * 1024 ** 2 }, { pid: 7, name: "<img src=x onerror=alert(1)>", cpu: 1, mem: 1024 }] };
  await page.addInitScript(([v, s]) => {
    window.__SAMPLE = s;
    window.__MOCK__.setView(v);
    window.__MOCK__.on("pane_blocks", () => [{ pane: 2, kind: "monitor", source: "local", path: "", source_label: "This computer" }]);
    window.__MOCK__.on("monitor_sample", () => ({ state: "live", ...structuredClone(window.__SAMPLE) }));
    window.__MOCK__.on("block_open", () => window.__MOCK__.view());
  }, [view, sample]);
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".block-monitor .mon-row");
  await page.waitForFunction(() => document.querySelector(".block-monitor .mon-value")?.textContent === "38%");
  const rows = await page.locator(".block-monitor .mon-row").evaluateAll((n) => n.map((r) => Array.from(r.children).map((c) => c.textContent).filter(Boolean).join(" ")));
  assert.equal(rows[0], "CPU 38%");
  assert.equal(rows[1], "MEM 6.1/16G");
  assert.equal(rows[2], "NET ↓ 1.2 MB/s ↑ 84 KB/s");
  assert.equal(rows[3], "UP 1 h 30 min · 8 cores");
  assert.equal(await page.locator(".block-monitor .mon-bar").first().locator("i[data-on='ok']").count(), 3, "three of eight segments, in the ok level");
  assert.equal(await page.locator(".block-monitor .mon-bar").first().getAttribute("aria-label"), "38 percent");
  assert.equal(await page.locator('.panehead[data-pane="2"] .ph-title').textContent(), "system");
  assert.equal(await page.locator('.panehead[data-pane="2"] .ph-ctx').textContent(), "local · 1 s");
  // The local monitor says nothing on a line of its own, and asks for the sample of its pane.
  assert.equal(await page.locator(".block-monitor .mon-note").isHidden(), true, "no note for this computer");
  assert.equal(await page.locator(".block-monitor .mon-note").textContent(), "");
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.callsOf("monitor_sample")[0]), { pane: 2 });

  // Process names are text, never markup.
  const names = await page.locator(".block-monitor .mon-name").allTextContents();
  assert.equal(names[0], "holzwave");
  assert.equal(names[1], "<img src=x onerror=alert(1)>");
  assert.equal(await page.locator(".block-monitor img").count(), 0);

  // A high load is told in words and underline as well as in colour.
  await page.evaluate(() => { window.__SAMPLE = { ...window.__SAMPLE, cpu: 95 }; });
  await page.waitForFunction(() => document.querySelector(".block-monitor .mon-value")?.dataset.level === "danger", null, { timeout: 4000 });
  assert.equal(await page.locator(".block-monitor .mon-value").first().evaluate((n) => getComputedStyle(n).textDecorationLine), "underline");

  // The palette opens one.
  await page.evaluate(async () => {
    const reg = await import(new URL("actions.js", location.href).href);
    reg.runAction("monitor.open");
  });
  await page.waitForFunction(() => window.__MOCK__.callsOf("block_open").length === 1);
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.callsOf("block_open")[0]), { kind: "monitor", source: "local", path: "", placement: "tab" });
  assert.deepEqual(errors, [], "the page reported errors");
}
