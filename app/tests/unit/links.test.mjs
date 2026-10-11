// URL, path and hash detection over one row of cells, on the compiled output in dist/: no DOM is needed. The input
// is one string per cell column (a wide character is followed by an empty string for its spacer); targets come
// back in column order with both ends inclusive.
import test from "node:test";
import assert from "node:assert/strict";
import { findTargets, isLinkModifier, linkAction, targetAt } from "../../dist/links.js";

/** A row of narrow cells: one string per character. */
const row = (text) => Array.from(text);
const kinds = (targets) => targets.map((t) => `${t.kind}:${t.text}`);

test("the module imports and runs without a DOM", () => {
  assert.equal(typeof document, "undefined");
  assert.equal(typeof findTargets, "function");
});

test("two URLs: a trailing comma and an unbalanced closing parenthesis and the final period are not part of them", () => {
  const text = "see https://example.com/a?b=1, and (https://x.org/p).";
  const found = findTargets(row(text));
  assert.deepEqual(kinds(found), ["url:https://example.com/a?b=1", "url:https://x.org/p"]);
  const first = "https://example.com/a?b=1";
  assert.equal(found[0].startCol, text.indexOf(first));
  assert.equal(found[0].endCol, text.indexOf(first) + first.length - 1, "the end column is the last cell, inclusive");
  const second = "https://x.org/p";
  assert.equal(found[1].startCol, text.indexOf(second));
  assert.equal(found[1].endCol, text.indexOf(second) + second.length - 1);
});

test("a closing parenthesis that has its opening one stays in the URL", () => {
  assert.deepEqual(kinds(findTargets(row("https://en.wikipedia.org/wiki/Foo_(bar)"))), ["url:https://en.wikipedia.org/wiki/Foo_(bar)"]);
  assert.deepEqual(kinds(findTargets(row("(https://en.wikipedia.org/wiki/Foo_(bar))."))), ["url:https://en.wikipedia.org/wiki/Foo_(bar)"]);
  assert.deepEqual(kinds(findTargets(row("[https://x.org/a]"))), ["url:https://x.org/a"]);
});

test("sentence punctuation and quotes after a URL are cut", () => {
  for (const [text, want] of [
    ["https://example.com.", "https://example.com"],
    ["go to https://example.com/x!", "https://example.com/x"],
    ["'https://example.com/x'", "https://example.com/x"],
    ['"https://example.com/x";', "https://example.com/x"],
    ["https://example.com/a;b?c=d", "https://example.com/a;b?c=d"],
  ]) {
    assert.deepEqual(kinds(findTargets(row(text))), [`url:${want}`], text);
  }
});

test("a mailto address is a URL; one without an address is nothing", () => {
  assert.deepEqual(kinds(findTargets(row("write mailto:alice@example.com now"))), ["url:mailto:alice@example.com"]);
  assert.deepEqual(findTargets(row("mailto: nobody")), []);
  assert.deepEqual(findTargets(row("alice@example.com")), [], "a bare address has no scheme and is not a link");
});

test("only http, https and mailto are links: no file, ftp, javascript or custom scheme", () => {
  for (const text of [
    "file:///etc/passwd", "ftp://example.com/a", "javascript:alert(1)", "data:text/html,hi", "custom://x/y", "ssh://host/p",
  ]) {
    assert.deepEqual(findTargets(row(text)).filter((t) => t.kind === "url"), [], text);
  }
  // A URL does not start in the middle of a word.
  assert.deepEqual(findTargets(row("xhttps://example.com")), []);
});

test("a URL is not reported a second time as a path", () => {
  const found = findTargets(row("https://example.com/a/b/c.txt"));
  assert.deepEqual(kinds(found), ["url:https://example.com/a/b/c.txt"]);
  assert.deepEqual(kinds(findTargets(row("//example.com/a"))), [], "a network path is not a file path");
});

