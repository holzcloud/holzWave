// URL, path and hash detection over one row of cells, on the compiled output in dist/: no DOM is needed. The input
// is one string per cell column (a wide character is followed by an empty string for its spacer); targets come
// back in column order with both ends inclusive.
import test from "node:test";
import assert from "node:assert/strict";
import { applyFrame } from "../../dist/frames.js";
import {
  cellInk, cutMiddle, cutShown, findTargets, isLinkModifier, linkAction, linkAllowed, linkTextDiffers, mergeTargets, osc8Targets,
  targetAt,
} from "../../dist/links.js";
import { LINK_REFUSED, refusalSentence } from "../../dist/links.js";

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

// ---- links the program marked (OSC 8) --------------------------------------------------------------------

const POLICY = { schemes: ["http", "https", "mailto", "file"], web: false, remote: false };

/** A mirror whose row 1 holds `text` and the spans `links`, built the way the page builds it. */
function mirrorOf(text, links, cols = 40) {
  const msg = {
    pane: 1, seq: 1, full: true, cols, rows: 3, fg: 1, bg: 0, cursor: null, scrollbar: [0, 3, 3], selection: [], matches: [],
    search: null, copy_cursor: null, modes: { alt: false, bracketed_paste: false, mouse: false },
    changed: [{ y: 1, runs: [[text.padEnd(cols), cols, 1, 0, 0]], links }],
  };
  return applyFrame(undefined, msg).mirror;
}

test("osc8Targets gives the spans of a row as targets with the text over their cells", () => {
  const mirror = mirrorOf("see docs here and mail", [[4, 7, "https://example.org/docs"], [18, 21, "mailto:me@example.org"]]);
  const found = osc8Targets(mirror, 1);
  assert.deepEqual(
    found.map((t) => [t.kind, t.source, t.startCol, t.endCol, t.text, t.shown]),
    [["url", "osc8", 4, 7, "https://example.org/docs", "docs"], ["url", "osc8", 18, 21, "mailto:me@example.org", "mail"]],
  );
  assert.deepEqual(osc8Targets(mirror, 0), [], "a row without links");
  assert.deepEqual(osc8Targets(mirror, 7), [], "a row outside the grid");
  assert.deepEqual(osc8Targets(mirror, 1.5), []);
  assert.deepEqual(osc8Targets(mirror, 1, (uri) => uri.startsWith("mailto:")).map((t) => t.startCol), [18], "the filter decides");
});

test("a wide character's spacer is not in the shown text", () => {
  const wide = applyFrame(undefined, {
    pane: 1, seq: 1, full: true, cols: 10, rows: 1, fg: 1, bg: 0, cursor: null, scrollbar: [0, 1, 1], selection: [], matches: [],
    search: null, copy_cursor: null, modes: { alt: false, bracketed_paste: false, mouse: false },
    changed: [{ y: 0, runs: [["a中\0b      ", 10, 1, 0, 16]], links: [[0, 3, "https://example.org/"]] }],
  }).mirror;
  assert.equal(osc8Targets(wide, 0)[0].shown, "a中b");
});

test("the scheme filter: the person's list, the five that no list can enable, the browser and the file rules", () => {
  const ok = (uri, over = {}) => linkAllowed(uri, { ...POLICY, ...over });
  assert.equal(ok("https://example.org/a"), true);
  assert.equal(ok("HTTPS://example.org/a"), true, "a scheme in capitals is the same scheme");
  assert.equal(ok("mailto:me@example.org"), true);
  assert.equal(ok("file:///home/u/a.txt"), true);
  assert.equal(ok("ssh://host/x"), false, "not in the list");
  assert.equal(ok("ssh://host/x", { schemes: ["ssh"] }), true, "in the list");
  assert.equal(ok("https://example.org/a", { schemes: ["https"] }), true);
  assert.equal(ok("mailto:me@example.org", { schemes: ["https"] }), false);
  assert.equal(ok("https://example.org/a", { schemes: [] }), false, "an empty list: nothing is a link");
  for (const bad of ["javascript:alert(1)", "data:text/html,x", "vbscript:x", "blob:https://x/y", "about:blank", "JavaScript:alert(1)"]) {
    assert.equal(ok(bad, { schemes: ["javascript", "data", "vbscript", "blob", "about"] }), false, `${bad} is never a link`);
  }
  assert.equal(ok("file:///a", { remote: true }), false, "a pane with a connection has no file link");
  assert.equal(ok("https://example.org/", { remote: true }), true);
  assert.equal(ok("file:///a", { web: true }), false, "the browser has no file link");
  assert.equal(ok("ssh://host/x", { web: true, schemes: ["ssh", "https"] }), false, "the browser opens only the three web schemes");
  assert.equal(ok("https://example.org/", { web: true }), true);
  assert.equal(ok("mailto:me@example.org", { web: true }), true);
  for (const bad of ["https://", "https:///x", "mailto:nobody", "mailto:@", "example.org", "", "https://exa mple.org", "https://x.org/\u0007", `https://x.org/${"a".repeat(2048)}`]) {
    assert.equal(ok(bad), false, JSON.stringify(bad));
  }
});

