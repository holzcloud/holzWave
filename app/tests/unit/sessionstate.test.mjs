// The pure part of the server sessions in the page (sessionstate.ts, docs/design.md 6.34): the strict reader of the host's list, the
// words of the record, the time, size and directory words, and the decisions of the server row.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import {
  ATTACH,
  END,
  END_ARMED,
  FREE_WORD,
  HELD_WORD,
  HELP_UNKNOWN,
  MAX_SESSIONS,
  NEW_SESSION,
  REMOTE_OFF,
  SERVER_LEGEND,
  STATE_WORDS,
  attachedText,
  dirText,
  durationWords,
  helpLine,
  readSessions,
  rowPlan,
  rowWords,
  serverName,
  sizeText,
  startText,
  untitled,
  OlderLatch,
  PANE_STATE_WORDS,
  UNREACHABLE_LINE,
  endedText,
  paneState,
} from "../../dist/sessionstate.js";
import { LOCKED_LINE, SIGN_IN_LINK, SIGN_OUT, SIGN_OUT_ARMED } from "../../dist/serverstate.js";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const record = readFileSync(join(ROOT, "docs", "design.md"), "utf8");
const section = record.slice(record.indexOf("### 6.34 Server connection and sessions"), record.indexOf("### 6.35 Devices and signing in to a server"));
const quoted = (word) => section.includes("`" + word + "`");

const ROW = { id: 3, title: "cargo build", cwd: "/home/holz/holzWave", cols: 120, rows: 40, started: 1_700_000_000, attached: 1, held: true };
const VIEW = { state: "signed_in", url: "https://holz.example.net", name: "Holz MacBook", locked: false, syncOn: false, remoteOn: true, error: null };

// ---- the words ---------------------------------------------------------------------------------------------------------------------

test("every word of the server row and the session rows is a word of the record", () => {
  for (const word of [SERVER_LEGEND, NEW_SESSION, ATTACH, END, END_ARMED, FREE_WORD, HELD_WORD, REMOTE_OFF, SIGN_IN_LINK, SIGN_OUT, SIGN_OUT_ARMED, LOCKED_LINE]) {
    assert.ok(quoted(word), `the record has no word ${word}`);
  }
  for (const word of Object.values(STATE_WORDS)) assert.ok(quoted(word), `the record has no state word ${word}`);
  assert.deepEqual(Object.values(STATE_WORDS), ["signed in", "not signed in", "signing in…", "unreachable"]);
  // The patterns of the table: `N attached` and `Session N`.
  assert.ok(quoted("N attached") && attachedText(0) === "0 attached" && attachedText(3) === "3 attached");
  assert.ok(quoted("Session N") && untitled(2) === "Session 2");
  assert.equal(sizeText(120, 40), "120×40");
  assert.ok(section.includes("`120×40`"), "the record writes the size with the multiplication sign");
});

test("the help line is the record's, with the idle limit in words when the page knows it and by name when it does not", () => {
  assert.ok(quoted(HELP_UNKNOWN), "the line without the limit is the record's");
  assert.equal(helpLine(null), HELP_UNKNOWN);
  assert.ok(quoted(helpLine(86_400)), "a day is the record's line");
  assert.equal(durationWords(6 * 3600), "6 hours");
  assert.equal(durationWords(3600), "an hour");
  assert.equal(durationWords(2 * 86_400), "2 days");
  assert.equal(durationWords(30 * 60), "30 minutes");
  assert.equal(durationWords(59), null);
  assert.equal(durationWords(61), null, "a limit the page cannot say in round words is said by name");
  assert.equal(helpLine(61), HELP_UNKNOWN);
});

test("the start of a session is a clock today, a weekday and a clock within a week, and the date otherwise", () => {
  const now = new Date(2026, 10, 12, 16, 30); // Thursday
  const at = (d) => Math.floor(d.getTime() / 1000);
  assert.equal(startText(at(new Date(2026, 10, 12, 14, 2)), now), "14:02");
  assert.equal(startText(at(new Date(2026, 10, 9, 9, 40)), now), "Mon 09:40");
  assert.equal(startText(at(new Date(2026, 10, 3, 9, 40)), now), "2026-11-03");
  assert.equal(startText(at(new Date(2026, 10, 13, 1, 0)), now), "2026-11-13", "a start in the future (a skewed clock) is a date");
});

test("a directory of the server shows its home as ~ on either convention", () => {
  assert.equal(dirText("/home/holz/holzWave"), "~/holzWave");
  assert.equal(dirText("/Users/holz/core"), "~/core");
  assert.equal(dirText("/srv/data"), "/srv/data");
});

