// The pure half of the palette actions and of the reply action (inlinestate.ts): the words, the choice of the source for an
// explanation, the one slot that keeps an intent until its chat is drawn. No DOM and no bridge.
import test from "node:test";
import assert from "node:assert/strict";
import {
  DESCRIBE_ACTION, DESCRIBE_TEXT, DESCRIBE_TITLE, EXPLAIN_ACTION, EXPLAIN_TEXT, EXPLAIN_TITLE, IntentSlot, MAX_BUTTONS, MAX_COMMAND, WORDS,
  commandVerdict, explainPlan, hasSelection, insertRefusal, insertedWords,
} from "../../dist/inlinestate.js";

test("an explain plan takes the selection when the pane shows one, else the last command, else the screen", () => {
  const rest = { kinds: ["last_command", "screen"], first: true };
  assert.deepEqual(explainPlan(undefined), rest);
  assert.deepEqual(explainPlan(null), rest);
  assert.deepEqual(explainPlan({ selection: [] }), rest);
  assert.deepEqual(explainPlan({ selection: [[3, 0, 9]] }), { kinds: ["selection"], first: false });

  // Only the presence of a span counts: the spans themselves are never read.
  assert.equal(hasSelection({ selection: [[0, 0, 1]] }), true);
  assert.equal(hasSelection({ selection: [{ nonsense: true }, null, "x"] }), true);
  assert.equal(hasSelection({ selection: [] }), false);
  assert.equal(hasSelection({}), false);
  assert.equal(hasSelection({ selection: "abc" }), false);
  assert.equal(hasSelection({ selection: { length: 3 } }), false);
  assert.equal(hasSelection(null), false);
  assert.equal(hasSelection(undefined), false);

  const mirror = Object.freeze({ selection: Object.freeze([[1, 2, 3]]) });
  const before = JSON.stringify(mirror);
  const a = explainPlan(mirror);
  const b = explainPlan(mirror);
  assert.equal(JSON.stringify(mirror), before, "the input is not changed");
  assert.notEqual(a.kinds, b.kinds, "the arrays are fresh on every call");
  a.kinds.push("file");
  assert.deepEqual(explainPlan(mirror).kinds, ["selection"]);
  assert.notEqual(explainPlan(null).kinds, explainPlan(null).kinds);
});

test("an intent waits in one slot until its chat mounts", () => {
  const i1 = { text: "one", kinds: [], first: false, ready: "r1" };
  const i2 = { text: "two", kinds: ["screen"], first: true, ready: "r2" };
  const slot = new IntentSlot();
  assert.equal(slot.take("chat-a"), null, "an empty slot answers null");

  slot.park("chat-a", i1);
  assert.equal(slot.take("chat-b"), null, "another chat takes nothing");
  assert.equal(slot.take("chat-a"), i1, "and leaves the slot as it was");
  assert.equal(slot.take("chat-a"), null, "an intent is taken once");

  slot.park("chat-a", i1);
  slot.park("chat-b", i2);
  assert.equal(slot.take("chat-a"), null, "the later park replaced the earlier one");
  assert.equal(slot.take("chat-b"), i2);

  slot.park("chat-a", i1);
  slot.drop("chat-b");
  assert.equal(slot.take("chat-a"), i1, "dropping another chat does nothing");
  slot.park("chat-a", i1);
  slot.drop("chat-a");
  assert.equal(slot.take("chat-a"), null, "dropping the chat of the slot empties it");

  slot.park("chat-a", i1);
  slot.clear();
  assert.equal(slot.take("chat-a"), null);
});

test("the words of the actions are fixed sentences", () => {
  assert.equal(EXPLAIN_ACTION, "ai.explain_error");
  assert.equal(DESCRIBE_ACTION, "ai.command_from_description");
  assert.equal(EXPLAIN_TITLE, "Explain error");
  assert.equal(DESCRIBE_TITLE, "Command from description");
  assert.equal(
    EXPLAIN_TEXT,
    "Explain the error in the attached terminal output: what went wrong, the most likely cause, and the command or change that fixes it.",
  );
  assert.equal(
    DESCRIBE_TEXT,
    "Write one shell command for this task. Reply with the command alone in one code block and, after it, one sentence saying what it does. Task: ",
  );
  assert.ok(DESCRIBE_TEXT.endsWith(": "));
  assert.equal(MAX_COMMAND, 1024);
  assert.equal(MAX_BUTTONS, 16);
  assert.equal(WORDS.explainReady, "Nothing is sent until you press Send.");
  assert.equal(WORDS.describeReady, 'Describe the task after "Task:", then press Send.');
  assert.equal(WORDS.notTerminal, "Focus a terminal pane to explain its error.");
  const words = Object.values(WORDS);
  assert.ok(words.length >= 8);
  for (const word of [...words, EXPLAIN_TEXT, DESCRIBE_TEXT]) {
    assert.equal(typeof word, "string");
    assert.ok(word.length > 0);
    assert.ok(!/[\r\n]/.test(word), `a single line: ${word}`);
    assert.ok(!word.includes("://"), word);
    assert.ok(!/[<>]/.test(word), word);
    assert.ok(!/[\u0000-\u001f\u007f-\u009f]/u.test(word), `no control character: ${word}`);
  }
  for (const text of [EXPLAIN_TEXT, DESCRIBE_TEXT]) assert.ok(new TextEncoder().encode(text).length < 131072);
});

