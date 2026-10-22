// The card of an integration action (docs/design.md section 6.26), the tracer: a pod deletion that the core prepared is shown field
// by field, in the core's order, as text, and one real press after the half second sends the ticket and the hash the core gave and
// nothing else. A card the page cannot read has no allow button.
import assert from "node:assert/strict";
import { HASH, HOSTILE, KUBE, TICKET, calls, cardNode, cardOf, gone, installCore, openConfirm, outcome, request, textOf, wait } from "./integrate-confirm-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await a_pod_deletion_card_is_shown_and_allowed_once(page, url);
  await an_unusable_card_can_only_be_denied(page, url);
  await hostile_fields_are_text(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

async function a_pod_deletion_card_is_shown_and_allowed_once(page, url) {
  await openConfirm(page, url, {}, { tools: [KUBE] });
  await page.waitForSelector(".block-integration .ig-table tbody tr");
  assert.equal(await cardNode(page).count(), 0, "no card before a request");
  await page.evaluate(() => {
    window.__before = document.activeElement;
  });
  await request(page);
  await cardNode(page).waitFor();

  // One layer in the window, with the words of the design.
  const card = cardNode(page);
  assert.equal(await card.count(), 1);
  assert.equal(await page.evaluate(() => document.querySelector("section.ic").parentElement === document.body), true);
  assert.equal(await card.getAttribute("role"), "alertdialog");
  assert.equal(await card.getAttribute("data-mode"), "ask");
  assert.equal(await textOf(page, ".ic-who"), "Confirm needed");
  assert.equal(await textOf(page, ".ic-title"), "Kubernetes · Delete pod");
  assert.equal(await textOf(page, ".ic-effect"), "Changes a cluster or an account. Check the target below.");
  const target = await card.locator(".ic-target .ic-row").evaluateAll((rows) => rows.map((r) => [r.children[0].textContent, r.children[1].textContent]));
  assert.deepEqual(target, [["context", "prod-eu"], ["namespace", "payments"], ["pod", "api-0"]]);
  const args = await card.locator(".ic-args .ic-row").evaluateAll((rows) => rows.map((r) => [r.children[0].textContent, r.children[1].textContent]));
  assert.deepEqual(args, [
    ["program", "kubectl"], ["arg 1", "delete"], ["arg 2", "pod"], ["arg 3", "--context=prod-eu"], ["arg 4", "--namespace=payments"],
    ["arg 5", "--wait=false"], ["arg 6", "--request-timeout=10s"], ["arg 7", "--"], ["arg 8", "api-0"],
  ]);
  assert.equal(await card.locator(".ic-guard").count(), 0, "no guard line without a production context");
  assert.equal(await textOf(page, ".ic-hash"), `hash ${HASH.slice(0, 12)}`);
  assert.equal(await textOf(page, ".ic-no"), "Deny");
  assert.equal(await textOf(page, ".ic-yes"), "Allow once");
  assert.equal(await textOf(page, ".ic-foot"), "Allows this one action only. Nothing is kept for later. Esc denies.");

  // The card never takes the keyboard, and nothing was sent.
  assert.equal(await page.evaluate(() => document.activeElement === window.__before), true, "the focus did not move");
  assert.equal((await calls(page, "integration_confirm")).length, 0);
  assert.deepEqual(await calls(page, "integration_prepare"), [{ tool: "kube", verb: "delete_pod", pane: null, args: { pod: "api-0" } }]);

  // After the half second one real press is one call with the ticket and the full hash, and nothing else.
  await wait(page, 600);
  await card.locator(".ic-yes").click();
  const done = await outcome(page);
  assert.equal(done.kind, "done");
  const sent = await calls(page, "integration_confirm");
  assert.deepEqual(sent, [{ ticket: TICKET, hash: HASH }]);
  assert.deepEqual(Object.keys(sent[0]), ["ticket", "hash"]);
  assert.equal(sent[0].hash.length, 64, "the full hash, not the 12 characters on the card");
  assert.equal(await card.getAttribute("data-mode"), "done");
  assert.equal(await textOf(page, ".ic-line"), 'Done: pod "api-0" deleted');
  assert.equal(await card.locator("button").count(), 0, "no buttons after the decision");
  assert.equal((await calls(page, "integration_cancel")).length, 0);
}

async function an_unusable_card_can_only_be_denied(page, url) {
  const broken = cardOf();
  delete broken.hash;
  await installCore(page, { cards: { "kube/delete_pod": broken } });
  await page.goto(`${url}index.html?renderer=canvas`);
  await request(page);
  await cardNode(page).waitFor();
  const card = cardNode(page);
  assert.equal(await card.getAttribute("data-mode"), "cannot");
  assert.equal(await textOf(page, ".ic-title"), "Cannot show this request");
  assert.equal(await textOf(page, ".ic-line"), "This request could not be read in full. It can only be denied.");
  assert.equal(await card.locator(".ic-yes").count(), 0, "no allow button");
  assert.deepEqual(await card.locator("button").allTextContents(), ["Deny"]);
  await wait(page, 600);
  await card.locator(".ic-no").click();
  await gone(page);
  assert.equal((await calls(page, "integration_confirm")).length, 0, "nothing was confirmed");
  assert.deepEqual(await calls(page, "integration_cancel"), [{ ticket: TICKET }]);
  assert.equal((await outcome(page)).kind, "denied");
}

async function hostile_fields_are_text(page, url) {
  const hostile = cardOf({
    argv: ["delete", "pod", `--context=${HOSTILE}`, "--", "api-0"],
    target: [["context", HOSTILE], ["pod", "a\\u{202e}b"]],
  });
  await installCore(page, { cards: { "kube/delete_pod": hostile } });
  await page.goto(`${url}index.html?renderer=canvas`);
  await request(page);
  await cardNode(page).waitFor();
  const card = cardNode(page);
  assert.equal(await card.locator("img").count(), 0, "a name that looks like an element is text");
  assert.equal(await page.locator("section.ic [onerror]").count(), 0);
  const rows = await card.locator(".ic-row").evaluateAll((r) => r.map((x) => x.children[1].textContent));
  assert.ok(rows.includes(HOSTILE), "the name is shown as those characters");
  assert.ok(rows.includes(`--context=${HOSTILE}`));
  assert.ok(rows.includes("a\\u{202e}b"), "the core's escape is shown as it came");
  await card.locator(".ic-no").click({ force: true });
  await gone(page);
}
