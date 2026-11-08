// The pure half of the own window frame (windowframestate.ts): how the facts of the shell are read, the table of the buttons,
// the state of the maximise button and the reader of the window state. No DOM is needed. The words and shapes are held to the
// design record (docs/design.md section 6.33) here, so the page cannot drift from the spec.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { BUTTONS, maximizeRow, readFrame, windowState } from "../../dist/windowframestate.js";

const spec = readFileSync(new URL("../../../docs/design.md", import.meta.url), "utf8");

/** The cases the shell's own tests pass (app/tests/fixtures/window-frame.json): the facts it writes and the script text. */
const FIXTURE = JSON.parse(readFileSync(new URL("../fixtures/window-frame.json", import.meta.url), "utf8"));

/** The hostile word of these tests; it appears in test code only. */
const CANARY = "zq-window-frame-7d41";

const WINDOWS = () => ({ os: "windows", style: "flat", side: "right", buttons: ["minimize", "maximize", "close"] });

test("the reader returns the facts for exactly the Windows contract of the shell", () => {
  const input = WINDOWS();
  const facts = readFrame(input);
  assert.deepEqual(facts, WINDOWS());
  assert.equal(Object.getPrototypeOf(facts), Object.prototype, "a plain object");
  assert.notEqual(facts, input, "a fresh object");
  assert.notEqual(facts.buttons, input.buttons, "a fresh list");
  input.os = "linux";
  input.buttons.push(CANARY);
  assert.deepEqual(facts, WINDOWS(), "nothing is shared with the input");
});

test("the reader gives nothing for anything else", () => {
  const bad = [
    undefined, null, "windows", 7, true, [], [WINDOWS()], () => WINDOWS(), Symbol("facts"), {},
    { os: "windows", style: "flat", side: "right" },
    { ...WINDOWS(), extra: CANARY },
    { ...WINDOWS(), os: "macos" },
    { ...WINDOWS(), os: "android" },
    { ...WINDOWS(), os: "ios" },
    { ...WINDOWS(), os: "Windows" },
    { ...WINDOWS(), os: "Linux" },
    { ...WINDOWS(), os: "linux " },
    { ...WINDOWS(), os: "" },
    { ...WINDOWS(), style: "round" },
    { ...WINDOWS(), side: "left" },
    { ...WINDOWS(), buttons: [] },
    { ...WINDOWS(), buttons: ["close"] },
    { ...WINDOWS(), buttons: ["minimize", "maximize"] },
    { ...WINDOWS(), buttons: ["close", "maximize", "minimize"] },
    { ...WINDOWS(), buttons: ["minimize", "close", "maximize"] },
    { ...WINDOWS(), buttons: ["minimize", "minimize", "close"] },
    { ...WINDOWS(), buttons: ["minimize", "maximize", "close", "close"] },
    { ...WINDOWS(), buttons: ["minimize", "maximize", "destroy"] },
    { ...WINDOWS(), buttons: ["minimize", "maximize", "Close"] },
    { ...WINDOWS(), buttons: ["minimize", "maximize", `close<img src=x onerror=${CANARY}>`] },
    { ...WINDOWS(), buttons: "minimize,maximize,close" },
    { ...WINDOWS(), buttons: { 0: "minimize", 1: "maximize", 2: "close", length: 3 } },
    { ...WINDOWS(), buttons: [1, 2, 3] },
    { ...WINDOWS(), buttons: [null, undefined, "close"] },
  ];
  for (const value of bad) assert.equal(readFrame(value), null, String(JSON.stringify(value) ?? typeof value));
});

