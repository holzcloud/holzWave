// The AI providers dialog (docs/design.md section 6.13): from `Set up…` in the chat bar a provider is added, saved with the
// one command and shown in the chat without a reload. The page validates nothing: the shapes it sends and reads are held to
// the golden that the core writes (`tests/fixtures/ai_profiles.json`).
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { boot, calls, choosePreset, fillField, openAi, openDialog, sendText } from "./ai-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await traceCase(page, url);
  await contractCase(page, url);
  await refusalCase(page, url);
  await keyCase(page, url);
  await shapeCase(page, url);
  await moveCase(page, url);
  await removeCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** The golden the core writes from its real tables. */
const golden = JSON.parse(readFileSync(new URL("../fixtures/ai_profiles.json", import.meta.url), "utf8"));

const dlg = ".dlg.ai-settings";
const count = async (page, cmd) => (await calls(page, cmd)).length;

/** Open a chat on a page with no saved provider and wait until the bar offers `Set up…`. */
async function chatWithoutProvider(page, url, extra = {}) {
  await boot(page, url, { profiles: [], ...extra });
  await openAi(page);
  await page.waitForFunction(() => document.activeElement?.matches("textarea.chat-input"));
  await page.waitForFunction(() => document.querySelector(".chat-setup")?.textContent === "Set up…");
}

/** Add the provider `Local` of the type Ollama in the open dialog and save it. */
async function addLocal(page) {
  await choosePreset(page, "Ollama");
  await fillField(page, "#aiset-name", "Local");
  await fillField(page, "#aiset-model", "llama3.2");
  await page.locator(".aiset-save").click();
  await page.waitForSelector("label.aiset-row");
}

async function traceCase(page, url) {
  await chatWithoutProvider(page, url);
  assert.equal(await page.locator(".chat-chip").textContent(), "no provider set up");
  assert.equal(await page.locator(".chat-chip").evaluate((n) => n.localName), "span", "the chip offers no button");
  assert.equal(await count(page, "ai_profiles"), 1, "the list was read once, at the focus of the composer");

  await page.locator(".chat-setup").click();
  await openDialog(page);
  assert.equal(await page.locator(dlg).getAttribute("role"), "dialog");
  assert.equal(await page.locator(dlg).getAttribute("aria-modal"), "true");
  assert.equal(await count(page, "ai_profiles"), 2, "opening the dialog reads the list once more");
  await page.waitForFunction(() => document.querySelector(".dlg.ai-settings")?.contains(document.activeElement));
  assert.equal(await page.locator(".aiset-empty").textContent(), "No provider yet.");
  assert.equal(await page.locator("fieldset.aiset-form > legend").textContent(), "New provider");

  // The eleven types in the order of the core, Anthropic chosen, and no API shape (it speaks one).
  const labels = await page.locator("fieldset.aiset-presets label.aiset-option").allTextContents();
  assert.deepEqual(labels, golden.presets.map((p) => p.label));
  assert.equal(await page.locator('fieldset.aiset-presets input[type="radio"]:checked').count(), 1);
  assert.equal(await page.locator("fieldset.aiset-presets label.aiset-option:has(input:checked)").textContent(), "Anthropic");
  assert.equal(await page.locator("fieldset.aiset-wires").isHidden(), true);

  // Ollama, a name and a model; the address stays empty. One save, with exactly the eight members.
  await choosePreset(page, "Ollama");
  await fillField(page, "#aiset-name", "Local");
  await fillField(page, "#aiset-model", "llama3.2");
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profile_save").length >= 1);
  assert.deepEqual(await calls(page, "ai_profile_save"), [
    {
      input: {
        id: null, name: "Local", preset: "ollama", wire: "ollama", base_url: "", model: "llama3.2", allow_lan_http: false,
        acknowledge_gateway: false,
      },
    },
  ]);
  await page.waitForSelector("label.aiset-row");
  assert.equal(await page.locator(".aiset-status").textContent(), "Saved.");
  for (const cmd of ["ai_profile_remove", "ai_key_set", "ai_key_clear", "ai_send"]) assert.equal(await count(page, cmd), 0, cmd);

  const rows = page.locator("label.aiset-row");
  assert.equal(await rows.count(), 1);
  assert.equal(await rows.locator(".aiset-name").textContent(), "Local");
  assert.equal(await rows.locator(".aiset-meta").textContent(), "Ollama · llama3.2");
  assert.equal(await rows.locator(".aiset-host").textContent(), "127.0.0.1:11434", "the host the core reported");
  assert.equal(await rows.locator(".aiset-key").textContent(), "no key");
  assert.equal(await rows.locator('input[type="radio"]').isChecked(), true);

  // Escape closes it, the composer has the keyboard back, and the bar names the provider without a reload.
  await page.keyboard.press("Escape");
  await page.waitForSelector(dlg, { state: "detached" });
  await page.waitForFunction(() => document.activeElement?.matches("textarea.chat-input"));
  await page.waitForFunction(() => document.querySelector(".chat-chip")?.textContent?.includes("127.0.0.1:11434"));
  const chip = await page.locator(".chat-chip").textContent();
  for (const part of ["Local", "llama3.2", "127.0.0.1:11434"]) assert.ok(chip.includes(part), `${part} in "${chip}"`);
  assert.equal(await page.locator(".chat-chip").evaluate((n) => n.localName), "span");
  assert.equal(await page.locator(".chat-setup").textContent(), "Providers…");

  await sendText(page, "hello");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  assert.equal((await calls(page, "ai_send"))[0].profile, "local");
  assert.equal(await count(page, "ai_profiles"), 3, "one read for the dialog, one after the save, none for the send");
}