// ---- the reader -------------------------------------------------------------------------------------------------------------------

test("the reader takes the documented rows, bounded, oldest first, and copies no member it does not name", () => {
  const older = { ...ROW, id: 1, title: "", started: 1_600_000_000, held: false, attached: 0, extra: "hwd_" + "A".repeat(43) };
  const rows = readSessions([ROW, older]);
  assert.deepEqual(rows.map((r) => r.id), [1, 3], "oldest first");
  assert.deepEqual(Object.keys(rows[0]).sort(), ["attached", "cols", "cwd", "held", "id", "rows", "started", "title"]);
  assert.ok(!JSON.stringify(rows).includes("hwd_"), "an unknown member never reaches the page");
  const long = readSessions([{ ...ROW, title: "x".repeat(500), cwd: "/" + "d".repeat(900) }]);
  assert.equal(long[0].title.length, 128);
  assert.equal(long[0].cwd.length, 512);
  const hostile = readSessions([{ ...ROW, title: "<img src=x onerror=alert(1)>\u0007‮" }]);
  assert.equal(hostile[0].title, "<img src=x onerror=alert(1)>", "markup stays text; control and direction characters go");
});

test("a malformed row is dropped and a list over the bound or not a list is refused", () => {
  const bad = [
    null, 7, "row", [], { ...ROW, id: -1 }, { ...ROW, id: 1.5 }, { ...ROW, cols: 0 }, { ...ROW, cols: 1001 }, { ...ROW, rows: 501 },
    { ...ROW, title: 4 }, { ...ROW, cwd: null }, { ...ROW, held: "yes" }, { ...ROW, attached: 70_000 }, { ...ROW, started: -5 },
  ];
  assert.deepEqual(readSessions([...bad, ROW]).map((r) => r.id), [3]);
  assert.deepEqual(readSessions([ROW, { ...ROW }]).length, 1, "an id is listed once");
  assert.equal(readSessions(Array.from({ length: MAX_SESSIONS + 1 }, (_, i) => ({ ...ROW, id: i }))), null);
  assert.equal(readSessions(Array.from({ length: MAX_SESSIONS }, (_, i) => ({ ...ROW, id: i }))).length, MAX_SESSIONS);
  for (const raw of [null, undefined, {}, "[]", 3]) assert.equal(readSessions(raw), null);
  assert.deepEqual(readSessions([]), []);
});

test("the words of a row: the title or Session N, the directory with ~, the size, the start, the attached count, held or free", () => {
  const now = new Date(1_700_000_000 * 1000);
  const w = rowWords({ ...ROW, title: "" }, 2, now);
  assert.equal(w.title, "Session 2");
  assert.equal(w.dir, "~/holzWave");
  assert.equal(w.size, "120×40");
  assert.equal(w.attached, "1 attached");
  assert.equal(w.writer, HELD_WORD);
  assert.equal(rowWords({ ...ROW, held: false }, 1, now).writer, FREE_WORD);
  assert.equal(rowWords(ROW, 1, now).title, "cargo build");
});

// ---- the server row ---------------------------------------------------------------------------------------------------------------

test("each state of the link shows the record's word, dot and buttons", () => {
  const plan = (view, failed = false) => rowPlan({ ...VIEW, ...view }, failed);
  assert.deepEqual(
    [plan({}).word, plan({}).dot, plan({}).buttons, plan({}).list, plan({}).help],
    ["signed in", "in", [SIGN_OUT, NEW_SESSION], true, true],
  );
  const off = plan({ remoteOn: false });
  assert.deepEqual([off.buttons, off.list, off.off, off.help], [[SIGN_OUT], false, true, false], "remote off: no New session, no list, the line");
  assert.deepEqual([plan({ state: "signed_out" }).word, plan({ state: "signed_out" }).dot, plan({ state: "signed_out" }).buttons], ["not signed in", "out", [SIGN_IN_LINK]]);
  assert.deepEqual([plan({ state: "signing_in" }).word, plan({ state: "signing_in" }).dot, plan({ state: "signing_in" }).buttons], ["signing in…", "wait", []]);
  assert.deepEqual([plan({ state: "unreachable" }).word, plan({ state: "unreachable" }).dot, plan({ state: "unreachable" }).buttons], ["unreachable", "down", [SIGN_OUT]]);
  const locked = plan({ locked: true });
  assert.deepEqual([locked.buttons, locked.locked], [[], true], "while locked the buttons give way to the locked line");
  const none = plan({ url: "", state: "signed_in" });
  assert.deepEqual([none.word, none.buttons, none.list], ["not signed in", [SIGN_IN_LINK], false], "no link: not signed in, Sign in… only");
  const failed = plan({}, true);
  assert.deepEqual([failed.word, failed.dot, failed.buttons, failed.list], ["unreachable", "down", [SIGN_OUT], false], "a list that cannot be read");
});