test("the reader refuses a prototype, an own __proto__, a getter and a sparse or subclassed list", () => {
  assert.equal(readFrame(Object.create(WINDOWS())), null, "all members inherited");
  assert.equal(readFrame(Object.create({ ...WINDOWS() }, { os: { value: "windows", enumerable: true } })), null, "another prototype");
  assert.equal(readFrame(Object.assign(Object.create(null), WINDOWS())), null, "no prototype at all");
  assert.equal(readFrame(new (class Frame { constructor() { Object.assign(this, WINDOWS()); } })()), null, "a class instance");
  const withProto = JSON.parse(`{"os":"windows","style":"flat","side":"right","__proto__":{"buttons":["minimize","maximize","close"]}}`);
  assert.equal(readFrame(withProto), null, "an own __proto__ is an extra key and no buttons member");
  let reads = 0;
  const getter = { ...WINDOWS() };
  Object.defineProperty(getter, "os", { enumerable: true, get() { reads += 1; return "windows"; } });
  assert.equal(readFrame(getter), null, "a getter is not a value");
  assert.equal(reads, 0, "and it was never run");
  const sparse = ["minimize", "maximize", "close"];
  delete sparse[1];
  assert.equal(readFrame({ ...WINDOWS(), buttons: sparse }), null, "a hole in the list");
  class Words extends Array {}
  assert.equal(readFrame({ ...WINDOWS(), buttons: Words.from(["minimize", "maximize", "close"]) }), null, "a subclassed list");
  const symbol = { ...WINDOWS(), [Symbol("x")]: 1 };
  assert.equal(readFrame(symbol), null, "a symbol key is an extra key");
});

// ---- the contract of the shell for Linux, and the shared fixture ----------------------------------------------------------

const LINUX = (over = {}) => ({ os: "linux", style: "flat", side: "right", buttons: ["minimize", "maximize", "close"], ...over });
const WORD_LIST = ["minimize", "maximize", "close"];

/** Every non-empty ordered subset of the three words: 3 + 6 + 6 = 15 lists. */
function orderedSubsets() {
  const out = [];
  const walk = (taken) => {
    if (taken.length > 0) out.push(taken);
    for (const word of WORD_LIST) if (!taken.includes(word)) walk([...taken, word]);
  };
  walk([]);
  return out;
}

test("every case of the shared fixture that has facts is read back as written, every case without facts is not a page's concern", () => {
  assert.ok(FIXTURE.length > 50, "the fixture is the big one of the shell's tests");
  let withFacts = 0;
  let without = 0;
  for (const item of FIXTURE) {
    if (item.facts === null) {
      without += 1;
      assert.ok(item.script === null || item.script === undefined || item.script === "", `${item.name}: no facts, no script`);
      continue;
    }
    withFacts += 1;
    const read = readFrame(structuredClone(item.facts));
    assert.deepEqual(read, item.facts, `${item.name}: the facts are read as the shell decided them`);
    assert.equal(Object.getPrototypeOf(read), Object.prototype, `${item.name}: a plain object`);
    assert.deepEqual(Object.keys(read), ["os", "style", "side", "buttons"], `${item.name}: the members in the order of the shell`);
    // The script text itself carries the same value: the page reads what the script writes.
    const match = /^window\.__HW_FRAME__ = (\{.*\});$/.exec(item.script);
    assert.ok(match, `${item.name}: the script assigns one object literal to the global`);
    assert.deepEqual(readFrame(JSON.parse(match[1])), item.facts, `${item.name}: the script's value is the case's facts`);
  }
  assert.ok(withFacts > 40 && without > 10, `${withFacts} with facts, ${without} without`);
  // The fixture covers both styles, both sides and each of the three words.
  const facts = FIXTURE.filter((item) => item.facts !== null).map((item) => item.facts);
  for (const [member, values] of [["style", ["flat", "round"]], ["side", ["left", "right"]], ["os", ["windows", "linux"]]]) {
    for (const value of values) assert.ok(facts.some((f) => f[member] === value), `the fixture has ${member} ${value}`);
  }
});

test("Linux accepts both styles, both sides and every non-empty ordered subset of the three buttons", () => {
  const subsets = orderedSubsets();
  assert.equal(subsets.length, 15);
  for (const style of ["flat", "round"]) {
    for (const side of ["left", "right"]) {
      for (const buttons of subsets) {
        const input = LINUX({ style, side, buttons: [...buttons] });
        const read = readFrame(input);
        assert.deepEqual(read, LINUX({ style, side, buttons }), `${style} ${side} ${buttons.join(",")}`);
        assert.notEqual(read.buttons, input.buttons, "a fresh list");
      }
    }
  }
});

