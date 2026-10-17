// The pure half of the conversation list (chatlist.ts): how the core's list of stored chats is read and what of it gets
// through, and how an age is told. The golden of the stored summary (tests/fixtures/ai_chats.json) is written by the core.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { LIST_NOTES, ageWords, armWords, headTitle, listRows, readChatList } from "../../dist/chatlist.js";

const GOLDEN = JSON.parse(readFileSync(new URL("../fixtures/ai_chats.json", import.meta.url), "utf8"));
const MEMBERS = ["created", "id", "model", "profile", "title", "updated"];

test("stored chats are read member by member and nothing but short clean text gets through", () => {
  const input = { chats: [GOLDEN.summary, GOLDEN.bare], notes: ["chat-x.json: damaged"] };
  const before = JSON.stringify(input);
  const list = readChatList(input);
  assert.equal(JSON.stringify(input), before, "reading changes none of its inputs");
  assert.equal(list.chats.length, 2);
  assert.deepEqual(list.chats.map((c) => c.id), ["chat-golden", "chat-bare"], "the core's order");
  for (const chat of list.chats) assert.deepEqual(Object.keys(chat).sort(), MEMBERS, "exactly the six members");
  assert.deepEqual(list.chats[0], {
    id: "chat-golden", title: "Why is talos-3 NotReady?", created: 1700000000, updated: 1700003600, profile: "local-1", model: "llama3.2",
  });
  assert.equal(list.chats[1].profile, "", "a null profile is the empty string");
  assert.equal(list.chats[1].model, "", "a null model is the empty string");
  assert.deepEqual(list.notes, ["chat-x.json: damaged"]);

  // Entries that are not a chat are dropped; members that are not in the contract never come through.
  const hostile = readChatList({
    chats: [
      { ...GOLDEN.summary, id: "../x" }, { ...GOLDEN.summary, id: "" }, { ...GOLDEN.summary, id: "a".repeat(65) },
      { ...GOLDEN.summary, id: 7 }, { ...GOLDEN.summary, id: null }, "text", null, 5, [GOLDEN.summary],
      { ...GOLDEN.bare, messages: [{ text: "x" }], api_key: "sk-zq", context: "c" },
      JSON.parse('{"id":"chat-proto","title":"t","created":1,"updated":2,"profile":null,"model":null,"__proto__":{"id":"evil","polluted":true}}'),
    ],
    notes: [],
  });
  assert.deepEqual(hostile.chats.map((c) => c.id), ["chat-bare", "chat-proto"]);
  for (const chat of hostile.chats) {
    assert.deepEqual(Object.keys(chat).sort(), MEMBERS);
    assert.equal(Object.hasOwn(chat, "messages") || Object.hasOwn(chat, "api_key") || Object.hasOwn(chat, "polluted"), false);
  }
  assert.equal({}.polluted, undefined, "nothing reached the prototype");

  // A title is one short clean line.
  const messy = readChatList({
    chats: [{ ...GOLDEN.summary, title: `a\u0000b‮c\nd\r\ne${"x".repeat(300)}`, profile: "p‮1", model: "m\n2" }],
    notes: [],
  }).chats[0];
  assert.equal(messy.title.includes("\n"), false);
  assert.equal(/[\u0000-\u001f‮]/.test(messy.title + messy.profile + messy.model), false);
  assert.equal(Array.from(messy.title).length, 80, "cut at 80 characters");
  assert.equal(messy.profile, "p 1");
  assert.equal(messy.model, "m 2");

  // Times are finite non-negative numbers, floored; anything else reads 0.
  const times = readChatList({
    chats: [
      { ...GOLDEN.bare, created: 12.9, updated: 99.5 }, { ...GOLDEN.bare, id: "c2", created: -1, updated: NaN },
      { ...GOLDEN.bare, id: "c3", created: Infinity, updated: "7" }, { ...GOLDEN.bare, id: "c4", created: null, updated: undefined },
    ],
    notes: [],
  }).chats;
  assert.deepEqual(times.map((c) => [c.created, c.updated]), [[12, 99], [0, 0], [0, 0], [0, 0]]);

  // Counts are capped, notes are cleaned and cut.
  const many = readChatList({
    chats: Array.from({ length: 501 }, (_, i) => ({ ...GOLDEN.bare, id: `chat-${i}` })),
    notes: Array.from({ length: 7 }, (_, i) => (i === 0 ? `n‮${"y".repeat(400)}\n` : `note ${i}`)),
  });
  assert.equal(many.chats.length, 500);
  assert.equal(many.notes.length, 5);
  assert.equal(Array.from(many.notes[0]).length, 300);
  assert.equal(many.notes[0].includes("‮"), false);

  for (const bad of [{}, { chats: "x" }, "x", null, undefined, 5, [], { chats: {}, notes: "n" }]) {
    assert.deepEqual(readChatList(bad), { chats: [], notes: [] });
  }
});

