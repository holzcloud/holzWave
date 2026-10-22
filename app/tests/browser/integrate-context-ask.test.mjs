// When the page asks the core for the context (design.md 6.11): at the first layout, when the core says something changed, when
// the focus or the layout changes (once per 250 ms), on the button, and every 30 s while the window is visible. A window that
// is hidden asks nothing, nothing asks from a draw, and a failed ask keeps the last rows, dimmed. The clock is Playwright's.
import assert from "node:assert/strict";
import { callCount, callsOf, layout, openContext, sectionShown } from "./integrate-context-lib.mjs";

const hide = (page, hidden) =>
  page.evaluate((h) => {
    Object.defineProperty(document, "visibilityState", { configurable: true, get: () => (h ? "hidden" : "visible") });
    Object.defineProperty(document, "hidden", { configurable: true, get: () => h });
    document.dispatchEvent(new Event("visibilitychange"));
  }, hidden);

const emit = (page, kind, seq) => page.evaluate(([k, n]) => window.__MOCK__.emit("integration", { seq: n, kind: k, tool: "kube", pane: null, code: null }), [kind, seq]);
const focus = (page, id) =>
  page.evaluate((v) => {
    window.__MOCK__.setView(v);
    window.__MOCK__.emit("layout", v);
  }, layout(id, [1, 2]));

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await page.clock.install();
  await openContext(page, url, {}, layout(1, [1, 2]));
  await sectionShown(page);
  const n0 = await callCount(page, "integration_context");
  assert.equal(n0, 1, "one ask when the first layout is drawn");
  assert.deepEqual((await callsOf(page, "integration_context"))[0], { pane: 1, refresh: false });

  // Nothing polls in a draw: the stats line is drawn again and again, and the core is asked about none of it.
  await page.clock.runFor(20000);
  assert.equal(await callCount(page, "integration_context"), n0, "no ask in 20 s");

  // Every 30 s while the window is visible.
  await page.clock.runFor(11000);
  await page.waitForFunction((n) => window.__MOCK__.callsOf("integration_context").length === n + 1, n0);

  // The core's events: context, tools and held ask (together, once); the others do not.
  const n1 = await callCount(page, "integration_context");
  await emit(page, "profiles", 1);
  await page.clock.runFor(1000);
  assert.equal(await callCount(page, "integration_context"), n1, "an event that is not about the context asks nothing");
  await emit(page, "context", 2);
  await emit(page, "tools", 3);
  await emit(page, "held", 4);
  await page.clock.runFor(300);
  await page.waitForFunction((n) => window.__MOCK__.callsOf("integration_context").length === n + 1, n1);
  assert.equal(await callCount(page, "integration_context"), n1 + 1, "three events in 250 ms are one ask");

  // The focus: two changes within 250 ms are one ask, for the pane that has the focus at the end.
  const n2 = await callCount(page, "integration_context");
  await focus(page, 2);
  await page.clock.runFor(100);
  await focus(page, 1);
  await page.clock.runFor(100);
  await focus(page, 2);
  assert.equal(await callCount(page, "integration_context"), n2, "debounced");
  await page.clock.runFor(300);
  await page.waitForFunction((n) => window.__MOCK__.callsOf("integration_context").length === n + 1, n2);
  assert.equal((await callsOf(page, "integration_context")).at(-1).pane, 2);

  // The button asks at once and says the tool should be looked for again.
  await page.locator(".cx-refresh").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_context").at(-1).refresh === true);
  assert.equal(await page.locator(".cx-refresh").getAttribute("aria-label"), "Read the context again");

  // A window that is hidden asks nothing, for the tick, for events or for the focus; it asks once when it is visible again.
  await hide(page, true);
  const n3 = await callCount(page, "integration_context");
  await page.clock.runFor(120000);
  await emit(page, "context", 5);
  await focus(page, 1);
  await page.clock.runFor(1000);
  assert.equal(await callCount(page, "integration_context"), n3, "a hidden window asks nothing");
  await hide(page, false);
  await page.clock.runFor(300);
  await page.waitForFunction((n) => window.__MOCK__.callsOf("integration_context").length === n + 1, n3);
  assert.equal((await callsOf(page, "integration_context")).at(-1).pane, 1, "for the pane that has the focus now");

  // A failed ask keeps the last rows, dimmed, and says so with the attribute the style keys on.
  const text = await page.locator('.cx-row[data-tool="kube"] .cx-sel').textContent();
  await page.evaluate(() => {
    window.__CX__.contextFault = "no answer";
  });
  await page.locator(".cx-refresh").click();
  await page.waitForFunction(() => document.querySelector(".cx-section")?.getAttribute("data-stale") === "true");
  assert.equal(await page.locator('.cx-row[data-tool="kube"] .cx-sel').textContent(), text, "the rows stay");
  assert.equal(await page.locator('.cx-row[data-tool="kube"]').getAttribute("data-stale"), "true");
  await page.evaluate(() => {
    window.__CX__.contextFault = null;
  });
  await page.locator(".cx-refresh").click();
  await page.waitForFunction(() => document.querySelector(".cx-section")?.getAttribute("data-stale") === null);
  assert.deepEqual(errors, [], "the page reported errors");
}
