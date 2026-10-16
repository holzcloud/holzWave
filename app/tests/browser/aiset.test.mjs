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
