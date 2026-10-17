// In-page stand-in for the Tauri global, injected before the page's own scripts (Playwright addInitScript).
//
//   window.__TAURI__.core.invoke(cmd, args)  -> Promise, answered by a scripted handler or the defaults below
//   window.__TAURI__.event.listen(name, cb, {target})  -> Promise<unlisten>; cb receives { event, id, payload }
//   window.__TAURI_INTERNALS__.metadata.currentWindow.label  the window's label: `main`, or the `?window=` of the URL
//   window.__MOCK__
//     calls            every invoke so far, as [cmd, args] pairs, oldest first
//     callsOf(cmd)     the args of every call to `cmd`, oldest first
//     label            this window's label
//     emit(name, p)    deliver `p` to every listener of `name` as { payload: p } (an event for all windows); returns how
//                      many got it
//     emitTo(label, name, p)  an event addressed to one window, delivered as Tauri does it: to the listeners that
//                      named that window and to those that named none (a listener without a target hears every window)
//     listening(name)  how many listeners `name` has
//     failLayout(msg)  the layout commands reject with `msg` (the shell's "no such window"); failLayout(null) stops it
//     on(cmd, fn)      script a command: fn(args) result is returned as is; on(cmd, null) removes it
//     setView(view)    replace the default layout; set_ratio then only changes the matching divider's ratio
//     view()           a copy of the layout the layout commands currently return
//     reset()          clear calls and handlers and go back to the default layout (listeners are kept); the chat below
//                      is reset too
//     ai               the AI chat of the core, played from the shapes of app/tests/fixtures/ai_stream.json:
//       profiles       the `profiles` member of the answer of `ai_profiles` (default none). A profile view carries the
//                      members of the core's `ProfileView` and the mock-only member `locality` (`on_device`,
//                      `loopback`, `lan` or `cloud`), which the route of a turn reports; it defaults to `loopback`.
//                      The answer of `ai_profiles` is `{profiles, notes}` with copies of these entries without the
//                      mock-only member; `ai_profile_save` changes this list (the profile part is live)
//       notes          the `notes` member of the answer of `ai_profiles` (default none): the core's lines about its file
//       acknowledged   the hosts whose disclosure the person allowed (`ai_profile_save` with `acknowledge_gateway`); the
//                      answer of `ai_profiles` carries `disclosure: {host, kind}` for a profile of a gateway type (OpenRouter,
//                      Vercel, Cloudflare, LiteLLM: `gateway`) or of the Custom type (`third_party`) whose host is not this
//                      computer and is not in this list, else null. A profile saved with another host asks again
//       keychain       true (default): `ai_key_set` stores; false: there is no keychain and a key write is refused with
//                      the reason `no_secure_store`
//       keys           what the keychain holds, by profile id: `{ [id]: { provider?: {scope, length}, gateway?: {...} } }`.
//                      Only the origin a key was saved for and its length are kept, never the key. The state of a key in
//                      `ai_profiles` is derived from it on every read (`present`, `wrong_address` once the address of the
//                      profile has another origin, `no_secure_store` when the keychain went away); a profile with no entry
//                      shows the `key` it was given
//       script(turn)   queue the script of the next turn: `reasoning` (text), `chunks` (the text deltas, default
//                      `["Hello ", "there."]`), `usage` ({input_tokens, output_tokens}, default 11 and 5), `fail`
//                      ({class, text}: the turn fails before any text), `limit` (an end word such as `limit_steps`: the
//                      turn finishes with it) and `stall` (the turn stays running after its first text until it is
//                      cancelled)
//       hold(n)        deliver n more events of the running turn, then pause the pump; hold(0) pauses at once
//       release()      let a paused pump go on
//       drop(n)        swallow the next n events before they are delivered (the state of the chat still takes them)
//       sent           every event the mock produced, in order, dropped ones included
//       chat(id)       a copy of the state of a chat (the empty state for an unknown chat)
//       setState(id, partial)  set members of a chat's state, for example an interrupted message or a pinned profile
//       context        the context commands of the core, played for the consent card:
//                      panes      pane number -> `{ kinds, items }`; `kinds` is what the pane offers (in the order to list
//                                 them) and `items[kind]` the item a read of that kind finds (`{ label, bytes, findings,
//                                 text }`, or a list of them) or nothing when it reads empty. `text` is mock-only: no answer
//                                 ever carries it.
//                                 Panes 1 and 2 are terminals offering `selection`, `screen` and `last_command`
//                      localContext  `ask` (default) or `auto`: `auto` makes a share unnecessary for a loopback or
//                                 on-device profile, as the core's `ai_local_context`
//                      localOnly  true refuses every request whose profile is not on this computer, with `local_only`
//                      ttl        the seconds an offer lives (default 300); an older one is `stale_offer`
//                      persist    the `persist` member of the answers (default false)
//                      shares     the records of shared panes: `[{ conversation, pane, route }]`
//                      offers     the live offers: `[{ id, conversation, pane, route, picked: [...], at }]`
//                      lookups    how many times a state, share or preview call classified a destination
//                      reads      how many kinds a preview read
//                      answers    every answer of the three commands, as `[command, answer]`, oldest first
//                      closePane(n)  the pane closes: its shares and offers end and it leaves the view
//                      expire()   every live offer is out of date
//                      fail(cmd, reason, text, code)  the command refuses with `{ code, reason, text }` (`code` defaults to
//                                 `context`) until `fail(cmd, null)`
//                      hold(cmd), release()  the answer of `cmd` waits until `release()`; the call is recorded at once
//                      A send with `offer` and `picked` redeems a live offer of this chat, pane and route and puts one
//                      `context` part before the text part of the user message; the refusals are the core's reasons.
//       script({tools}) a turn that asks for tools: `tools` is a list of calls `{ id, tool, path, pattern, rows, payload, result,
//                      error, target, remote, tainted, taint, resolved, summary }` (`tool` is one of the four read tools,
//                      default `read_file`; `payload` replaces the fields the mock builds in the core's order; `result` is
//                      what an allow returns, `error` marks it an error; `tainted` and `taint` replace what the chat's
//                      history gives) and `answer` the text after the last result (default `Done.`). The turn emits
//                      `turn_started`, `usage`, the assistant `message` with one `tool_call` part per call and one
//                      `tool_proposed` per call, and then waits: the state holds the cards in `pending` and `running`.
//       approval       the decision part of the core:
//                      decisions   every decision the core accepted, as `{conversation, call_id, decision, hash}`
//                      refuse(reason)  the next decision rejects with `{code: "tool", reason, text}`
//                      changeHash(callId)  the stored hash of a waiting call changes (no event): the old hash gets
//                                 `hash_mismatch` and the next snapshot carries the new one
//                      hold(), release()  the answer of a decision waits until `release()`; the call is recorded at once
//                      waiting(chat)  the ids of the calls that wait in a chat
//                      save(), restore(saved)  carry the chats and the open turns over a reload of the page
//                      `ai_decide(conversation, call_id, decision, hash)` rejects with `invalid_conversation` for an unknown
//                      chat, `invalid_decision` for a verdict other than `allow` or `deny`, `unknown_call` for a call that
//                      does not wait, `not_pending` for one that was decided and `hash_mismatch` for another hash; else it
//                      records the decision and queues the `tool_result` (the scripted result for an allow, `The user denied
//                      this action.` as an error for a deny). When every call of the turn has its result it queues the tool
//                      `message`, the answer, `usage`, the final `message` and `turn_ended`. A stop (`ai_cancel`) gives each
//                      waiting call `This call did not run: the turn was stopped.` and ends the turn `cancelled`. A snapshot
//                      of a chat that is not running holds `[tool output not stored]` for every tool result, as the store does.
//       files          the text of every file the write tools see, as an object from path to text (a copy); a path that is
//                      not here does not exist
//       write          the write tools of the core: `write_file` and `edit_file` calls in a script (`path`, `content`,
//                      `old_text`, `new_text`, `replace_all`, `diff` and `payload` replace what the mock builds). A card is made
//                      from the file as it is when the turn starts: the eleven members of the core's card, the payload in the
//                      core's order (`stamp` only for a file that exists, `base` the digest of its text or `none`) and a `diff`
//                      member of kind `replace`, `create` or `edit` with rows from a plain line diff with the core's context
//                      rules. A call the core would refuse before a card (no match, several matches, equal texts, no file, the
//                      same text) has no card: its `tool_result` is an error with the core's sentence. An allow compares the
//                      digest of the file now with the card's `base`: another text gives the core's changed-payload sentence and
//                      the file stays; the same text is replaced and the result is `Saved the file.` (or `Created the file.`)
//                      with the counts.
//                      seed(path, text)    a file exists with this text
//                      change(path, text)  the file changes without a call (a text of `null` removes it)
//                      text(path)          the text of a file, or null
//                      texts               `{ payload_changed, faults, undo }`: the core's sentences the mock holds
//                      undoRefuse(reason)  the next `ai_undo` rejects with `{code: "undo", reason, text}` and the sentence the shell
//                                          gives that reason; `undoRefuse(null)` stops it
//                      undoBare()          the next `ai_undo` rejects with a bare string, as an error of the shell itself
//                      undos               the argument object of every `ai_undo` call, oldest first
//                      An allowed write keeps what an undo needs: `ai_undo(conversation, call_id)` puts the old text back only while the
//                      file still holds the text the write saved, once; a file the write made is refused as `created`, a text that
//                      changed as `file_changed` and a change nobody knows as `unknown_change`.
//       exec           the command tool of the core, as far as the page can see it: `run_command` calls in a script (`command`, `cwd`,
//                      `timeout_secs`, `target`, `remote`, `tainted`, `taint`, `output`, `failed`, `overrides` and `payload` replace what
//                      the mock builds). The card has the eleven members of the core's card with the effect `exec`, the payload in the
//                      core's order (`tool`, `pane`, `target`, `remote`, `command` with the core's escapes, `cwd` only when the call gave
//                      one, `timeout_secs`, `resolved_cwd`), and its hash is made from the call id and the payload. An allow "runs" the
//                      command: it is recorded in `runs` and its `tool_result` is the scripted `output` (an error result when `failed`).
//                      runs         every command the mock ran, oldest first
//                      hold(), release()  an allowed command keeps running (no result) until `release()`; a stop ends it with the
//                                   sentence of the loop for a stopped call
//                      output(text, failed)  the next command that is allowed answers this text instead of the scripted one
//     The commands `ai_conversation_new`, `ai_conversation_list`, `ai_conversation_delete`, `ai_send`, `ai_cancel`,
//     `ai_decide`, `ai_turn_snapshot`, `ai_profiles`, `ai_profile_save`, `ai_profile_remove`, `ai_key_set` and `ai_key_clear` have
//     defaults; a rejection is the object `{code, reason, text}` as the core's. `ai_profile_save(input)` refuses an input
//     with a member outside the eight of the shell's `ProfileInputMsg`, checks it in the order of the core (id, type,
//     shape, name, model, address, count) with the stable reason names, makes the id of a new profile from its name and
//     answers the id; the address of an empty `base_url` comes from a small table of the mock's own (not the core's) and
//     the address rules are the small subset the cases need. `ai_key_set(profile, slot, api_key)` stores the origin and the
//     length of the key and answers nothing; `ai_key_clear(profile, slot)` forgets them; `ai_profile_remove(profile)`
//     answers `{removed, key_deleted}`.
//     `markdown_render` answers the text as one escaped paragraph; a test scripts other HTML with `on("markdown_render", fn)`.
//
// Defaults: one workspace, one tab, panes 1 and 2 side by side and one Row divider at ratio 0.5. The layout
// commands (the close commands too) return the current view, `set_ratio` clamps to 0.1..0.9, `release_outside` answers `{outcome: "cancelled"}`,
// every other command returns null. The commands between windows (`tear_off_pane`, `tear_off_tab`, `attach_pane`,
// `attach_tab`) answer the current view like the other layout commands; `window_ready` answers null.
// `drag_over` and `drag_end` (the live outline across windows) answer null; a test scripts them with `on` to play the shell.
(() => {
  const LAYOUT_COMMANDS = new Set([
    "get_layout", "split", "new_tab", "new_workspace", "close_pane", "close_tab", "close_workspace", "focus_pane",
    "cycle_focus", "select_tab", "select_workspace", "set_ratio", "rename_pane", "rename_tab", "rename_workspace", "move_pane", "move_pane_to_workspace", "move_tab", "move_tab_to_workspace",
    "tear_off_pane", "tear_off_tab", "attach_pane", "attach_tab",
  ]);
  const clamp = (r) => Math.min(0.9, Math.max(0.1, r));

  const label = new URLSearchParams(location.search).get("window") || "main";
  const calls = [];
  const handlers = new Map();
  const listeners = new Map(); // event name -> Set of { cb, target }
  let ratio = 0.5;
  let custom = null;
  let layoutError = null;
  let mockAi = null;

  // What the shell puts into every webview: the label of the window the page is in.
  window.__TAURI_INTERNALS__ = { metadata: { currentWindow: { label }, currentWebview: { label, windowLabel: label } } };

  /** Deliver to the listeners `wanted` accepts; returns how many got it. */
  const deliver = (name, payload, wanted) => {
    const set = listeners.get(name);
    if (!set) return 0;
    const targets = [...set].filter(wanted);
    for (const l of targets) l.cb({ event: name, id: 0, payload });
    return targets.length;
  };

  const defaultView = () => ({
    workspaces: [{ name: "main", active: true, panes: 2 }],
    tabs: [{ title: "shell", active: true, focus: 2, panes: 2 }],
    focus: 2,
    panes: [[1, { x: 0, y: 0, w: ratio, h: 1 }], [2, { x: ratio, y: 0, w: 1 - ratio, h: 1 }]],
    dividers: [{ split: 10, axis: "Row", area: { x: 0, y: 0, w: 1, h: 1 }, ratio }],
    pane_names: [],
    windows: 1,
  });
  const view = () => structuredClone(custom ?? defaultView());

  function setRatio(args) {
    const r = args && args.ratio;
    if (typeof r !== "number" || !Number.isFinite(r)) return; // the core ignores NaN and infinity too
    if (!custom) {
      ratio = clamp(r);
      return;
    }
    const d = custom.dividers.find((x) => x.split === args.split) ?? custom.dividers[0];
    if (d) d.ratio = clamp(r);
  }

  // ---- the AI chat -------------------------------------------------------------------------------------------------
  // One state per chat id, built to the member names of the recorded run. A turn is a list of events produced from a
  // script and pumped one event per timer of zero delay: `turn_started`, `reasoning`, one `delta` per chunk, `usage`,
  // `message`, `turn_ended`. The state takes every event, also one that `drop` swallows, so a snapshot tells the truth.
  const NOW = 1800000000;
  const ai = {
    profiles: [],
    sent: [],
    chats: new Map(),
    scripts: [],
    chatNo: 0,
    turnNo: NOW,
    queue: [], // events of the running turn that are not produced yet
    running: null, // the id of the chat whose turn runs
    holdLeft: null, // events to produce before the pump pauses; null: no pause
    dropLeft: 0,
    timer: 0,
    notes: [],
    keychain: true,
    keys: {},
    acknowledged: [],
  };
  const emptyState = (id) => ({
    conversation: id, title: "", profile: null, model: null, turn: 0, seq: 0, running: false, messages: [],
    partial: { text: "", reasoning: "" }, pending: [], usage: { input_tokens: 0, output_tokens: 0 },
    trimmed: { results: 0, exchanges: 0 }, ended: null,
  });
  const chatOf = (id) => {
    if (!ai.chats.has(id)) ai.chats.set(id, emptyState(id));
    return ai.chats.get(id);
  };
  const fault = (code, text, reason = null) => ({ code, reason, text });
  const titleOf = (text) => {
    const line = text.split("\n").map((l) => l.trim().replace(/\s+/g, " ")).find((l) => l !== "") ?? "";
    const chars = Array.from(line);
    return chars.length <= 60 ? line : `${chars.slice(0, 60).join("").trimEnd()}…`;
  };
  /** The state takes one event: the rules of the core's reducer, enough for the events the mock produces. */
  const take = (state, e) => {
    state.turn = e.turn;
    state.seq = e.seq;
    switch (e.kind) {
      case "turn_started":
        if (state.title === "") state.title = titleOf(e.message.parts.find((part) => part.type === "text").text);
        state.messages.push(e.message);
        state.running = true;
        state.partial = { text: "", reasoning: "" };
        state.pending = [];
        state.usage = { input_tokens: 0, output_tokens: 0 };
        state.trimmed = { results: 0, exchanges: 0 };
        state.ended = null;
        state.profile = e.route.profile;
        state.model = e.route.model;
        break;
      case "delta":
        state.partial.text += e.text;
        break;
      case "reasoning":
        state.partial.reasoning += e.text;
        break;
      case "usage":
        state.usage = { input_tokens: e.input_tokens, output_tokens: e.output_tokens };
        break;
      case "message":
        state.messages.push(e.message);
        state.partial = { text: "", reasoning: "" };
        break;
      case "tool_proposed":
        state.pending.push(structuredClone(e.pending));
        break;
      case "tool_result":
        state.pending = state.pending.filter((p) => p.call_id !== e.call_id);
        break;
      case "turn_ended":
        state.running = false;
        state.partial = { text: "", reasoning: "" };
        state.pending = [];
        state.ended = { end: e.end, error: e.error, rounds: e.rounds, usage: e.usage, saved: e.saved };
        break;
    }
  };
  /** Produce one event: the state takes it, and it is delivered unless `drop` swallows it. */
  const produce = (id, event) => {
    take(chatOf(id), event);
    ai.sent.push(structuredClone(event));
    if (ai.dropLeft > 0) ai.dropLeft -= 1;
    else mock.emit("ai", structuredClone(event));
  };
  const pump = () => {
    if (ai.timer || ai.running === null) return;
    ai.timer = setTimeout(() => {
      ai.timer = 0;
      if (ai.running === null) return;
      if (ai.holdLeft !== null && ai.holdLeft <= 0) return; // paused until release()
      const next = ai.queue.shift();
      if (next === undefined) return;
      if (ai.holdLeft !== null) ai.holdLeft -= 1;
      if (next.kind === "turn_ended") ai.running = null;
      produce(next.conversation, next);
      pump();
    }, 0);
  };
  // ---- tool calls ----------------------------------------------------------------------------------------------------
  // The tool turn of the core, played from the shapes of app/tests/fixtures/ai_tool_stream.json: a card per call (the eleven
  // members of the core's card, the payload in the core's field order for the four read tools, a 64-character hash), the
  // decision command with the core's refusals, the result of an allow or a deny, and the end of the turn.
  const appr = { decisions: [], refuse: null, held: false, waiters: [], live: new Map() };
  /** The command tool: the commands that were run, a held run, the runs that wait for `release()`, and the next scripted answer. */
  const ran = { runs: [], held: false, waiting: [], next: null };
  const STORED = "[tool output not stored]";
  const REFUSALS = {
    unknown_call: "No open call has this id.",
    not_pending: "The call was decided already.",
    hash_mismatch: "The decision does not match the action it was asked for.",
    invalid_decision: "That is not a decision.",
    denied: "This location is protected and tools cannot use it.",
  };
  const bytesOf = (text) => new TextEncoder().encode(text).length;
  /** Mock-only: a 64-character lower-case hexadecimal value made from `text` (eight rounds of a 32-bit hash). */
  const digestOf = (text) => {
    let out = "";
    for (let round = 0; round < 8; round++) {
      let h = (0x811c9dc5 + Math.imul(round + 1, 0x9e3779b1)) >>> 0;
      const part = `${round}|${text}`;
      for (let i = 0; i < part.length; i++) {
        h ^= part.charCodeAt(i);
        h = Math.imul(h, 0x01000193) >>> 0;
      }
      out += h.toString(16).padStart(8, "0");
    }
    return out;
  };
  // ---- write tools ----------------------------------------------------------------------------------------------------
  // The write half of the core, as far as the page can see it: files in memory, the card with its diff, the refusals the core
  // makes before a card exists and the check at the allow. The sentences are copies of the core's (held to the golden by the
  // contract case), the line diff is a plain longest-common-subsequence one with the core's rules for the unchanged lines.
  const wfiles = new Map();
  const WRITE_STAMP = 1700000000;
  const PAYLOAD_CHANGED = "The target changed after the approval, so the action was not run.";
  const FAULTS = {
    no_file: "the file does not exist; use write_file to create it",
    no_match: "old_text was not found in the file",
    ambiguous: "old_text occurs more than once; add surrounding text to make it unique or set replace_all",
    same: "old_text and new_text are the same",
    unchanged: "the file already holds that text",
    too_large: "the result would be larger than the agent may write",
    not_shown: "the change is too large to show; make smaller changes",
  };
  const INVALID = "The arguments of the call are not valid: ";
  /** The sentences the shell gives for an undo it refuses, by reason: copies, held to the shell's golden by a case. */
  const UNDO_TEXTS = {
    created: "The assistant created this file. Undo cannot remove it.",
    denied: "That file is in a protected place. Nothing was restored.",
    file_changed: "The file changed after the assistant saved it. Nothing was restored.",
    unknown_change: "That change can no longer be undone here.",
    unreadable: "The file could not be read or saved. Nothing was restored.",
  };
  /** What an undo can still do, by chat and call: the file, the text it held before (undefined for a made file) and what was saved. */
  const undoable = new Map();
  const undone = { refuse: null, bare: false, calls: [] };
  const isWrite = (tool) => tool === "write_file" || tool === "edit_file";
  /** The characters the card writes out: controls, direction marks, zero-width and format characters, the tag block. */
  const hiddenCode = (n) =>
    n <= 0x1f || (n >= 0x7f && n <= 0x9f) || n === 0xad || n === 0x61c || (n >= 0x200b && n <= 0x200f) || (n >= 0x2028 && n <= 0x202e) ||
    (n >= 0x2060 && n <= 0x2064) || (n >= 0x2066 && n <= 0x2069) || n === 0xfeff || (n >= 0xe0000 && n <= 0xe007f);
  /** The core's `visible` and `visible_multiline`: hidden characters and backslashes written out. */
  const visibleText = (text, keepLines) => {
    let out = "";
    for (const c of text) {
      if (c === "\\") out += "\\\\";
      else if (c === "\n") out += keepLines ? "\n" : "\\n";
      else if (c === "\t") out += keepLines ? "\t" : "\\t";
      else if (c === "\r") out += "\\r";
      else if (hiddenCode(c.codePointAt(0))) out += `\\u{${c.codePointAt(0).toString(16)}}`;
      else out += c;
    }
    return out;
  };
  /** The digest a card carries for the text of a file: `none` for no file, else `sha256:` and 64 hexadecimal digits. */
  const digestText = (text) => (text === undefined ? "none" : `sha256:${digestOf(`file|${text}`)}`);
  const lineCount = (n) => `${n} ${n === 1 ? "line" : "lines"}`;
  /** The lines of a text with their line breaks kept. */
  const piecesOf = (text) => text.match(/[^\n]*\n|[^\n]+/g) ?? [];
  const countMatches = (text, needle) => {
    let n = 0;
    for (let at = text.indexOf(needle); at >= 0; at = text.indexOf(needle, at + needle.length)) n++;
    return n;
  };
  /** What a write call would turn the file into: `{ before, after, kind }`, or `{ fault }` when the core would not make a card. */
  const planOf = (tool, args, base) => {
    if (tool === "write_file") {
      if (base !== undefined && base === args.content) return { fault: "unchanged" };
      return { before: base, after: args.content, kind: base === undefined ? "create" : "replace" };
    }
    if (base === undefined) return { fault: "no_file" };
    if (args.old_text === args.new_text) return { fault: "same" };
    const found = countMatches(base, args.old_text);
    if (found === 0) return { fault: "no_match" };
    const all = args.replace_all === true;
    if (found > 1 && !all) return { fault: "ambiguous" };
    const after = all ? base.split(args.old_text).join(args.new_text) : base.replace(args.old_text, () => args.new_text);
    return { before: base, after, kind: "edit" };
  };
  /** The rows of a change: every changed line and three unchanged lines around each change, longer runs as a gap. */
  const diffOf = (change, context = 3) => {
    const oldP = piecesOf(change.before ?? "");
    const newP = piecesOf(change.after);
    const n = oldP.length;
    const m = newP.length;
    const table = Array.from({ length: n + 1 }, () => new Array(m + 1).fill(0));
    for (let i = n - 1; i >= 0; i--) {
      for (let j = m - 1; j >= 0; j--) {
        table[i][j] = oldP[i] === newP[j] ? table[i + 1][j + 1] + 1 : Math.max(table[i + 1][j], table[i][j + 1]);
      }
    }
    const ops = [];
    let i = 0;
    let j = 0;
    while (i < n && j < m) {
      if (oldP[i] === newP[j]) ops.push(["keep", i++, j++]);
      else if (table[i + 1][j] >= table[i][j + 1]) ops.push(["del", i++]);
      else ops.push(["add", j++]);
    }
    while (i < n) ops.push(["del", i++]);
    while (j < m) ops.push(["add", j++]);
    const shown = (piece) => visibleText(piece.endsWith("\n") ? piece.slice(0, -1) : piece, false);
    const rows = [];
    const lineOf = (op) => {
      const [kind, a, b] = op;
      if (kind === "keep") rows.push({ op: "ctx", old: a + 1, new: b + 1, text: shown(oldP[a]) });
      else if (kind === "del") rows.push({ op: "del", old: a + 1, new: null, text: shown(oldP[a]) });
      else rows.push({ op: "add", old: null, new: a + 1, text: shown(newP[a]) });
      const piece = kind === "add" ? newP[a] : oldP[a];
      if (!piece.endsWith("\n")) rows.push({ op: "note", text: "No newline at end of file" });
    };
    for (let at = 0; at < ops.length;) {
      if (ops[at][0] !== "keep") {
        lineOf(ops[at++]);
        continue;
      }
      const start = at;
      while (at < ops.length && ops[at][0] === "keep") at++;
      const length = at - start;
      const before = start > 0;
      const after = at < ops.length;
      if (!before && !after) continue;
      const folds = before && after ? length > 2 * context + 1 : length > context + 1;
      const front = !folds ? length : before ? context : 0;
      const back = !folds ? 0 : after ? context : 0;
      for (let k = 0; k < front; k++) lineOf(ops[start + k]);
      if (folds) {
        rows.push({ op: "gap", skipped: length - front - back });
        for (let k = length - back; k < length; k++) lineOf(ops[start + k]);
      }
    }
    return {
      kind: change.kind, added: ops.filter((o) => o[0] === "add").length, removed: ops.filter((o) => o[0] === "del").length,
      before_bytes: change.before === undefined ? null : bytesOf(change.before), after_bytes: bytesOf(change.after), rows,
    };
  };
  /** The write part of a call: what the card was made from, what an allow would save, and the diff. A fault when there is no card. */
  const writeOf = (tool, spec) => {
    const args = tool === "write_file"
      ? { path: spec.path ?? "", content: spec.content ?? "" }
      : { path: spec.path ?? "", old_text: spec.old_text ?? "", new_text: spec.new_text ?? "", replace_all: spec.replace_all === true };
    const base = wfiles.get(args.path);
    const change = planOf(tool, args, base);
    if (change.fault !== undefined) return { args, base, fault: change.fault };
    return { args, base, baseDigest: digestText(base), after: change.after, change, diff: diffOf(change) };
  };

  /** The call of a script entry: its id, tool, arguments, the summary the assistant message carries and its result. */
  const toolCall = (spec, index) => {
    const tool = spec.tool ?? "read_file";
    const id = spec.id ?? `call_${index + 1}`;
    const write = isWrite(tool) ? writeOf(tool, spec) : null;
    const command = tool === "run_command" ? { command: spec.command ?? "", cwd: spec.cwd, timeout_secs: spec.timeout_secs ?? 60 } : null;
    const args =
      command !== null ? { command: command.command, ...(command.cwd === undefined ? {} : { cwd: command.cwd }), timeout_secs: command.timeout_secs }
      : tool === "grep" ? { pattern: spec.pattern ?? "", path: spec.path ?? "" }
      : tool === "read_pane" ? { rows: spec.rows ?? 50 }
      : write !== null ? write.args
      : { path: spec.path ?? "" };
    return {
      id, tool, spec: structuredClone(spec), summary: spec.summary ?? JSON.stringify(args),
      result: spec.output ?? spec.result ?? "ok", error: spec.failed === true || spec.error === true, decided: null, out: null, write, command,
      running: false,
    };
  };
  /** The payload of a card: the fields of the call in the core's order, each with the length of its raw value. */
  const payloadOf = (call, pane) => {
    const spec = call.spec;
    if (Array.isArray(spec.payload)) return structuredClone(spec.payload);
    const path = spec.path ?? "";
    const resolved = spec.resolved ?? path;
    if (call.command !== null) {
      const c = call.command;
      const top = [["tool", call.tool], ["pane", String(pane)], ["target", spec.target ?? "This computer"], ["remote", String(spec.remote === true)]];
      const rest = [["command", c.command]];
      if (c.cwd !== undefined) rest.push(["cwd", c.cwd]);
      rest.push(["timeout_secs", String(c.timeout_secs)], ["resolved_cwd", spec.resolved ?? c.cwd ?? "/work/proj"]);
      const changed = spec.overrides ?? {};
      return [...top, ...rest].map(([name, value]) => {
        const raw = Object.hasOwn(changed, name) ? changed[name] : value;
        return { name, value: visibleText(raw, false), bytes: bytesOf(raw) };
      });
    }
    if (call.write !== null) {
      const a = call.write.args;
      const raw = call.tool === "write_file"
        ? [["path", path], ["content", a.content, true]]
        : [["path", path], ["old_text", a.old_text, true], ["new_text", a.new_text, true], ["replace_all", String(a.replace_all)]];
      raw.push(["resolved_path", resolved]);
      if (call.write.base !== undefined) raw.push(["stamp", String(WRITE_STAMP)]);
      raw.push(["base", call.write.baseDigest]);
      const top = [["tool", call.tool], ["pane", String(pane)], ["target", spec.target ?? "This computer"], ["remote", String(spec.remote === true)]];
      return [...top, ...raw].map(([name, value, lines]) => ({ name, value: visibleText(value, lines === true), bytes: bytesOf(value) }));
    }
    const head = [["tool", call.tool], ["pane", String(pane)], ["target", spec.target ?? "This computer"], ["remote", String(spec.remote === true)]];
    const rest =
      call.tool === "list_dir" ? [["path", path], ["max_entries", "200"], ["resolved_path", resolved]]
      : call.tool === "grep" ? [["pattern", spec.pattern ?? ""], ["path", path], ["ignore_case", "false"], ["max_matches", "50"], ["resolved_path", resolved]]
      : call.tool === "read_pane" ? [["rows", String(spec.rows ?? 50)]]
      : [["path", path], ["offset", "0"], ["limit", "16384"], ["resolved_path", resolved]];
    return [...head, ...rest].map(([name, value]) => ({ name, value, bytes: bytesOf(value) }));
  };
  /** The labels the history gives a later card: one `output of <tool>` for each call of the chat that has a result. */
  const earlierTaint = (conversation) => {
    const messages = chatOf(conversation).messages;
    const answered = new Set(messages.flatMap((m) => m.parts.filter((p) => p.type === "tool_result").map((p) => p.call_id)));
    const labels = [];
    for (const m of messages) {
      for (const part of m.parts) {
        const label = part.type === "tool_call" && answered.has(part.id) ? `output of ${part.name}` : null;
        if (label !== null && !labels.includes(label)) labels.push(label);
      }
    }
    return labels.slice(0, 8);
  };
  const pendingOf = (conversation, call, pane, earlier) => {
    const spec = call.spec;
    const payload = payloadOf(call, pane);
    const taint = spec.taint ?? earlier;
    const card = {
      conversation, call_id: call.id, tool: call.tool, effect: call.command !== null ? "exec" : call.write === null ? "read" : "write", pane, target: spec.target ?? "This computer",
      remote: spec.remote === true, payload, hash: digestOf(`${call.id}|${JSON.stringify(payload)}`),
      tainted: spec.tainted ?? taint.length > 0, taint: structuredClone(taint),
    };
    if (call.write !== null) card.diff = structuredClone(spec.diff ?? call.write.diff);
    return card;
  };
  const toolCallMessage = (turn, model, calls) => ({
    id: `m${turn}-1`, role: "assistant", ts: NOW, model, status: "complete", usage: null,
    parts: calls.map((c) => ({ type: "tool_call", id: c.id, name: c.tool, summary: c.summary })),
  });
  const toolResultMessage = (turn, calls) => ({
    id: `m${turn}-2`, role: "tool", ts: NOW, model: null, status: "complete", usage: null,
    parts: calls.map((c) => ({ type: "tool_result", call_id: c.id, content: c.out.content, is_error: c.out.is_error })),
  });
  /** Queue `events` of the running turn behind the ones that wait, with the next positions of the turn. */
  const queueMore = (conversation, events) => {
    const state = chatOf(conversation);
    let seq = ai.queue.length > 0 ? ai.queue[ai.queue.length - 1].seq : state.seq;
    for (const e of events) ai.queue.push({ conversation, turn: state.turn, seq: ++seq, ...e });
    pump();
  };
  /** What follows the last result of a round: the tool message, the answer and the end of the turn. */
  const afterResults = (live, model) => {
    const usage = live.usage;
    return [
      { kind: "message", message: toolResultMessage(live.turn, live.calls) },
      { kind: "delta", text: live.answer },
      { kind: "usage", input_tokens: usage.input_tokens, output_tokens: usage.output_tokens },
      { kind: "message", message: { id: `m${live.turn}-3`, role: "assistant", ts: NOW, model, status: "complete", usage, parts: [{ type: "text", text: live.answer }] } },
      { kind: "turn_ended", end: "done", error: null, rounds: 2, usage, saved: true },
    ];
  };
  /** An allowed write: the file is saved only if it still holds the text the card was made from. */
  const saveWrite = (conversation, call) => {
    const w = call.write;
    if (digestText(wfiles.get(w.args.path)) !== w.baseDigest) return { content: PAYLOAD_CHANGED, is_error: true };
    wfiles.set(w.args.path, w.after);
    const created = w.base === undefined;
    undoable.set(`${conversation}|${call.id}`, { path: w.args.path, old: w.base, written: w.after });
    return {
      content: `${created ? "Created the file." : "Saved the file."} ${lineCount(w.diff.added)} added, ${w.diff.removed} removed.`,
      is_error: false,
    };
  };
  const decide = async ({ conversation, call_id, decision, hash }) => {
    const state = ai.chats.get(conversation);
    if (state === undefined) throw fault("invalid_conversation", "there is no such chat");
    if (appr.held) await new Promise((go) => appr.waiters.push(go));
    if (appr.refuse !== null) {
      const reason = appr.refuse;
      appr.refuse = null;
      throw fault("tool", REFUSALS[reason] ?? `The call was refused (${reason}).`, reason);
    }
    if (decision !== "allow" && decision !== "deny") throw fault("tool", REFUSALS.invalid_decision, "invalid_decision");
    const entry = state.pending.find((p) => p.call_id === call_id);
    const live = appr.live.get(conversation);
    const call = live?.calls.find((c) => c.id === call_id);
    if (call?.decided || call?.running) throw fault("tool", REFUSALS.not_pending, "not_pending");
    if (entry === undefined || call === undefined) throw fault("tool", REFUSALS.unknown_call, "unknown_call");
    if (hash !== entry.hash) throw fault("tool", REFUSALS.hash_mismatch, "hash_mismatch");
    appr.decisions.push({ conversation, call_id, decision, hash });
    if (decision === "allow" && call.command !== null) {
      // The command "runs": it is recorded now, and its result comes at once or, while the run is held, with `release()`.
      ran.runs.push(call.command.command);
      if (ran.held) {
        call.running = true;
        ran.waiting.push({ conversation, call });
        return null;
      }
    }
    settle(conversation, state, live, call, decision);
    return null;
  };
  /** The result of a decided call, and what follows it when it was the last call of the round. */
  const settle = (conversation, state, live, call, decision) => {
    call.running = false;
    call.decided = decision;
    if (decision === "allow" && call.write !== null) call.out = saveWrite(conversation, call);
    else {
      call.out = decision === "allow"
        ? ran.next !== null && call.command !== null
          ? { content: ran.next.text, is_error: ran.next.failed }
          : { content: call.result, is_error: call.error }
        : { content: "The user denied this action.", is_error: true };
      if (decision === "allow" && call.command !== null) ran.next = null;
    }
    const events = [{ kind: "tool_result", call_id: call.id, tool: call.tool, content: call.out.content, is_error: call.out.is_error, redactions: 0 }];
    if (live.calls.every((c) => c.decided !== null)) {
      live.closed = true;
      events.push(...afterResults(live, state.model));
    }
    queueMore(conversation, events);
  };
  /** A stop: every call without a result gets the fixed sentence, then the round's tool message. */
  const stopTools = (conversation, state) => {
    const live = appr.live.get(conversation);
    if (live === undefined || live.closed) return [];
    const open = state.pending.map((p) => p.call_id);
    const out = [];
    for (const call of live.calls) {
      if (!open.includes(call.id)) continue;
      call.out = { content: "This call did not run: the turn was stopped.", is_error: true };
      out.push({ kind: "tool_result", call_id: call.id, tool: call.tool, content: call.out.content, is_error: true, redactions: 0 });
    }
    live.closed = true;
    ran.waiting = ran.waiting.filter((w) => w.conversation !== conversation);
    for (const call of live.calls) {
      call.running = false;
      call.out ??= { content: "This call did not run: the turn was stopped.", is_error: true };
    }
    out.push({ kind: "message", message: toolResultMessage(live.turn, live.calls) });
    return out;
  };
  const userMessage = (turn, text, before = []) => ({
    id: `m${turn}-user`, role: "user", ts: NOW, model: null, status: "complete", usage: null, parts: [...before, { type: "text", text }],
  });
  const assistantMessage = (turn, model, text, status, usage) => ({
    id: `m${turn}-1`, role: "assistant", ts: NOW, model, status, usage, parts: [{ type: "text", text }],
  });
  /** The events of one turn from its script, with the head (`conversation`, `turn`) but not the `seq` yet. */
  const turnEvents = (conversation, turn, text, route, script, before = [], pane = 1) => {
    const out = [{ kind: "turn_started", message: userMessage(turn, text, before), route }];
    if (script.tools) {
      // A turn that asks for tools: the assistant's message with one `tool_call` part per call, then one card per call.
      // The turn waits there; the decisions (`ai_decide`) and a stop produce the rest.
      const calls = script.tools.map((spec, i) => toolCall(spec, i));
      appr.live.set(conversation, {
        turn, pane, answer: script.answer ?? "Done.", usage: script.usage ?? { input_tokens: 11, output_tokens: 5 }, calls, closed: false,
      });
      const earlier = earlierTaint(conversation);
      out.push({ kind: "usage", input_tokens: 0, output_tokens: 0 });
      out.push({ kind: "message", message: toolCallMessage(turn, route.model, calls) });
      for (const call of calls) {
        if (call.write?.fault !== undefined) {
          // The core refuses the change before any card exists: the call has its result at once.
          call.decided = "refused";
          call.out = { content: `${INVALID}${FAULTS[call.write.fault]}`, is_error: true };
          out.push({ kind: "tool_result", call_id: call.id, tool: call.tool, content: call.out.content, is_error: true, redactions: 0 });
        } else {
          out.push({ kind: "tool_proposed", pending: pendingOf(conversation, call, pane, earlier) });
        }
      }
      if (calls.every((c) => c.decided !== null)) {
        appr.live.get(conversation).closed = true;
        out.push(...afterResults(appr.live.get(conversation), route.model));
      }
      return out.map((e, i) => ({ conversation, turn, seq: i + 1, ...e }));
    }
    if (script.reasoning) out.push({ kind: "reasoning", text: script.reasoning });
    const usage = script.usage ?? { input_tokens: 11, output_tokens: 5 };
    if (script.fail) {
      out.push({
        kind: "turn_ended", end: "failed", error: { class: script.fail.class, text: script.fail.text }, rounds: 1,
        usage: { input_tokens: 0, output_tokens: 0 }, saved: true,
      });
    } else {
      const chunks = script.chunks ?? ["Hello ", "there."];
      for (const c of chunks) out.push({ kind: "delta", text: c });
      if (script.stall) {
        // The turn stays running after its first text; only a cancel ends it.
        out.length = Math.min(out.length, script.reasoning ? 3 : 2);
      } else {
        out.push({ kind: "usage", input_tokens: usage.input_tokens, output_tokens: usage.output_tokens });
        out.push({ kind: "message", message: assistantMessage(turn, route.model, chunks.join(""), "complete", usage) });
        out.push({
          kind: "turn_ended", end: script.limit ?? "done", error: null, rounds: 1, usage, saved: true,
        });
      }
    }
    return out.map((e, i) => ({ conversation, turn, seq: i + 1, ...e }));
  };
  // ---- the providers: the profile part of the core, played from the shapes of the shell's golden -----------------------
  /** The saved entries, whichever form `profiles` was given in (a list, or a whole `{profiles, notes}` answer). */
  const stored = () => (Array.isArray(ai.profiles) ? ai.profiles : (ai.profiles.profiles ??= []));
  const noteLines = () => (ai.notes.length > 0 || Array.isArray(ai.profiles) ? ai.notes : (ai.profiles.notes ?? []));
  /** Mock-only: the address a type starts with and the shapes it speaks. These are the mock's values, not the core's. */
  const MOCK_TYPES = {
    anthropic: { base: "https://api.anthropic.com", wires: ["anthropic"] },
    openai: { base: "https://api.openai.com/v1", wires: ["openai_responses"] },
    gemini: { base: "https://generativelanguage.googleapis.com/v1beta", wires: ["gemini"] },
    ollama: { base: "http://127.0.0.1:11434", wires: ["ollama"] },
    lm_studio: { base: "http://127.0.0.1:1234/v1", wires: ["openai_chat"] },
    llama_cpp: { base: "http://127.0.0.1:9931/v1", wires: ["openai_chat"] },
    openrouter: { base: "https://openrouter.ai/api/v1", wires: ["openai_chat"] },
    vercel_gateway: { base: "https://ai-gateway.vercel.sh/v1", wires: ["openai_chat", "anthropic"] },
    cloudflare_gateway: { base: "", wires: ["anthropic", "openai_responses", "openai_chat", "gemini"] },
    litellm: { base: "http://127.0.0.1:4000/v1", wires: ["openai_chat"] },
    custom: { base: "", wires: ["openai_chat", "openai_responses", "anthropic", "gemini", "ollama"] },
  };
  const SAVE_MEMBERS = ["id", "name", "preset", "wire", "base_url", "model", "allow_lan_http", "acknowledge_gateway"];
  const hostOf = (base) => {
    try {
      return new URL(base).host;
    } catch {
      return "";
    }
  };
  const localityOf = (base) => {
    let host = "";
    try {
      host = new URL(base).hostname;
    } catch {
      return "cloud";
    }
    return host === "localhost" || host.endsWith(".localhost") || host === "[::1]" || /^127\./.test(host) ? "loopback" : "cloud";
  };
  const idFrom = (name, taken) => {
    const base = name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 40).replace(/-+$/g, "") || "provider";
    let id = base;
    for (let n = 2; taken(id); n += 1) id = `${base}-${n}`;
    return id;
  };
  const originOf = (base) => {
    try {
      return new URL(base).origin;
    } catch {
      return "";
    }
  };
  /** The state of a key slot: derived from what the keychain holds, else the state the entry was given. */
  const slotState = (entry, slot, given) => {
    const held = ai.keys[entry.id]?.[slot];
    if (!held) return given;
    if (!ai.keychain) return "no_secure_store";
    return held.scope === originOf(entry.base_url) ? "present" : "wrong_address";
  };
  /** The kind of notice a type asks for before its first send (mock-only table). */
  const DISCLOSURE_KINDS = {
    openrouter: "gateway", vercel_gateway: "gateway", cloudflare_gateway: "gateway", litellm: "gateway", custom: "third_party",
  };
  /** The notice of an entry: its host and kind, unless the host is this computer or was allowed. */
  const disclosureOf = (entry) => {
    const kind = DISCLOSURE_KINDS[entry.preset];
    if (!kind || entry.locality === "loopback" || ai.acknowledged.includes(entry.host)) return null;
    return { host: entry.host, kind };
  };
  /** The entries as `ai_profiles` lists them: the mock-only members are left out, the key states and notices are derived. */
  const listed = () =>
    stored().map((entry) => {
      const { locality: _locality, ...view } = structuredClone(entry);
      view.key = slotState(entry, "provider", entry.key);
      if (entry.gateway_key !== null) view.gateway_key = slotState(entry, "gateway", entry.gateway_key);
      view.disclosure = disclosureOf(entry);
      return view;
    });
  /** The fixed phrase of a refusal; it holds nothing the person typed. */
  const PHRASES = {
    id: "the id is not valid", reserved_id: "this id is reserved", name: "the name must be 1 to 80 characters without control characters",
    model: "the model is not valid", wire: "this kind of provider does not speak that wire",
    preset_change: "the kind of provider of a profile cannot change", needs_address: "the address is empty or still has a part to fill in",
    too_many: "there are already 32 profiles", unknown_profile: "there is no profile with this id", syntax: "the text is not an address",
    too_long: "the address is too long", scheme: "the scheme is not allowed", credentials: "the address carries a user name or a password",
    host: "the address has no host", port: "the port is not allowed", query: "the address carries a query or a fragment",
    plain_http: "plain http is not allowed here", link_local: "a link-local address is not allowed", metadata: "a metadata address is not allowed",
  };
  const refuse = (reason) => fault("profile", PHRASES[reason] ?? "the request was refused", reason);
  const KEY_PHRASES = {
    no_secure_store: "there is no system keychain, so a key cannot be saved", invalid: "the key cannot be used",
    no_such_slot: "this kind of provider has no such key", unknown_profile: "there is no profile with this id",
  };
  const keyFault = (code, reason) => fault(code, KEY_PHRASES[reason], reason);
  const hasControl = (text) => /[\u0000-\u001f\u007f-\u009f]/.test(text);
  const isLoopbackHost = (h) => h === "localhost" || h.endsWith(".localhost") || h === "[::1]" || /^127\./.test(h);
  const isPrivateHost = (h) => /^(10\.|192\.168\.|172\.(1[6-9]|2\d|3[01])\.)/.test(h);
  /** The small subset of the address rules that the cases need; the core has the real ones. */
  const checkAddress = (typed, lan) => {
    if (typed === "" || /[{}]|%7b|%7d/i.test(typed)) throw refuse("needs_address");
    if (typed.length > 2048) throw refuse("too_long");
    let url;
    try {
      url = new URL(typed);
    } catch {
      throw refuse("syntax");
    }
    if (url.protocol !== "http:" && url.protocol !== "https:") throw refuse("scheme");
    if (url.username !== "" || url.password !== "") throw refuse("credentials");
    if (url.hostname === "") throw refuse("host");
    if (url.search !== "" || url.hash !== "") throw refuse("query");
    const host = url.hostname;
    if (host === "169.254.169.254") throw refuse("metadata");
    if (host.startsWith("169.254.") || host.startsWith("[fe80:")) throw refuse("link_local");
    if (url.protocol === "http:" && !isLoopbackHost(host) && !(lan && isPrivateHost(host))) throw refuse("plain_http");
  };
  // ---- the context: the share, the preview and the send with ids, played from the shapes of the shell's golden --------------
  // The mock models the rules the page relies on: a pane is read only after a share (unless the core says none is needed), a
  // call on another route removes the share, one offer lives per chat and is spent by one send, and every refusal is the
  // object `{ code: "context", reason, text }`. The texts of the items are mock-only and no answer ever carries them.
  const CANARY_TEXT = "zq-consent-text-71b9e4";
  const CANARY_KEY = "sk-ant-api03-zqconsent5e90b3aXYZ0123456789";
  const CONTEXT_KINDS = ["selection", "screen", "last_command", "file", "preview"];
  const REASON_TEXT = {
    not_shared: "This pane is not shared with the chat yet. Share it first.",
    stale_offer: "The attached context is no longer valid. Read it again.",
    bad_pick: "The chosen items do not match what was read. Read the context again.",
    bad_kinds: "That choice of content cannot be read. Tick one to five kinds.",
    unsupported_kind: "This pane cannot offer that kind of content.",
    nothing_to_share: "There is nothing in this pane that can be shared.",
    pane_gone: "The pane is closed or no longer shows what was shared.",
    denied: "This place may not be shared.",
    not_text: "This file is not text, so it cannot be shared.",
    unreadable: "The pane or file could not be read. Try again.",
    busy: "The pane did not answer within two seconds. Try again.",
    destination: "The address of this provider cannot be checked or is not allowed. Nothing was sent.",
    local_only: "Local only is on, and this provider is not on this computer.",
  };
  const ctx = {
    panes: {},
    localContext: "ask",
    localOnly: false,
    ttl: 300,
    persist: false,
    shares: [],
    offers: [],
    sentHashes: new Map(), // conversation -> hashes already sent
    lookups: 0,
    reads: 0,
    failures: new Map(), // command -> { reason, text, code }
    holdCommand: null,
    waiting: [],
    offerNo: 0,
    answers: [],
  };
  /** A terminal pane as the mock offers it: the kinds and the item each kind reads. */
  const terminalPane = (n) => ({
    kinds: ["selection", "screen", "last_command"],
    items: {
      screen: { label: `Screen of pane ${n}`, bytes: 2457, findings: [], text: `${CANARY_TEXT} ${CANARY_KEY}` },
      last_command: { label: `Last command output of pane ${n}`, bytes: 918, findings: [], text: `${CANARY_TEXT} last` },
    },
  });
  const resetContext = () => {
    Object.assign(ctx, {
      panes: { 1: terminalPane(1), 2: terminalPane(2) }, localContext: "ask", localOnly: false, ttl: 300, persist: false, shares: [],
      offers: [], sentHashes: new Map(), lookups: 0, reads: 0, holdCommand: null, offerNo: 0, answers: [],
    });
    ctx.failures.clear();
    ctx.waiting.length = 0;
  };
  resetContext();
  const paneOf = (n) => ctx.panes[n] ?? { kinds: [], items: {} };
  const contextFault = (reason, text, code = "context") => fault(code, text ?? REASON_TEXT[reason] ?? "the request was refused", reason);
  const LOCAL = ["on_device", "loopback"];
  /** The route a context call names: the profile (given, the chat's pin or the only one), the model and the address. */
  const contextRoute = (conversation, profile, model) => {
    const list = Array.isArray(ai.profiles) ? ai.profiles : ai.profiles.profiles ?? [];
    const id = profile || chatOf(conversation).profile || (list.length === 1 ? list[0].id : null);
    if (!id) throw fault("no_profile", "no provider is chosen for this chat");
    const known = list.find((p) => p.id === id);
    if (!known) throw fault("profile", "there is no such profile", "unknown_profile");
    const route = {
      profile: id, model: model || known.model || "", host: hostOf(known.base_url) || known.host || "", locality: known.locality ?? "loopback",
    };
    route.key = `${route.profile}|${route.model}|${known.base_url}`;
    return route;
  };
  const paneOpen = (n) => view().panes.some(([id]) => id === n);
  /** The checks every context call makes first; a call on another route removes the shares of the chat. */
  const contextCall = (command, { conversation, pane, profile, model }) => {
    const failure = ctx.failures.get(command);
    if (failure) throw contextFault(failure.reason, failure.text, failure.code);
    ctx.lookups += 1;
    if (typeof conversation !== "string" || !Number.isInteger(pane)) throw fault("invalid_conversation", "the request was not understood");
    const route = contextRoute(conversation, profile, model);
    ctx.shares = ctx.shares.filter((s) => s.conversation !== conversation || s.route === route.key);
    if (!paneOpen(pane)) throw fault("no_such_pane", "this pane is not open");
    if (ctx.localOnly && !LOCAL.includes(route.locality)) throw contextFault("local_only");
    return route;
  };
  const required = (route) => !(LOCAL.includes(route.locality) && ctx.localContext === "auto");
  const isShared = (conversation, pane, route) =>
    ctx.shares.some((s) => s.conversation === conversation && s.pane === pane && s.route === route.key);
  const contextState = (conversation, pane, route) => ({
    profile: route.profile, model: route.model, host: route.host, locality: route.locality, required: required(route),
    shared: isShared(conversation, pane, route), kinds: [...paneOf(pane).kinds], persist: ctx.persist,
  });
  /** An answer that waits for `release()` when the command is held; the call is recorded and acted on at once. */
  const answered = (command, value) => {
    ctx.answers.push([command, structuredClone(value)]);
    if (ctx.holdCommand !== command) return value;
    return new Promise((resolve) => ctx.waiting.push(() => resolve(value)));
  };
  const hex = (n) => Array.from({ length: n }, () => Math.floor(Math.random() * 16).toString(16)).join("");
  const hashOf = (text) => {
    let out = "";
    for (let seed = 0; seed < 8; seed += 1) {
      let h = 0x811c9dc5 ^ (seed * 0x9e3779b1);
      for (let i = 0; i < text.length; i += 1) h = Math.imul(h ^ text.charCodeAt(i), 0x01000193);
      out += (h >>> 0).toString(16).padStart(8, "0");
    }
    return out;
  };
  const contextCommands = {
    ai_context_state: (args) => {
      const route = contextCall("ai_context_state", args);
      return answered("ai_context_state", contextState(args.conversation, args.pane, route));
    },
    ai_context_share: (args) => {
      const route = contextCall("ai_context_share", args);
      if (paneOf(args.pane).kinds.length === 0) throw contextFault("nothing_to_share");
      ctx.shares = ctx.shares.filter((s) => !(s.conversation === args.conversation && s.pane === args.pane));
      if (args.shared === true) ctx.shares.push({ conversation: args.conversation, pane: args.pane, route: route.key });
      else ctx.offers = ctx.offers.filter((o) => !(o.conversation === args.conversation && o.pane === args.pane));
      return answered("ai_context_share", contextState(args.conversation, args.pane, route));
    },
    ai_context_preview: (args) => {
      const route = contextCall("ai_context_preview", args);
      if (required(route) && !isShared(args.conversation, args.pane, route)) throw contextFault("not_shared");
      const kinds = args.kinds;
      if (!Array.isArray(kinds) || kinds.length === 0 || kinds.length > 5 || new Set(kinds).size !== kinds.length
        || kinds.some((k) => !CONTEXT_KINDS.includes(k))) throw contextFault("bad_kinds");
      const pane = paneOf(args.pane);
      if (kinds.some((k) => !pane.kinds.includes(k))) throw contextFault("unsupported_kind");
      const items = [];
      const empty = [];
      const texts = [];
      for (const kind of kinds) {
        ctx.reads += 1;
        const read = pane.items[kind];
        const found = Array.isArray(read) ? read : read ? [read] : [];
        if (found.length === 0) empty.push(kind);
        for (const one of found) {
          items.push({ id: items.length + 1, kind, label: one.label, bytes: one.bytes, findings: structuredClone(one.findings) });
          texts.push(one.text ?? one.label);
        }
      }
      if (items.length === 0) throw contextFault("nothing_to_share");
      ctx.offers = ctx.offers.filter((o) => o.conversation !== args.conversation);
      const id = hex(32);
      ctx.offers.push({
        id, conversation: args.conversation, pane: args.pane, route: route.key, items: structuredClone(items), texts, at: Date.now(),
        profile: route.profile, locality: route.locality, expired: false,
      });
      ctx.offerNo += 1;
      return answered("ai_context_preview", {
        offer: id, profile: route.profile, model: route.model, host: route.host, locality: route.locality, ttl_seconds: ctx.ttl, items,
        empty, bytes: items.reduce((sum, item) => sum + item.bytes, 0), persist: ctx.persist,
      });
    },
  };
  /** The `context` part that a send with an offer puts before its text, or `[]` for a send without one. */
  const redeem = (conversation, pane, route, offer, picked) => {
    if (offer === undefined && picked === undefined) return [];
    if (offer === undefined || picked === undefined || offer === null || picked === null) throw contextFault("bad_pick");
    const at = ctx.offers.findIndex((o) => o.id === offer && o.conversation === conversation);
    const live = at < 0 ? null : ctx.offers[at];
    if (live === null || live.expired || Date.now() - live.at > ctx.ttl * 1000 || live.pane !== pane || live.route !== route.key) {
      throw contextFault("stale_offer");
    }
    if (!Array.isArray(picked) || picked.length === 0 || picked.length > 8 || new Set(picked).size !== picked.length
      || picked.some((n) => !live.items.some((item) => item.id === n))) throw contextFault("bad_pick");
    ctx.offers.splice(at, 1);
    const chosen = live.items.filter((item) => picked.includes(item.id));
    const hash = hashOf(chosen.map((item) => `${item.kind}|${item.label}|${live.texts[live.items.indexOf(item)]}`).join("\n"));
    const sent = ctx.sentHashes.get(conversation) ?? new Set();
    if (sent.has(hash)) throw fault("context_duplicate", "the same context is attached twice");
    sent.add(hash);
    ctx.sentHashes.set(conversation, sent);
    return [{
      type: "context", hash, items: chosen.map((item) => ({ kind: item.kind, label: item.label, bytes: item.bytes })),
      provider: live.profile, locality: live.locality,
    }];
  };
  const aiCommands = {
    ...contextCommands,
    ai_conversation_new: () => `chat-mock-${++ai.chatNo}`,
    ai_conversation_list: () => ({ chats: [], notes: [] }),
    ai_conversation_delete: ({ conversation }) => {
      for (const key of [...undoable.keys()]) if (key.startsWith(`${conversation}|`)) undoable.delete(key);
      return ai.chats.delete(conversation);
    },
    ai_profiles: () => ({ profiles: listed(), notes: structuredClone(noteLines()) }),
    ai_profile_save: ({ input }) => {
      if (input === null || typeof input !== "object" || Object.keys(input).some((name) => !SAVE_MEMBERS.includes(name))) {
        throw fault("profile", "the request was not understood");
      }
      const list = stored();
      // The order of the core: the id, the type, the shape, the name, the model, the address, the count.
      let found;
      if (input.id !== null && input.id !== undefined) {
        if (typeof input.id !== "string" || !/^[a-z0-9_-]{1,40}$/.test(input.id)) throw refuse("id");
        if (input.id === "apple") throw refuse("reserved_id");
        found = list.find((p) => p.id === input.id);
        if (!found) throw refuse("unknown_profile");
      }
      const type = MOCK_TYPES[input.preset];
      if (!type) throw fault("profile", "the request was not understood");
      if (found && found.preset !== input.preset) throw refuse("preset_change");
      const wire = input.wire ?? found?.wire ?? type.wires[0];
      if (!type.wires.includes(wire)) throw refuse("wire");
      const name = typeof input.name === "string" ? input.name.trim() : "";
      if (name === "" || Array.from(name).length > 80 || hasControl(name)) throw refuse("name");
      const model = typeof input.model === "string" ? input.model.trim() : "";
      if (Array.from(model).length > 200 || hasControl(model)) throw refuse("model");
      const typed = typeof input.base_url === "string" ? input.base_url.trim() : "";
      const base = typed === "" ? type.base : typed;
      checkAddress(base, input.allow_lan_http === true);
      if (!found && list.length >= 32) throw refuse("too_many");
      const entry = {
        id: found ? found.id : idFrom(name, (id) => list.some((p) => p.id === id)),
        name,
        preset: input.preset,
        wire,
        base_url: base,
        model,
        allow_lan_http: input.allow_lan_http === true,
        host: hostOf(base),
        key: found ? found.key : "not_set",
        gateway_key: found ? found.gateway_key : input.preset === "cloudflare_gateway" ? "not_set" : null,
        disclosure: null,
        locality: localityOf(base),
      };
      if (found) Object.assign(found, entry);
      else list.push(entry);
      // The notice is allowed only by a call that says so, and only when one applies to this host.
      if (input.acknowledge_gateway === true && disclosureOf(entry) !== null) ai.acknowledged.push(entry.host);
      return entry.id;
    },
    ai_profile_remove: ({ profile }) => {
      const list = stored();
      const at = list.findIndex((p) => p.id === profile);
      if (at < 0) return { removed: false, key_deleted: false };
      list.splice(at, 1);
      delete ai.keys[profile];
      return { removed: true, key_deleted: true };
    },
    ai_key_set: ({ profile, slot, api_key }) => {
      const entry = stored().find((p) => p.id === profile);
      if (!entry) throw keyFault("profile", "unknown_profile");
      if ((slot !== "provider" && slot !== "gateway") || (slot === "gateway" && entry.gateway_key === null)) {
        throw keyFault("key", "no_such_slot");
      }
      if (!ai.keychain) throw keyFault("key", "no_secure_store");
      const key = typeof api_key === "string" ? api_key.replace(/^[ \t\r\n\f]+|[ \t\r\n\f]+$/g, "") : "";
      if (key === "" || key.length > 2048 || !/^[\x21-\x7e]+$/.test(key)) throw keyFault("key", "invalid");
      // Only the origin the key is filed for and its length are kept, never the key.
      (ai.keys[profile] ??= {})[slot] = { scope: originOf(entry.base_url), length: key.length };
      return null;
    },
    ai_key_clear: ({ profile, slot }) => {
      const entry = stored().find((p) => p.id === profile);
      if (!entry) throw keyFault("profile", "unknown_profile");
      if ((slot !== "provider" && slot !== "gateway") || (slot === "gateway" && entry.gateway_key === null)) {
        throw keyFault("key", "no_such_slot");
      }
      if (ai.keys[profile]) {
        delete ai.keys[profile][slot];
        if (Object.keys(ai.keys[profile]).length === 0) delete ai.keys[profile];
      }
      if (slot === "provider") entry.key = "not_set";
      else entry.gateway_key = "not_set";
      return null;
    },
    ai_turn_snapshot: ({ conversation }) => {
      const copy = structuredClone(chatOf(conversation));
      // The store keeps no tool output as text: a chat that is not running shows a marker in its place.
      if (!copy.running) {
        for (const m of copy.messages) for (const part of m.parts) if (part.type === "tool_result") part.content = STORED;
      }
      return copy;
    },
    ai_decide: decide,
    ai_undo: ({ conversation, call_id }) => {
      undone.calls.push({ conversation, call_id });
      if (undone.bare) {
        undone.bare = false;
        throw "the undo could not be done";
      }
      if (undone.refuse !== null) {
        const reason = undone.refuse;
        undone.refuse = null;
        throw fault("undo", UNDO_TEXTS[reason], reason);
      }
      const key = `${conversation}|${call_id}`;
      const record = undoable.get(key);
      if (record === undefined) throw fault("undo", UNDO_TEXTS.unknown_change, "unknown_change");
      if (record.old === undefined) throw fault("undo", UNDO_TEXTS.created, "created");
      if (wfiles.get(record.path) !== record.written) throw fault("undo", UNDO_TEXTS.file_changed, "file_changed");
      wfiles.set(record.path, record.old);
      undoable.delete(key);
      return null;
    },
    // The core's renderer of Markdown: here the text as one escaped paragraph, unless a test scripts it with `on`.
    markdown_render: ({ text }) =>
      `<p>${String(text).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;")}</p>`,
    ai_send: ({ conversation, text, pane, profile, model, offer, picked }) => {
      if (typeof text !== "string" || text.trim() === "") throw fault("empty_text", "the message is empty");
      if (ai.running !== null) throw fault("busy", "a reply is already running");
      const chat = chatOf(conversation);
      const id = profile || chat.profile;
      if (!id) throw fault("no_profile", "no provider is chosen");
      const list = Array.isArray(ai.profiles) ? ai.profiles : ai.profiles.profiles ?? [];
      const known = list.find((p) => p.id === id);
      if (!known) throw fault("profile", "there is no such profile", "unknown_profile");
      const route = { profile: id, model: model || known.model || chat.model || "", host: known.host ?? null, locality: known.locality ?? "loopback" };
      const before = redeem(conversation, pane, contextRoute(conversation, id, model), offer, picked);
      const turn = ++ai.turnNo;
      ai.queue = turnEvents(conversation, turn, text, route, ai.scripts.shift() ?? {}, before, pane);
      ai.running = conversation;
      pump();
      return { conversation, turn };
    },
    ai_cancel: ({ conversation }) => {
      if (ai.running !== conversation) return false;
      const state = chatOf(conversation);
      ai.queue = [];
      ai.running = null;
      clearTimeout(ai.timer);
      ai.timer = 0;
      // The end of a cancelled turn: the text so far is kept as a stopped message, then the turn ends.
      setTimeout(() => {
        let seq = state.seq;
        for (const e of stopTools(conversation, state)) {
          produce(conversation, { conversation, turn: state.turn, seq: ++seq, ...e });
        }
        if (state.partial.text !== "") {
          produce(conversation, {
            conversation, turn: state.turn, seq: ++seq, kind: "message",
            message: assistantMessage(state.turn, state.model, state.partial.text, "cancelled", null),
          });
        }
        produce(conversation, {
          conversation, turn: state.turn, seq: ++seq, kind: "turn_ended", end: "cancelled", error: null, rounds: 1,
          usage: { input_tokens: 0, output_tokens: 0 }, saved: true,
        });
      }, 0);
      return true;
    },
  };
  mockAi = {
    get profiles() {
      return ai.profiles;
    },
    set profiles(value) {
      ai.profiles = value;
    },
    get notes() {
      return ai.notes;
    },
    set notes(value) {
      ai.notes = value;
    },
    get acknowledged() {
      return ai.acknowledged;
    },
    set acknowledged(value) {
      ai.acknowledged = value;
    },
    get keychain() {
      return ai.keychain;
    },
    set keychain(value) {
      ai.keychain = value === true;
    },
    get keys() {
      return ai.keys;
    },
    set keys(value) {
      ai.keys = value;
    },
    sent: ai.sent,
    context: {
      get panes() {
        return ctx.panes;
      },
      set panes(value) {
        ctx.panes = value;
      },
      get localContext() {
        return ctx.localContext;
      },
      set localContext(value) {
        ctx.localContext = value;
      },
      get localOnly() {
        return ctx.localOnly;
      },
      set localOnly(value) {
        ctx.localOnly = value === true;
      },
      get ttl() {
        return ctx.ttl;
      },
      set ttl(value) {
        ctx.ttl = value;
      },
      get persist() {
        return ctx.persist;
      },
      set persist(value) {
        ctx.persist = value === true;
      },
      get shares() {
        return ctx.shares;
      },
      get offers() {
        return ctx.offers;
      },
      get lookups() {
        return ctx.lookups;
      },
      get reads() {
        return ctx.reads;
      },
      get answers() {
        return ctx.answers;
      },
      closePane(n) {
        ctx.shares = ctx.shares.filter((s) => s.pane !== n);
        ctx.offers = ctx.offers.filter((o) => o.pane !== n);
        delete ctx.panes[n];
        const v = view();
        v.panes = v.panes.filter(([id]) => id !== n);
        mock.setView(v);
      },
      expire() {
        for (const o of ctx.offers) o.expired = true;
      },
      fail(cmd, reason, text, code) {
        if (reason === null || reason === undefined) ctx.failures.delete(cmd);
        else ctx.failures.set(cmd, { reason, text, code });
      },
      hold(cmd) {
        ctx.holdCommand = cmd;
      },
      release() {
        ctx.holdCommand = null;
        for (const go of ctx.waiting.splice(0)) go();
      },
    },
    get files() {
      return Object.fromEntries(wfiles);
    },
    write: {
      seed: (path, text) => void wfiles.set(path, text),
      change(path, text) {
        if (text === null) wfiles.delete(path);
        else wfiles.set(path, text);
      },
      text: (path) => wfiles.get(path) ?? null,
      /** The sentences the mock holds for the core's refusals: copies, held to the golden of the core by the contract case. */
      texts: { payload_changed: PAYLOAD_CHANGED, faults: structuredClone(FAULTS), undo: structuredClone(UNDO_TEXTS) },
      undoRefuse(reason) {
        undone.refuse = reason;
      },
      undoBare() {
        undone.bare = true;
      },
      get undos() {
        return structuredClone(undone.calls);
      },
    },
    exec: {
      get runs() {
        return structuredClone(ran.runs);
      },
      hold() {
        ran.held = true;
      },
      release() {
        ran.held = false;
        for (const { conversation, call } of ran.waiting.splice(0)) {
          const state = ai.chats.get(conversation);
          const live = appr.live.get(conversation);
          if (state === undefined || live === undefined || live.closed) continue;
          settle(conversation, state, live, call, "allow");
        }
      },
      output(text, failed = false) {
        ran.next = { text, failed };
      },
    },
    approval: {
      get decisions() {
        return appr.decisions;
      },
      refuse(reason) {
        appr.refuse = reason;
      },
      changeHash(callId) {
        for (const state of ai.chats.values()) {
          for (const entry of state.pending) if (entry.call_id === callId) entry.hash = digestOf(`${entry.hash}|changed`);
        }
      },
      hold() {
        appr.held = true;
      },
      release() {
        appr.held = false;
        for (const go of appr.waiters.splice(0)) go();
      },
      waiting: (chat) => chatOf(chat).pending.map((p) => p.call_id),
      save: () =>
        structuredClone({
          chats: [...ai.chats.entries()], live: [...appr.live.entries()], running: ai.running, turnNo: ai.turnNo, chatNo: ai.chatNo,
          decisions: appr.decisions, files: [...wfiles.entries()], undoable: [...undoable.entries()], undos: undone.calls, runs: ran.runs,
        }),
      restore(saved) {
        ai.chats = new Map(saved.chats);
        appr.live = new Map(saved.live);
        ai.running = saved.running;
        ai.turnNo = saved.turnNo;
        ai.chatNo = saved.chatNo;
        appr.decisions.push(...saved.decisions);
        wfiles.clear();
        for (const [path, text] of saved.files ?? []) wfiles.set(path, text);
        undoable.clear();
        for (const [key, record] of saved.undoable ?? []) undoable.set(key, record);
        undone.calls.push(...(saved.undos ?? []));
        ran.runs.push(...(saved.runs ?? []));
      },
    },
    script: (turn) => void ai.scripts.push(turn),
    hold: (n) => void (ai.holdLeft = n),
    release() {
      ai.holdLeft = null;
      pump();
    },
    drop: (n) => void (ai.dropLeft = n),
    chat: (id) => structuredClone(chatOf(id)),
    setState(id, partial) {
      Object.assign(chatOf(id), structuredClone(partial));
    },
    reset() {
      clearTimeout(ai.timer);
      Object.assign(ai, {
        profiles: [], scripts: [], chatNo: 0, turnNo: NOW, queue: [], running: null, holdLeft: null, dropLeft: 0, timer: 0,
        notes: [], keychain: true, keys: {}, acknowledged: [],
      });
      ai.chats.clear();
      ai.sent.length = 0;
      appr.decisions.length = 0;
      appr.refuse = null;
      appr.held = false;
      for (const go of appr.waiters.splice(0)) go();
      appr.live.clear();
      wfiles.clear();
      undoable.clear();
      Object.assign(undone, { refuse: null, bare: false });
      undone.calls.length = 0;
      Object.assign(ran, { held: false, waiting: [], next: null });
      ran.runs.length = 0;
      resetContext();
    },
  };

  const mock = {
    calls,
    label,
    callsOf: (cmd) => calls.filter((c) => c[0] === cmd).map((c) => c[1]),
    emit: (name, payload) => deliver(name, payload, () => true),
    emitTo: (to, name, payload) => deliver(name, payload, (l) => l.target === undefined || l.target === to),
    listening: (name) => listeners.get(name)?.size ?? 0,
    failLayout(message) {
      layoutError = message ?? null;
    },
    on(cmd, fn) {
      if (fn) handlers.set(cmd, fn);
      else handlers.delete(cmd);
    },
    setView(v) {
      custom = structuredClone(v);
    },
    view,
    reset() {
      calls.length = 0;
      handlers.clear();
      ratio = 0.5;
      custom = null;
      layoutError = null;
      mockAi.reset();
    },
    get ai() {
      return mockAi;
    },
  };

  window.__MOCK__ = mock;
  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args) => {
        calls.push([cmd, args]);
        const handler = handlers.get(cmd);
        if (handler) return (await handler(args)) ?? null;
        if (Object.hasOwn(aiCommands, cmd)) return aiCommands[cmd](args ?? {}) ?? null;
        if (LAYOUT_COMMANDS.has(cmd) && layoutError !== null) throw layoutError; // Tauri rejects with the error text
        if (cmd === "set_ratio") setRatio(args);
        if (cmd === "release_outside") return { outcome: "cancelled" };
        return LAYOUT_COMMANDS.has(cmd) ? view() : null;
      },
    },
    event: {
      // `options.target` is the label of the window whose events are wanted; without it the listener hears all.
      listen: async (name, cb, options) => {
        if (!listeners.has(name)) listeners.set(name, new Set());
        const entry = { cb, target: typeof options?.target === "string" ? options.target : undefined };
        listeners.get(name).add(entry);
        return () => listeners.get(name)?.delete(entry);
      },
    },
  };
})();
