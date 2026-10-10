// The TypeScript side of the protocol golden (tests/fixtures/protocol.json, written by the Rust test
// `protocol_samples_match_golden`): the exact key sets and tags the page depends on, and a scan of every call site in
// src/ against the argument names of the Rust commands. Nothing is imported from the app: the golden and the
// TypeScript sources are read as text.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const golden = JSON.parse(readFileSync(join(here, "../fixtures/protocol.json"), "utf8"));
const srcDir = join(here, "../../src");

const keys = (object) => Object.keys(object).sort();

// ---- payload shapes ---------------------------------------------------------------------------------------------

test("the view has exactly the documented keys at every level", () => {
  const view = golden.view;
  assert.deepEqual(keys(view), ["dividers", "focus", "panes", "tabs", "workspaces"]);
  assert.equal(view.workspaces.length, 2);
  assert.equal(view.tabs.length, 3);
  assert.equal(view.panes.length, 2);
  assert.equal(view.dividers.length, 1);
  for (const w of view.workspaces) assert.deepEqual(keys(w), ["active", "name"]);
  for (const t of view.tabs) assert.deepEqual(keys(t), ["active", "focus", "title"]);
  for (const pane of view.panes) {
    assert.equal(pane.length, 2, "a pane is [id, rect]");
    assert.equal(typeof pane[0], "number");
    assert.deepEqual(keys(pane[1]), ["h", "w", "x", "y"]);
  }
  for (const d of view.dividers) {
    assert.deepEqual(keys(d), ["area", "axis", "ratio", "split"]);
    assert.deepEqual(keys(d.area), ["h", "w", "x", "y"]);
    assert.ok(d.axis === "Row" || d.axis === "Column", d.axis);
  }
  assert.equal(view.tabs.filter((t) => t.active).length, 1);
  assert.equal(view.workspaces.filter((w) => w.active).length, 1);
});

test("the event names and the title, cwd and pane-exit payloads are pinned", () => {
  assert.deepEqual(golden.events.names, ["frame", "layout", "title", "cwd", "pane-exit"]);
  assert.deepEqual(keys(golden.events.title), ["pane", "title"]);
  assert.deepEqual(keys(golden.events.cwd), ["cwd", "pane"]);
  const exits = golden.events["pane-exit"];
  assert.equal(exits.length, 2, "one sample with a code and one without");
  for (const e of exits) assert.deepEqual(keys(e), ["code", "pane"]);
  assert.equal(typeof exits[0].code, "number");
  assert.equal(exits[1].code, null);
});

test("the op tags, modes and motions are the exact strings the page sends", () => {
  const select = golden.ops.select;
  assert.deepEqual([...new Set(select.map((o) => o.kind))].sort(), ["all", "begin", "clear", "extend"]);
  for (const o of select) {
    if (o.kind === "begin") assert.deepEqual(keys(o), ["kind", "mode", "x", "y"]);
    if (o.kind === "extend") assert.deepEqual(keys(o), ["kind", "x", "y"]);
    if (o.kind === "all" || o.kind === "clear") assert.deepEqual(keys(o), ["kind"]);
  }
  assert.deepEqual(
    select.filter((o) => o.kind === "begin").map((o) => o.mode).sort(),
    ["cell", "line", "word"],
  );

  const search = golden.ops.search;
  assert.deepEqual([...new Set(search.map((o) => o.kind))].sort(), ["clear", "next", "prev", "set"]);
  for (const o of search) assert.deepEqual(keys(o), o.kind === "set" ? ["kind", "needle"] : ["kind"]);

  const copy = golden.ops.copy_mode;
  assert.deepEqual([...new Set(copy.map((o) => o.kind))].sort(), ["enter", "exit", "move", "toggle", "yank"]);
  for (const o of copy) assert.deepEqual(keys(o), o.kind === "move" ? ["kind", "motion"] : ["kind"]);
  assert.deepEqual(
    copy.filter((o) => o.kind === "move").map((o) => o.motion).sort(),
    [
      "bottom", "down", "left", "line_end", "line_start", "page_down", "page_up",
      "right", "top", "up", "word_back", "word_forward",
    ],
  );
});

