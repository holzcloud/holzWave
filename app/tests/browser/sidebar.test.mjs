// The sidebar (design.md section 6.11): it takes 18 rem from the stage, shows hosts, history and widgets, puts commands on
// the prompt without running them, saves a note, runs a Pomodoro clock, and remembers whether it was shown.
import assert from "node:assert/strict";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  const widgets = {
    widgets: [
      { id: "notes", title: "Notes", kind: "note" },
      { id: "pomodoro", title: "Pomodoro", kind: "timer", work_min: 25, break_min: 5 },
      { id: "deploy", title: "Deploy", kind: "list", items: [{ label: "Pods", command: "kubectl get pods" }, { label: "Docs", url: "https://example.org/docs" }] },
    ],
    notes: { notes: "buy wood" },
    warnings: ["widgets/bad.json: the kind \"x\" is not note, timer or list"],
  };
  const talos = { id: "talos", name: "Talos", kind: "ssh", host: "h", port: 22, user: "u", auth: "key", key_path: "k", jump: null, startup: "", reconnect_attempts: 5, reconnect_delay_secs: 3, shell: "", has_secret: false };
  await page.addInitScript(([w, c]) => {
    window.__MOCK__.on("widgets_list", () => structuredClone(w));
    window.__MOCK__.on("widget_note_save", () => null);
    window.__MOCK__.on("open_url", () => null);
    window.__MOCK__.on("paste", () => "done");
    window.__MOCK__.on("connect", () => window.__MOCK__.view());
    window.__MOCK__.on("connections_list", () => [c]);
    window.__MOCK__.on("pane_connections", () => [{ pane: 1, profile: "talos", name: "Talos", state: "reconnecting", attempt: 2, of: 5, secs: 3 }]);
    window.__MOCK__.on("history_search", ({ query }) => [{ cmd: query ? `grep ${query}` : "ls -la", cwd: "/", host: "local", last: 1, count: 1, exit: 0, marks: [] }]);
  }, [widgets, talos]);
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForSelector(".sidebar-btn");
  assert.equal(await page.locator("#sidebar").isHidden(), true, "hidden until asked");
  const stageWidth = await page.locator("#stage").evaluate((n) => n.getBoundingClientRect().width);

  await page.keyboard.press("Meta+b");
  await page.waitForSelector("#sidebar:not([hidden])");
  const rem = await page.evaluate(() => parseFloat(getComputedStyle(document.documentElement).fontSize));
  const narrower = await page.locator("#stage").evaluate((n) => n.getBoundingClientRect().width);
  assert.ok(Math.abs(stageWidth - narrower - 18 * rem) < 1, `the sidebar takes 18 rem (${stageWidth} to ${narrower})`);
  assert.equal(await page.locator(".sidebar-btn").getAttribute("aria-pressed"), "true");
  await page.waitForFunction(() => window.__MOCK__.callsOf("resize").length >= 1);

  // Hosts: the state is a dot and a word.
  await page.waitForSelector(".sb-host");
  assert.equal(await page.locator(".sb-host .sb-tag").textContent(), "⟳ 2/5");
  assert.equal(await page.locator(".sb-host .sb-dot").getAttribute("data-state"), "wait");
  assert.equal(await page.locator(".sb-host .sb-dot").evaluate((n) => getComputedStyle(n).borderStyle), "dotted", "a shape, not only a colour");
  await page.locator(".sb-host").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("connect").length === 1);

  // History: a click puts the command on the prompt, never with a newline.
  await page.waitForSelector(".sb-row .sb-cmd");
  await page.locator(".sb-input").fill("kube");
  await page.waitForFunction(() => document.querySelector(".sb-row .sb-cmd")?.textContent === "grep kube");
  await page.locator(".sb-input").press("Enter");
  const pasted = await page.evaluate(() => window.__MOCK__.callsOf("paste").at(-1));
  assert.deepEqual([pasted.text, pasted.allow_unsafe], ["grep kube", false]);
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime", "the terminal has the keyboard again");

  // Widgets: the note, the clock, the list, and a widget file's problem in words.
  assert.equal(await page.locator(".sb-note").inputValue(), "buy wood");
  await page.locator(".sb-note").fill("buy wood and nails");
  await page.waitForFunction(() => window.__MOCK__.callsOf("widget_note_save").length === 1, null, { timeout: 4000 });
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.callsOf("widget_note_save")[0]), { id: "notes", text: "buy wood and nails" });
  assert.equal(await page.locator(".sb-clock").textContent(), "25:00");
  await page.getByRole("button", { name: "Start" }).click();
  assert.equal(await page.locator(".sb-widget .sb-hint").nth(1).textContent(), "working");
  await page.getByRole("button", { name: "Pause" }).click();
  assert.equal(await page.locator(".sb-widget .sb-hint").nth(1).textContent(), "working · paused");
  await page.getByRole("button", { name: "Reset" }).click();
  assert.equal(await page.locator(".sb-clock").textContent(), "25:00");
  await page.getByRole("button", { name: /Pods/ }).click();
  assert.equal((await page.evaluate(() => window.__MOCK__.callsOf("paste").at(-1))).text, "kubectl get pods");
  await page.getByRole("button", { name: /Docs/ }).click();
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.callsOf("open_url")[0]), { url: "https://example.org/docs" });
  assert.match(await page.locator(".sb-warn").textContent(), /widgets\/bad\.json/);

  // Typing in a field is the field's; the shortcut hides it again, and the choice is remembered.
  await page.locator(".sb-note").click();
  await page.keyboard.type("!");
  assert.equal((await page.evaluate(() => window.__MOCK__.callsOf("key"))).length, 0, "no key reached the shell");
  await page.keyboard.press("Meta+b");
  await page.waitForSelector("#sidebar", { state: "hidden" });
  assert.equal(await page.evaluate(() => localStorage.getItem("holzwave.sidebar")), "0");
  assert.deepEqual(errors, [], "the page reported errors");
}
