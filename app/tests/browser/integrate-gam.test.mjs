// GAM7 end to end on the page: the answers are the files the core wrote (core/tests/integrate_e2e_gam.rs writes them from
// a fake `gam`), and the page must draw exactly what the core produced, as inert text. The mock core answers the commands of
// the block with those files; nothing here is typed by hand except the tool's own entry, which is the core's list for GAM7.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { openBlock } from "./integrate-lib.mjs";
import { layout, openContext } from "./integrate-context-lib.mjs";

/** An answer file of the core, as the page receives it. */
const golden = (name) => JSON.parse(readFileSync(new URL(`../fixtures/integration/${name}`, import.meta.url), "utf8"));

const verb = (id, label, args = []) => ({ id, label, effect: "read", output: "view", refresh_secs: null, exit_is_state: false, args });

/** The entry of GAM7 in the core's tool list (integration_tools): three views, one flag, one selection field, no Local verb. */
const GAM = {
  id: "gam", label: "GAM7", state: "found", path: "/home/u/bin/gam7/gam", place: "fixed_dir", binary: null, reason: null,
  version: "7.46.02", searched: [], selects: ["section"],
  verbs: [verb("sections", "Sections"), verb("domain", "Domain"), verb("users", "Users", [{ kind: "flag", name: "as_json", required: false }])],
};

const BLOCK = (path) => ({ pane: 2, kind: "integration", source: "gam", path, source_label: "GAM7" });

/** Open the page with a GAM7 block on `path` whose reads are answered with `runs` (verb id to answer file). */
const open = (page, url, path, runs, extra = {}) =>
  openBlock(page, url, { tools: [GAM], blocks: [BLOCK(path)], runs, ...extra });

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  const cases = [
    the_users_golden_is_drawn,
    the_section_chip_switches_for_this_pane_and_has_no_make_default,
    the_json_and_domain_views_are_drawn,
    large_and_hostile_documents_are_drawn_inert,
    an_overflow_is_drawn_as_a_fault,
    the_palette_opens_a_users_block,
  ];
  for (const run of cases) {
    try {
      await run(page, url);
    } catch (e) {
      throw new Error(`${run.name}: ${e instanceof Error ? e.message : e}`, { cause: e });
    }
  }
  assert.deepEqual(errors, [], "the page reported errors");
}

async function the_users_golden_is_drawn(page, url) {
  const users = golden("gam_users.json");
  await open(page, url, "users", { users });
  await page.waitForSelector(".block-integration .ig-table tbody tr");

  // The header says what the pane reads and from where: the tool and the section the core resolved.
  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="2"] .ph-ctx')?.textContent === "GAM7 · work");

  // The columns are the golden's, in its order, and there is one row per user.
  const head = await page.locator(".block-integration .ig-table thead th").allTextContents();
  assert.deepEqual(head, users.view.columns.map((c) => c.label), "the columns of the core's answer");
  const rows = await page.locator(".block-integration .ig-table tbody tr").evaluateAll((trs) => trs.map((tr) => [...tr.querySelectorAll("td")].map((td) => td.textContent)));
  assert.deepEqual(rows, users.view.rows.map((r) => r.cells), "every cell is the core's, row for row");
  assert.equal(rows.length, 5);

  // The multi-line cell of the directory is one cell on one line: no line break reaches the page.
  assert.equal(rows[1][4], "Line one, with a comma line two with \"quotes\"");
  assert.ok(rows.every((r) => r.every((c) => !c.includes("\n"))), "no cell holds a line break");
  // The suspended user is dim, as the core said.
  assert.equal(await page.locator(".block-integration tr.ig-row").nth(2).getAttribute("data-tone"), "dim");

  // The core was asked for the one view of this pane and nothing else.
  const calls = await page.evaluate(() => window.__MOCK__.callsOf("integration_run"));
  assert.deepEqual(calls[0], { tool: "gam", verb: "users", pane: 2, args: {} });
}

