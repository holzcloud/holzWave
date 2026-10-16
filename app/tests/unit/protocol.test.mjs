// The TypeScript side of the protocol golden (tests/fixtures/protocol.json, written by the Rust test
// `protocol_samples_match_golden`): the exact key sets and tags the page depends on, and a scan of every call site in
// src/ against the command names and the argument names of the Rust commands. Nothing is imported from the app: the golden and the
// TypeScript sources are read as text.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const golden = JSON.parse(readFileSync(join(here, "../fixtures/protocol.json"), "utf8"));
const srcDir = join(here, "../../src");

const keys = (object) => Object.keys(object).sort();

// ---- payload shapes ---------------------------------------------------------------------------------------------

test("the view has exactly the documented keys at every level", () => {
  const view = golden.view;
  assert.deepEqual(keys(view), ["dividers", "focus", "pane_names", "panes", "rev", "tabs", "windows", "workspaces"]);
  assert.equal(typeof view.rev, "number", "the order of the views, newest wins");
  assert.equal(view.workspaces.length, 2);
  assert.equal(view.tabs.length, 3);
  assert.equal(view.panes.length, 2);
  assert.equal(view.dividers.length, 1);
  assert.ok(Array.isArray(view.pane_names), "the names of the shown tab's panes, as [id, name] pairs");
  for (const w of view.workspaces) assert.deepEqual(keys(w), ["active", "name", "panes"]);
  for (const t of view.tabs) assert.deepEqual(keys(t), ["active", "focus", "panes", "title"]);
  assert.equal(view.windows, 1, "how many windows the app has: the close confirmation says what closing everything ends");
  assert.deepEqual(view.workspaces.map((w) => w.panes), [4, 1], "panes of all tabs of the workspace");
  assert.deepEqual(view.tabs.map((t) => t.panes), [2, 1, 1]);
  for (const pane of view.panes) {
    assert.equal(pane.length, 2, "a pane is [id, rect]");
    assert.equal(typeof pane[0], "number");
    assert.deepEqual(keys(pane[1]), ["h", "w", "x", "y"]);
  }
  for (const d of view.dividers) {
    assert.deepEqual(keys(d), ["area", "axis", "ratio", "split"]);
    assert.deepEqual(keys(d.area), ["h", "w", "x", "y"]);
    assert.ok(d.axis === "Row" || d.axis === "Column", d.axis);
  }
  assert.equal(view.tabs.filter((t) => t.active).length, 1);
  assert.equal(view.workspaces.filter((w) => w.active).length, 1);
});

test("the event names and the title, cwd and pane-exit payloads are pinned", () => {
  assert.deepEqual(golden.events.names, ["frame", "layout", "title", "cwd", "pane-exit", "settings", "incoming-drop", "drag-hover", "drag-end", "ai"]);
  assert.deepEqual(keys(golden.events.title), ["pane", "title"]);
  assert.deepEqual(keys(golden.events.cwd), ["cwd", "pane"]);
  const exits = golden.events["pane-exit"];
  assert.equal(exits.length, 2, "one sample with a code and one without");
  for (const e of exits) assert.deepEqual(keys(e), ["code", "pane"]);
  assert.equal(typeof exits[0].code, "number");
  assert.equal(exits[1].code, null);
});