/** The members of an object, sorted. */
const members = (o) => Object.keys(o).sort();

async function contractCase(page, url) {
  await chatWithoutProvider(page, url);
  await page.locator(".chat-setup").click();
  await openDialog(page);
  await addLocal(page);
  await page.keyboard.press("Escape");
  await page.waitForSelector(dlg, { state: "detached" });

  // The answer of the mock has the shape of the core's: the members of a golden view, states from the golden's names.
  const shape = await page.evaluate(async () => {
    const answer = await window.__TAURI__.core.invoke("ai_profiles");
    const { readProfileViews } = await import(new URL("aistate.js", location.href).href);
    return { answer, read: readProfileViews(answer) };
  });
  assert.deepEqual(members(shape.answer), ["notes", "profiles"]);
  assert.equal(shape.answer.profiles.length, 1);
  for (const view of shape.answer.profiles) {
    assert.deepEqual(members(view), members(golden.views[0]));
    assert.ok(golden.key_states.includes(view.key), view.key);
    assert.ok(view.gateway_key === null || golden.key_states.includes(view.gateway_key), String(view.gateway_key));
  }
  assert.deepEqual(shape.read.profiles, shape.answer.profiles, "the page's reader keeps every member");

  // A save with a member outside the eight is refused by the mock as the shell refuses it.
  const refused = await page.evaluate(async () => {
    const input = {
      id: null, name: "Extra", preset: "ollama", wire: "ollama", base_url: "", model: "", allow_lan_http: false,
      acknowledge_gateway: false, api_key: "never",
    };
    try {
      await window.__TAURI__.core.invoke("ai_profile_save", { input });
      return null;
    } catch (e) {
      return e;
    }
  });
  assert.equal(refused?.code, "profile");
  assert.equal((await page.evaluate(() => window.__MOCK__.ai.profiles.length)), 1, "the refused input saved nothing");

  // The core's notes are plain text: no element, no control character; no note, no list.
  await page.evaluate(() => {
    window.__MOCK__.ai.notes = ["<b>profile #2</b> is not <img src=x>\u0007 usable"];
  });
  await page.locator(".chat-setup").click();
  await openDialog(page);
  assert.equal(await page.locator(".aiset-notes li").count(), 1);
  assert.equal(await page.locator(".aiset-notes li").textContent(), "<b>profile #2</b> is not <img src=x> usable");
  assert.equal(await page.locator(".aiset-notes li").evaluate((n) => n.children.length), 0);
  assert.equal(await page.locator(".aiset-notes").isVisible(), true);
  await page.keyboard.press("Escape");
  await page.waitForSelector(dlg, { state: "detached" });
  await page.evaluate(() => {
    window.__MOCK__.ai.notes = [];
  });
  await page.locator(".chat-setup").click();
  await openDialog(page);
  assert.equal(await page.locator(".aiset-notes").isHidden(), true, "no note, no list");
  await page.keyboard.press("Escape");
}

