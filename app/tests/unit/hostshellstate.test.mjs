// The pure half of the group `Host shell` (hostshellstate.ts): how the answer of the commands is read and which words the
// page has for each state. No DOM is needed. The words are held to the design record (docs/design.md 6.32) here.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { GROUP, STATE_WORDS, describeHostShell, readHostShell } from "../../dist/hostshellstate.js";

const golden = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));
const spec = readFileSync(new URL("../../../docs/design.md", import.meta.url), "utf8");

/** The hostile word of these tests; it appears in test code only. */
const CANARY = "zq-host-shell-3c9e";

test("every sample of the golden is read as the shell wrote it", () => {
  const samples = golden.answers.host_shell;
  assert.equal(samples.length, 4, "the view outside a Flatpak and one view per state word inside it");
  for (const sample of samples) {
    assert.deepEqual(Object.keys(sample).sort(), ["applies", "state"]);
    const view = readHostShell(sample);
    assert.deepEqual(view, { applies: sample.applies, state: sample.state }, JSON.stringify(sample));
    assert.equal(Object.getPrototypeOf(view), Object.prototype);
  }
  assert.deepEqual(
    samples.filter((s) => s.applies).map((s) => s.state).sort(),
    ["off", "on", "unavailable"],
  );
  assert.equal(samples.filter((s) => !s.applies).length, 1);
});

test("the table has exactly the three keys of the shell and every row is complete", () => {
  assert.deepEqual(Object.keys(STATE_WORDS).sort(), ["off", "on", "unavailable"]);
  for (const [key, row] of Object.entries(STATE_WORDS)) {
    assert.deepEqual(
      Object.keys(row).sort(),
      ["advice", "checked", "enabled", "mark", "tone", "underline", "word"],
      key,
    );
    assert.ok(row.word.length > 0 && row.advice.length > 0, key);
  }
  assert.deepEqual(
    Object.entries(STATE_WORDS).map(([key, row]) => [key, row.mark, row.tone, row.enabled, row.checked]),
    [
      ["off", "", "plain", true, false],
      ["on", "!", "warn", true, true],
      ["unavailable", "?", "plain", false, false],
    ],
  );
  assert.equal(STATE_WORDS.on.underline, true, "`on` is also underlined");
  assert.equal(STATE_WORDS.off.underline, false);
});

test("hostile input is never kept", () => {
  // Not an object, or no boolean `applies`, or no string `state`: null.
  for (const bad of [null, undefined, [], [{ applies: true }], "applies", 7, true, {}, { applies: true }, { state: "off" },
    { applies: 1, state: "off" }, { applies: "true", state: "off" }, { applies: true, state: 3 }, { applies: true, state: null }]) {
    assert.equal(readHostShell(bad), null, JSON.stringify(bad));
  }
  // A word that is not exactly one of the three reads as `unavailable`.
  for (const word of ["", "ON", "On", "on ", " off", "enabled", "true", `<img src=x onerror=${CANARY}>`, "__proto__", "toString"]) {
    assert.deepEqual(readHostShell({ applies: true, state: word }), { applies: true, state: "unavailable" }, word);
  }
  // Only the two members are read: an extra member and an own `__proto__` never reach the result.
  const extra = JSON.parse(`{"applies":true,"state":"off","__proto__":{"state":"on"},"${CANARY}":1}`);
  const view = readHostShell(extra);
  assert.deepEqual(view, { applies: true, state: "off" });
  assert.equal(Object.keys(view).length, 2);
  assert.ok(!JSON.stringify(view).includes(CANARY));
});

test("a state that is in no table is the `unavailable` row, and so is no view at all", () => {
  for (const view of [null, { applies: true, state: "mystery" }, { applies: false, state: "unavailable" }]) {
    const row = describeHostShell(view);
    assert.equal(row.state, "unavailable");
    assert.equal(row.enabled, false);
    assert.equal(row.mark, "?");
  }
  for (const state of ["off", "on", "unavailable"]) {
    const row = describeHostShell({ applies: true, state });
    assert.equal(row.state, state);
    assert.equal(row.word, STATE_WORDS[state].word);
  }
});

// The sentences below are the design record's (docs/design.md 6.32). The page may not say a word the spec does not say.
test("every word and sentence of the page is in the design record", () => {
  const section = spec.slice(spec.indexOf("### 6.32 Host shell in a Flatpak"));
  assert.ok(section.length > 1000, "section 6.32 is found");
  assert.equal(GROUP.legend, "Host shell");
  assert.equal(GROUP.name, "Run panes on the host");
  assert.equal(GROUP.meta, "Flatpak");
  assert.ok(section.includes("The group `Host shell`."), "the legend");
  assert.ok(section.includes("the name `Run panes on the host`"), "the name of the row");
  assert.ok(section.includes("the dim meta `Flatpak`"), "the meta");
  for (const [key, row] of Object.entries(STATE_WORDS)) {
    // The table row of the spec: `| `key` | word | ... | advice |`.
    const line = section.split("\n").find((l) => l.startsWith(`| \`${key}\` |`));
    assert.ok(line, `the spec has a row for ${key}`);
    const cells = line.split("|").map((c) => c.trim());
    assert.equal(cells[2], row.word, `${key}: the state word`);
    assert.equal(cells[4], row.advice, `${key}: the advice`);
    // The second cues of the row are named in the spec, so the page carries them.
    if (row.mark !== "") assert.ok(cells[3].includes(`the mark \`${row.mark}\``), `${key}: the mark`);
    if (row.underline) assert.ok(cells[3].includes("an underline"), `${key}: the underline`);
    if (!row.enabled) assert.ok(cells[3].includes("disabled checkbox"), `${key}: the disabled box`);
    if (row.checked) assert.ok(cells[3].includes("tick"), `${key}: the tick`);
  }
});