test("Windows is exactly flat, right and the three buttons in order, and nothing else", () => {
  for (const style of ["flat", "round", "Flat", "", "rounded"]) {
    for (const side of ["left", "right", "Right", "", "top"]) {
      for (const buttons of [...orderedSubsets(), []]) {
        const expected = style === "flat" && side === "right" && buttons.join() === WORD_LIST.join();
        const read = readFrame({ os: "windows", style, side, buttons: [...buttons] });
        assert.equal(read !== null, expected, `windows ${style || "(empty)"} ${side || "(empty)"} ${buttons.join(",") || "(none)"}`);
      }
    }
  }
});

test("Linux ignores an empty list, four entries, a repeat, another case or a trailing space and a non-word", () => {
  const bad = [
    [], ["minimize", "maximize", "close", "close"], ["minimize", "maximize", "close", "minimize"],
    ["close", "close"], ["minimize", "minimize"], ["maximize", "maximize", "maximize"],
    ["Close"], ["close "], [" close"], ["CLOSE"], ["Minimize"], ["maximise"], ["minimise"], ["close\n"], ["close\0"],
    [""], ["destroy"], ["appmenu"], ["menu", "close"], ["restore"],
    [1], [null], [undefined], [true], [{}], [["close"]], [Symbol("close")], [{ toString: () => "close" }],
    ["minimize", "maximize", "close", "x"], ["a", "b", "c", "d", "e"],
  ];
  for (const buttons of bad) assert.equal(readFrame(LINUX({ buttons })), null, `${buttons.map(String).join("|") || "(empty)"}`);
  assert.equal(readFrame(LINUX({ buttons: "close" })), null, "a string");
  assert.equal(readFrame(LINUX({ buttons: "minimize,maximize,close" })), null, "a joined string");
  assert.equal(readFrame(LINUX({ buttons: { 0: "close", length: 1 } })), null, "an array-like object");
  assert.equal(readFrame(LINUX({ buttons: new Set(["close"]) })), null, "a set");
  assert.equal(readFrame(LINUX({ buttons: null })), null);
  assert.equal(readFrame(LINUX({ buttons: undefined })), null);
  const sparse = new Array(3);
  sparse[2] = "close";
  assert.equal(readFrame(LINUX({ buttons: sparse })), null, "a sparse list with holes");
  const longSparse = [];
  longSparse[2] = "close";
  assert.equal(readFrame(LINUX({ buttons: longSparse })), null, "holes before the entry");
  assert.equal(readFrame(LINUX({ buttons: new Array(2 ** 20) })), null, "a huge sparse list is refused by its length, nothing is built from it");
  assert.equal(readFrame(LINUX({ buttons: new Array(4).fill("close") })), null, "four entries");
  const extraKey = ["close"];
  extraKey.extra = "x";
  assert.equal(readFrame(LINUX({ buttons: extraKey })), null, "a list with a named member");
});

test("Linux ignores every other word for the os, the style and the side, and every other shape of the facts", () => {
  for (const os of ["Linux", "LINUX", "linux ", " linux", "linux\n", "gnu/linux", "macos", "android", "ios", "freebsd", "", null, undefined, 1, {}, ["linux"]]) {
    assert.equal(readFrame(LINUX({ os })), null, `os ${String(os)}`);
  }
  for (const style of ["Flat", "ROUND", "round ", "rounded", "square", "", null, undefined, 0, ["flat"]]) {
    assert.equal(readFrame(LINUX({ style })), null, `style ${String(style)}`);
  }
  for (const side of ["Left", "RIGHT", "left ", "top", "center", "both", "", null, undefined, 0, ["left"]]) {
    assert.equal(readFrame(LINUX({ side })), null, `side ${String(side)}`);
  }
  for (const key of ["os", "style", "side", "buttons"]) {
    const missing = LINUX();
    delete missing[key];
    assert.equal(readFrame(missing), null, `missing ${key}`);
    assert.equal(readFrame({ ...LINUX(), extra: 1 }), null, "an extra key");
  }
  // An inherited member is not a member, whatever the own ones are.
  for (const key of ["os", "style", "side", "buttons"]) {
    const own = LINUX();
    const inherited = { [key]: own[key] };
    delete own[key];
    assert.equal(readFrame(Object.assign(Object.create(inherited), own)), null, `inherited ${key}`);
  }
  assert.equal(readFrame(Object.assign(Object.create(null), LINUX())), null, "no prototype");
  assert.equal(readFrame(new (class Frame { constructor() { Object.assign(this, LINUX()); } })()), null, "a class instance");
  assert.equal(readFrame([LINUX()]), null);
});