test("a marked link wins over a guessed target that overlaps it, and a guessed one beside it stays", () => {
  const text = "https://example.org/x and https://other.example/y";
  const guessed = findTargets(row(text));
  assert.equal(guessed.length, 2);
  const mirror = mirrorOf(text, [[0, 20, "https://example.org/z"]]);
  const merged = mergeTargets(osc8Targets(mirror, 1), guessed);
  assert.deepEqual(merged.map((t) => [t.source ?? "text", t.text]), [["osc8", "https://example.org/z"], ["text", "https://other.example/y"]]);
  assert.deepEqual(mergeTargets([], guessed), guessed);
});

test("cutMiddle keeps the scheme, the host and the last 12 characters", () => {
  const url = "https://example.com/downloads/archive/2026/08/holzwave-setup.dmg";
  assert.equal(cutMiddle(url, 200), url, "a target that fits is whole");
  assert.equal(cutMiddle(url, url.length), url);
  assert.equal(cutMiddle(url, 40), "https://example.com/downloa…ve-setup.dmg", "the start of the path, the cut, the last 12");
  assert.equal([...cutMiddle(url, 40)].length, 40);
  assert.equal(cutMiddle(url, 10), "https://example.com…ve-setup.dmg", "never less than the scheme, the host and the tail");
  assert.equal(cutMiddle("https://example.com/a", 5), "https://example.com/a", "when the cut would not be shorter the target stays whole");
  assert.equal(cutMiddle("mailto:me@example.org", 10), "mailto:me@example.org");
  assert.equal(cutMiddle("", 5), "");
});

test("cellInk is the colour a cell is drawn in: the foreground, the background where inverse, the pane's where unknown", () => {
  const mirror = applyFrame(undefined, {
    pane: 1, seq: 1, full: true, cols: 6, rows: 2, fg: 0x111111, bg: 0, cursor: null, scrollbar: [0, 2, 2], selection: [], matches: [],
    search: null, copy_cursor: null, modes: { alt: false, bracketed_paste: false, mouse: false },
    changed: [{ y: 0, runs: [["ab", 2, 0xaa0000, 0x00bb00, 0], ["cd", 2, 0xaa0000, 0x00bb00, 8]] }],
  }).mirror;
  assert.equal(cellInk(mirror, 0, 1), 0xaa0000);
  assert.equal(cellInk(mirror, 0, 2), 0x00bb00);
  assert.equal(cellInk(mirror, 0, 5), 0x111111, "past the runs");
  assert.equal(cellInk(mirror, 1, 0), 0x111111, "a row nobody sent");
});

// ---- the question: does the text say what the link does? ------------------------------------------------

test("linkTextDiffers: equal after the scheme, one trailing slash, blanks and the case of the host are ignored", () => {
  const same = [
    ["example.org", "https://Example.org/"],
    ["https://example.org", "https://example.org"],
    ["http://example.org", "https://example.org/"],
    ["  example.org/docs  ", "https://example.org/docs"],
    ["EXAMPLE.ORG/docs", "https://example.org/docs"],
    ["example.org/docs/", "https://example.org/docs"],
    ["me@example.org", "mailto:me@example.org"],
    ["mailto:me@EXAMPLE.org", "mailto:me@example.org"],
    ["ssh://host/x", "ssh://host/x"],
    ["file:///home/u/a.txt", "file:///home/u/a.txt"],
  ];
  for (const [shown, target] of same) assert.equal(linkTextDiffers(shown, target), false, `${shown} == ${target}`);
});

