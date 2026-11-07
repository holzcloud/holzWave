// The guard between the page and the spec: every string of the vault's page (vaultstate.ts `STRINGS`) is in section 6.6 of
// docs/design.md word for word, and every text that the spec's rules put in backticks is a string of the page. A word changed on
// one side only fails here, and the message names it.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { STRINGS } from "../../dist/vaultstate.js";

const spec = readFileSync(new URL("../../../docs/design.md", import.meta.url), "utf8");
const start = spec.indexOf("### 6.6 Mobile");
const end = spec.indexOf("### 6.7");
const section = spec.slice(start, end);

/** The paragraph of the rule `name` (they open with the bold name and a full stop). */
function rule(name) {
  const at = section.indexOf(`**${name}.**`);
  assert.ok(at >= 0, `the spec has no rule named ${name}`);
  const stop = section.indexOf("\n\n", at);
  return section.slice(at, stop < 0 ? undefined : stop);
}

/** Every text in backticks in `paragraph`. */
const quoted = (paragraph) => [...paragraph.matchAll(/`([^`]+)`/g)].map((m) => m[1]);

const values = new Set(Object.values(STRINGS));
const RULES = ["Lock chip", "Unlock sheet", "Locked answers", "Sync while locked", "No screen lock", "Preferences row", "Vault lost"];
/** Backticked words of a rule that are not texts of the vault's page: a counter-example, the names of style hooks and the two buttons of the
 *  sync popover that already existed (sync.ts owns them; the rule only says that they stay available while the vault is locked). */
const NOT_TEXT = new Set(["☁ sync error", "contrast-high", "forced-colors", ".label", "Close", "Sync now"]);

test("section 6.6 exists with the rules the page builds", () => {
  assert.ok(start >= 0 && end > start, "docs/design.md has no section 6.6");
  for (const name of RULES) rule(name);
});

test("every string of the page is in the spec word for word", () => {
  for (const [key, text] of Object.entries(STRINGS)) {
    assert.ok(section.includes(text), `the page string ${key} (${JSON.stringify(text)}) is not in section 6.6 of docs/design.md`);
  }
});

test("every text the spec's rules name is a string of the page", () => {
  for (const name of RULES) {
    for (const text of quoted(rule(name))) {
      if (NOT_TEXT.has(text)) continue;
      assert.ok(values.has(text), `the spec text ${JSON.stringify(text)} of the rule "${name}" is not a string of the page (vaultstate.ts STRINGS)`);
    }
  }
});

test("the guard is not empty: it checks dozens of strings", () => {
  assert.ok(Object.keys(STRINGS).length >= 30);
  assert.ok(RULES.flatMap((name) => quoted(rule(name))).length >= 30);
});
