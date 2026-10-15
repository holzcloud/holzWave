// The history search (design.md section 6.10): fuzzy results with the matched letters marked, Enter puts the command on
// the prompt without running it, Shift+Delete forgets one, Clear all asks twice. The stub plays the core.
import assert from "node:assert/strict";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  const hits = [
    { cmd: "kubectl get pods -n kube-system", cwd: "/home/holz/infra", host: "local", last: Math.floor(Date.now() / 1000) - 7200, count: 14, exit: 0, marks: [0, 8, 12] },
    { cmd: "make <b>test</b>", cwd: "/x", host: "local", last: Math.floor(Date.now() / 1000) - 60, count: 1, exit: 2, marks: [] },
  ];
  await page.addInitScript((h) => {
    window.__HITS = h;
    window.__MOCK__.on("history_search", ({ query }) => (query === "zzz" ? [] : structuredClone(window.__HITS)));
    window.__MOCK__.on("history_delete", () => 1);
    window.__MOCK__.on("history_clear", () => null);
    window.__MOCK__.on("paste", () => "done");
  }, hits);
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 1);

  // The chord of the platform opens it.
  await page.keyboard.press("Meta+Shift+H");
  await page.waitForSelector(".history .history-row");
  assert.equal(await page.locator(".history-row").count(), 2);
  const first = page.locator(".history-row").first();
  assert.equal(await first.locator(".history-cmd").textContent(), "kubectl get pods -n kube-system");
  assert.deepEqual(await first.locator("mark").allTextContents(), ["k", "g", "p"], "the matched letters are marked");
  assert.equal(await first.locator("mark").first().evaluate((n) => getComputedStyle(n).textDecorationLine), "underline", "a match is not colour alone");
  assert.match(await first.locator(".history-meta").textContent(), /^~?\/home\/holz\/infra · 2 h ago · ×14$/);
  assert.match(await page.locator(".history-row").nth(1).locator(".history-meta").textContent(), /1 min ago · exit 2$/, "a failure is said in words");

  // A command is text, never markup.
  assert.equal(await page.locator(".history-row").nth(1).locator("b").count(), 0);
  assert.equal(await page.locator(".history-row").nth(1).locator(".history-cmd").textContent(), "make <b>test</b>");

  // Typing searches; no match says so.
  await page.keyboard.type("zzz");
  await page.waitForSelector(".history .palette-empty:not([hidden])");
  assert.match(await page.locator(".history .palette-empty").textContent(), /Nothing matches "zzz"/);
  for (let i = 0; i < 3; i++) await page.keyboard.press("Backspace");
  await page.waitForSelector(".history .history-row");

  // Down moves, Enter puts the second command on the prompt of the focused pane and closes. Nothing runs: no newline.
  await page.keyboard.press("ArrowDown");
  assert.equal(await page.locator(".history-row").nth(1).getAttribute("aria-selected"), "true");
  await page.keyboard.press("Enter");
  await page.waitForSelector(".history", { state: "detached" });
  const pasted = await page.evaluate(() => window.__MOCK__.callsOf("paste").at(-1));
  assert.equal(pasted.text, "make <b>test</b>");
  assert.equal(pasted.allow_unsafe, false);
  assert.ok(!pasted.text.includes("\n"));

  // Shift+Delete forgets the chosen command.
  await page.keyboard.press("Meta+Shift+H");
  await page.waitForSelector(".history .history-row");
  await page.keyboard.press("Shift+Delete");
  await page.waitForFunction(() => window.__MOCK__.callsOf("history_delete").length === 1);
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.callsOf("history_delete")[0]), { cmd: "kubectl get pods -n kube-system" });

  // Clear all needs a second click; Escape closes.
  await page.locator(".history-clear").click();
  assert.equal((await page.evaluate(() => window.__MOCK__.callsOf("history_clear"))).length, 0);
  await page.locator(".history-clear").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("history_clear").length === 1);
  await page.waitForSelector(".history", { state: "detached" });
  await page.keyboard.press("Meta+Shift+H");
  await page.waitForSelector(".history");
  await page.keyboard.press("Escape");
  await page.waitForSelector(".history", { state: "detached" });
  assert.deepEqual(errors, [], "the page reported errors");
}