test("the name of the server is its host", () => {
  assert.equal(serverName("https://holz.example.net"), "holz.example.net");
  assert.equal(serverName("https://holz.example.net:8443"), "holz.example.net:8443");
  assert.equal(serverName(""), "");
});

// ---- the states of a pane ----------------------------------------------------------------------------------------------------------

const seen = (state, phase = "") => ({ state, remote: { phase } });

test("each state of a pane says the record's word in the context slot, with a dot shape, and the label only where it is known", () => {
  const table = section.slice(section.indexOf("**The states of a pane.**"), section.indexOf("On attach the server sends"));
  const row = (state) => table.split("\n").find((line) => line.startsWith(`| ${state} |`)).split("|").map((c) => c.trim());
  const cases = [
    ["attaching", paneState(seen("connected", "attaching"))],
    ["loading older lines", paneState(seen("connected", "loading_older"))],
    ["resyncing", paneState(seen("connected", "resyncing"))],
    ["session ended", paneState(seen("closed"), { status: 3 })],
    ["reconnecting", paneState(seen("reconnecting"))],
    ["server unreachable", paneState(seen("failed"))],
  ];
  const dots = { "dotted ring": "wait", "as before": null, "hollow with a dash": "failed" };
  for (const [name, view] of cases) {
    const [, , dot, context, line, label] = row(name);
    const word = context.match(/`([^`]+)`/)[1];
    assert.equal(view.text, word.replace("exit N", "exit 3"), `${name}: the context word`);
    assert.equal(view.dot, dots[dot], `${name}: the dot`);
    assert.equal(view.warn, context.includes("warn"), `${name}: the warn tone with its mark and underline`);
    assert.equal(view.label, label === "shown", `${name}: the writer label`);
    assert.ok(line.includes("`"), `${name} has a line on the screen`);
  }
  for (const word of Object.values(PANE_STATE_WORDS)) assert.ok(section.includes(word), `the record has ${word}`);
  assert.ok(quoted(UNREACHABLE_LINE));
  assert.ok(quoted("Session ended · exit N"));
  assert.equal(endedText(0), "Session ended · exit 0");
  assert.equal(endedText(null), "Session ended", "no status: the host's own notice");
  assert.equal(paneState(seen("closed")).text, "Session ended");
  assert.equal(paneState(seen("connecting")).state, "attaching", "a link that said nothing yet is attaching");
  assert.equal(paneState(seen("connected")), null, "a pane that shows its session has no state");
  assert.equal(paneState(seen("connected", "exploding")), null, "an unknown phase reads as none");
});

// ---- older lines -------------------------------------------------------------------------------------------------------------------

test("the older-lines latch asks once at the top, only for a remote pane with more lines, and opens again after the phase", () => {
  const latch = new OlderLatch();
  const remote = (more = true, phase = "") => ({ more, phase });
  const top = [0, 3000, 40];
  const at = (pane, r, scrollbar = top, connected = true) => latch.want(pane, { remote: r, connected, scrollbar });
  assert.equal(at(1, undefined), false, "a pane that is not remote never asks");
  assert.equal(at(2, remote(false)), false, "no more lines on the server");
  assert.equal(at(2, remote(true, "resyncing")), false, "a phase runs");
  assert.equal(at(2, remote(), [5, 3000, 40]), false, "not at the top");
  assert.equal(at(2, remote(), [0, 40, 40]), false, "no more history than the viewport");
  assert.equal(at(2, remote(), top, false), false, "the connection is down");
  assert.equal(at(2, remote()), true, "at the top with more: ask");
  for (let i = 0; i < 20; i++) assert.equal(at(2, remote()), false, "a held scroll key does not ask twice");
  assert.equal(at(2, remote(true, "loading_older")), false, "while the older lines load");
  assert.equal(at(2, remote()), true, "the phase came and went: at the top again it asks again");
  assert.equal(at(2, remote()), false);
  assert.equal(at(2, remote(), [12, 3000, 40]), false, "the person left the top");
  assert.equal(at(2, remote()), true, "and came back");
  latch.forget(2);
  assert.equal(at(2, remote(false)), false, "when more turns false the page stops asking");
});