test("the paste outcomes and the search and copy-mode answers are pinned", () => {
  assert.deepEqual(golden.paste, ["written", "empty", "rejected"]);
  for (const a of golden.answers.search) assert.deepEqual(keys(a), ["current", "total"]);
  for (const a of golden.answers.copy_mode) assert.deepEqual(keys(a), ["active", "selecting", "yanked"]);
});

test("the command table has snake_case argument names and no raw input command", () => {
  const names = Object.keys(golden.commands);
  assert.ok(names.length >= 20, `only ${names.length} commands`);
  assert.ok(!names.includes("input"), "the raw input command must not exist");
  for (const [name, args] of Object.entries(golden.commands)) {
    for (const arg of args) assert.match(arg, /^[a-z][a-z0-9]*(_[a-z0-9]+)*$/, `${name}: ${arg}`);
  }
  assert.deepEqual(golden.commands.paste, ["pane", "text", "allow_unsafe"]);
  assert.deepEqual(golden.commands.start_pane, ["pane", "cols", "rows", "cell_w", "cell_h"]);
});

// ---- call-site scanner --------------------------------------------------------------------------------------------

/** The index just past the string or comment that starts at `i`, or `i` when none starts there. */
function skipLiteral(text, i) {
  const c = text[i];
  if (c === '"' || c === "'") {
    let j = i + 1;
    while (j < text.length && text[j] !== c) j += text[j] === "\\" ? 2 : 1;
    return j + 1;
  }
  if (c === "`") {
    let j = i + 1;
    while (j < text.length && text[j] !== "`") {
      if (text[j] === "\\") j += 2;
      else if (text[j] === "$" && text[j + 1] === "{") j = skipBraces(text, j + 1);
      else j += 1;
    }
    return j + 1;
  }
  if (c === "/" && text[i + 1] === "/") {
    const end = text.indexOf("\n", i);
    return end < 0 ? text.length : end;
  }
  if (c === "/" && text[i + 1] === "*") {
    const end = text.indexOf("*/", i + 2);
    return end < 0 ? text.length : end + 2;
  }
  return i;
}

/** The index just past the `}` that closes the `{` at `open`. */
function skipBraces(text, open) {
  let depth = 0;
  let i = open;
  while (i < text.length) {
    const next = skipLiteral(text, i);
    if (next !== i) {
      i = next;
      continue;
    }
    if (text[i] === "{") depth += 1;
    if (text[i] === "}") {
      depth -= 1;
      if (depth === 0) return i + 1;
    }
    i += 1;
  }
  throw new Error("unbalanced braces");
}

/** `text` without its comments; strings and templates are kept as they are. */
function stripComments(text) {
  let out = "";
  let i = 0;
  while (i < text.length) {
    const next = skipLiteral(text, i);
    const isComment = text[i] === "/" && (text[i + 1] === "/" || text[i + 1] === "*");
    if (next === i) {
      out += text[i];
      i += 1;
      continue;
    }
    if (!isComment) out += text.slice(i, next);
    i = next;
  }
  return out;
}

/** The top-level keys of the object literal whose `{` is at `open`. Spreads and computed keys are reported as such. */
function objectKeys(text, open) {
  const end = skipBraces(text, open) - 1;
  const found = [];
  let depth = 0;
  let entryStart = open + 1;
  const flush = (to) => {
    const entry = stripComments(text.slice(entryStart, to)).trim();
    if (entry === "") return;
    if (entry.startsWith("...")) return void found.push("...spread");
    if (entry.startsWith("[")) return void found.push("[computed]");
    const m = /^(?:"([^"]+)"|'([^']+)'|([A-Za-z_$][\w$]*))\s*(?::|$|\()/.exec(entry);
    found.push(m ? (m[1] ?? m[2] ?? m[3]) : `[unreadable: ${entry.slice(0, 20)}]`);
  };
  let i = open + 1;
  while (i < end) {
    const next = skipLiteral(text, i);
    if (next !== i) {
      i = next;
      continue;
    }
    const c = text[i];
    if (c === "{" || c === "[" || c === "(") depth += 1;
    else if (c === "}" || c === "]" || c === ")") depth -= 1;
    else if (c === "," && depth === 0) {
      flush(i);
      entryStart = i + 1;
    }
    i += 1;
  }
  flush(end);
  return found;
}

