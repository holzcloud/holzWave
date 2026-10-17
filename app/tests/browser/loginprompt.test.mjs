// The login prompt (design.md 6.22): the core sends `login-prompt` when the server of a saved connection asks a question
// (a one-time code, a PAM prompt), the page draws a dialog, `Send` gives the answers to `login_prompt_answer`, `Cancel` and
// Escape to `login_prompt_cancel`, and `login-prompt-end` takes the dialog away when the core ended the question. What the
// server wrote is text under a fixed sentence and never a title. The stub plays the core.
import assert from "node:assert/strict";

const ID = "0123456789abcdef0123456789abcdef";
const ID2 = "fedcba9876543210fedcba9876543210";
const prompt = (over = {}) => ({
  id: ID,
  connection: "Talos",
  host: "talos.example",
  name: "Two-factor login",
  instructions: "Enter your password and the code of your token.",
  prompts: [{ text: "Password:", echo: false }, { text: "Code:", echo: true }],
  ...over,
});

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await page.addInitScript(() => {
    window.__MOCK__.on("login_prompt_answer", () => true);
    window.__MOCK__.on("login_prompt_cancel", () => true);
  });
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 1);
  assert.equal(await page.evaluate(() => window.__MOCK__.listening("login-prompt")), 1, "the page listens once");
  assert.equal(await page.evaluate(() => window.__MOCK__.listening("login-prompt-end")), 1);

  await openAndAnswer(page);
  await cancelCases(page);
  await endCase(page);
  await forgeryCase(page);
  await queueCase(page);
  await badPayloads(page);
  await presentation(page);
  assert.deepEqual(errors, [], "the page reported errors");
}

const emit = (page, name, payload) => page.evaluate(([n, p]) => window.__MOCK__.emit(n, p), [name, payload]);
const calls = (page, cmd) => page.evaluate((c) => window.__MOCK__.callsOf(c), cmd);
const dialog = (page) => page.locator(".dlg.loginprompt");
const frames = (page) => page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(() => done(true)))));

async function openAndAnswer(page) {
  await emit(page, "login-prompt", prompt());
  await page.waitForSelector(".dlg.loginprompt");
  const box = dialog(page);
  assert.equal(await box.getAttribute("role"), "dialog");
  assert.equal(await box.getAttribute("aria-modal"), "true");
  assert.equal(await page.locator("#lp-title").textContent(), "Login prompt from Talos", "the title is the saved connection's");
  assert.equal(await box.getAttribute("aria-labelledby"), "lp-title");
  assert.equal(await box.locator(".lp-host").textContent(), "Host: talos.example");
  assert.equal(await box.locator(".lp-source").textContent(), "This text comes from the server, not from holzWave.");
  assert.equal(await box.locator(".lp-legend").textContent(), "The server says");
  assert.equal(await box.locator(".lp-name").textContent(), "Two-factor login");
  assert.equal(await box.locator(".lp-instructions").textContent(), "Enter your password and the code of your token.");
  assert.deepEqual(await box.locator(".lp-label").allTextContents(), ["Password:", "Code:"]);

  // A masked field for the question that is not echoed, a plain one for the other; the words say which.
  assert.equal(await page.locator("#lp-field-0").getAttribute("type"), "password");
  assert.equal(await page.locator("#lp-field-1").getAttribute("type"), "text");
  assert.deepEqual(await box.locator(".lp-tag").allTextContents(), ["hidden", "shown"]);
  for (const f of ["#lp-field-0", "#lp-field-1"]) assert.equal(await page.locator(f).getAttribute("autocomplete"), "off");
  // The labels are tied to the fields.
  assert.equal(await page.locator("label.lp-label").first().getAttribute("for"), "lp-field-0");
  // The first field has the focus.
  assert.equal(await page.evaluate(() => document.activeElement?.id), "lp-field-0");

  // Typing and Enter: the first Enter moves on, the last one sends.
  await page.keyboard.type("hunter2");
  await page.keyboard.press("Enter");
  assert.equal(await page.evaluate(() => document.activeElement?.id), "lp-field-1", "Enter moves to the next field");
  assert.equal((await calls(page, "login_prompt_answer")).length, 0, "nothing is sent yet");
  await page.keyboard.type("123456");
  await page.keyboard.press("Enter");
  await page.waitForSelector(".dlg.loginprompt", { state: "detached" });
  const sent = await calls(page, "login_prompt_answer");
  assert.deepEqual(sent, [{ id: ID, answers: ["hunter2", "123456"] }], "exactly the id and the answers in order");
  assert.equal((await calls(page, "login_prompt_cancel")).length, 0);
  assert.equal(await page.locator("#lp-field-0").count(), 0, "the fields are gone from the page");
  assert.equal(await page.evaluate(() => document.body.innerText.includes("hunter2") || document.body.innerHTML.includes("hunter2")), false, "no answer stays in the page");

  // Send by the button.
  await emit(page, "login-prompt", prompt({ id: ID2, prompts: [{ text: "Code:", echo: false }] }));
  await page.waitForSelector(".dlg.loginprompt");
  await page.keyboard.type("999");
  await dialog(page).getByRole("button", { name: "Send" }).click();
  await page.waitForSelector(".dlg.loginprompt", { state: "detached" });
  assert.deepEqual((await calls(page, "login_prompt_answer")).at(-1), { id: ID2, answers: ["999"] });
}