test("an age is told from the core's seconds", () => {
  const t = 1700000000;
  assert.equal(ageWords(t, t), "now");
  assert.equal(ageWords(t, t + 59), "now");
  assert.equal(ageWords(t, t + 60), "1 min ago");
  assert.equal(ageWords(t, t + 7200), "2 h ago");
  assert.equal(ageWords(t, t + 3 * 86400), "3 d ago");
  assert.equal(ageWords(t, t + 14 * 86400), "2 w ago");
  assert.equal(ageWords(t, t + 90 * 86400), "3 mo ago");
  assert.equal(ageWords(t + 100, t), "now", "a time in the future of now");
  for (const bad of [0, -5, NaN, Infinity]) assert.equal(ageWords(bad, t), "", `${bad}`);
  assert.equal(ageWords(t, NaN), "");
});

const NOW = 1700010000;
const A = { id: "A", title: "Why is talos-3 NotReady?", created: 1, updated: NOW - 7200, profile: "local-1", model: "llama3.2" };
const B = { id: "B", title: "Plan the backup job", created: 1, updated: NOW - 3 * 86400, profile: "", model: "llama3.2" };

test("the chat on screen is always a row, a chat open in a pane is marked, and only a saved chat outside a pane can be deleted", () => {
  const rows = listRows([A, B], "B", "", new Map(), NOW);
  assert.deepEqual(rows.map((r) => [r.id, r.mark, r.current, r.canDelete]), [["A", "○", false, true], ["B", "●", true, true]]);
  assert.equal(rows[0].meta, "llama3.2 · 2 h ago");
  assert.equal(rows[0].pane, null);

  // A chat with no file yet is the first row, titled and told as new.
  const fresh = listRows([A], "chat-new", "", new Map(), NOW);
  assert.deepEqual(fresh[0], { id: "chat-new", mark: "●", title: "new chat", meta: "nothing sent yet", current: true, pane: null, canDelete: false });
  assert.deepEqual([fresh[1].id, fresh[1].mark], ["A", "○"]);
  assert.equal(listRows([A], "chat-new", "Hello there", new Map(), NOW)[0].title, "Hello there");

  // A chat that is listed is not added twice.
  assert.equal(listRows([A, B], "A", "", new Map(), NOW).filter((r) => r.id === "A").length, 1);

  // A chat in a pane says so instead of its age and has no delete.
  const paned = listRows([A, B], "A", "", new Map([["B", 3]]), NOW).find((r) => r.id === "B");
  assert.equal(paned.pane, 3);
  assert.equal(paned.meta, "llama3.2 · in pane 3");
  assert.equal(paned.canDelete, false);

  // Titles and meta pieces.
  assert.equal(listRows([{ ...A, title: "" }], "A", "", new Map(), NOW)[0].title, "untitled chat");
  assert.equal(listRows([{ ...A, model: "" }], "x", "", new Map(), NOW)[1].meta, "2 h ago");
  assert.equal(listRows([{ ...A, updated: 0 }], "x", "", new Map(), NOW)[1].meta, "llama3.2");
  assert.equal(listRows([{ ...A, model: "", updated: 0 }], "x", "", new Map(), NOW)[1].meta, "");
  const long = readChatList({ chats: [{ ...GOLDEN.summary, title: "t".repeat(200) }] }).chats;
  assert.equal(Array.from(listRows(long, "x", "", new Map(), NOW)[1].title).length, 80);

  // Never more rows than the core keeps, plus the one on screen.
  const many = Array.from({ length: 600 }, (_, i) => ({ ...A, id: `chat-${i}` }));
  assert.ok(listRows(many, "chat-new", "", new Map(), NOW).length <= 501);
});

test("the words of the list are fixed sentences", () => {
  assert.equal(headTitle(""), "new chat");
  assert.equal(headTitle("  Why  is\u202e x\n"), "Why is x");
  assert.equal(headTitle("\u202e\n"), "new chat");
  assert.equal(armWords(false), "×");
  assert.equal(armWords(true), "Delete?");
  assert.deepEqual({ ...LIST_NOTES }, {
    deleted: "Chat deleted.", otherTab: "That chat is open in another tab.", unreadable: "The list was not understood.",
  });
  for (const word of [...Object.values(LIST_NOTES), armWords(true), armWords(false), headTitle("")]) {
    assert.equal(/:\/\/|[<>&"]/.test(word), false, word);
  }
});