test("linkTextDiffers: a different word, path, host, query or case in the path is a difference", () => {
  const differ = [
    ["docs", "https://example.org/docs"],
    ["click here", "https://example.org/docs"],
    ["https://example.org/a", "https://example.org/b"],
    ["example.org/docs", "https://example.org/Docs"], // the path is case-sensitive
    ["example.org", "https://example.org/docs"],
    ["https://paypal.com", "https://evil.example"],
    ["paypal.com", "https://paypal.com.evil.example/"],
    ["https://paypal.com", "https://paypal.com@evil.example/"],
    ["example.org/docs", "https://example.org/docs?next=https://evil.example"],
    ["me@example.org", "mailto:you@example.org"],
    ["Me@example.org", "mailto:me@example.org"], // the part before @ is case-sensitive
    ["example.org", "https://example.org:8443/"],
    ["example.org", "ssh://example.org/"], // not a web scheme: it is not ignored
  ];
  for (const [shown, target] of differ) assert.equal(linkTextDiffers(shown, target), true, `${shown} != ${target}`);
});

test("linkTextDiffers: a blank text differs from every target, so a hidden link asks", () => {
  for (const shown of ["", " ", "   ", "\t", "\0", " \0 "]) assert.equal(linkTextDiffers(shown, "https://example.org/"), true, JSON.stringify(shown));
  assert.equal(linkTextDiffers("", ""), true, "even against an empty target");
});

test("linkTextDiffers: the NUL that marks the second cell of a wide character is ignored", () => {
  assert.equal(linkTextDiffers("exa\0mple.org", "https://example.org/"), false);
  assert.equal(linkTextDiffers("中\0.example", "https://中.example/"), false);
  assert.equal(linkTextDiffers("中\0.example", "https://xn--fiq.example/"), true, "a punycode host is not the same text");
});

test("cutShown cuts the visible text at 120 code points with an ellipsis and leaves a shorter one alone", () => {
  assert.equal(cutShown("short"), "short");
  assert.equal(cutShown("a".repeat(120)), "a".repeat(120));
  assert.equal(cutShown("a".repeat(121)), `${"a".repeat(120)}…`);
  assert.equal([...cutShown("\u{1F600}".repeat(130))].length, 121, "code points, not UTF-16 units: a pair is not split");
  assert.equal(cutShown("abcdef", 3), "abc…");
});

// ---- the answer to a click the host refuses ----------------------------------------------------------------

test("refusalSentence: a plain host sentence is said as it is, cut at 200 code points", () => {
  const host = "file links are not in the list of link schemes in the settings";
  assert.equal(refusalSentence(host), host);
  assert.equal(refusalSentence("a".repeat(200)), "a".repeat(200));
  assert.equal(refusalSentence("a".repeat(500)), "a".repeat(200));
  assert.equal([...refusalSentence("\u{1F600}".repeat(300))].length, 200, "code points: a pair is not split");
  assert.equal(refusalSentence("<b>x</b> & more"), "<b>x</b> & more", "the text is not changed: it is shown as text");
});

test("refusalSentence: anything that is not a plain string is the one fixed sentence", () => {
  assert.equal(LINK_REFUSED, "The link could not be opened.");
  const bad = [
    undefined, null, 0, true, {}, { text: "no" }, ["a"], new Error("x"), "", " ", "\t", "a\u0007b", "a\nb", "a\u0000b", "a\u007Fb",
    "a\u202Eb", "a\u2066b", "a\u200Bb", "a\uFEFFb", "a\u2028b", "a\u2029b", "a\uD800b", "a\uE000b",
  ];
  for (const reason of bad) assert.equal(refusalSentence(reason), LINK_REFUSED, JSON.stringify(reason) ?? String(reason));
});