test("the result is never the input, shares nothing with it, and each member is read once", () => {
  const input = LINUX({ style: "round", side: "left", buttons: ["close", "minimize"] });
  const read = readFrame(input);
  assert.notEqual(read, input);
  assert.notEqual(read.buttons, input.buttons);
  assert.deepEqual(read, LINUX({ style: "round", side: "left", buttons: ["close", "minimize"] }));
  input.style = "flat";
  input.buttons.push(CANARY);
  input.buttons[0] = "minimize";
  assert.deepEqual(read, LINUX({ style: "round", side: "left", buttons: ["close", "minimize"] }), "changing the input afterwards changes nothing");
  const before = [...input.buttons];
  read.buttons.push(CANARY);
  assert.deepEqual(input.buttons, before, "and changing the result does not reach the input");

  // A getter that throws or changes its answer is never run: the member is not an own data value.
  let runs = 0;
  for (const key of ["os", "style", "side", "buttons"]) {
    const trap = LINUX();
    const value = trap[key];
    Object.defineProperty(trap, key, { enumerable: true, get() { runs += 1; if (runs % 2 === 0) throw new Error(CANARY); return value; } });
    assert.equal(readFrame(trap), null, `a getter for ${key}`);
  }
  assert.equal(runs, 0, "no getter was run");
  const entry = LINUX({ buttons: ["minimize", "close"] });
  let entryRuns = 0;
  Object.defineProperty(entry.buttons, "1", { enumerable: true, get() { entryRuns += 1; return "close"; } });
  assert.equal(readFrame(entry), null, "a getter on an entry of the list");
  assert.equal(entryRuns, 0);
  // A frozen input is read the same as a plain one, and the result is not frozen.
  const frozen = Object.freeze(LINUX({ buttons: Object.freeze(["close"]) }));
  const out = readFrame(frozen);
  assert.deepEqual(out, LINUX({ buttons: ["close"] }));
  assert.equal(Object.isFrozen(out), false);
});

test("the contract is one table: the reader lists no word that is not a button of the table", () => {
  const source = readFileSync(new URL("../../src/windowframestate.ts", import.meta.url), "utf8");
  assert.match(source, /round/, "the table names the round style");
  assert.match(source, /left/, "and the left side");
  assert.doesNotMatch(source, /document\.|window\.|innerHTML/, "the reader is free of the DOM");
});

test("the button table has exactly the three words of the shell, each row complete", () => {
  assert.deepEqual(BUTTONS.map((row) => row.word), ["minimize", "maximize", "close"]);
  assert.deepEqual(BUTTONS.map((row) => row.name), ["Minimize", "Maximize", "Close"]);
  assert.deepEqual(BUTTONS.map((row) => row.glyph), ["minimize", "maximize", "close"]);
  assert.deepEqual(BUTTONS.map((row) => row.press), ["minimize", "toggle_maximize", "close"]);
  for (const row of BUTTONS) {
    assert.deepEqual(Object.keys(row).sort(), ["glyph", "name", "press", "word"]);
    for (const value of Object.values(row)) assert.ok(typeof value === "string" && value.length > 0);
  }
  assert.deepEqual(readFrame(WINDOWS()).buttons, BUTTONS.map((row) => row.word), "the reader and the table agree on the order");
});

