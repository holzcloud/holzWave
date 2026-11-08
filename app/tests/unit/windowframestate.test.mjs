// The pure half of the own window frame (windowframestate.ts): how the facts of the shell are read, the table of the buttons,
// the state of the maximise button and the reader of the window state. No DOM is needed. The words and shapes are held to the
// design record (docs/design.md section 6.33) here, so the page cannot drift from the spec.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { BUTTONS, maximizeRow, readFrame, windowState } from "../../dist/windowframestate.js";

const spec = readFileSync(new URL("../../../docs/design.md", import.meta.url), "utf8");

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
    { ...WINDOWS(), os: "linux" },
    { ...WINDOWS(), os: "macos" },
    { ...WINDOWS(), os: "Windows" },
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

test("the button table has exactly the three words of the shell, each row complete", () => {
  assert.deepEqual(BUTTONS.map((row) => row.word), ["minimize", "maximize", "close"]);
  assert.deepEqual(BUTTONS.map((row) => row.name), ["Minimize", "Maximize", "Close"]);
  assert.deepEqual(BUTTONS.map((row) => row.glyph), ["minimize", "maximize", "close"]);
  assert.deepEqual(BUTTONS.map((row) => row.command), ["minimize", "toggle_maximize", "close"]);
  for (const row of BUTTONS) {
    assert.deepEqual(Object.keys(row).sort(), ["command", "glyph", "name", "word"]);
    for (const value of Object.values(row)) assert.ok(typeof value === "string" && value.length > 0);
  }
  assert.deepEqual(readFrame(WINDOWS()).buttons, BUTTONS.map((row) => row.word), "the reader and the table agree on the order");
});

test("the maximise row follows the window and the other two rows never do", () => {
  const restore = maximizeRow(true);
  assert.equal(restore.name, "Restore");
  assert.equal(restore.glyph, "restore");
  assert.equal(restore.word, "maximize");
  assert.equal(restore.command, "toggle_maximize", "a press is the same command in both states");
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