test("the commands and events between windows are pinned (DESK-29 part 2)", () => {
  // A release over another window reaches that window as `incoming-drop` with the pane and the pointer in its client coordinates.
  const drops = golden.events["incoming-drop"];
  assert.equal(drops.length, 2, "one sample for a pane and one for a tab");
  assert.deepEqual(drops.map((d) => d.kind), ["pane", "tab"]);
  for (const d of drops) {
    assert.deepEqual(keys(d), ["kind", "pane", "x", "y"], "the pane is named: the answer has to carry it");
    assert.equal(typeof d.pane, "number");
    assert.equal(typeof d.x, "number");
    assert.equal(typeof d.y, "number");
  }
  // The live outline across windows: the same payload as the drop (ids and the pointer in the window's own coordinates),
  // and an end that carries nothing.
  const hovers = golden.events["drag-hover"];
  assert.deepEqual(hovers.map((d) => d.kind), ["pane", "tab"]);
  for (const d of hovers) assert.deepEqual(keys(d), ["kind", "pane", "x", "y"]);
  assert.equal(golden.events["drag-end"], null);
  assert.deepEqual(golden.commands.drag_over, ["pane", "kind"], "ids and a kind: the shell reads the pointer");
  assert.deepEqual(golden.commands.drag_end, []);
  assert.deepEqual(golden.ops.drop_kind, ["pane", "tab"]);
  // The answers of release_outside: a window of its own, handed to another window, or nothing.
  assert.deepEqual(golden.answers.release_outside.map((a) => a.outcome), ["torn_off", "handed", "cancelled"]);
  assert.deepEqual(keys(golden.answers.release_outside[0]), ["label", "outcome"]);
  assert.deepEqual(keys(golden.answers.release_outside[1]), ["outcome", "to"]);
  assert.deepEqual(keys(golden.answers.release_outside[2]), ["outcome"]);
  // The gestures carry pane ids and nothing about where the pointer is; the calling window is no argument.
  assert.deepEqual(golden.commands.tear_off_pane, ["pane", "at_cursor"]);
  assert.deepEqual(golden.commands.tear_off_tab, ["pane", "at_cursor"]);
  assert.deepEqual(golden.commands.release_outside, ["pane", "kind"]);
  assert.deepEqual(golden.commands.attach_pane, ["pane", "target", "edge", "workspace"]);
  assert.deepEqual(golden.commands.attach_tab, ["pane", "workspace"]);
  assert.deepEqual(golden.commands.window_ready, []);
  for (const name of ["get_layout", "new_tab"]) assert.deepEqual(golden.commands[name], [], `${name} takes no argument`);
  assert.deepEqual(golden.commands.split, ["column"]);
  for (const [name, args] of Object.entries(golden.commands)) {
    assert.ok(!args.includes("window"), `${name}: the calling window is filled in by Tauri, the page never sends it`);
  }
});