async function the_section_chip_switches_for_this_pane_and_has_no_make_default(page, url) {
  const users = golden("gam_users.json");
  const sections = golden("gam_sections.json");
  await open(page, url, "users", { users, sections });
  await page.waitForSelector(".block-integration .ig-table tbody tr");
  const chip = page.locator('.block-integration .ig-chip[data-select="section"]');
  assert.equal(await chip.textContent(), "section work", "the chip says the section the core resolved for this pane");
  assert.equal(await page.locator(".block-integration .ig-chip").count(), 1, "GAM7 selects a section and nothing else");

  // The list under the chip is the core's own reading of the sections, with the selected one marked.
  await chip.click();
  await page.waitForSelector(".block-integration .ig-list .ig-opt");
  const labels = await page.locator(".block-integration .ig-list .ig-opt-label").allTextContents();
  assert.deepEqual(labels, sections.view.rows.map((r) => r.key), "one entry for each section of the core's answer");
  assert.deepEqual(
    await page.locator(".block-integration .ig-list .ig-opt").evaluateAll((opts) => opts.map((o) => o.getAttribute("aria-selected"))),
    ["false", "false", "true"],
    "the selected section is marked",
  );
  const reads = await page.evaluate(() => window.__MOCK__.callsOf("integration_run"));
  assert.deepEqual(reads.at(-1), { tool: "gam", verb: "sections", pane: 2, args: {} }, "the list is a read the core makes for this pane");
  // GAM7 has no Local verb: there is no way to make a section the tool's default, so the entry is not drawn.
  assert.equal(await page.locator(".block-integration .ig-opt-default").count(), 0);
  assert.ok(!(await page.locator(".block-integration .ig-list").textContent()).includes("Make default"), "no Make default entry");

  // A choice goes to the core for this pane only, and the view is read again under the new section.
  await page.locator(".block-integration .ig-list .ig-opt", { hasText: "lab" }).click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_select").length === 1);
  const picked = (await page.evaluate(() => window.__MOCK__.callsOf("integration_select")))[0];
  assert.equal(picked.tool, "gam");
  assert.equal(picked.pane, 2, "for this pane");
  assert.equal(picked.selection.section, "lab");
  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="2"] .ph-ctx')?.textContent === "GAM7 \u00b7 lab");
  assert.equal(await chip.textContent(), "section lab");
  assert.equal((await page.evaluate(() => window.__MOCK__.callsOf("integration_prepare"))).length, 0, "nothing asked for a card: a section is not saved anywhere");
  assert.equal((await page.evaluate(() => window.__MOCK__.callsOf("integration_launch"))).length, 0);
}

async function the_json_and_domain_views_are_drawn(page, url) {
  // The users as JSON: one preformatted text, exactly the core's re-serialised and bounded document, with its note.
  const asJson = golden("gam_users_json.json");
  await open(page, url, "users", { users: asJson });
  await page.waitForSelector(".block-integration pre.ig-text");
  assert.equal(await page.locator(".block-integration pre.ig-text").textContent(), asJson.view.text);
  assert.equal(await page.locator(".block-integration .ig-table").count(), 0, "a text is not a table");
  assert.equal(await page.locator(".block-integration .ig-note").textContent(), asJson.view.note);

  // The domain: label and value rows from the members the core knows; the dropped members are only counted.
  const domain = golden("gam_domain.json");
  await open(page, url, "domain", { domain });
  await page.waitForSelector(".block-integration .ig-kv .ig-kvrow");
  const rows = await page.locator(".block-integration .ig-kvrow").evaluateAll((lines) => lines.map((l) => [l.querySelector(".ig-k").textContent, l.querySelector(".ig-v").textContent, l.getAttribute("data-tone")]));
  assert.deepEqual(rows, domain.view.rows.map((r) => [r.label, r.value, r.tone === "normal" ? null : r.tone]));
  assert.equal(await page.locator(".block-integration .ig-note").textContent(), "2 other members are not shown.");
  assert.equal(await page.locator(".block-integration .ig-act").count(), 0, "a domain has no action");
  const shown = await page.locator(".block-integration").textContent();
  assert.ok(!shown.includes("SECRET-VALUE") && !shown.includes("TOKEN-VALUE"), "a member the core dropped is nowhere on the page");
}

async function large_and_hostile_documents_are_drawn_inert(page, url) {
  // 3000 rows were read; the core kept 2000 and said so, and the page draws the cap it was given and its two sentences.
  const truncated = golden("gam_users_truncated.json");
  await open(page, url, "users", { users: truncated });
  await page.waitForFunction(() => document.querySelectorAll(".block-integration .ig-table tbody tr").length === 2000);
  assert.equal(await page.locator(".block-integration .ig-table tbody tr").count(), truncated.view.rows.length);
  assert.equal(await page.locator(".block-integration .ig-table tbody tr:last-child td:first-child").textContent(), truncated.view.rows.at(-1).key);
  assert.equal(await page.locator('.block-integration .ig-note[data-kind="cut"]').textContent(), truncated.view.truncated.note);
  assert.equal(await page.locator(".block-integration .ig-note:not([data-kind])").textContent(), truncated.view.note);

  // The hostile directory: every character of it is text. Nothing hidden, nothing that moves text around, no element.
  const hostile = golden("gam_users_multiline.json");
  await open(page, url, "users", { users: hostile });
  await page.waitForSelector(".block-integration .ig-table tbody tr");
  const cells = await page.locator(".block-integration .ig-table td").evaluateAll((tds) => tds.map((td) => [td.textContent, td.getAttribute("title")]));
  assert.equal(cells.length, hostile.view.rows.length * hostile.view.columns.length);
  const unclean = /[\p{Cc}\p{Cf}\p{Zl}\p{Zp}]/u;
  const everything = await page.locator(".block-integration").textContent();
  assert.ok(!unclean.test(everything), "no control, format, direction or zero-width character is in the block's text");
  for (const [text] of cells) assert.ok([...text].length <= 240, `a cell is at most 240 characters, not ${[...text].length}`);
  // The cell of 5000 characters was cut by the core at 4096 bytes and is shown cut again at 240, the whole of it in the title.
  const big = hostile.view.rows.find((r) => r.key === "big@example.com").cells[1];
  assert.ok(big.length > 4000 && big.length <= 4096, `the core's cell is ${big.length} characters`);
  assert.deepEqual(cells.find(([, title]) => title === big)?.[0].length, 240, "shown at 240, the title holds the core's cell");
  // A name that looks like an element is those characters, and nothing was made from it.
  assert.ok(cells.some(([text]) => text === "<img src=x onerror=alert(1)>"));
  assert.equal(await page.locator(".block-integration img, .block-integration [onerror], .block-integration script").count(), 0);
  // The columns the core made unique are drawn as they came.
  assert.deepEqual(await page.locator(".block-integration .ig-table thead th").allTextContents(), hostile.view.columns.map((c) => c.label));
}