const CLEAN = "find . -type f -size +1G -exec ls -lh {} +";

test("a code block of a reply is a command only when it is one clean line", () => {
  assert.deepEqual(commandVerdict(`${CLEAN}\n`), { kind: "command", text: CLEAN });
  assert.deepEqual(commandVerdict(`${CLEAN}\r\n`), { kind: "command", text: CLEAN });
  assert.deepEqual(commandVerdict("  git status  "), { kind: "command", text: "git status" });
  assert.deepEqual(commandVerdict("echo héllo"), { kind: "command", text: "echo héllo" });
  const edge = "a".repeat(MAX_COMMAND);
  assert.deepEqual(commandVerdict(edge), { kind: "command", text: edge });
  const astral = "\u{1f600}".repeat(600);
  assert.equal(astral.length, 1200, "1200 UTF-16 units");
  assert.deepEqual(commandVerdict(astral), { kind: "command", text: astral }, "characters are counted as code points");
  assert.deepEqual(commandVerdict("a".repeat(MAX_COMMAND + 1)), { kind: "none" });
  assert.deepEqual(commandVerdict("\u{1f600}".repeat(MAX_COMMAND + 1)), { kind: "none" });

  for (const none of ["", "\n", "   ", "ls\npwd", "ls\n\npwd", "a\tb", "ls\n\n", "x".repeat(65537)]) {
    assert.deepEqual(commandVerdict(none), { kind: "none" }, JSON.stringify(none.slice(0, 20)));
  }

  const hidden = {
    "an escape sequence": "echo \u001b[31mred",
    "an OSC 52 sequence": "printf \u001b]52;c;Zm9v\u0007",
    "a C1 control": "\u009b2J",
    DEL: "rm\u007f",
    "a carriage return inside a line": "ls\rpwd",
    "a bidirectional override": "echo \u202eevil",
    "a bidirectional isolate": "\u2066x\u2069",
    "the Arabic letter mark": "\u061c",
    "a zero-width space": "ec\u200bho hi",
    "the word joiner": "\u2060",
    "a byte order mark": "\ufeff",
    "a soft hyphen": "ec\u00adho",
    "a tag character": "\u{e0041}",
    "a private-use character": "\ue000",
    "a line separator": "a\u2028b",
    "a paragraph separator": "a\u2029b",
    "a no-break space": "a\u00a0b",
    "an ideographic space": "a\u3000b",
    "an en space": "a\u2002b",
    "a lone surrogate": "\ud800",
    "NEL": "a\u0085b",
    "a null byte": "a\u0000b",
  };
  for (const [what, text] of Object.entries(hidden)) assert.deepEqual(commandVerdict(text), { kind: "hidden" }, what);
  // The hidden rule comes before the multi-line rule; ordinary multi-line text stays none.
  assert.deepEqual(commandVerdict("ls\n\u001b[2Jpwd"), { kind: "hidden" });
  assert.deepEqual(commandVerdict("ls\n\tpwd\nexit"), { kind: "none" });

  // Pure: the same answer twice, and a command has no line break.
  for (const text of [`${CLEAN}\n`, "a\u202eb", "x\ny"]) assert.deepEqual(commandVerdict(text), commandVerdict(text));
  const verdict = commandVerdict(`${CLEAN}\n`);
  assert.ok(verdict.kind === "command" && !/[\r\n]/.test(verdict.text));
});

test("an insert is refused with a sentence unless the target is a terminal pane at a prompt", () => {
  const ok = { pane: 1, terminal: true, inView: true, fullScreen: false };
  assert.equal(insertRefusal(ok), null);
  assert.equal(insertRefusal({ ...ok, pane: null }), WORDS.noTerminal);
  assert.equal(insertRefusal({ ...ok, terminal: false }), WORDS.noTerminal);
  assert.equal(insertRefusal({ ...ok, inView: false }), WORDS.noTerminal);
  assert.equal(insertRefusal({ ...ok, fullScreen: true }), WORDS.fullScreen);
  assert.equal(insertRefusal({ pane: 1, terminal: false, inView: true, fullScreen: true }), WORDS.noTerminal, "no terminal comes first");
  assert.equal(insertedWords(3), "Inserted at the prompt of pane 3. It was not run.");
  for (const odd of [-1, 1.5, Number.NaN, Number.POSITIVE_INFINITY]) assert.equal(insertedWords(odd), "Inserted at the prompt. It was not run.");
});

test("the insert words are fixed sentences", () => {
  assert.equal(WORDS.insertLabel, "Insert at prompt");
  assert.equal(WORDS.notInsertable, "! Not insertable: it holds characters that are not shown.");
  assert.equal(WORDS.noTerminal, "There is no terminal pane to insert into.");
  assert.equal(WORDS.fullScreen, "That pane shows a full-screen program, so nothing was inserted.");
  assert.equal(WORDS.refused, "The terminal did not take the text, so nothing was inserted.");
  for (const word of [WORDS.insertLabel, WORDS.notInsertable, WORDS.noTerminal, WORDS.fullScreen, WORDS.refused]) {
    assert.ok(!word.includes("://") && !/[<>]/.test(word) && !/[\u0000-\u001f\u007f-\u009f]/u.test(word), word);
  }
  for (const word of [WORDS.noTerminal, WORDS.fullScreen, WORDS.refused]) assert.ok(word.endsWith("."), word);
});
