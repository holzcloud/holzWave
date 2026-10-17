// How the line of the monitor of another computer looks and behaves in the places the design cares about: it wraps on a
// narrow window, it follows the modes (`contrast-high`, `forced-colors`), its warn state has the cues besides colour, and
// nothing in the monitor has a gradient or a filter (design.md sections 1 and 6.5). Shares the stub of `monitor-ssh`.
import assert from "node:assert/strict";
import { BOX, LIVE, TALOS, VIEW, open, setup } from "./monitor-ssh.test.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(6000);
  // The pane fills the window, as every pane does on a narrow screen.
  const single = { ...VIEW, panes: [[2, { x: 0, y: 0, w: 1, h: 1 }]], dividers: [], workspaces: [{ name: "main", active: true, panes: 1 }], tabs: [{ title: "sys", active: true, focus: 2, panes: 1 }], focus: 2 };
  await page.addInitScript(setup, [single, [TALOS, BOX]]);
  await narrowCase(page, url);
  await modesCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

const note = (page) => page.locator(".block-monitor .mon-note");

async function noteBox(page) {
  await page.waitForFunction(() => document.querySelector(".block-monitor .mon-note")?.hidden === false);
  return page.evaluate(() => {
    const n = document.querySelector(".block-monitor .mon-note");
    const box = n.getBoundingClientRect();
    const mon = document.querySelector(".block-monitor .mon");
    return {
      height: box.height,
      right: box.right,
      line: parseFloat(getComputedStyle(n).lineHeight),
      scrollWidth: mon.scrollWidth,
      clientWidth: mon.clientWidth,
      width: window.innerWidth,
    };
  });
}

async function narrowCase(page, url) {
  const reason = "the connection was lost while the host was busy and nothing came back from it ".repeat(3).slice(0, 200);
  await page.setViewportSize({ width: 390, height: 780 });
  await open(page, url, [{ fail: reason }]);
  const narrow = await noteBox(page);
  assert.ok(narrow.height > narrow.line * 1.5, `the 200-character reason wraps (${narrow.height} px high, line ${narrow.line})`);
  assert.ok(narrow.right <= narrow.width, "the line stays inside the window");
  assert.ok(narrow.scrollWidth <= narrow.clientWidth, "the monitor does not scroll sideways");
  assert.ok((await note(page).textContent()).includes(reason.slice(0, 40)));

  await page.setViewportSize({ width: 1000, height: 640 });
  await open(page, url, [{ fail: "the host did not answer in time" }]);
  const wide = await noteBox(page);
  assert.ok(wide.height <= wide.line * 2 + 1, `a short reason takes at most two lines on a wide window (${wide.height} px)`);
}

async function modesCase(page, url) {
  await page.setViewportSize({ width: 1000, height: 640 });
  await open(page, url, [{ hold: true }]);
  await page.waitForFunction(() => document.querySelector(".block-monitor .mon-note")?.hidden === false);
  assert.equal(await note(page).getAttribute("data-tone"), "plain");
  const colour = () => note(page).evaluate((n) => getComputedStyle(n).color);
  const before = await colour();
  await page.evaluate(() => document.documentElement.classList.add("contrast-high"));
  assert.notEqual(await colour(), before, "the plain line follows the dim colour, which contrast-high raises");
  await page.evaluate(() => document.documentElement.classList.remove("contrast-high"));

  // The warn state: underline and mark besides colour, and nothing decorative in the monitor.
  await open(page, url, [LIVE, { fail: "the connection was lost" }]);
  await page.waitForFunction(() => document.querySelector(".block-monitor .mon-note")?.getAttribute("data-tone") === "warn", null, { timeout: 4000 });
  const warn = await note(page).evaluate((n) => ({ line: getComputedStyle(n).textDecorationLine, mark: getComputedStyle(n, "::before").content, outline: getComputedStyle(n).outlineStyle }));
  assert.equal(warn.line, "underline");
  assert.ok(warn.mark.includes("!"));
  assert.equal(warn.outline, "none", "the outline is only for forced colours");
  const decorated = await page.evaluate(() =>
    Array.from(document.querySelectorAll(".block-monitor, .block-monitor *")).filter((n) => {
      const s = getComputedStyle(n);
      return s.backgroundImage !== "none" || s.filter !== "none" || s.boxShadow !== "none";
    }).length);
  assert.equal(decorated, 0, "no gradient, filter or shadow");

  // The rule for forced colours exists and names the line.
  const rules = await page.evaluate(() => {
    const out = [];
    for (const sheet of document.styleSheets) {
      let list;
      try { list = sheet.cssRules; } catch { continue; }
      for (const rule of list) if (rule.media && rule.conditionText?.includes("forced-colors")) out.push(rule.cssText);
    }
    return out;
  });
  assert.ok(rules.some((text) => text.includes(".mon-note")), "a forced-colors rule names .mon-note");
}
