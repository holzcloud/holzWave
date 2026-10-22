// The states of the integration block, in words (design.md section 6.25): reading, tool missing with a path field,
// unusable, an error that keeps the last rows dimmed, a cut list, an empty one, the age, and the other two shapes.
import assert from "node:assert/strict";
import { HOSTILE, KUBE, answer, openBlock, podsView, rowsShown } from "./integrate-lib.mjs";

const status = (page) => page.locator(".block-integration .ig-status");

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  const MISSING = "kubectl was not found on this computer. Install it or set where it is.";
  const SEARCHED = ["/usr/local/bin", "/opt/homebrew/bin"];
  await openBlock(page, url, {
    delay: 600,
    tools: [{ ...KUBE, state: "not_found", path: null, searched: SEARCHED }],
    faults: { pods: { code: "missing", reason: "not_found", text: MISSING } },
  });

  // While the first answer is on its way the line says so and there are no rows.
  await page.waitForSelector(".block-integration .ig-status");
  assert.equal(await status(page).textContent(), "Reading …");
  assert.equal(await page.locator(".block-integration .ig-row").count(), 0);

  // A tool that was not found: the core's sentence, the places looked in, a field for the path and two buttons.
  await page.waitForFunction(() => document.querySelector(".block-integration .ig-status")?.textContent !== "Reading …");
  assert.equal(await status(page).textContent(), MISSING);
  assert.equal(await status(page).getAttribute("data-tone"), "warn");
  assert.equal(await status(page).evaluate((n) => getComputedStyle(n).textDecorationLine), "underline", "warn is underlined as well as coloured");
  assert.deepEqual(await page.locator(".block-integration .ig-places li").allTextContents(), SEARCHED);
  assert.equal(await page.locator(".block-integration input.ig-path").count(), 1);
  assert.equal(await page.locator(".block-integration input.ig-path").getAttribute("aria-label"), "Path of the program");
  assert.deepEqual(await page.locator(".block-integration .ig-pathbtn").allTextContents(), ["Set path", "Check again"]);

  // The path field keeps its keys; Escape gives the keyboard back to the terminal.
  await page.evaluate(() => {
    window.__IG__.delay = 0;
  });
  await page.locator(".block-integration input.ig-path").fill("/tmp/kubectl");
  await page.keyboard.press("Escape");
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime", "Escape returns the keyboard to the terminal");
  assert.equal(await page.locator(".block-integration input.ig-path").inputValue(), "/tmp/kubectl", "what was typed is kept");

  // A path the core refuses is the core's sentence on the status line; the field stays.
  await page.evaluate(() => {
    window.__MOCK__.on("integration_binary_set", () => {
      throw { code: "refused", reason: "name", text: "The file must be named kubectl." };
    });
  });
  await page.locator(".block-integration .ig-pathbtn", { hasText: "Set path" }).click();
  await page.waitForFunction(() => document.querySelector(".block-integration .ig-status")?.textContent === "The file must be named kubectl.");
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.callsOf("integration_binary_set")), [{ tool: "kube", path: "/tmp/kubectl" }]);
  assert.equal(await page.locator(".block-integration input.ig-path").count(), 1);

  // Check again asks for the tools once more and then reads.
  const asked = await page.evaluate(() => window.__MOCK__.callsOf("integration_tools").length);
  await page.locator(".block-integration .ig-pathbtn", { hasText: "Check again" }).click();
  await page.waitForFunction((n) => window.__MOCK__.callsOf("integration_tools").length > n, asked);

  // A path the core accepts: the tools come back, the block reads again and shows the rows.
  await page.evaluate((view) => {
    window.__IG__.faults = {};
    window.__IG__.runs.pods = view;
    window.__MOCK__.on("integration_binary_set", () => window.__IG__.tools);
    window.__IG__.tools = [{ ...window.__IG__.tools[0], state: "found", path: "/tmp/kubectl", searched: [] }];
  }, answer(podsView()));
  await page.locator(".block-integration .ig-pathbtn", { hasText: "Set path" }).click();
  await rowsShown(page);
  assert.equal(await page.locator(".block-integration input.ig-path").count(), 0);
  await page.waitForFunction(() => /^Updated /.test(document.querySelector(".block-integration .ig-status")?.textContent ?? ""));

  // An error keeps the last rows, dimmed, until a good answer.
  const BOOM = `Kubernetes reported an error: ${HOSTILE}`;
  await page.evaluate((text) => {
    window.__IG__.faults = { pods: { code: "failed", reason: "exit_status", text } };
  }, BOOM);
  await page.locator(".block-integration .ig-refresh").click();
  await page.waitForFunction((text) => document.querySelector(".block-integration .ig-status")?.textContent === text, BOOM);
  assert.equal(await status(page).getAttribute("data-tone"), "warn");
  assert.equal(await page.locator(".block-integration .ig-row").count(), 2, "the last rows stay");
  assert.equal(await page.locator(".block-integration .ig-body").getAttribute("data-stale"), "true");
  assert.equal(await page.locator(".block-integration .ig-status img").count(), 0, "a fault text is text");
  const dim = await page.locator(".block-integration .ig-row .ig-cell").first().evaluate((n) => getComputedStyle(n).color);
  const ink = await page.locator(".block-integration .ig-th").first().evaluate((n) => getComputedStyle(n).color);
  assert.equal(dim, ink, "dimmed cells have the dim colour of the header");
  await page.evaluate(() => {
    window.__IG__.faults = {};
  });
  await page.locator(".block-integration .ig-refresh").click();
  await page.waitForFunction(() => document.querySelector(".block-integration .ig-body")?.getAttribute("data-stale") === null);
  assert.match((await status(page).textContent()) ?? "", /^Updated (just now|\d+ s ago)$/);
  assert.equal(await status(page).getAttribute("data-tone"), null);

  // A tool that cannot be used says why, in the core's words, and offers the same field.
  const UNUSABLE = "kubectl cannot be used: writable by everyone.";
  await page.evaluate((text) => {
    window.__IG__.tools = [{ ...window.__IG__.tools[0], state: "unusable", reason: text }];
    window.__IG__.faults = { pods: { code: "missing", reason: "unusable", text } };
  }, UNUSABLE);
  await page.locator(".block-integration .ig-refresh").click();
  await page.waitForFunction((text) => document.querySelector(".block-integration .ig-status")?.textContent === text, UNUSABLE);
  assert.equal(await page.locator(".block-integration input.ig-path").count(), 1);
  assert.equal(await page.locator(".block-integration .ig-row").count(), 0, "a tool that cannot be used shows no old rows");

  // A cut list says so under the table; an empty one says that there is nothing.
  await page.evaluate((view) => {
    window.__IG__.faults = {};
    window.__IG__.runs.pods = view;
  }, answer(podsView({ truncated: { shown: 2000, note: "Showing the first 2000 of 2500 rows." }, note: "Read from prod" })));
  await page.locator(".block-integration .ig-refresh").click();
  await page.waitForSelector('.block-integration .ig-note[data-kind="cut"]');
  assert.equal(await page.locator('.block-integration .ig-note[data-kind="cut"]').textContent(), "Showing the first 2000 of 2500 rows.");
  assert.equal(await page.locator(".block-integration .ig-note:not([data-kind])").textContent(), "Read from prod");
  await page.evaluate((view) => {
    window.__IG__.runs.pods = view;
  }, answer(podsView({ rows: [] })));
  await page.locator(".block-integration .ig-refresh").click();
  await page.waitForSelector('.block-integration .ig-note[data-kind="empty"]');
  assert.equal(await page.locator('.block-integration .ig-note[data-kind="empty"]').textContent(), "Nothing to show.");
  assert.equal(await page.locator(".block-integration .ig-th").count(), 5, "the header stays");

  // The other two shapes are drawn as the core sent them, as text.
  await page.evaluate((kv) => {
    window.__IG__.runs.pods = kv;
  }, answer({
    kind: "kv", title: "Account", fetched: null, note: "Signed out",
    rows: [{ label: "State", value: "Signed out", tone: "warn" }, { label: "Name", value: HOSTILE, tone: "normal" }],
    actions: [],
  }));
  await page.locator(".block-integration .ig-refresh").click();
  await page.waitForSelector(".block-integration .ig-kvrow");
  assert.deepEqual(await page.locator(".block-integration .ig-v").allTextContents(), ["Signed out", HOSTILE]);
  assert.equal(await page.locator(".block-integration .ig-kvrow").first().getAttribute("data-tone"), "warn");
  assert.equal(await page.locator(".block-integration .ig-note").textContent(), "Signed out");
  await page.evaluate((text) => {
    window.__IG__.runs.pods = text;
  }, answer({ kind: "text", title: "Info", text: `line one\nline <b>two</b>\n\tindented`, format: "plain", truncated: false, note: null, fetched: null }));
  await page.locator(".block-integration .ig-refresh").click();
  await page.waitForSelector(".block-integration .ig-text");
  assert.equal(await page.locator(".block-integration .ig-text").textContent(), "line one\nline <b>two</b>\n\tindented");
  assert.equal(await page.locator(".block-integration .ig-text b").count(), 0);

  assert.deepEqual(errors, [], "the page reported errors");
}
