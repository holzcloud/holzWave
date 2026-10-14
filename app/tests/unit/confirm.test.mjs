// The words and the decisions of the close confirmation (what closes, how many shells, what else ends) on the compiled
// output in dist/. None of it touches the DOM at import, so node imports the module.
import test from "node:test";
import assert from "node:assert/strict";
import { closeWording, endsWith, paneAsk, tabAsk, windowPanes, workspaceAsk } from "../../dist/confirm.js";

const view = (over = {}) => ({
  workspaces: [{ name: "alpha", active: true, panes: 4 }, { name: "beta", active: false, panes: 1 }],
  tabs: [{ title: "one", active: true, focus: 1, panes: 3 }, { title: "two", active: false, focus: 5, panes: 1 }],
  focus: 1,
  panes: [[1, {}], [2, {}], [3, {}]],
  windows: 1,
  ...over,
});
const lone = (windows) =>
  view({ workspaces: [{ name: "a", active: true, panes: 2 }], tabs: [{ title: "t", active: true, focus: 1, panes: 2 }], windows });

test("a window holds the panes of every workspace; a view without counts holds the shown panes", () => {
  assert.equal(windowPanes(view()), 5);
  assert.equal(windowPanes(view({ workspaces: [{ name: "a", active: true }, { name: "b", active: false }] })), 3);
});

test("only everything in the window ends the window or the app, and the number of windows tells which", () => {
  assert.equal(endsWith(view(), 4), "nothing");
  assert.equal(endsWith(view(), 5), "app");
  assert.equal(endsWith(view({ windows: 2 }), 5), "window");
  assert.equal(endsWith(view({ windows: 3 }), 1), "nothing");
  assert.equal(endsWith(view({ windows: undefined }), 5), "app", "a view without the count is the only window");
});

test("the ask of a tab, a workspace and a pane names it and counts its panes", () => {
  assert.deepEqual(tabAsk(view(), 0, "one"), { kind: "tab", name: "one", panes: 3, ends: "nothing" });
  assert.deepEqual(tabAsk(view(), 1, "two"), { kind: "tab", name: "two", panes: 1, ends: "nothing" });
  assert.equal(tabAsk(view(), 2, "x"), null, "no such tab");
  assert.deepEqual(workspaceAsk(view(), 0, "alpha"), { kind: "workspace", name: "alpha", panes: 4, ends: "nothing" });
  assert.equal(workspaceAsk(view(), 2, "x"), null, "no such workspace");
  assert.deepEqual(paneAsk(view(), "api"), { kind: "pane", name: "api", panes: 1, ends: "nothing" });
  assert.equal(workspaceAsk(lone(1), 0, "a")?.ends, "app");
  assert.equal(tabAsk(lone(2), 0, "t")?.ends, "window");
  assert.equal(paneAsk(view({ workspaces: [{ name: "a", active: true, panes: 1 }], windows: 1 }), "p").ends, "app");
});

test("a view without counts asks about one pane", () => {
  const old = view({ tabs: [{ title: "one", active: true, focus: 1 }] });
  assert.equal(tabAsk(old, 0, "one")?.panes, 1);
});

test("the wording says what closes, how many shells end and what else ends", () => {
  assert.deepEqual(closeWording({ kind: "tab", name: "two", panes: 1, ends: "nothing" }), {
    title: "Close tab “two”?",
    body: "The shell in it ends.",
    ok: "Close",
  });
  assert.equal(closeWording({ kind: "workspace", name: "alpha", panes: 4, ends: "nothing" }).body, "The 4 shells in it end.");
  assert.equal(
    closeWording({ kind: "pane", name: "p", panes: 1, ends: "app" }).body,
    "The shell in it ends. It is everything holzWave has open, so holzWave quits.",
  );
  assert.equal(
    closeWording({ kind: "tab", name: "t", panes: 2, ends: "window" }).body,
    "The 2 shells in it end. It is everything in this window, so the window closes.",
  );
  assert.equal(closeWording({ kind: "pane", name: "", panes: 1, ends: "nothing" }).title, "Close pane?", "no name, no quotes");
});

test("a long name is cut to 40 characters in the question", () => {
  const long = "x".repeat(100);
  const title = closeWording({ kind: "tab", name: long, panes: 1, ends: "nothing" }).title;
  assert.equal(title, `Close tab “${"x".repeat(40)}…”?`);
  const emoji = "\u{1F332}".repeat(41);
  assert.equal(closeWording({ kind: "tab", name: emoji, panes: 1, ends: "nothing" }).title, `Close tab “${"\u{1F332}".repeat(40)}…”?`, "cut by characters, not by UTF-16 units");
});