const CALL = /\b(?:call|invoke)\s*(?:<[^()]*>)?\s*\(\s*(["'`])([A-Za-z0-9_]+)\1\s*(?:,\s*(\{))?/g;

/** Every `call("name", {...})`, `call<T>("name", {...})` and `invoke("name", {...})` of a source text. */
function callSites(text) {
  const sites = [];
  for (const m of text.matchAll(CALL)) {
    const braceAt = m[3] ? m.index + m[0].length - 1 : -1;
    sites.push({
      command: m[2],
      keys: braceAt >= 0 ? objectKeys(text, braceAt) : [],
      line: text.slice(0, m.index).split("\n").length,
    });
  }
  return sites;
}

/** The sites of `text` that pass an argument the Rust command does not have. Unknown commands are skipped. */
function violations(file, text, commands) {
  const bad = [];
  for (const site of callSites(text)) {
    const args = commands[site.command];
    if (!args) continue;
    for (const key of site.keys) {
      if (!args.includes(key)) bad.push(`${file}:${site.line}: ${site.command} has no argument "${key}" (${args.join(", ")})`);
    }
  }
  return bad;
}

function sources(dir) {
  const out = [];
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) out.push(...sources(path));
    else if (name.endsWith(".ts")) out.push(path);
  }
  return out;
}

test("the scanner reads nested braces, strings, templates and generics", () => {
  const text = [
    'call("a", { pane: 1, text: `x}${f({ q: 1 })}y`, allow_unsafe: g({ z: 2 }, [1, 2]) });',
    'call<Record<string, number>>("b", { pane, "cols": 1, \'rows\': 2 });',
    'void invoke("c");',
    'call("d", { ...rest, [k]: 1, // trailing, comment\n  last: 1 });',
  ].join("\n");

  const sites = callSites(text);

  assert.deepEqual(sites.map((s) => s.command), ["a", "b", "c", "d"]);
  assert.deepEqual(sites[0].keys, ["pane", "text", "allow_unsafe"]);
  assert.deepEqual(sites[1].keys, ["pane", "cols", "rows"]);
  assert.deepEqual(sites[2].keys, []);
  assert.deepEqual(sites[3].keys, ["...spread", "[computed]", "last"]);
});

test("every call site in src/ uses only argument names of its Rust command", () => {
  let checked = 0;
  const bad = [];
  for (const file of sources(srcDir)) {
    const text = readFileSync(file, "utf8");
    checked += callSites(text).filter((s) => golden.commands[s.command]).length;
    bad.push(...violations(file.slice(srcDir.length + 1), text, golden.commands));
  }
  assert.ok(checked >= 10, `only ${checked} call sites of known commands were found: is the scanner broken?`);
  assert.deepEqual(bad, []);
});

test("a wrong key in a copy of a real call site is reported", () => {
  const text = readFileSync(join(srcDir, "overlay.ts"), "utf8");
  assert.ok(text.includes('call("set_ratio", {'), "overlay.ts no longer calls set_ratio the way this test expects");
  assert.deepEqual(violations("overlay.ts", text, golden.commands), []);

  const broken = text.replace('call("set_ratio", {', 'call("set_ratio", { splitId: 1,');

  const bad = violations("overlay.ts", broken, golden.commands);
  assert.equal(bad.length, 1, bad.join("\n"));
  assert.match(bad[0], /set_ratio has no argument "splitId"/);
});

test("a camelCase spelling of a snake_case argument is reported", () => {
  const bad = violations("x.ts", 'call("paste", { pane: 1, text: "a", allowUnsafe: true });', golden.commands);
  assert.equal(bad.length, 1);
  assert.match(bad[0], /"allowUnsafe"/);
});
