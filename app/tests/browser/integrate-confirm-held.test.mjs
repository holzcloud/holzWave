// A held terminal and an Exec verb (docs/design.md section 6.26): a pane the core lists as held has a hollow dot with a dash and the
// context slot `exited 1` in the warn colour with the mark; a pane that is not held has the header it always had and keeps its chip.
// A confirmed Exec verb opens a pane, the layout is applied and the pane's header has the chip of the context.
import assert from "node:assert/strict";
import { HASH, OPENED, TICKET, allow, calls, cardNode, cardOf, installCore, outcome, request, textOf } from "./integrate-confirm-lib.mjs";
import { KUBE_FULL, layout, openContext, paneInfo } from "./integrate-context-lib.mjs";
import { LAUNCHED } from "./integrate-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await a_held_pane_is_said_in_its_header(page, url);
  await an_exec_confirm_opens_a_pane(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

const head = (page, pane) => page.locator(`.panehead[data-pane="${pane}"]`);

const emit = (page, kind, pane) =>
  page.evaluate(([k, p]) => window.__MOCK__.emit("integration", { seq: (window.__seq = (window.__seq ?? 100) + 1), kind: k, tool: "kube", pane: p, code: null }), [kind, pane]);

async function a_held_pane_is_said_in_its_header(page, url) {
  await openContext(page, url, { panes: [paneInfo(1, { view: "exec", held: false }), paneInfo(2, { view: "shell", held: false })], sidebar: false }, layout(1, [1, 2]));
  await page.waitForSelector('.panehead[data-pane="1"] .ph-chip');
  const plain = await head(page, 1).locator(".ph-ctx").textContent();
  assert.ok(!plain.startsWith("exited"), plain);
  assert.equal(await head(page, 1).locator(".ph-dot").getAttribute("data-state"), "focused");

  // The core says pane 1 is held with status 1: the dot is hollow with a dash, the slot says so in the warn colour, with the mark.
  await page.evaluate(() => {
    window.__CX__.panes = [
      { pane: 1, tool: "kube", view: "exec", context: "prod-eu", namespace: "payments", production: null, held: true, status: 1 },
      { pane: 2, tool: "kube", view: "shell", context: "prod-eu", namespace: "payments", production: null, held: false, status: null },
    ];
  });
  await emit(page, "held", 1);
  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="1"] .ph-ctx')?.textContent === "exited 1");
  assert.equal(await head(page, 1).locator(".ph-dot").getAttribute("data-state"), "exited");
  const ctx = await head(page, 1).locator(".ph-ctx").evaluate((n) => ({
    tone: n.dataset.tone, line: getComputedStyle(n).textDecorationLine, mark: getComputedStyle(n.firstElementChild, "::before").content,
  }));
  assert.equal(ctx.tone, "warn");
  assert.ok(ctx.line.includes("underline"));
  assert.ok(ctx.mark.includes("!"));
  assert.equal(await head(page, 1).locator(".ph-chip").count(), 1, "the chip stays");
  assert.equal(await head(page, 2).locator(".ph-ctx").getAttribute("data-tone"), null, "another pane is unchanged");
  assert.ok(!(await head(page, 2).locator(".ph-ctx").textContent()).startsWith("exited"));

  // A held pane the core gives no status for says `exited`.
  await page.evaluate(() => {
    window.__CX__.panes[0].status = null;
  });
  await emit(page, "held", 1);
  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="1"] .ph-ctx')?.textContent === "exited");

  // Not held any more: the header is what it was.
  await page.evaluate(() => {
    window.__CX__.panes[0].held = false;
  });
  await emit(page, "held", 1);
  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="1"] .ph-ctx')?.textContent?.startsWith("exited") === false);
  assert.equal(await head(page, 1).locator(".ph-dot").getAttribute("data-state"), "focused");
  assert.equal(await head(page, 1).locator(".ph-ctx").getAttribute("data-tone"), null);
}

async function an_exec_confirm_opens_a_pane(page, url) {
  const exec = cardOf({ verb: "exec", effect: "exec", argv: ["exec", "-i", "-t", "--context=prod-eu", "--", "api-0"], target: [["context", "prod-eu"], ["pod", "api-0"]] });
  await installCore(page, { cards: { "kube/exec": exec }, steps: [{ answer: OPENED(LAUNCHED, 3) }] });
  await openContext(page, url, { panes: [paneInfo(3, { view: "exec" })], sidebar: false }, layout(1, [1, 2]));
  await page.waitForSelector(".panehead");
  assert.equal(await head(page, 3).count(), 0);
  await request(page, "kube", "exec", 1, { pod: "api-0" });
  await cardNode(page).waitFor();
  await allow(page);
  await page.waitForFunction(() => document.querySelector(".ic-line")?.textContent === "Opened pane 3");
  assert.deepEqual(await calls(page, "integration_confirm"), [{ ticket: TICKET, hash: HASH }]);
  // The returned layout is applied: the new pane has a header, and its header has the chip of the context the core gave it.
  await head(page, 3).waitFor();
  await page.waitForSelector('.panehead[data-pane="3"] .ph-chip');
  assert.equal(await head(page, 3).locator(".ph-chip-text").textContent(), "⎈ prod-eu");
}
