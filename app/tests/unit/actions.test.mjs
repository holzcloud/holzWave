// The action registry and the chord table on the compiled output in dist/: neither touches the DOM, so node can
// import and run them. Every case registers under its own ids because the registry is one module-level object.
import test from "node:test";
import assert from "node:assert/strict";
import { listActions, registerAction, replaceAction, runAction, unregisterAction } from "../../dist/actions.js";
import { CHORDS, actionIdFor } from "../../dist/chords.js";

let n = 0;
/** An id no earlier case used. */
const fresh = (what) => `test.${what}.${++n}`;

test("the registry imports and runs without a DOM", () => {
  assert.equal(typeof document, "undefined");
  assert.equal(typeof window, "undefined");
});

test("runAction runs the registered action once and reports that it did", () => {
  const id = fresh("run");
  let runs = 0;
  registerAction({ id, title: "Run me", run: () => void runs++ });
  assert.equal(runAction(id), true);
  assert.equal(runs, 1);
});

test("a duplicate id throws and leaves the first action in place", () => {
  const id = fresh("dup");
  let which = "";
  registerAction({ id, title: "First", run: () => void (which = "first") });
  assert.throws(() => registerAction({ id, title: "Second", run: () => void (which = "second") }), /already registered/);
  runAction(id);
  assert.equal(which, "first");
});

test("replaceAction swaps the behaviour and keeps the position in the list", () => {
  const a = fresh("replace-a");
  const b = fresh("replace-b");
  let which = "";
  registerAction({ id: a, title: "A", run: () => void (which = "old") });
  registerAction({ id: b, title: "B", run: () => {} });
  replaceAction({ id: a, title: "A2", run: () => void (which = "new") });
  runAction(a);
  assert.equal(which, "new");
  const ids = listActions().map((x) => x.id);
  assert.ok(ids.indexOf(a) < ids.indexOf(b), "the replaced action moved behind a later one");
  assert.equal(listActions().find((x) => x.id === a)?.title, "A2");
});

test("replaceAction for an unknown id registers it", () => {
  const id = fresh("replace-new");
  let runs = 0;
  replaceAction({ id, title: "New", run: () => void runs++ });
  assert.equal(runAction(id), true);
  assert.equal(runs, 1);
});

test("unregisterAction removes the action and the id can be registered again", () => {
  const id = fresh("unreg");
  let runs = 0;
  registerAction({ id, title: "Gone", run: () => void runs++ });
  assert.equal(unregisterAction(id), true);
  assert.equal(runAction(id), false);
  assert.equal(runs, 0);
  assert.equal(unregisterAction(id), false, "a second removal reports that nothing was there");
  registerAction({ id, title: "Back", run: () => void runs++ });
  assert.equal(runAction(id), true);
  assert.equal(runs, 1);
});

test("an unknown id is a no-op that returns false", () => {
  assert.equal(runAction("no.such.action"), false);
  assert.equal(runAction(""), false);
});

test("an id that is an Object.prototype name is unknown, not an inherited function", () => {
  for (const id of ["constructor", "toString", "__proto__", "hasOwnProperty"]) {
    assert.equal(runAction(id), false, id);
  }
});

test("listActions keeps the registration order and returns a copy", () => {
  const ids = [fresh("order-1"), fresh("order-2"), fresh("order-3")];
  for (const id of ids) registerAction({ id, title: id, run: () => {} });
  const seen = listActions().map((x) => x.id).filter((id) => ids.includes(id));
  assert.deepEqual(seen, ids);
  listActions().length = 0; // changing the returned array must not change the registry
  assert.ok(listActions().length >= ids.length);
});

test("an action that throws is not swallowed", () => {
  const id = fresh("throws");
  registerAction({ id, title: "Boom", run: () => { throw new Error("boom"); } });
  assert.throws(() => runAction(id), /boom/);
});

// ---- key auto-repeat -----------------------------------------------------------------------------

const REPEATABLE = ["pane.focus_next", "pane.focus_prev", "font.increase", "font.decrease"];

test("an action that is not allowed to repeat is consumed on a repeat and runs once per press", () => {
  const id = fresh("once");
  let runs = 0;
  registerAction({ id, title: "Once", run: () => void runs++ });
  assert.equal(runAction(id, { repeat: true }), true, "a repeat of a known action is consumed, not unknown");
  assert.equal(runs, 0, "the repeat did not run it");
  assert.equal(runAction(id), true);
  assert.equal(runs, 1, "a press without options runs it");
  assert.equal(runAction(id, { repeat: false }), true);
  assert.equal(runs, 2, "a press that is not a repeat runs it");
});

test("the four stepping actions run on every repeat", () => {
  for (const id of REPEATABLE) {
    let runs = 0;
    // The page registers these ids; node has no page, so the case registers its own under the real id.
    registerAction({ id, title: id, run: () => void runs++ });
    assert.equal(runAction(id, { repeat: true }), true, id);
    assert.equal(runAction(id, { repeat: true }), true, id);
    assert.equal(runs, 2, `${id} ran on both repeats`);
    assert.equal(unregisterAction(id), true);
  }
});

test("an unknown id is false on a repeat as well", () => {
  assert.equal(runAction("no.such.action", { repeat: true }), false);
  assert.equal(runAction("constructor", { repeat: true }), false);
});

// ---- the chord table -----------------------------------------------------------------------------

const EXPECTED = {
  "Cmd+D": "pane.split_right",
  "Cmd+Shift+D": "pane.split_down",
  "Cmd+T": "tab.new",
  "Cmd+Shift+N": "workspace.new",
  "Cmd+W": "pane.close",
  "Cmd+]": "pane.focus_next",
  "Cmd+[": "pane.focus_prev",
  "Cmd+C": "edit.copy",
  "Cmd+K": "palette.open",
  "Cmd+F": "search.open",
  "Cmd+Shift+X": "copy_mode.enter",
  "Cmd+Shift+Space": "hints.open",
  "Cmd+=": "font.increase",
  "Cmd+-": "font.decrease",
  "Cmd+0": "font.reset",
  "Cmd+,": "settings.open",
};
for (let i = 1; i <= 9; i++) {
  EXPECTED[`Cmd+${i}`] = `tab.select_${i}`;
  EXPECTED[`Cmd+Alt+${i}`] = `workspace.select_${i}`;
}

test("the chord table holds every id of the shared interface under its default chord", () => {
  for (const [chord, id] of Object.entries(EXPECTED)) {
    assert.equal(actionIdFor(chord), id, chord);
  }
  assert.equal(CHORDS.size, Object.keys(EXPECTED).length, "the table has chords nobody listed");
});

test("chords are in the canonical form and each action has exactly one chord", () => {
  const order = ["Cmd", "Ctrl", "Alt", "Shift"];
  const ids = new Set();
  for (const [chord, id] of CHORDS) {
    const parts = chord.split("+");
    const mods = parts.slice(0, -1);
    assert.deepEqual(mods, order.filter((m) => mods.includes(m)), `${chord}: modifiers out of order`);
    const key = parts[parts.length - 1];
    assert.ok(key === "" || key === key.toUpperCase() || key.length > 1, `${chord}: letters are upper case`);
    assert.ok(!ids.has(id), `${id} appears twice`);
    ids.add(id);
  }
});

test("a chord that is not in the table resolves to nothing", () => {
  for (const chord of ["Cmd+Z", "D", "Cmd+d", "Ctrl+D", "constructor", "__proto__", ""]) {
    assert.equal(actionIdFor(chord), undefined, JSON.stringify(chord));
  }
});