// ---- helpers of the cases below -------------------------------------------------------------------------------------------

/** The words the page has for a reason of the core, from its own module. */
const words = (page, reason, context = "profile") =>
  page.evaluate(
    async ([r, c]) => {
      const { describeFault } = await import(new URL("aistate.js", location.href).href);
      return describeFault({ code: "profile", reason: r, text: "x" }, c);
    },
    [reason, context],
  );

/** Open a chat on a page with no provider, open the dialog from the chat bar and wait for its list. */
async function openFresh(page, url, extra = {}) {
  await chatWithoutProvider(page, url, extra);
  await page.locator(".chat-setup").click();
  await openDialog(page);
}

/** The commands the page called, in order, without the reads. */
const writes = (page) =>
  page.evaluate(() =>
    window.__MOCK__.calls
      .filter(([cmd]) => /^ai_(profile_save|profile_remove|key_set|key_clear)$/.test(cmd))
      .map(([cmd]) => cmd),
  );

/** Type a name, a model and an address into the form (the address is left alone when `address` is null). */
async function fillForm(page, { name, model = "m", address = null }) {
  await fillField(page, "#aiset-name", name);
  await fillField(page, "#aiset-model", model);
  if (address !== null) await fillField(page, "#aiset-address", address);
}

/** Choose an API shape by clicking the text of its radio. */
const chooseWire = (page, label) => page.locator("fieldset.aiset-wires").getByText(label, { exact: true }).click();

const rowNames = (page) => page.locator("label.aiset-row .aiset-name").allTextContents();

async function refusalCase(page, url) {
  await openFresh(page, url);
  await choosePreset(page, "Custom");
  await fillForm(page, { name: "Probe" });
  const field = page.locator("#aiset-address");
  const error = page.locator('.aiset-error[data-for="address"]');
  const attempts = [
    ["ftp://x.example", "scheme", ["x.example"]],
    ["https://user:pw@h.example", "credentials", ["pw@", "h.example"]],
    ["https://h.example/?a=1", "query", ["a=1"]],
    ["http://api.example.com", "plain_http", ["api.example.com"]],
    ["https://169.254.169.254", "metadata", ["169.254"]],
    ["https://[fe80::1]/", "link_local", ["fe80"]],
    ["", "needs_address", []],
  ];
  let saves = 0;
  for (const [address, reason, typed] of attempts) {
    await fillField(page, "#aiset-address", address);
    await page.locator(".aiset-save").click();
    saves += 1;
    await page.waitForFunction((n) => window.__MOCK__.callsOf("ai_profile_save").length === n, saves);
    await error.waitFor({ state: "visible" });
    const expected = (await words(page, reason)).text;
    assert.equal(await error.textContent(), expected, reason);
    for (const bit of typed) assert.equal(expected.includes(bit), false, `${reason}: the words hold what was typed`);
    assert.equal(await field.getAttribute("aria-invalid"), "true", reason);
    assert.ok((await field.getAttribute("aria-describedby")).split(" ").includes("aiset-error-address"), reason);
    assert.equal(await field.inputValue(), address, "the typed address stays in the field");
    assert.equal(await page.locator("label.aiset-row").count(), 0, "the list gained no row");
    assert.equal(await page.evaluate(() => document.activeElement?.id), "aiset-address", "the focus is on the field");
    assert.equal(await page.locator(".aiset-status").getAttribute("data-kind"), null, "a field's refusal is not in the status line");
  }
  assert.equal((await calls(page, "ai_profile_save")).length, attempts.length, "one call per Save");

  // A blank name is refused at the name with the words of `name`.
  await fillField(page, "#aiset-address", "https://llm.example.org/v1");
  await fillField(page, "#aiset-name", "   ");
  await page.locator(".aiset-save").click();
  await page.locator('.aiset-error[data-for="name"]').waitFor({ state: "visible" });
  assert.equal(await page.locator('.aiset-error[data-for="name"]').textContent(), (await words(page, "name")).text);
  assert.equal(await page.locator("#aiset-name").getAttribute("aria-invalid"), "true");
  assert.equal(await field.getAttribute("aria-invalid"), null, "the mark of the address went when it was saved");
  assert.equal(await error.isHidden(), true);

  // An edit takes its mark away; a good save clears the rest.
  await fillField(page, "#aiset-name", "Probe");
  assert.equal(await page.locator("#aiset-name").getAttribute("aria-invalid"), null);
  await page.locator(".aiset-save").click();
  await page.waitForSelector("label.aiset-row");
  assert.deepEqual(await rowNames(page), ["Probe"]);
  assert.equal(await page.locator(".aiset-status").textContent(), "Saved.");

  // A reason the page has no words for shows the core's own line in the status line.
  await page.evaluate(() =>
    window.__MOCK__.on("ai_profile_save", () => {
      throw { code: "config_write", reason: null, text: "the configuration file could not be written" };
    }),
  );
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => document.querySelector(".aiset-status")?.dataset.kind === "error");
  assert.equal(await page.locator(".aiset-status").textContent(), "the configuration file could not be written");
  assert.equal(await page.locator("[aria-invalid]").count(), 0);
  await page.evaluate(() => window.__MOCK__.on("ai_profile_save", null));
  await page.keyboard.press("Escape");
}