test("absolute, home and relative paths are found", () => {
  assert.deepEqual(kinds(findTargets(row("/abs/path"))), ["path:/abs/path"]);
  assert.deepEqual(kinds(findTargets(row("cd ~/dir/file.txt now"))), ["path:~/dir/file.txt"]);
  assert.deepEqual(kinds(findTargets(row("see ./rel/file.rs"))), ["path:./rel/file.rs"]);
  assert.deepEqual(kinds(findTargets(row("up ../up/file"))), ["path:../up/file"]);
  assert.deepEqual(kinds(findTargets(row("(./a/b.c)"))), ["path:./a/b.c"], "brackets are not part of a path");
  assert.deepEqual(kinds(findTargets(row("see ./a/b.c, and"))), ["path:./a/b.c"], "a trailing comma is not");
  assert.deepEqual(kinds(findTargets(row("PATH=/usr/bin:/bin"))), ["path:/usr/bin", "path:/bin"]);
});

test("a :line or :line:col suffix is captured and covered, not part of the path", () => {
  const one = findTargets(row("at ./src/main.rs:12"));
  assert.equal(one.length, 1);
  assert.deepEqual({ ...one[0] }, { kind: "path", startCol: 3, endCol: 18, text: "./src/main.rs", line: 12 });
  const two = findTargets(row("/a/b.ts:120:34: error"));
  assert.equal(two.length, 1);
  assert.deepEqual({ ...two[0] }, { kind: "path", startCol: 0, endCol: 13, text: "/a/b.ts", line: 120, col: 34 });
  const none = findTargets(row("/a/b.ts: error"));
  assert.equal(none[0].line, undefined);
  assert.equal(none[0].endCol, 6, "a colon without digits is not a suffix");
});

test("a bare word, a lone slash and a tilde are not paths", () => {
  for (const text of ["hello", "and/or", "a/b", "/", "~", "~user", "x.rs", "file.txt", ". .. ...", "./", "//"]) {
    assert.deepEqual(findTargets(row(text)).filter((t) => t.kind === "path"), [], JSON.stringify(text));
  }
});

test("a string of 7 to 40 lowercase hex characters with a digit is a hash", () => {
  const forty = "0123456789abcdef0123456789abcdef01234567";
  assert.deepEqual(kinds(findTargets(row("commit 3f2a9c1 done"))), ["hash:3f2a9c1"]);
  assert.deepEqual(kinds(findTargets(row(forty))), [`hash:${forty}`]);
  assert.deepEqual(kinds(findTargets(row("(3f2a9c1)"))), ["hash:3f2a9c1"]);
  for (const text of [
    "deadbee", // no digit
    "3f2a9c", // six characters
    `${forty}0`, // forty-one
    "3F2A9C1", // upper case
    "3f2a9cg1", // not hex
    "v3f2a9c1x", // inside a word
  ]) {
    assert.deepEqual(findTargets(row(text)), [], text);
  }
});

test("a hash inside a URL or a path belongs to that target", () => {
  assert.deepEqual(kinds(findTargets(row("https://x.org/3f2a9c1d"))), ["url:https://x.org/3f2a9c1d"]);
  assert.deepEqual(kinds(findTargets(row("/tmp/abc1234def"))), ["path:/tmp/abc1234def"]);
});

test("columns are cell columns: a wide character takes two", () => {
  // two wide characters, then a URL: the URL starts at column 4, not at string index 2
  const wide = ["中", "", "文", "", ...row("https://x.org")];
  const [url] = findTargets(wide);
  assert.equal(url.text, "https://x.org");
  assert.equal(url.startCol, 4);
  assert.equal(url.endCol, 4 + "https://x.org".length - 1);

  // a wide character inside a path: its second column is covered
  const inside = ["/", "t", "中", "", "x"];
  const [path] = findTargets(inside);
  assert.equal(path.text, "/t中x");
  assert.deepEqual([path.startCol, path.endCol], [0, 4]);

  // a wide character before a path
  const before = ["a", "中", "", ...row(" /tmp/x")];
  assert.deepEqual(findTargets(before).map((t) => [t.text, t.startCol, t.endCol]), [["/tmp/x", 4, 9]]);
});

