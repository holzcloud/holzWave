// The controller of Undo presses (undo.ts): one undo command through an injected invoke, a refusal told and never retried, and the
// page's reasons held to the shell's golden. No DOM and no bridge are needed.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { UNDO_GONE, UNDO_REASONS, WORDS, canUndo } from "../../dist/approvalstate.js";
import { Undos } from "../../dist/undo.js";

const PROTOCOL = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));
const CHAT = "chat-undo-1";
/** A character that must never survive in a sentence the page shows. */
const BIDI = String.fromCharCode(0x202e);

/** A controller over a fake invoke that records its calls and answers as `answer` says (a function that may reject). */
function make(answer = () => Promise.resolve(null)) {
  const calls = [];
  const undos = new Undos({
    invoke: (cmd, args) => {
      calls.push([cmd, structuredClone(args)]);
      return answer(cmd, args);
    },
    conversation: () => CHAT,
  });
  return { undos, calls };
}

const refusal = (reason, text = "The core says no.") => Promise.reject({ code: "undo", reason, text });

test("an undo sends the two ids once", async () => {
  const { undos, calls } = make();
  const seen = [];
  const stop = undos.subscribe(() => seen.push(undos.model("call_7").state));
  // Nothing is asked at construction, on subscribe or on a read.
  assert.equal(undos.model("call_7").state, "idle");
  assert.deepEqual(calls, []);

  const pressed = undos.undo("call_7");
  assert.equal(undos.model("call_7").state, "sending");
  await undos.undo("call_7");
  assert.equal(calls.length, 1, "a second press while it travels makes no call");
  await pressed;
  assert.equal(undos.model("call_7").state, "done");
  assert.deepEqual(calls, [["ai_undo", { conversation: CHAT, call_id: "call_7" }]]);
  assert.deepEqual(Object.keys(calls[0][1]), ["conversation", "call_id"], "exactly two keys");
  assert.deepEqual(seen, ["sending", "done"]);

  await undos.undo("call_7");
  assert.equal(calls.length, 1, "a call that is done is not undone again");
  stop();
  await undos.undo("call_8");
  assert.equal(seen.length, 2, "an unsubscribed listener hears nothing");
});

test("a refusal tells its reason, hides the button for a gone change and is never retried", async () => {
  const cases = [
    ["file_changed", "idle"],
    ["unknown_change", "gone"],
    ["created", "gone"],
    ["denied", "idle"],
    ["unreadable", "idle"],
  ];
  for (const [reason, state] of cases) {
    const { undos, calls } = make(() => refusal(reason, `Sentence for ${reason}.`));
    await undos.undo("call_1");
    const model = undos.model("call_1");
    assert.equal(model.state, state, reason);
    assert.deepEqual(model.fault, { reason, text: `Sentence for ${reason}.` }, reason);
    assert.equal(calls.length, 1, "nothing is sent again by itself");
    await new Promise((go) => setTimeout(go, 5));
    assert.equal(calls.length, 1);
  }

  // The core's text is cleaned and cut at 300 characters.
  const long = make(() => refusal("file_changed", `a${BIDI}${"b".repeat(400)}`));
  await long.undos.undo("call_1");
  const text = long.undos.model("call_1").fault.text;
  assert.ok(!text.includes(BIDI) && text.length <= 301, `${text.length}`);

  // Anything that is not the core's refusal is the fixed sentence, with no reason, and leaves the button.
  for (const raw of ["boom", null, 7, { code: "tool", reason: "unknown_call", text: "x" }, { reason: "file_changed", text: "x" }]) {
    const { undos } = make(() => Promise.reject(raw));
    await undos.undo("call_1");
    assert.equal(undos.model("call_1").state, "idle", JSON.stringify(raw));
    assert.deepEqual(undos.model("call_1").fault, { reason: null, text: WORDS.undoCouldNot });
  }
  assert.equal(WORDS.undoCouldNot, "The undo could not be sent.");

  // A later press after a refusal is a new call; a gone change ignores it.
  let n = 0;
  const again = make(() => (++n === 1 ? refusal("denied") : Promise.resolve(null)));
  await again.undos.undo("call_1");
  assert.equal(again.undos.model("call_1").state, "idle");
  await again.undos.undo("call_1");
  assert.equal(again.calls.length, 2);
  assert.equal(again.undos.model("call_1").state, "done");
  assert.equal(again.undos.model("call_1").fault, null, "the refusal is gone after a success");
  const gone = make(() => refusal("created"));
  await gone.undos.undo("call_1");
  await gone.undos.undo("call_1");
  assert.equal(gone.calls.length, 1, "a gone change makes no second call");

  // Disposed before the answer: the answer sets nothing.
  let release;
  const late = make(() => new Promise((go) => { release = go; }));
  const pressed = late.undos.undo("call_1");
  const seen = [];
  late.undos.subscribe(() => seen.push("heard"));
  late.undos.dispose();
  release(null);
  await pressed;
  assert.deepEqual(seen, [], "nothing changed after dispose");
  await late.undos.undo("call_2");
  assert.equal(late.calls.length, 1, "a disposed controller makes no call");
});

test("the page's undo reasons are the core's", () => {
  assert.deepEqual([...UNDO_REASONS].sort(), Object.keys(PROTOCOL.ops.ai_undo).sort());
  assert.deepEqual([...UNDO_GONE].sort(), ["created", "unknown_change"]);
  for (const reason of UNDO_GONE) assert.ok(UNDO_REASONS.includes(reason), reason);
  // Undo is offered for a write that ran, and for nothing else.
  const row = (name, state) => ({ id: "c", name, summary: "", state, content: null, bytes: 0 });
  for (const tool of ["write_file", "edit_file"]) {
    assert.equal(canUndo(row(tool, "ran")), true, tool);
    for (const state of ["error", "waiting", "none"]) assert.equal(canUndo(row(tool, state)), false, `${tool} ${state}`);
  }
  for (const tool of ["read_file", "list_dir", "grep", "read_pane", "run_command", "x"]) {
    for (const state of ["ran", "error", "waiting", "none"]) assert.equal(canUndo(row(tool, state)), false, `${tool} ${state}`);
  }
});