const CANARY = "zq-set-key-2f9a81d4c7";
const TOKEN = "zq-set-token-90be3c5d17";

/** Everything a person or a script could read from the page, as one text. */
const pageText = (page) =>
  page.evaluate(() => {
    const store = (s) => Object.keys(s).map((k) => `${k}=${s.getItem(k)}`).join("\n");
    return [document.documentElement.outerHTML, document.body.innerText, store(localStorage), store(sessionStorage)].join("\n");
  });

async function keyCase(page, url) {
  const consoleText = [];
  page.on("console", (m) => consoleText.push(m.text()));
  await openFresh(page, url);
  await fillForm(page, { name: "Claude", model: "claude-x" });
  await fillField(page, "#aiset-key", CANARY);
  const field = page.locator("#aiset-key");
  assert.equal(await field.getAttribute("type"), "password");
  assert.equal(await field.getAttribute("autocomplete"), "new-password");
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_key_set").length === 1);
  await page.waitForFunction(() => document.querySelector(".aiset-status")?.textContent === "Saved.");

  assert.deepEqual(await writes(page), ["ai_profile_save", "ai_key_set"], "the profile first, then the key, once each");
  assert.deepEqual((await calls(page, "ai_key_set"))[0], { profile: "claude", slot: "provider", api_key: CANARY });
  const others = await page.evaluate(() => window.__MOCK__.calls.filter(([cmd]) => cmd !== "ai_key_set"));
  assert.equal(JSON.stringify(others).includes(CANARY), false, "the key is in no other call");
  assert.equal(JSON.stringify((await calls(page, "ai_profile_save"))[0]).includes(CANARY), false);
  assert.equal(await field.inputValue(), "", "the field is empty again");
  assert.equal(await field.getAttribute("type"), "password");
  assert.equal(await page.locator(".aiset-keystate").textContent(), "key saved");
  assert.equal(await page.locator("label.aiset-row .aiset-key").textContent(), "key saved");
  assert.equal(await page.locator("label.aiset-row .aiset-key").getAttribute("data-tone"), "ok");
  assert.equal(await field.getAttribute("placeholder"), "Enter a new key to replace it");
  assert.equal((await pageText(page)).includes(CANARY), false, "the key is in no attribute, text or storage");
  assert.equal(consoleText.some((t) => t.includes(CANARY)), false, "the key is in no console message");

  // Closing and opening the dialog, and selecting another row and back, leave the key field empty.
  await page.keyboard.press("Escape");
  await page.waitForSelector(dlg, { state: "detached" });
  await page.locator(".chat-setup").click();
  await openDialog(page);
  assert.equal(await page.locator("#aiset-key").inputValue(), "");
  await page.locator(".aiset-add").click();
  await fillForm(page, { name: "Other", model: "o" });
  await choosePreset(page, "Ollama");
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => document.querySelectorAll("label.aiset-row").length === 2);
  await page.locator('label.aiset-row[data-id="claude"] input').check();
  assert.equal(await page.locator("#aiset-key").inputValue(), "");
  assert.equal(await page.locator(".aiset-keystate").textContent(), "key saved");
  // A key that was typed and not saved is lost with the row, and never comes back into the field.
  await fillField(page, "#aiset-key", "zq-unsaved-77");
  await page.locator('label.aiset-row[data-id="zq-none"] input, label.aiset-row:not([data-id="claude"]) input').first().check();
  assert.equal(await page.locator(".aiset-status").textContent(), "Changes were not saved.");
  assert.equal(await page.locator("#aiset-key").inputValue(), "");
  await page.locator('label.aiset-row[data-id="claude"] input').check();
  assert.equal(await page.locator("#aiset-key").inputValue(), "");
  assert.equal((await pageText(page)).includes("zq-unsaved-77"), false);

  // Remove key: one call, and the word becomes `no key`.
  await page.locator(".aiset-key-clear").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_key_clear").length === 1);
  assert.deepEqual((await calls(page, "ai_key_clear"))[0], { profile: "claude", slot: "provider" });
  await page.waitForFunction(() => document.querySelector(".aiset-keystate")?.textContent === "no key");
  assert.equal(await page.locator(".aiset-key-clear").isHidden(), true);
  assert.equal(await page.locator("#aiset-key").getAttribute("placeholder"), "API key");

  // A key with a space is refused at the key field; the profile is saved and the typed text stays.
  await fillField(page, "#aiset-key", "has a space");
  await page.locator(".aiset-save").click();
  await page.locator('.aiset-error[data-for="key"]').waitFor({ state: "visible" });
  assert.equal(await page.locator('.aiset-error[data-for="key"]').textContent(), (await words(page, "invalid", "key")).text);
  assert.equal(await page.locator("#aiset-key").inputValue(), "has a space");
  assert.equal(await page.locator("#aiset-key").getAttribute("aria-invalid"), "true");
  assert.equal(await page.locator(".aiset-keystate").textContent(), "no key");
  assert.equal(await page.locator(".aiset-status").textContent(), "Saved.");

  // With no keychain the profile is saved and the words of `no_secure_store` stand at the key field.
  await page.evaluate(() => {
    window.__MOCK__.ai.keychain = false;
  });
  await fillField(page, "#aiset-key", "abc123");
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_key_set").length === 3);
  await page.waitForFunction(() => document.querySelector('.aiset-error[data-for="key"]')?.textContent !== "");
  assert.equal(await page.locator('.aiset-error[data-for="key"]').textContent(), (await words(page, "no_secure_store", "key")).text);
  assert.equal(await page.evaluate(() => JSON.stringify(window.__MOCK__.ai.keys)), "{}", "the mock kept no key");
  await page.evaluate(() => {
    window.__MOCK__.ai.keychain = true;
  });

  // The gateway token has a slot of its own: provider key first, then the token, each in its own call.
  await page.locator(".aiset-add").click();
  await choosePreset(page, "Cloudflare AI Gateway");
  await fillForm(page, { name: "Edge", address: "https://gateway.example.net/v1/a/b/anthropic" });
  await fillField(page, "#aiset-key", CANARY);
  await fillField(page, "#aiset-gateway-key", TOKEN);
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_key_set").length === 5);
  const sets = await calls(page, "ai_key_set");
  assert.deepEqual(sets.slice(3), [
    { profile: "edge", slot: "provider", api_key: CANARY },
    { profile: "edge", slot: "gateway", api_key: TOKEN },
  ]);
  await page.waitForFunction(() => document.querySelector(".aiset-gateway-state")?.textContent === "key saved");
  assert.equal(await page.locator("#aiset-gateway-key").inputValue(), "");
  const text = await pageText(page);
  assert.equal(text.includes(CANARY) || text.includes(TOKEN), false, "no key in the page");
  assert.equal(consoleText.some((t) => t.includes(CANARY) || t.includes(TOKEN)), false);
  page.removeAllListeners("console");
  await page.keyboard.press("Escape");
}

