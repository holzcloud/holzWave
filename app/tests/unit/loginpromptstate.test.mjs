// The reading of a login question and the queue of questions (loginpromptstate.ts): no DOM is needed.
import test from "node:test";
import assert from "node:assert/strict";
import { INTERACTIVE_HINT, MAX_PROMPTS, PromptQueue, readEnd, readPrompt, serverTextLine } from "../../dist/loginpromptstate.js";

const ID = "0123456789abcdef0123456789abcdef";
const good = (over = {}) => ({
  id: ID,
  connection: "Talos",
  host: "talos.example",
  name: "Two-factor login",
  instructions: "Enter the code.",
  prompts: [{ text: "Password:", echo: false }, { text: "Code:", echo: true }],
  ...over,
});

test("readPrompt_is_strict", () => {
  const read = readPrompt(good());
  assert.deepEqual(read, good());
  // The id: exactly 32 lowercase hex characters.
  for (const id of ["", "abc", ID.slice(1), `${ID}0`, ID.toUpperCase(), "g".repeat(32), 5, null, undefined, [ID]]) {
    assert.equal(readPrompt(good({ id })), null, `id ${JSON.stringify(id)}`);
  }
  // Not an object.
  for (const raw of [null, undefined, "x", 3, true, [], [good()]]) assert.equal(readPrompt(raw), null);
  // The prompts: one to five, each with a text and a strict boolean.
  const line = { text: "Code:", echo: false };
  assert.equal(readPrompt(good({ prompts: [] })), null, "no question is no dialog");
  assert.equal(readPrompt(good({ prompts: Array(MAX_PROMPTS + 1).fill(line) })), null, "six questions");
  assert.notEqual(readPrompt(good({ prompts: Array(MAX_PROMPTS).fill(line) })), null, "five are fine");
  assert.equal(readPrompt(good({ prompts: "Code:" })), null);
  for (const bad of [{ text: "", echo: false }, { text: "   ", echo: false }, { echo: false }, { text: 5, echo: false },
    { text: "x", echo: "false" }, { text: "x", echo: 0 }, { text: "x" }, null, "x", [], [line]]) {
    assert.equal(readPrompt(good({ prompts: [line, bad] })), null, `prompt ${JSON.stringify(bad)}`);
  }
  // The title is the person's own connection: none, no dialog.
  for (const connection of ["", "   ", "‮​", 5, null]) assert.equal(readPrompt(good({ connection })), null);
  assert.equal(readPrompt(good({ host: 1 })), null);
  assert.equal(readPrompt(good({ name: null })), null);
  assert.equal(readPrompt(good({ instructions: [] })), null);
});

test("readPrompt cuts every string to its bound and removes what nobody can see", () => {
  const read = readPrompt(good({
    connection: "C".repeat(400),
    host: "h".repeat(400),
    name: "N".repeat(400),
    instructions: "i".repeat(900),
    prompts: [{ text: "P".repeat(900), echo: true }],
  }));
  assert.equal(read.connection.length, 256);
  assert.equal(read.host.length, 256);
  assert.equal(read.name.length, 60);
  assert.equal(read.instructions.length, 300);
  assert.equal(read.prompts[0].text.length, 200);
  // Control, direction and zero-width characters are gone; a line break stays in the instructions.
  const cleaned = readPrompt(good({
    name: "a‮b​c\u0007d\u001b[2Je",
    instructions: "one\ntwo⁦x⁩",
    prompts: [{ text: "Co﻿de:\u0000", echo: false }],
  }));
  assert.equal(cleaned.name, "abcd[2Je");
  assert.equal(cleaned.instructions, "one\ntwox");
  assert.equal(cleaned.prompts[0].text, "Code:");
  // Characters are counted as characters: a cut never splits one.
  const emoji = readPrompt(good({ name: "\u{1F512}".repeat(100) }));
  assert.equal(Array.from(emoji.name).length, 60);
  // Extra keys and deep nesting are ignored, not read.
  const deep = { a: { a: { a: { a: {} } } } };
  assert.deepEqual(readPrompt({ ...good(), extra: deep, prompts: [{ text: "x", echo: true, extra: deep }] }).prompts, [{ text: "x", echo: true }]);
});

test("the fixed sentence is exactly the one the design says", () => {
  assert.equal(serverTextLine, "This text comes from the server, not from holzWave.");
  assert.equal(INTERACTIVE_HINT, "The server asks for your answer when you connect. Nothing is stored.");
});

test("readEnd reads the id of an ended question and nothing else", () => {
  for (const outcome of ["answered", "cancelled", "timed_out"]) assert.equal(readEnd({ id: ID, outcome }), ID);
  assert.equal(readEnd({ id: ID, outcome: "later" }), null);
  assert.equal(readEnd({ id: ID }), null);
  assert.equal(readEnd({ id: "x", outcome: "answered" }), null);
  for (const raw of [null, undefined, "x", [], 1]) assert.equal(readEnd(raw), null);
});

test("the queue shows one question at a time and the next one after it", () => {
  const q = new PromptQueue();
  const a = readPrompt(good({ id: "a".repeat(32) }));
  const b = readPrompt(good({ id: "b".repeat(32) }));
  const c = readPrompt(good({ id: "c".repeat(32) }));
  assert.equal(q.push(a), a, "the first is shown at once");
  assert.equal(q.current, a);
  assert.equal(q.push(b), null, "the second waits");
  assert.equal(q.push(b), null, "an id once");
  assert.equal(q.push(a), null, "the one on screen is not queued again");
  assert.equal(q.push(c), null);
  assert.deepEqual(q.end("d".repeat(32)), { closed: false, next: null }, "an unknown id changes nothing");
  assert.equal(q.current, a);
  // A waiting question that ended in the core is never shown.
  assert.deepEqual(q.end("b".repeat(32)), { closed: false, next: null });
  assert.deepEqual(q.end(a.id), { closed: true, next: c });
  assert.equal(q.current, c);
  assert.deepEqual(q.end(c.id), { closed: true, next: null });
  assert.equal(q.current, null);
  assert.equal(q.push(b), b, "after the last one the next arrival is shown at once");
});

test("the queue holds at most sixteen waiting questions", () => {
  const q = new PromptQueue();
  const at = (i) => readPrompt(good({ id: i.toString(16).padStart(32, "0") }));
  assert.notEqual(q.push(at(0)), null);
  for (let i = 1; i <= 40; i += 1) q.push(at(i));
  let shown = 0;
  let id = at(0).id;
  for (;;) {
    const r = q.end(id);
    if (r.next === null) break;
    shown += 1;
    id = r.next.id;
  }
  assert.equal(shown, 16);
});
