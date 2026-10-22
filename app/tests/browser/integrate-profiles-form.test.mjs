// The form of the profiles dialog (design.md 6.27): a new profile, the fields that follow the tool the core lists, the flags that
// are sent back with every save, and the id of a new profile.
import assert from "node:assert/strict";
import { ALL_TOOLS, AZURE_PROFILE, PROD, STAGING, callsOf, countOf, openDialog, openPage, profile } from "./integrate-profiles-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await followCase(page, url);
  await idCase(page, url);
  await flagCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

const labels = (page) => page.locator(".ip-fields .ip-field > label").allTextContents();
const toolRadios = (page) => page.locator("fieldset.ip-tools label.aiset-option").allTextContents();

async function chooseTool(page, label) {
  await page.locator("fieldset.ip-tools label.aiset-option", { hasText: label }).locator("input").check();
}

async function followCase(page, url) {
  await openPage(page, url, { profiles: [PROD, STAGING, AZURE_PROFILE] });
  await openDialog(page);
  await page.waitForSelector("label.ip-row");

  // A saved profile fills the form: its name, its tool, its pinned fields and its flags, from the core's answer.
  assert.equal(await page.locator("fieldset.ip-form > legend").textContent(), "Profile");
  assert.equal(await page.locator("#ip-name").inputValue(), "Production EU");
  assert.deepEqual(await toolRadios(page), ["Kubernetes", "Talos", "Azure", "GAM"], "the tools the core lists, with the core's names");
  assert.equal(await page.locator("fieldset.ip-tools label:has(input:checked)").textContent(), "Kubernetes");
  assert.deepEqual(await labels(page), ["Context", "Namespace"]);
  assert.equal(await page.locator("#ip-field-context").inputValue(), "prod-eu");
  assert.equal(await page.locator("#ip-field-namespace").inputValue(), "payments");
  assert.equal(await page.locator("#ip-production").isChecked(), true);
  assert.equal(await page.locator("#ip-isolated").count(), 0, "Kubernetes has no folder of its own");

  // The sentences of the two switches.
  assert.equal(await page.locator("label[for=ip-production]").textContent(), "Production context");
  assert.equal(await page.locator("#ip-production-hint").textContent(), "Actions on this context ask twice.");

  // The fields are text fields with the example as the placeholder and the core's bound.
  const context = page.locator("#ip-field-context");
  assert.equal(await context.getAttribute("placeholder"), "prod-eu");
  assert.equal(await context.getAttribute("maxlength"), "253");
  assert.equal(await page.locator("#ip-name").getAttribute("maxlength"), "64");

  // The tool decides the fields: nothing of a field the new tool does not select is kept, and the name and the flags are.
  await page.locator("#ip-name").fill("Renamed");
  await chooseTool(page, "Talos");
  assert.deepEqual(await labels(page), ["Context", "Node"]);
  assert.equal(await page.locator("#ip-field-context").inputValue(), "prod-eu", "a field the new tool also selects keeps its text");
  assert.equal(await page.locator("#ip-field-node").inputValue(), "");
  assert.equal(await page.locator("#ip-field-node").getAttribute("placeholder"), "node-1");
  assert.equal(await page.locator("#ip-name").inputValue(), "Renamed");
  assert.equal(await page.locator("#ip-production").isChecked(), true);
  await chooseTool(page, "Azure");
  assert.deepEqual(await labels(page), ["Subscription"]);
  assert.equal(await page.locator("#ip-isolated").count(), 1, "Azure keeps a folder of its own");
  assert.equal(await page.locator("label[for=ip-isolated]").textContent(), "Own configuration folder");
  assert.equal(await page.locator("#ip-isolated-hint").textContent(), "Sign in again inside it. holzWave keeps it on this computer.");
  await page.locator("#ip-isolated").check();
  await chooseTool(page, "GAM");
  assert.deepEqual(await labels(page), ["Section"]);
  assert.equal(await page.locator("#ip-isolated").isChecked(), true, "the flag stays through a tool change that still takes it");
  await chooseTool(page, "Kubernetes");
  assert.deepEqual(await labels(page), ["Context", "Namespace"]);
  assert.equal(await page.locator("#ip-field-context").inputValue(), "", "the context was dropped on the way through Azure");
  assert.equal(await page.locator("#ip-isolated").count(), 0);
  await chooseTool(page, "Azure");
  assert.equal(await page.locator("#ip-isolated").isChecked(), false, "a flag the tool could not take was dropped");

  // A save sends nothing for a field the tool does not select.
  await page.locator("#ip-field-subscription").fill("11111111-2222-3333-4444-555555555555");
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_profile_save").length === 1);
  const sent = (await callsOf(page, "integration_profile_save"))[0].profile;
  assert.equal(sent.tool, "azure");
  assert.equal(sent.subscription, "11111111-2222-3333-4444-555555555555");
  assert.equal(sent.context, null);
  assert.equal(sent.namespace, null);
  assert.equal(sent.node, null);
  assert.equal(sent.section, null);
  await page.keyboard.press("Escape");

  // The form follows the core's list of what the tool selects, not a table of the page: a core that lists a tool with other
  // selections gets that form.
  const tools = structuredClone(ALL_TOOLS);
  tools[0].selects = ["namespace"];
  tools[1].selects = ["node", "section"];
  await page.evaluate((t) => {
    window.__CX__.tools = t;
  }, tools);
  await page.evaluate(() => window.__MOCK__.emit("integration", { seq: 1, kind: "tools", tool: null, pane: null, code: null }));
  await page.waitForFunction(() => window.__CX__.tools[0].selects.length === 1);
  await openDialog(page);
  await page.locator("label.ip-row", { hasText: "Renamed" }).locator("input").check();
  await chooseTool(page, "Kubernetes");
  assert.deepEqual(await labels(page), ["Namespace"]);
  await chooseTool(page, "Talos");
  assert.deepEqual(await labels(page), ["Node", "Section"]);
  assert.equal(await page.locator("#ip-isolated").count(), 1, "a tool that selects a section is offered a folder");
  await page.keyboard.press("Escape"); // the form was changed: this asks
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.ip-box", { state: "detached" });
}