async function shapeCase(page, url) {
  await openFresh(page, url);
  const wires = page.locator("fieldset.aiset-wires");
  const wireLabels = () => page.locator("fieldset.aiset-wires label.aiset-option").allTextContents();
  const hint = () => page.locator("#aiset-address-hint").textContent();
  const LEAVE = "Leave empty to use the address of this type.";
  const REQUIRED = "Required: the address of your gateway or server.";

  assert.equal(await wires.isHidden(), true, "Anthropic speaks one API shape");
  assert.equal(await hint(), LEAVE);
  assert.equal(await page.locator("#aiset-gateway-key").isHidden(), true);
  await fillForm(page, { name: "Shape", model: "mm", address: "https://typed.example.org/x" });

  await choosePreset(page, "Vercel AI Gateway");
  assert.deepEqual(await wireLabels(), ["OpenAI Chat Completions", "Anthropic Messages"]);
  assert.equal(await page.locator('fieldset.aiset-wires input[value="openai_chat"]').isChecked(), true, "the first is chosen");
  assert.equal(await hint(), LEAVE);
  assert.equal(await page.locator("#aiset-name").inputValue(), "Shape", "the typed name stays");
  assert.equal(await page.locator("#aiset-address").inputValue(), "https://typed.example.org/x");
  assert.equal(await page.locator("#aiset-model").inputValue(), "mm");
  await chooseWire(page, "Anthropic Messages");
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profile_save").length === 1);
  assert.equal((await calls(page, "ai_profile_save"))[0].input.wire, "anthropic", "the chosen shape is sent");
  await page.waitForSelector("label.aiset-row");

  // A saved provider: the type is fixed with the fixed sentence, the shape can change and goes with the saved id.
  assert.equal(await page.locator('fieldset.aiset-presets input[type="radio"]:not(:disabled)').count(), 0);
  assert.equal(await page.locator("fieldset.aiset-presets .aiset-hint").textContent(), "The type of a saved provider cannot change. Add a new provider for another type.");
  assert.equal(await page.locator('fieldset.aiset-wires input[type="radio"]:disabled').count(), 0);
  await chooseWire(page, "OpenAI Chat Completions");
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profile_save").length === 2);
  const second = (await calls(page, "ai_profile_save"))[1].input;
  assert.equal(second.id, "shape");
  assert.equal(second.wire, "openai_chat");
  assert.equal(second.preset, "vercel_gateway");

  // A new provider: Custom has five shapes and no default address; changing the type resets the shape.
  await page.locator(".aiset-add").click();
  assert.equal(await page.locator('fieldset.aiset-presets input[type="radio"]:disabled').count(), 0, "a new provider can pick a type");
  assert.equal(await page.locator("fieldset.aiset-presets .aiset-hint").isHidden(), true);
  await choosePreset(page, "Custom");
  assert.equal((await wireLabels()).length, 5);
  assert.equal(await hint(), REQUIRED);
  await choosePreset(page, "Cloudflare AI Gateway");
  assert.equal((await wireLabels()).length, 4);
  assert.equal(await page.locator('fieldset.aiset-wires input[value="anthropic"]').isChecked(), true, "reset to the first shape");
  assert.equal(await hint(), REQUIRED);
  assert.equal(await page.locator("#aiset-gateway-key").isVisible(), true);
  assert.equal(await page.locator("#aiset-key").isVisible(), true);
  await choosePreset(page, "OpenAI");
  assert.equal(await page.locator("#aiset-gateway-key").isHidden(), true, "no other type has a token");
  assert.equal(await hint(), LEAVE);

  // Choosing another row while the form differs says so and shows the other row's values.
  await fillField(page, "#aiset-name", "Draft");
  await page.locator('label.aiset-row[data-id="shape"] input').check();
  assert.equal(await page.locator(".aiset-status").textContent(), "Changes were not saved.");
  assert.equal(await page.locator("#aiset-name").inputValue(), "Shape");
  assert.equal(await page.locator("fieldset.aiset-form > legend").textContent(), "Provider");
  await page.keyboard.press("Escape");
}