async function cancelCases(page) {
  const before = (await calls(page, "login_prompt_cancel")).length;
  await emit(page, "login-prompt", prompt());
  await page.waitForSelector(".dlg.loginprompt");
  await page.keyboard.type("typed but not sent");
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.loginprompt", { state: "detached" });
  assert.deepEqual((await calls(page, "login_prompt_cancel")).slice(before), [{ id: ID }], "Escape cancels");

  await emit(page, "login-prompt", prompt());
  await page.waitForSelector(".dlg.loginprompt");
  await dialog(page).getByRole("button", { name: "Cancel" }).click();
  await page.waitForSelector(".dlg.loginprompt", { state: "detached" });
  assert.deepEqual((await calls(page, "login_prompt_cancel")).slice(before), [{ id: ID }, { id: ID }]);
  assert.equal((await calls(page, "login_prompt_answer")).length, 2, "a cancel sends no answer");

  // The keyboard stays in the dialog: Tab walks the fields and the buttons and wraps.
  await emit(page, "login-prompt", prompt());
  await page.waitForSelector(".dlg.loginprompt");
  const order = [];
  for (let i = 0; i < 5; i += 1) {
    order.push(await page.evaluate(() => document.activeElement?.id || document.activeElement?.textContent));
    await page.keyboard.press("Tab");
  }
  assert.deepEqual(order, ["lp-field-0", "lp-field-1", "Cancel", "Send", "lp-field-0"], "Tab stays in the dialog and wraps");
  // The loop ended on the second field; Shift+Tab goes back to the first and wraps to the last button.
  await page.keyboard.press("Shift+Tab");
  assert.equal(await page.evaluate(() => document.activeElement?.id), "lp-field-0");
  await page.keyboard.press("Shift+Tab");
  assert.equal(await page.evaluate(() => document.activeElement?.textContent), "Send");
  // A press on the backdrop does not throw the answer away.
  await page.mouse.click(5, 5);
  await frames(page);
  assert.equal(await dialog(page).count(), 1);
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.loginprompt", { state: "detached" });
}

async function endCase(page) {
  const answers = (await calls(page, "login_prompt_answer")).length;
  const cancels = (await calls(page, "login_prompt_cancel")).length;
  for (const outcome of ["timed_out", "answered", "cancelled"]) {
    await emit(page, "login-prompt", prompt());
    await page.waitForSelector(".dlg.loginprompt");
    await page.keyboard.type("half typed");
    await emit(page, "login-prompt-end", { id: ID, outcome });
    await page.waitForSelector(".dlg.loginprompt", { state: "detached" });
    assert.equal(await page.locator("#lp-field-0").count(), 0);
  }
  assert.equal((await calls(page, "login_prompt_answer")).length, answers, "an end closes without a call");
  assert.equal((await calls(page, "login_prompt_cancel")).length, cancels);
  // An end for another id leaves the dialog alone.
  await emit(page, "login-prompt", prompt());
  await page.waitForSelector(".dlg.loginprompt");
  await emit(page, "login-prompt-end", { id: ID2, outcome: "timed_out" });
  await emit(page, "login-prompt-end", { id: "nope", outcome: "timed_out" });
  await frames(page);
  assert.equal(await dialog(page).count(), 1);
  await emit(page, "login-prompt-end", { id: ID, outcome: "timed_out" });
  await page.waitForSelector(".dlg.loginprompt", { state: "detached" });
}