async function idCase(page, url) {
  await page.reload();
  await page.evaluate(([p]) => {
    window.__PF__.profiles = [p];
  }, [STAGING]);
  await page.waitForSelector(".cx-row");
  await openDialog(page);
  await page.waitForSelector("label.ip-row");

  // `＋ New` clears the form to a new profile; nothing is invented in it except the first tool the core lists.
  await page.locator(".ip-add").click();
  assert.equal(await page.locator("fieldset.ip-form > legend").textContent(), "New profile");
  assert.equal(await page.locator("#ip-name").inputValue(), "");
  assert.equal(await page.locator("fieldset.ip-tools label:has(input:checked)").textContent(), "Kubernetes");
  assert.equal(await page.locator("#ip-field-context").inputValue(), "");
  assert.equal(await page.locator("#ip-production").isChecked(), false);
  assert.equal(await page.locator("label.ip-row[data-chosen=true]").count(), 0, "no row is chosen for a new profile");
  await page.waitForFunction(() => document.activeElement?.id === "ip-name");

  // The id is a slug of the name, made unique among the listed ids: a new profile never replaces another.
  await page.locator("#ip-name").fill("Staging");
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_profile_save").length === 1);
  assert.equal((await callsOf(page, "integration_profile_save"))[0].profile.id, "staging-2");
  await page.waitForFunction(() => document.querySelectorAll("label.ip-row").length === 2);
  assert.equal(await page.locator("label.ip-row[data-chosen=true] .ip-name").textContent(), "Staging", "the saved profile is the chosen row");
  assert.equal(await page.locator("label.ip-row[data-chosen=true]").getAttribute("data-id"), "staging-2");

  // An edit of a saved profile keeps its id, whatever the name says now.
  await page.locator("#ip-name").fill("Another name");
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_profile_save").length === 2);
  assert.equal((await callsOf(page, "integration_profile_save"))[1].profile.id, "staging-2");

  // A name with no letter or digit still gets an id; the core says whether it accepts the name.
  await page.locator(".ip-add").click();
  await page.locator("#ip-name").fill("日本語");
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_profile_save").length === 3);
  assert.equal((await callsOf(page, "integration_profile_save"))[2].profile.id, "profile");
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.ip-box", { state: "detached" });
}

async function flagCase(page, url) {
  await page.reload();
  await page.evaluate(([a, b]) => {
    window.__PF__.profiles = [a, b];
  }, [PROD, profile({ id: "dev", name: "Dev", production: false, context: "dev", namespace: null })]);
  await page.waitForSelector(".cx-row");
  await openDialog(page);
  await page.waitForSelector("label.ip-row");

  // The production flag is loaded from the core and sent back by an edit that does not touch it.
  assert.equal(await page.locator("#ip-production").isChecked(), true);
  await page.locator("#ip-name").fill("Production EU (edited)");
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_profile_save").length === 1);
  const first = (await callsOf(page, "integration_profile_save"))[0].profile;
  assert.equal(first.production, true, "an edit of a name does not drop the production flag");
  assert.equal(first.id, "prod-eu");
  assert.equal(first.context, "prod-eu");
  assert.equal(first.namespace, "payments");
  assert.deepEqual(first.env, { AWS_PROFILE: "prod" }, "the plain variables are sent back too");
  assert.deepEqual(first.secret_env, ["AWS_SECRET_ACCESS_KEY"], "the names of the secret variables are sent back too");

  // The flag is a checkbox and nothing else: a change is sent as it is.
  await page.locator("#ip-production").uncheck();
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_profile_save").length === 2);
  assert.equal((await callsOf(page, "integration_profile_save"))[1].profile.production, false);

  // The other profile is not production and its box is not checked.
  await page.locator("label.ip-row", { hasText: "Dev" }).locator("input").check();
  assert.equal(await page.locator("#ip-production").isChecked(), false);
  assert.equal(await countOf(page, "integration_profile_remove"), 0);
  await page.keyboard.press("Escape");
}