async function moveCase(page, url) {
  await openFresh(page, url);
  await fillForm(page, { name: "Claude", model: "c" });
  await fillField(page, "#aiset-key", CANARY);
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => document.querySelector(".aiset-keystate")?.textContent === "key saved");
  assert.equal(await page.locator("#aiset-address").inputValue(), "https://api.anthropic.com", "the address the core reported");

  // The address changes to another one the core accepts: the key was filed for the old one.
  await fillField(page, "#aiset-address", "https://proxy.example.net");
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => document.querySelector(".aiset-keystate")?.textContent === "key for another address");
  const word = page.locator(".aiset-keystate");
  assert.equal(await word.getAttribute("data-tone"), "warn");
  assert.equal(await page.locator("label.aiset-row .aiset-key").textContent(), "key for another address");
  const advice = await page.locator(".aiset-keyfield").first().locator(".aiset-hint").textContent();
  assert.equal(advice, "The address changed after the key was saved, so the key is not used. Enter the key again.");

  // Entering the key again files it for the new address.
  await fillField(page, "#aiset-key", CANARY);
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_key_set").length === 2);
  await page.waitForFunction(() => document.querySelector(".aiset-keystate")?.textContent === "key saved");
  assert.equal(await word.getAttribute("data-tone"), "ok");
  assert.equal((await pageText(page)).includes(CANARY), false);
  assert.equal(await page.evaluate(() => JSON.stringify(window.__MOCK__.ai.keys).includes("zq-")), false, "the mock kept only an origin and a length");
  await page.keyboard.press("Escape");
}