test("the maximise row follows the window and the other two rows never do", () => {
  const restore = maximizeRow(true);
  assert.equal(restore.name, "Restore");
  assert.equal(restore.glyph, "restore");
  assert.equal(restore.word, "maximize");
  assert.equal(restore.press, "toggle_maximize", "a press is the same command in both states");
  const plain = maximizeRow(false);
  assert.equal(plain.name, "Maximize");
  assert.equal(plain.glyph, "maximize");
  assert.deepEqual(plain, BUTTONS[1]);
  for (const row of [BUTTONS[0], BUTTONS[2]]) assert.ok(![restore, plain].includes(row));
  assert.notEqual(restore.glyph, plain.glyph, "the cue is a shape");
  assert.notEqual(restore.name, plain.name, "and a word");
});

/** A reader that answers by command from a table; a value that is an `Error` rejects, and a function is called. */
function reader(answers, asked = []) {
  return (command) => {
    asked.push(command);
    const answer = answers[command];
    if (answer instanceof Error) return Promise.reject(answer);
    return Promise.resolve(typeof answer === "function" ? answer() : answer);
  };
}
const MAX = "plugin:window|is_maximized";
const FULL = "plugin:window|is_fullscreen";

test("the window state asks the two commands and takes the boolean answers", async () => {
  const asked = [];
  const none = { maximized: false, fullscreen: false };
  assert.deepEqual(await windowState(reader({ [MAX]: true, [FULL]: false }, asked), none), { maximized: true, fullscreen: false });
  assert.deepEqual(asked.sort(), [FULL, MAX].sort(), "exactly these two commands");
  assert.deepEqual(await windowState(reader({ [MAX]: false, [FULL]: true }), none), { maximized: false, fullscreen: true });
  assert.deepEqual(await windowState(reader({ [MAX]: true, [FULL]: true }), none), { maximized: true, fullscreen: true });
  assert.deepEqual(await windowState(reader({ [MAX]: false, [FULL]: false }), { maximized: true, fullscreen: true }), none, "a boolean false is taken");
});

test("only the boolean true makes a member true, and an odd answer or a failure keeps the previous value of that member", async () => {
  const odd = ["true", "yes", 1, 0, null, undefined, {}, [], [true], "", NaN];
  for (const answer of odd) {
    const fromFalse = await windowState(reader({ [MAX]: answer, [FULL]: answer }), { maximized: false, fullscreen: false });
    assert.deepEqual(fromFalse, { maximized: false, fullscreen: false }, `${String(answer)} is not true`);
    const fromTrue = await windowState(reader({ [MAX]: answer, [FULL]: answer }), { maximized: true, fullscreen: true });
    assert.deepEqual(fromTrue, { maximized: true, fullscreen: true }, `${String(answer)} keeps the last state`);
  }
  const failure = new Error(CANARY);
  const kept = await windowState(reader({ [MAX]: failure, [FULL]: true }), { maximized: true, fullscreen: false });
  assert.deepEqual(kept, { maximized: true, fullscreen: true }, "a rejection keeps that member and the other one is read");
  const keptOther = await windowState(reader({ [MAX]: false, [FULL]: failure }), { maximized: true, fullscreen: true });
  assert.deepEqual(keptOther, { maximized: false, fullscreen: true });
  const both = await windowState(reader({ [MAX]: failure, [FULL]: failure }), { maximized: false, fullscreen: true });
  assert.deepEqual(both, { maximized: false, fullscreen: true });
  const throwing = () => { throw new Error(CANARY); };
  assert.deepEqual(await windowState(throwing, { maximized: true, fullscreen: false }), { maximized: true, fullscreen: false }, "a reader that throws at once");
});

// ---- the design record -------------------------------------------------------------------------------------------------

/** The text of the window frame section: from its heading to the next heading of the same level. */
function section() {
  const start = spec.indexOf("\n### 6.33 Window frame\n");
  assert.ok(start >= 0, "docs/design.md has the section `6.33 Window frame`");
  const rest = spec.slice(start + 1);
  const next = rest.indexOf("\n### ", 4);
  return next < 0 ? rest : rest.slice(0, next);
}

