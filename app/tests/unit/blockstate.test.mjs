// The reading and the words of block panes and file lists (blockstate.ts): no DOM is needed.
import test from "node:test";
import assert from "node:assert/strict";
import {
  baseName, blockContext, blockTitle, chatTitles, entryIcon, goneChats, humanSize, joinPath, paneBlocks, readBlocks, readDigest, readListing, readSaved,
  setChatTitle, setPaneBlocks, shortDate, visibleEntries,
} from "../../dist/blockstate.js";

test("the list of blocks is read entry by entry and what is not well formed is dropped", () => {
  const list = readBlocks([
    { pane: 1, kind: "files", source: "local", path: "/home", source_label: "This computer" },
    { pane: 2, kind: "terminal", source: "local", path: "/" },
    { pane: "x", kind: "files", source: "local", path: "/" },
    null,
    { pane: 3, kind: "preview", source: "talos", path: "/a.md" },
  ]);
  assert.deepEqual(list.map((b) => b.pane), [1, 3]);
  assert.equal(list[1].source_label, "talos", "a missing label is the source's id");
});

// The object the core pins for a chat block (blocks.rs, a_chat_block_holds_a_conversation_id_and_survives_a_restart).
const CHAT = { pane: 1, kind: "chat", source: "local", path: "chat-golden", source_label: "This computer" };

test("a chat block is read with its conversation id and a bad id drops only that entry", () => {
  const files = { pane: 5, kind: "files", source: "local", path: "/home", source_label: "This computer" };
  const sent = structuredClone([
    CHAT,
    { ...CHAT, pane: 2, path: "../x" },
    { ...CHAT, pane: 3, path: "" },
    { ...CHAT, pane: 4, path: "a b" },
    { ...CHAT, pane: 6, path: "a".repeat(65) },
    { ...CHAT, pane: 7, path: 42 },
    files,
  ]);
  const list = readBlocks(sent);
  assert.deepEqual(list, [CHAT, files]);
  assert.deepEqual(list[0], sent[0], "the block comes back as it was passed in");
});

test("a chat block has the words of a chat", () => {
  chatTitles.clear();
  assert.equal(blockTitle(CHAT), "chat");
  assert.equal(blockContext(CHAT), "new chat", "a chat with no title yet says so");
  const monitor = { pane: 2, kind: "monitor", source: "local", path: "", source_label: "This computer" };
  assert.equal(blockTitle(monitor), "system");
  assert.equal(blockContext(monitor), "local \u00b7 1 s");
});

test("a chat block shows the title of its chat", () => {
  chatTitles.clear();
  assert.equal(blockContext(CHAT), "new chat");
  assert.equal(setChatTitle("chat-golden", "Why is talos-3 NotReady?"), true, "a new title is a change");
  assert.equal(blockContext(CHAT), "Why is talos-3 NotReady?");
  assert.equal(setChatTitle("chat-golden", "Why is talos-3 NotReady?"), false, "the same title is no change");
  assert.equal(blockTitle(CHAT), "chat", "the title word of the header stays");
  // A title is cut at 120 characters and loses control characters.
  setChatTitle("chat-golden", `a\u0000b${"x".repeat(200)}`);
  const cut = blockContext(CHAT);
  assert.equal(Array.from(cut).length, 120);
  assert.ok(cut.startsWith("ab"), "the control character is gone");
  // An empty title puts `new chat` back and the entry is removed.
  assert.equal(setChatTitle("chat-golden", ""), true);
  assert.equal(blockContext(CHAT), "new chat");
  assert.equal(chatTitles.has("chat-golden"), false);
  // The other kinds keep their words.
  const monitor = { pane: 2, kind: "monitor", source: "local", path: "", source_label: "This computer" };
  const files = { pane: 3, kind: "files", source: "local", path: "/home", source_label: "This computer" };
  assert.equal(blockContext(monitor), "local \u00b7 1 s");
  assert.equal(blockContext(files), "This computer \u00b7 /home");
});

test("goneChats lists the chats whose pane left the list", () => {
  const chat = (pane, path) => ({ pane, kind: "chat", source: "local", path, source_label: "This computer" });
  const files = { pane: 1, kind: "files", source: "local", path: "/home", source_label: "This computer" };
  assert.deepEqual(goneChats([files, chat(2, "chat-a"), chat(3, "chat-b")], [chat(3, "chat-b")]), ["chat-a"]);
  assert.deepEqual(goneChats([files, chat(2, "chat-a")], []), ["chat-a"], "an empty list closes every chat");
  assert.deepEqual(goneChats([files], []), [], "a block of another kind is never listed");
  assert.deepEqual(goneChats([chat(2, "chat-a")], [chat(2, "chat-a"), files]), [], "equal lists lose nothing");
  assert.deepEqual(goneChats([], [chat(2, "chat-a")]), [], "a new chat is not gone");
  assert.deepEqual(goneChats([chat(2, "chat-a"), chat(3, "chat-a")], []), ["chat-a"], "a chat is listed once");
});

test("a chat that moved to another pane is not gone", () => {
  const chat = (pane, path) => ({ pane, kind: "chat", source: "local", path, source_label: "This computer" });
  assert.deepEqual(goneChats([chat(2, "chat-a")], [chat(5, "chat-a")]), []);
  assert.deepEqual(goneChats([chat(2, "chat-a"), chat(3, "chat-b")], [chat(2, "chat-b"), chat(3, "chat-a")]), []);
});