test("the settings view has exactly the documented keys at every level and the settings event carries it", () => {
  const view = golden.settings_view;
  assert.deepEqual(keys(view), ["keybindings", "settings", "theme", "themes", "warnings"]);
  assert.deepEqual(golden.events.settings, view, "the settings event payload is the settings view");
  assert.deepEqual(keys(view.settings), [
    "ai_local_context", "ai_local_only", "ai_persist_context", "confirm_multiline_paste", "contrast", "cursor_blink",
    "cursor_trail", "font_size", "keybindings", "motion", "option_as_alt", "scrollback_lines", "shell", "smooth_scroll",
    "theme", "ui_scale", "version",
  ]);
  assert.equal(typeof view.settings.theme, "string");
  assert.equal(typeof view.settings.option_as_alt, "boolean");
  assert.equal(view.settings.smooth_scroll, false, "DESK-28: the effects are off by default");
  assert.equal(view.settings.cursor_trail, false, "DESK-28: the effects are off by default");
  assert.equal(view.settings.shell, "", "DESK-30: empty means the platform default; the dialog has no row for it");
  assert.equal(view.settings.ai_local_only, false, "the switch that blocks every destination off this computer starts off");
  assert.equal(view.settings.ai_local_context, "ask", "a model on this computer asks like a cloud one by default");
  assert.equal(view.settings.ai_persist_context, false, "terminal text is not kept in a saved chat by default");
  assert.ok(view.themes.length >= 3);
  for (const t of view.themes) {
    assert.deepEqual(keys(t), ["builtin", "kind", "name"]);
    assert.ok(t.kind === "dark" || t.kind === "light", t.kind);
  }
  assert.deepEqual(keys(view.theme), ["chrome", "kind", "name", "terminal", "version"]);
  assert.deepEqual(keys(view.theme.terminal), ["ansi", "background", "cursor", "foreground", "selection", "selection_alpha"]);
  assert.equal(view.theme.terminal.ansi.length, 16);
  assert.deepEqual(keys(view.theme.chrome), [
    "danger", "edge", "ground", "ground_2", "info", "ink", "ink_2", "ink_3", "ok", "pane", "rose", "rose_deep", "warn",
  ]);
  for (const color of [view.theme.terminal.foreground, ...view.theme.terminal.ansi, ...Object.values(view.theme.chrome)]) {
    assert.match(color, /^#[0-9A-F]{6}$/, "every colour is #RRGGBB");
  }
  assert.ok(view.keybindings.length >= 30);
  for (const row of view.keybindings) {
    assert.deepEqual(keys(row), ["action", "chord", "is_default", "title"]);
    assert.ok(row.chord === null || typeof row.chord === "string");
  }
  assert.ok(Array.isArray(view.warnings) && view.warnings.every((w) => typeof w === "string"));
});

test("the app info and the pointer commands are pinned", () => {
  assert.deepEqual(keys(golden.app_info), ["hostname", "os", "version"]);
  for (const value of Object.values(golden.app_info)) assert.equal(typeof value, "string");
  assert.deepEqual(golden.mouse.actions, ["press", "release", "motion"]);
  assert.deepEqual(golden.mouse.buttons, ["left", "middle", "right", "none"]);
  assert.deepEqual(golden.commands.app_info, []);
  assert.deepEqual(golden.commands.open_url, ["url"]);
  assert.deepEqual(golden.commands.reveal_path, ["path", "base"]);
  assert.deepEqual(golden.commands.mouse, ["pane", "action", "button", "x", "y", "mods"]);
  assert.deepEqual(golden.commands.wheel, ["pane", "rows", "x", "y", "mods"]);
  for (const name of ["note_window", "placement"]) {
    assert.ok(!(name in golden.commands), `${name} is a method of the core, not a command`);
  }
});

test("the settings commands take snake_case arguments", () => {
  assert.deepEqual(golden.commands.get_settings, []);
  assert.deepEqual(golden.commands.set_setting, ["key", "value"]);
  assert.deepEqual(golden.commands.reset_setting, ["key"]);
  assert.deepEqual(golden.commands.reload_settings, []);
});

test("the op tags, modes and motions are the exact strings the page sends", () => {
  const select = golden.ops.select;
  assert.deepEqual([...new Set(select.map((o) => o.kind))].sort(), ["all", "begin", "clear", "extend"]);
  for (const o of select) {
    if (o.kind === "begin") assert.deepEqual(keys(o), ["kind", "mode", "x", "y"]);
    if (o.kind === "extend") assert.deepEqual(keys(o), ["kind", "x", "y"]);
    if (o.kind === "all" || o.kind === "clear") assert.deepEqual(keys(o), ["kind"]);
  }
  assert.deepEqual(
    select.filter((o) => o.kind === "begin").map((o) => o.mode).sort(),
    ["cell", "line", "word"],
  );

  const search = golden.ops.search;
  assert.deepEqual([...new Set(search.map((o) => o.kind))].sort(), ["clear", "next", "prev", "set"]);
  for (const o of search) assert.deepEqual(keys(o), o.kind === "set" ? ["kind", "needle"] : ["kind"]);

  const copy = golden.ops.copy_mode;
  assert.deepEqual([...new Set(copy.map((o) => o.kind))].sort(), ["enter", "exit", "move", "toggle", "yank"]);
  for (const o of copy) assert.deepEqual(keys(o), o.kind === "move" ? ["kind", "motion"] : ["kind"]);
  assert.deepEqual(
    copy.filter((o) => o.kind === "move").map((o) => o.motion).sort(),
    [
      "bottom", "down", "left", "line_end", "line_start", "page_down", "page_up",
      "right", "top", "up", "word_back", "word_forward",
    ],
  );
});

test("the paste outcomes and the search and copy-mode answers are pinned", () => {
  assert.deepEqual(golden.paste, ["written", "empty", "rejected"]);
  for (const a of golden.answers.search) assert.deepEqual(keys(a), ["current", "total"]);
  for (const a of golden.answers.copy_mode) assert.deepEqual(keys(a), ["active", "selecting", "yanked"]);
});

test("the command table has snake_case argument names and no raw input command", () => {
  const names = Object.keys(golden.commands);
  assert.ok(names.length >= 20, `only ${names.length} commands`);
  assert.ok(!names.includes("input"), "the raw input command must not exist");
  for (const [name, args] of Object.entries(golden.commands)) {
    for (const arg of args) assert.match(arg, /^[a-z][a-z0-9]*(_[a-z0-9]+)*$/, `${name}: ${arg}`);
  }
  assert.deepEqual(golden.commands.paste, ["pane", "text", "allow_unsafe"]);
  assert.deepEqual(golden.commands.start_pane, ["pane", "cols", "rows", "cell_w", "cell_h"]);
});

// ---- the AI chat --------------------------------------------------------------------------------------------------

test("the ai event and the twelve ai commands are pinned", () => {
  // One event of a chat turn, flat, for every window: the page routes it by conversation.
  assert.equal(golden.events.names.at(-1), "ai");
  assert.deepEqual(keys(golden.events.ai), ["conversation", "kind", "seq", "text", "turn"]);
  assert.deepEqual(Object.keys(golden.events.ai), ["conversation", "kind", "seq", "text", "turn"].sort(), "the golden is written key-sorted");
  // The commands: exactly these and exactly these arguments. A send has no argument that could carry terminal text,
  // a context offer or a decision; a key command has no way to hand a key back.
  const expected = {
    ai_conversation_new: [],
    ai_conversation_list: [],
    ai_conversation_delete: ["conversation"],
    ai_send: ["conversation", "text", "pane", "profile", "model"],
    ai_cancel: ["conversation"],
    ai_turn_snapshot: ["conversation"],
    ai_profiles: [],
    ai_profile_save: ["input"],
    ai_profile_remove: ["profile"],
    ai_key_set: ["profile", "slot", "api_key"],
    ai_key_clear: ["profile", "slot"],
    ai_has_key: ["profile", "slot"],
  };
  const found = Object.fromEntries(Object.entries(golden.commands).filter(([name]) => name.startsWith("ai_")));
  assert.deepEqual(keys(found), keys(expected));
  for (const [name, args] of Object.entries(expected)) assert.deepEqual(golden.commands[name], args, name);
});

test("the markdown command takes only the text", () => {
  assert.deepEqual(golden.commands.markdown_render, ["text"]);
  assert.ok(!golden.commands.markdown_render.includes("pane"), "no argument names a pane or a file");
  const sites = sources(srcDir).flatMap((file) =>
    callSites(readFileSync(file, "utf8")).map((site) => ({ ...site, file: file.slice(srcDir.length + 1) })),
  );
  const render = sites.filter((s) => s.command === "markdown_render");
  assert.ok(render.length >= 1, "the chat renders a finished reply through the command");
  for (const site of render) assert.deepEqual(site.keys, ["text"], `${site.file}:${site.line}`);
  const open = sites.filter((s) => s.command === "open_url" && s.file === "chat.ts");
  assert.ok(open.length >= 1, "a link of a reply is opened through open_url");
  for (const site of open) assert.deepEqual(site.keys, ["url"], `${site.file}:${site.line}`);
});

test("the answers and inputs of the ai commands are pinned", () => {
  const answers = golden.answers.ai;
  assert.deepEqual(keys(answers), ["conversations", "fault", "profiles", "removal", "ticket"]);
  assert.deepEqual(keys(answers.ticket), ["conversation", "turn"]);
  // A refusal is an object, never a string: a code, a reason a dialog can act on (or null) and a fixed text.
  assert.equal(answers.fault.length, 2, "one with a reason and one without");
  for (const fault of answers.fault) {
    assert.deepEqual(keys(fault), ["code", "reason", "text"]);
    assert.equal(typeof fault.code, "string");
    assert.equal(typeof fault.text, "string");
  }
  assert.equal(typeof answers.fault[0].reason, "string");
  assert.equal(answers.fault[1].reason, null);
  assert.deepEqual(keys(answers.conversations), ["chats", "notes"]);
  for (const chat of answers.conversations.chats) {
    assert.deepEqual(keys(chat), ["created", "id", "model", "profile", "title", "updated"]);
  }
  assert.ok(answers.conversations.notes.every((note) => typeof note === "string"));
  assert.deepEqual(keys(answers.profiles), ["notes", "profiles"]);
  for (const profile of answers.profiles.profiles) {
    assert.deepEqual(keys(profile), [
      "allow_lan_http", "base_url", "disclosure", "gateway_key", "host", "id", "key", "model", "name", "preset", "wire",
    ]);
    assert.equal(typeof profile.key, "string", "a key is a status, never the key");
  }
  assert.deepEqual(keys(answers.removal), ["key_deleted", "removed"]);

  // What the settings dialog sends: eight members at most, none of them a key.
  const allowed = ["acknowledge_gateway", "allow_lan_http", "base_url", "id", "model", "name", "preset", "wire"];
  assert.ok(golden.ops.ai_profile_input.length >= 2);
  for (const input of golden.ops.ai_profile_input) {
    assert.ok(keys(input).every((member) => allowed.includes(member)), keys(input).join());
    assert.ok("name" in input && "preset" in input);
  }
  assert.deepEqual(golden.ops.key_slot, ["provider", "gateway"]);
});

/** Every member name of every object in `value`. */
function memberNames(value, out = []) {
  if (Array.isArray(value)) value.forEach((item) => memberNames(item, out));
  else if (value !== null && typeof value === "object") {
    for (const [name, member] of Object.entries(value)) {
      out.push(name);
      memberNames(member, out);
    }
  }
  return out;
}

test("the recorded ai stream is pinned and holds no key, native turn or stored context", () => {
  const stream = JSON.parse(readFileSync(join(here, "../fixtures/ai_stream.json"), "utf8"));
  assert.deepEqual(keys(stream), ["conversation", "events", "snapshots", "states"]);
  assert.equal(stream.conversation, "chat-golden");
  assert.equal(stream.events.length, stream.states.length, "one state after every event");
  assert.ok(stream.events.length > 0);

  const kinds = ["turn_started", "delta", "reasoning", "usage", "trimmed", "message", "tool_proposed", "tool_result", "turn_ended"];
  for (const event of stream.events) {
    for (const member of ["conversation", "turn", "seq", "kind"]) assert.ok(member in event, `${member} in ${event.kind}`);
    assert.ok(kinds.includes(event.kind), event.kind);
    assert.equal(event.conversation, stream.conversation);
  }

  // Turns run in order; each starts and ends the same way and counts its seq 1, 2, 3 without a hole.
  const turns = [];
  for (const event of stream.events) {
    const last = turns.at(-1);
    if (last && last.turn === event.turn) last.events.push(event);
    else turns.push({ turn: event.turn, events: [event] });
  }
  assert.equal(turns.length, 3);
  assert.equal(new Set(turns.map((t) => t.turn)).size, 3, "a turn number is not used twice");
  for (const { events } of turns) {
    assert.equal(events[0].kind, "turn_started");
    assert.equal(events.at(-1).kind, "turn_ended");
    assert.deepEqual(events.map((e) => e.seq), events.map((_, i) => i + 1));
  }
  assert.deepEqual(turns.map((t) => t.events.at(-1).end), ["done", "failed", "cancelled"]);

  // The state after each event follows the event, and has exactly the documented members.
  const stateMembers = ["conversation", "ended", "messages", "model", "partial", "pending", "profile", "running", "seq", "title", "trimmed", "turn", "usage"];
  stream.states.forEach((state, i) => {
    assert.equal(state.seq, stream.events[i].seq);
    assert.equal(state.turn, stream.events[i].turn);
    assert.deepEqual(keys(state), stateMembers);
  });

  // A snapshot taken live is the state after the event it names.
  assert.equal(stream.snapshots.length, 4);
  for (const snapshot of stream.snapshots) {
    assert.deepEqual(keys(snapshot), ["after", "state"]);
    assert.deepEqual(snapshot.state, stream.states[snapshot.after]);
  }
  assert.equal(stream.snapshots[2].state.running, true, "one snapshot is taken in the middle of a turn");

  // The page gets views: no provider-native turn, no stored context text, no key.
  const names = memberNames(stream);
  for (const forbidden of ["native", "stored", "api_key", "key"]) {
    assert.ok(!names.includes(forbidden), `the stream has a member named ${forbidden}`);
  }
});

// ---- call-site scanner --------------------------------------------------------------------------------------------

/** The index just past the string or comment that starts at `i`, or `i` when none starts there. */
function skipLiteral(text, i) {
  const c = text[i];
  if (c === '"' || c === "'") {
    let j = i + 1;
    while (j < text.length && text[j] !== c) j += text[j] === "\\" ? 2 : 1;
    return j + 1;
  }
  if (c === "`") {
    let j = i + 1;
    while (j < text.length && text[j] !== "`") {
      if (text[j] === "\\") j += 2;
      else if (text[j] === "$" && text[j + 1] === "{") j = skipBraces(text, j + 1);
      else j += 1;
    }
    return j + 1;
  }
  if (c === "/" && text[i + 1] === "/") {
    const end = text.indexOf("\n", i);
    return end < 0 ? text.length : end;
  }
  if (c === "/" && text[i + 1] === "*") {
    const end = text.indexOf("*/", i + 2);
    return end < 0 ? text.length : end + 2;
  }
  return i;
}

/** The index just past the `}` that closes the `{` at `open`. */
function skipBraces(text, open) {
  let depth = 0;
  let i = open;
  while (i < text.length) {
    const next = skipLiteral(text, i);
    if (next !== i) {
      i = next;
      continue;
    }
    if (text[i] === "{") depth += 1;
    if (text[i] === "}") {
      depth -= 1;
      if (depth === 0) return i + 1;
    }
    i += 1;
  }
  throw new Error("unbalanced braces");
}

/** `text` without its comments; strings and templates are kept as they are. */
function stripComments(text) {
  let out = "";
  let i = 0;
  while (i < text.length) {
    const next = skipLiteral(text, i);
    const isComment = text[i] === "/" && (text[i + 1] === "/" || text[i + 1] === "*");
    if (next === i) {
      out += text[i];
      i += 1;
      continue;
    }
    if (!isComment) out += text.slice(i, next);
    i = next;
  }
  return out;
}

/** The top-level keys of the object literal whose `{` is at `open`. Spreads and computed keys are reported as such. */
function objectKeys(text, open) {
  const end = skipBraces(text, open) - 1;
  const found = [];
  let depth = 0;
  let entryStart = open + 1;
  const flush = (to) => {
    const entry = stripComments(text.slice(entryStart, to)).trim();
    if (entry === "") return;
    if (entry.startsWith("...")) return void found.push("...spread");
    if (entry.startsWith("[")) return void found.push("[computed]");
    const m = /^(?:"([^"]+)"|'([^']+)'|([A-Za-z_$][\w$]*))\s*(?::|$|\()/.exec(entry);
    found.push(m ? (m[1] ?? m[2] ?? m[3]) : `[unreadable: ${entry.slice(0, 20)}]`);
  };
  let i = open + 1;
  while (i < end) {
    const next = skipLiteral(text, i);
    if (next !== i) {
      i = next;
      continue;
    }
    const c = text[i];
    if (c === "{" || c === "[" || c === "(") depth += 1;
    else if (c === "}" || c === "]" || c === ")") depth -= 1;
    else if (c === "," && depth === 0) {
      flush(i);
      entryStart = i + 1;
    }
    i += 1;
  }
  flush(end);
  return found;
}