/** The fault of a run that printed more than is read, as the core sends it (core/tests/integrate_e2e_gam.rs holds the same text). */
const OVERFLOW = { code: "overflow", reason: "overflow", text: "The answer of GAM7 is larger than is read." };

async function an_overflow_is_drawn_as_a_fault(page, url) {
  await open(page, url, "users", {}, { faults: { users: OVERFLOW } });
  await page.waitForFunction((text) => document.querySelector(".block-integration .ig-status")?.textContent === text, OVERFLOW.text);
  const status = page.locator(".block-integration .ig-status");
  assert.equal(await status.getAttribute("data-tone"), "warn");
  // The mark `!` is drawn before the sentence besides the colour and the underline.
  assert.equal(await status.evaluate((n) => getComputedStyle(n, "::before").content), '"! "');
  assert.equal(await page.locator(".block-integration .ig-table, .block-integration .ig-row, .block-integration pre.ig-text").count(), 0, "no rows");
}

async function the_palette_opens_a_users_block(page, url) {
  // GAM7 is reached from the command palette like the other tools: its views are entries, and the block reads from the core.
  const users = golden("gam_users.json");
  const BLOCK_USERS = { pane: 2, kind: "integration", source: "gam", path: "users", source_label: "GAM7" };
  await openContext(page, url, { sidebar: false, rows: [], tools: [GAM], runs: { users } }, layout(1, [1]));
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_tools").length >= 1);
  await page.evaluate(async () => {
    window.__registry = await import(new URL("actions.js", location.href).href);
  });
  await page.waitForFunction(() => window.__registry.listActions().some((a) => a.id === "integration.open.gam.users"));
  assert.deepEqual(
    await page.evaluate(() => window.__registry.listActions().filter((a) => a.id.startsWith("integration.")).map((a) => [a.id, a.title]).filter(([id]) => id.startsWith("integration.open."))),
    [["integration.open.gam.sections", "GAM7: Sections"], ["integration.open.gam.domain", "GAM7: Domain"], ["integration.open.gam.users", "GAM7: Users"]],
    "one entry for each view, with the core's labels",
  );
  assert.equal(await page.evaluate(() => window.__registry.listActions().some((a) => a.id.startsWith("integration.run.gam."))), false, "GAM7 has no terminal verb");
  // The mock core opens the block the way the real one does: a second pane that the block list names.
  await page.evaluate((block) => {
    window.__MOCK__.on("block_open", () => {
      window.__CX__.blocks = [block];
      window.__MOCK__.setView({
        workspaces: [{ name: "main", active: true, panes: 2 }],
        tabs: [{ title: "users", active: true, focus: 2, panes: 2 }],
        focus: 2,
        panes: [[1, { x: 0, y: 0, w: 0.5, h: 1 }], [2, { x: 0.5, y: 0, w: 0.5, h: 1 }]],
        dividers: [{ split: 10, axis: "Row", area: { x: 0, y: 0, w: 1, h: 1 }, ratio: 0.5 }],
        pane_names: [],
        windows: 1,
      });
      return window.__MOCK__.view();
    });
  }, BLOCK_USERS);
  await page.evaluate(() => document.getElementById("term").focus());
  await page.waitForFunction(() => document.activeElement?.id === "ime");
  await page.keyboard.press("Meta+k");
  await page.waitForFunction(() => document.activeElement?.classList.contains("palette-input") === true);
  await page.keyboard.type("GAM7: Users");
  await page.keyboard.press("Enter");
  await page.waitForSelector(".block-integration .ig-table tbody tr");
  assert.deepEqual((await page.evaluate(() => window.__MOCK__.callsOf("block_open")))[0], { kind: "integration", source: "gam", path: "users", placement: "tab" });
  assert.equal(await page.locator(".block-integration .ig-table tbody tr").count(), users.view.rows.length);
}