test("the same answer twice is no change", () => {
  paneBlocks.clear();
  const one = [{ pane: 1, kind: "files", source: "local", path: "/", source_label: "This computer" }];
  assert.equal(setPaneBlocks(one), true);
  assert.equal(setPaneBlocks(structuredClone(one)), false);
  assert.equal(setPaneBlocks([]), true);
});

test("a listing is read with its entries cleaned", () => {
  const l = readListing({
    path: "/home/holz",
    parent: "/home",
    entries: [
      { name: "docs", kind: "dir", link: false, size: 0, modified: 100, hidden: false },
      { name: "", kind: "file" },
      { name: "a.txt", kind: "weird", size: -5, modified: "x", hidden: true },
    ],
    truncated: true,
  });
  assert.equal(l.parent, "/home");
  assert.equal(l.truncated, true);
  assert.deepEqual(l.entries.map((e) => [e.name, e.kind, e.size, e.modified]), [["docs", "dir", 0, 100], ["a.txt", "other", 0, null]]);
  assert.equal(readListing(null), null);
  assert.equal(readListing({ path: 3 }), null);
});

test("paths are joined and cut with the separator they use", () => {
  assert.equal(joinPath("/home/holz", "a.txt"), "/home/holz/a.txt");
  assert.equal(joinPath("/", "etc"), "/etc");
  assert.equal(joinPath("C:\\Users\\holz", "a.txt"), "C:\\Users\\holz\\a.txt");
  assert.equal(baseName("/home/holz/notes.md"), "notes.md");
  assert.equal(baseName("C:\\Users\\holz\\notes.md"), "notes.md");
  assert.equal(baseName("/home/holz/"), "holz");
  assert.equal(baseName("/"), "/");
});

test("an icon is a shape for the kind of file", () => {
  assert.equal(entryIcon({ name: "src", kind: "dir" }), "▸");
  assert.equal(entryIcon({ name: "shot.PNG", kind: "file" }), "▣");
  assert.equal(entryIcon({ name: "demo.mp4", kind: "file" }), "▶");
  assert.equal(entryIcon({ name: "README.md", kind: "file" }), "▤");
  assert.equal(entryIcon({ name: "main.rs", kind: "file" }), "✎");
  assert.equal(entryIcon({ name: "sock", kind: "other" }), "·");
});

test("sizes read like a person would say them", () => {
  assert.equal(humanSize(0), "0 B");
  assert.equal(humanSize(1023), "1023 B");
  assert.equal(humanSize(1536), "1.5 KB");
  assert.equal(humanSize(12 * 1024 * 1024), "12 MB");
  assert.equal(humanSize(5 * 1024 ** 3), "5 GB");
  assert.equal(humanSize(-1), "");
});

test("a date shows the time this year and the year before", () => {
  const now = new Date(2026, 9, 15, 12, 0);
  assert.equal(shortDate(new Date(2026, 9, 15, 14, 2).getTime() / 1000, now), "Oct 15 14:02");
  assert.equal(shortDate(new Date(2025, 2, 3, 9, 0).getTime() / 1000, now), "Mar  3  2025");
  assert.equal(shortDate(null, now), "");
});

test("a header says what a block shows", () => {
  const files = { pane: 1, kind: "files", source: "local", path: "/home/holz", source_label: "This computer" };
  const preview = { pane: 2, kind: "preview", source: "talos", path: "/srv/notes.md", source_label: "Talos" };
  assert.equal(blockTitle(files), "files");
  assert.equal(blockContext(files), "This computer · /home/holz");
  assert.equal(blockTitle(preview), "notes.md");
  assert.equal(blockContext(preview), "Talos · preview");
});

test("hidden entries are shown only when asked", () => {
  const list = [{ name: ".git", hidden: true }, { name: "src", hidden: false }];
  assert.deepEqual(visibleEntries(list, false).map((e) => e.name), ["src"]);
  assert.deepEqual(visibleEntries(list, true).map((e) => e.name), [".git", "src"]);
});

test("readSaved accepts the old and the new answer of a save", () => {
  const digest = "0123456789abcdef".repeat(4);
  assert.deepEqual(readSaved({ modified: 1760000099, digest }), { modified: 1760000099, digest });
  assert.deepEqual(readSaved(1760000099), { modified: 1760000099, digest: null }, "a bare number is the older answer");
  assert.deepEqual(readSaved({ modified: null, digest }), { modified: null, digest }, "a file whose time is unknown");
  assert.deepEqual(readSaved(null), { modified: null, digest: null });
  assert.deepEqual(readSaved({ modified: "x", digest: "XYZ" }), { modified: null, digest: null }, "anything else is dropped");
  assert.deepEqual(readSaved(Number.NaN), { modified: null, digest: null });
});

test("a digest is 64 lowercase hex characters and nothing else", () => {
  const ok = "ab".repeat(32);
  assert.equal(readDigest(ok), ok);
  for (const bad of [null, undefined, 5, "", "ab", ok.toUpperCase(), `${ok}0`, "g".repeat(64)]) assert.equal(readDigest(bad), null);
});