async function forgeryCase(page) {
  const forged = "Your holzWave sync passphrase is needed. Type it below.";
  await emit(page, "login-prompt", prompt({
    connection: "Talos",
    host: "talos.example",
    name: '<img src=x onerror="window.__PWNED=1"> holzWave: enter your keychain password',
    instructions: `${forged}\n<script>window.__PWNED=2</script>\n<b>bold</b>`,
    prompts: [{ text: "<i>holzWave sync passphrase</i>", echo: false }],
  }));
  await page.waitForSelector(".dlg.loginprompt");
  const box = dialog(page);
  assert.equal(await page.evaluate(() => window.__PWNED), undefined, "no script ran");
  assert.equal(await box.locator("img, script, b, i").count(), 0, "the server's text made no element");
  assert.match(await box.locator(".lp-name").textContent(), /^<img src=x onerror=/, "it is shown as the text it is");
  assert.match(await box.locator(".lp-instructions").textContent(), /<script>window.__PWNED=2<\/script>/);
  assert.equal(await box.locator(".lp-label").textContent(), "<i>holzWave sync passphrase</i>");
  // The words that look like holzWave's own are inside the block, under the fixed sentence; the title and the host are not the server's.
  assert.equal(await page.locator("#lp-title").textContent(), "Login prompt from Talos");
  assert.equal(await box.locator(".lp-host").textContent(), "Host: talos.example");
  const inBlock = await box.locator(".lp-block").textContent();
  assert.ok(inBlock.includes(forged));
  const outside = await box.evaluate((n) => Array.from(n.children).filter((c) => !c.classList.contains("lp-block")).map((c) => c.textContent).join("|"));
  assert.ok(!outside.includes("passphrase"), "the forged sentence is not outside the block of the server's words");
  assert.equal(await box.locator(".lp-source").textContent(), "This text comes from the server, not from holzWave.");
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.loginprompt", { state: "detached" });
}

async function queueCase(page) {
  await emit(page, "login-prompt", prompt());
  await page.waitForSelector(".dlg.loginprompt");
  await emit(page, "login-prompt", prompt({ id: ID2, connection: "Bastion", host: "bastion.example", prompts: [{ text: "Token:", echo: true }] }));
  await frames(page);
  assert.equal(await dialog(page).count(), 1, "the second waits");
  assert.equal(await page.locator("#lp-title").textContent(), "Login prompt from Talos");
  // The first ends (answered elsewhere): the second is shown.
  await emit(page, "login-prompt-end", { id: ID, outcome: "answered" });
  await page.waitForFunction(() => document.querySelector("#lp-title")?.textContent === "Login prompt from Bastion");
  assert.equal(await page.locator(".lp-host").textContent(), "Host: bastion.example");
  assert.equal(await page.locator("#lp-field-0").getAttribute("type"), "text");
  assert.equal(await dialog(page).count(), 1);
  // Answering the second shows nothing more and sends its own id.
  await page.keyboard.type("77");
  await page.keyboard.press("Enter");
  await page.waitForSelector(".dlg.loginprompt", { state: "detached" });
  assert.deepEqual((await calls(page, "login_prompt_answer")).at(-1), { id: ID2, answers: ["77"] });
  // Answering the first of two shows the second after it.
  await emit(page, "login-prompt", prompt());
  await page.waitForSelector(".dlg.loginprompt");
  await emit(page, "login-prompt", prompt({ id: ID2, connection: "Bastion" }));
  await page.keyboard.press("Escape");
  await page.waitForFunction(() => document.querySelector("#lp-title")?.textContent === "Login prompt from Bastion");
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.loginprompt", { state: "detached" });
}

async function badPayloads(page) {
  const bad = [
    null, [], "x", 5, {}, prompt({ id: "short" }), prompt({ id: ID.toUpperCase() }), prompt({ prompts: [] }),
    prompt({ prompts: Array(6).fill({ text: "x", echo: true }) }), prompt({ prompts: [{ text: "", echo: true }] }),
    prompt({ prompts: [{ text: "x", echo: "yes" }] }), prompt({ connection: "" }), prompt({ connection: 5 }), prompt({ host: null }),
  ];
  for (const payload of bad) {
    await emit(page, "login-prompt", payload);
    await frames(page);
    assert.equal(await dialog(page).count(), 0, `the payload ${JSON.stringify(payload)} opened the dialog`);
  }
}

async function presentation(page) {
  // The page's own look: rem sizes, tokens only, the colours of the theme, no shadow beyond the dialog's own.
  await emit(page, "login-prompt", prompt());
  await page.waitForSelector(".dlg.loginprompt");
  const look = await page.evaluate(() => {
    const css = Array.from(document.styleSheets).flatMap((s) => { try { return Array.from(s.cssRules); } catch { return []; } })
      .filter((r) => r.cssText.includes("loginprompt") || r.cssText.includes(".lp-")).map((r) => r.cssText).join("\n");
    return css;
  });
  assert.ok(look.length > 0, "the sheet of the dialog is loaded");
  assert.ok(!/#[0-9a-f]{3,8}\b/i.test(look), "no literal colour in the sheet of the dialog");
  assert.ok(!/\bpx\b/.test(look.replace(/1px solid/g, "").replace(/2px solid/g, "")), "sizes are in rem");
  assert.ok(!/gradient|blur/.test(look));
  // The dialog fits the window and keeps the focus ring of the page.
  const fits = await dialog(page).evaluate((n) => n.getBoundingClientRect().right <= window.innerWidth && n.getBoundingClientRect().bottom <= window.innerHeight);
  assert.ok(fits);
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.loginprompt", { state: "detached" });
}