async function removeCase(page, url) {
  await openFresh(page, url);
  await fillForm(page, { name: "Gone", model: "g" });
  await fillField(page, "#aiset-key", CANARY);
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => document.querySelector(".aiset-keystate")?.textContent === "key saved");
  assert.equal(await page.evaluate(() => Object.keys(window.__MOCK__.ai.keys).join()), "gone");

  const remove = page.locator(".aiset-remove");
  assert.equal(await remove.textContent(), "Remove provider");
  assert.equal(await remove.getAttribute("data-armed"), null);
  await remove.click();
  assert.equal(await remove.getAttribute("data-armed"), "true");
  assert.equal(await remove.textContent(), "Click again to remove");
  assert.equal((await calls(page, "ai_profile_remove")).length, 0, "the first click calls nothing");

  // A click on another control, a key in a field and another row take the question back.
  await page.locator("#aiset-model").click();
  assert.equal(await remove.getAttribute("data-armed"), null);
  assert.equal(await remove.textContent(), "Remove provider");
  await remove.click();
  await page.locator("#aiset-model").press("a");
  assert.equal(await remove.getAttribute("data-armed"), null, "a key in a field");
  await fillField(page, "#aiset-model", "g");
  await remove.click();
  await page.locator(".aiset-add").click();
  assert.equal(await remove.getAttribute("data-armed"), null, "Add provider");
  await page.locator('label.aiset-row[data-id="gone"] input').check();
  assert.equal(await remove.isVisible(), true);
  assert.equal((await calls(page, "ai_profile_remove")).length, 0);

  await remove.click();
  await remove.click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profile_remove").length === 1);
  assert.deepEqual(await calls(page, "ai_profile_remove"), [{ profile: "gone" }]);
  await page.waitForFunction(() => document.querySelectorAll("label.aiset-row").length === 0);
  assert.equal(await page.locator(".aiset-status").textContent(), "Removed.");
  assert.equal(await page.locator("fieldset.aiset-form > legend").textContent(), "New provider");
  assert.equal(await remove.isHidden(), true);
  assert.equal(await page.evaluate(() => JSON.stringify(window.__MOCK__.ai.keys)), "{}", "the mock deleted the key too");

  // With no provider left the chat says so again.
  await page.keyboard.press("Escape");
  await page.waitForSelector(dlg, { state: "detached" });
  await page.waitForFunction(() => document.querySelector(".chat-chip")?.textContent === "no provider set up");
  assert.equal(await page.locator(".chat-setup").textContent(), "Set up…");
}