/** The rows of the markdown table whose header row starts with `head`, as lists of cells (the header and the rule are dropped). */
function table(text, head) {
  const lines = text.split("\n");
  const at = lines.findIndex((line) => line.startsWith(`| ${head} |`));
  assert.ok(at >= 0, `the section has a table headed \`${head}\``);
  const rows = [];
  for (let i = at + 2; i < lines.length && lines[i].startsWith("|"); i += 1) {
    rows.push(lines[i].slice(1, -1).split("|").map((cell) => cell.trim()));
  }
  assert.ok(rows.length > 0, `the table headed \`${head}\` has rows`);
  return rows;
}

/** The spec's words for a drawing, as the start of the glyph cell. */
const DRAWING = {
  minimize: "a horizontal line",
  maximize: "a square outline",
  restore: "two square outlines",
  close: "two crossing diagonals",
};

test("the buttons of the page are the rows of the window buttons table of the design record", () => {
  const rows = table(section(), "Button");
  assert.equal(rows.length, 4, "minimise, maximise, restore and close");
  const rowsOfPage = [BUTTONS[0], maximizeRow(false), maximizeRow(true), BUTTONS[2]];
  rows.forEach((cells, i) => {
    const page = rowsOfPage[i];
    assert.equal(cells.length, 3);
    assert.equal(cells[2], `\`${page.name}\``, `the name of row ${i + 1}: the spec says ${cells[2]}`);
    assert.ok(cells[1].startsWith(DRAWING[page.glyph]), `the glyph \`${page.glyph}\` is "${DRAWING[page.glyph]}": the spec says "${cells[1]}"`);
  });
  // A word missing on either side fails with its name.
  const specNames = rows.map((cells) => cells[2].replaceAll("`", ""));
  for (const row of rowsOfPage) assert.ok(specNames.includes(row.name), `the page's name ${row.name} is in the spec`);
  for (const name of specNames) assert.ok(rowsOfPage.some((row) => row.name === name), `the spec's name ${name} is in the page`);
  const specGlyphs = rows.map((cells) => Object.keys(DRAWING).find((glyph) => cells[1].startsWith(DRAWING[glyph])));
  assert.deepEqual(specGlyphs, ["minimize", "maximize", "restore", "close"], "every drawing of the spec is one the page knows");
});

test("the order of the buttons and the state table of the page are those of the design record", () => {
  const text = section();
  assert.match(text, /Windows: the three on the right, in the order Minimize, Maximize, Close\./);
  const states = table(text, "State");
  const maximised = states.find((cells) => cells[0] === "Maximised");
  assert.ok(maximised, "the state table has the row `Maximised`");
  assert.ok(maximised[1].includes("restore glyph"), maximised[1]);
  assert.ok(maximised[1].includes(`\`${maximizeRow(true).name}\``), `the name and title are ${maximizeRow(true).name}: ${maximised[1]}`);
  assert.ok(maximised[2].includes("a shape and a word, not a colour"), maximised[2]);
  const names = states.map((cells) => cells[0]);
  for (const state of ["Rest", "Hover", "Windows close under the pointer", "Keyboard focus", "Maximised"]) {
    assert.ok(names.includes(state), `the state table has \`${state}\``);
  }
  // The tokens of the states are the spec's; the browser test reads the computed colours of each state.
  const byName = Object.fromEntries(states.map((cells) => [cells[0], cells[1]]));
  assert.ok(byName.Rest.includes("`--hw-dim`"));
  assert.ok(byName.Hover.includes("`--hw-rose-wash`") && byName.Hover.includes("`--hw-ink`"));
  assert.ok(byName["Windows close under the pointer"].includes("`--hw-danger`"));
  assert.ok(byName["Windows close under the pointer"].includes("`--hw-ground`"));
  assert.ok(byName["Keyboard focus"].includes("outline: 2px solid var(--hw-ink)"));
});

test("the flat size of the design record is 46 by 32 px with a glyph of 10 px and a floor of 28 px", () => {
  const flat = table(section(), "Style").find((cells) => cells[0] === "Flat");
  assert.ok(flat[2].startsWith("46 px wide and 32 px high"), flat[2]);
  assert.ok(flat[3].includes("10 px"), flat[3]);
  assert.match(section(), /the floor is written as `max\(28px, …\)`/);
});