const CALL = /\b(?:call|invoke)\s*(?:<[^()]*>)?\s*\(\s*(["'`])([A-Za-z0-9_]+)\1\s*(?:,\s*(\{))?/g;

/** Every `call("name", {...})`, `call<T>("name", {...})` and `invoke("name", {...})` of a source text. */
function callSites(text) {
  const sites = [];
  for (const m of text.matchAll(CALL)) {
    const braceAt = m[3] ? m.index + m[0].length - 1 : -1;
    sites.push({
      command: m[2],
      keys: braceAt >= 0 ? objectKeys(text, braceAt) : [],
      line: text.slice(0, m.index).split("\n").length,
    });
  }
  return sites;
}

/**
 * The sites of `text` that name a command the golden does not have, or pass an argument the Rust command does not
 * have. `Object.hasOwn` keeps names such as "constructor" from passing as commands.
 */
function violations(file, text, commands) {
  const bad = [];
  for (const site of callSites(text)) {
    if (!Object.hasOwn(commands, site.command)) {
      bad.push(`${file}:${site.line}: "${site.command}" is not a command of the protocol golden`);
      continue;
    }
    const args = commands[site.command];
    for (const key of site.keys) {
      if (!args.includes(key)) bad.push(`${file}:${site.line}: ${site.command} has no argument "${key}" (${args.join(", ")})`);
    }
  }
  return bad;
}

/** `command: "name"` fields of action records (links.ts, hints.ts), which reach `call(action.command, ...)`. */
const COMMAND_FIELD = /\bcommand\s*:\s*(["'`])([A-Za-z0-9_]+)\1/g;

function sources(dir) {
  const out = [];
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) out.push(...sources(path));
    else if (name.endsWith(".ts")) out.push(path);
  }
  return out;
}

test("the scanner reads nested braces, strings, templates and generics", () => {
  const text = [
    'call("a", { pane: 1, text: `x}${f({ q: 1 })}y`, allow_unsafe: g({ z: 2 }, [1, 2]) });',
    'call<Record<string, number>>("b", { pane, "cols": 1, \'rows\': 2 });',
    'void invoke("c");',
    'call("d", { ...rest, [k]: 1, // trailing, comment\n  last: 1 });',
  ].join("\n");

  const sites = callSites(text);

  assert.deepEqual(sites.map((s) => s.command), ["a", "b", "c", "d"]);
  assert.deepEqual(sites[0].keys, ["pane", "text", "allow_unsafe"]);
  assert.deepEqual(sites[1].keys, ["pane", "cols", "rows"]);
  assert.deepEqual(sites[2].keys, []);
  assert.deepEqual(sites[3].keys, ["...spread", "[computed]", "last"]);
});

test("every call site in src/ names a command of the golden and uses only its argument names", () => {
  let checked = 0;
  const bad = [];
  for (const file of sources(srcDir)) {
    const text = readFileSync(file, "utf8");
    checked += callSites(text).length;
    bad.push(...violations(file.slice(srcDir.length + 1), text, golden.commands));
  }
  assert.ok(checked >= 30, `only ${checked} call sites were found: is the scanner broken?`);
  assert.deepEqual(bad, []);
});

test("every command named by an action record in src/ exists in the golden", () => {
  let checked = 0;
  const bad = [];
  for (const file of sources(srcDir)) {
    const text = readFileSync(file, "utf8");
    for (const m of text.matchAll(COMMAND_FIELD)) {
      checked += 1;
      if (!Object.hasOwn(golden.commands, m[2])) bad.push(`${file.slice(srcDir.length + 1)}: ${m[2]}`);
    }
  }
  assert.ok(checked >= 2, `only ${checked} command fields were found: is the scanner broken?`);
  assert.deepEqual(bad, []);
});

test("an unknown command name and an inherited property name are reported", () => {
  const bad = violations("x.ts", 'call("no_such_command", { pane: 1 });\ncall("constructor");', golden.commands);
  assert.equal(bad.length, 2, bad.join("\n"));
  assert.match(bad[0], /"no_such_command" is not a command/);
  assert.match(bad[1], /"constructor" is not a command/);
  assert.deepEqual(violations("x.ts", 'call("get_layout");', golden.commands), []);
});

test("the legacy input command is not called anywhere in src/", () => {
  const bad = [];
  for (const file of sources(srcDir)) {
    for (const site of callSites(readFileSync(file, "utf8"))) {
      if (site.command === "input") bad.push(`${file.slice(srcDir.length + 1)}:${site.line}`);
    }
  }
  assert.deepEqual(bad, []);
});

test("a wrong key in a copy of a real call site is reported", () => {
  const text = readFileSync(join(srcDir, "overlay.ts"), "utf8");
  assert.ok(text.includes('call("set_ratio", {'), "overlay.ts no longer calls set_ratio the way this test expects");
  assert.deepEqual(violations("overlay.ts", text, golden.commands), []);

  const broken = text.replace('call("set_ratio", {', 'call("set_ratio", { splitId: 1,');

  const bad = violations("overlay.ts", broken, golden.commands);
  assert.equal(bad.length, 1, bad.join("\n"));
  assert.match(bad[0], /set_ratio has no argument "splitId"/);
});

test("a camelCase spelling of a snake_case argument is reported", () => {
  const bad = violations("x.ts", 'call("paste", { pane: 1, text: "a", allowUnsafe: true });', golden.commands);
  assert.equal(bad.length, 1);
  assert.match(bad[0], /"allowUnsafe"/);
});