test("a cluster tail inside a cell is part of the text and a control character ends a target", () => {
  const row1 = [...row("/tmp/e"), "é", ...row("x")];
  assert.deepEqual(findTargets(row1).map((t) => [t.text, t.startCol, t.endCol]), [["/tmp/eéx", 0, 7]]);
  const bell = row("https://x.org/a\u0007b");
  assert.deepEqual(kinds(findTargets(bell)), ["url:https://x.org/a"]);
});

test("targets come in column order and do not overlap", () => {
  const found = findTargets(row("/a/b https://x.org 3f2a9c1 ./c"));
  assert.deepEqual(kinds(found), ["path:/a/b", "url:https://x.org", "hash:3f2a9c1", "path:./c"]);
  for (let i = 1; i < found.length; i++) assert.ok(found[i].startCol > found[i - 1].endCol);
});

test("a very long row is scanned in linear time and an empty or blank row has no targets", () => {
  assert.deepEqual(findTargets([]), []);
  assert.deepEqual(findTargets(row("     ")), []);
  const long = row("a".repeat(20000));
  const t0 = performance.now();
  findTargets(long);
  findTargets(row("/".repeat(20000)));
  findTargets(row("https://" + "a.".repeat(5000)));
  assert.ok(performance.now() - t0 < 1500, "detection took too long on a long row");
});

test("a URL longer than the core accepts is not a link", () => {
  assert.deepEqual(findTargets(row(`https://x.org/${"a".repeat(2100)}`)), []);
});

test("targetAt finds the target that covers a column", () => {
  const found = findTargets(row("go https://x.org now"));
  assert.equal(targetAt(found, 2), null);
  assert.equal(targetAt(found, 3).kind, "url");
  assert.equal(targetAt(found, 15).kind, "url");
  assert.equal(targetAt(found, 16), null);
});

test("a click on a URL opens exactly that URL; a path is revealed with the pane's directory as the base", () => {
  const [url] = findTargets(row("https://example.com/a?b=1"));
  assert.deepEqual(linkAction(url, "/home/u"), { command: "open_url", args: { url: "https://example.com/a?b=1" } });
  const [path] = findTargets(row("./src/main.rs:12:3"));
  assert.deepEqual(linkAction(path, "/home/u/proj"), { command: "reveal_path", args: { path: "./src/main.rs", base: "/home/u/proj" } });
  assert.deepEqual(linkAction(path, null), { command: "reveal_path", args: { path: "./src/main.rs", base: null } });
  const [hash] = findTargets(row("3f2a9c1"));
  assert.equal(linkAction(hash, "/x"), null, "a hash is nothing to open");
});

test("a target with another scheme is never turned into an open_url", () => {
  const forged = (text) => ({ kind: "url", startCol: 0, endCol: text.length - 1, text });
  for (const text of ["file:///etc/passwd", "javascript:alert(1)", "ftp://x.org", "https:x", "HTTPS://x.org", "https://x.org/\u0007"]) {
    assert.equal(linkAction(forged(text), null), null, text);
  }
  assert.notEqual(linkAction(forged("mailto:a@b.c"), null), null);
});

test("the link modifier is Cmd on macOS and Ctrl elsewhere", () => {
  assert.equal(isLinkModifier({ metaKey: true }, "macos"), true);
  assert.equal(isLinkModifier({ ctrlKey: true }, "macos"), false, "Ctrl+click is the context menu on macOS");
  assert.equal(isLinkModifier({ ctrlKey: true }, "linux"), true);
  assert.equal(isLinkModifier({ ctrlKey: true }, "windows"), true);
  assert.equal(isLinkModifier({ metaKey: true }, "linux"), false);
  assert.equal(isLinkModifier({ ctrlKey: true }, ""), true, "an unknown platform uses Ctrl");
  assert.equal(isLinkModifier({}, "macos"), false);
});
