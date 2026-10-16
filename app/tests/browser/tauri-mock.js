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
//     The commands `ai_conversation_new`, `ai_conversation_list`, `ai_conversation_delete`, `ai_send`, `ai_cancel`,
//     `ai_turn_snapshot`, `ai_profiles`, `ai_profile_save`, `ai_profile_remove`, `ai_key_set` and `ai_key_clear` have
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
        if (state.title === "") state.title = titleOf(e.message.parts[0].text);
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
  const userMessage = (turn, text) => ({
    id: `m${turn}-user`, role: "user", ts: NOW, model: null, status: "complete", usage: null, parts: [{ type: "text", text }],
  });
  const assistantMessage = (turn, model, text, status, usage) => ({
    id: `m${turn}-1`, role: "assistant", ts: NOW, model, status, usage, parts: [{ type: "text", text }],
  });
  /** The events of one turn from its script, with the head (`conversation`, `turn`) but not the `seq` yet. */
  const turnEvents = (conversation, turn, text, route, script) => {
    const out = [{ kind: "turn_started", message: userMessage(turn, text), route }];
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
  const aiCommands = {
    ai_conversation_new: () => `chat-mock-${++ai.chatNo}`,
    ai_conversation_list: () => ({ chats: [], notes: [] }),
    ai_conversation_delete: ({ conversation }) => ai.chats.delete(conversation),
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
    ai_turn_snapshot: ({ conversation }) => structuredClone(chatOf(conversation)),
    // The core's renderer of Markdown: here the text as one escaped paragraph, unless a test scripts it with `on`.
    markdown_render: ({ text }) =>
      `<p>${String(text).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;")}</p>`,
    ai_send: ({ conversation, text, profile, model }) => {
      if (typeof text !== "string" || text.trim() === "") throw fault("empty_text", "the message is empty");
      if (ai.running !== null) throw fault("busy", "a reply is already running");
      const chat = chatOf(conversation);
      const id = profile || chat.profile;
      if (!id) throw fault("no_profile", "no provider is chosen");
      const list = Array.isArray(ai.profiles) ? ai.profiles : ai.profiles.profiles ?? [];
      const known = list.find((p) => p.id === id);
      if (!known) throw fault("profile", "there is no such profile", "unknown_profile");
      const turn = ++ai.turnNo;
      const route = { profile: id, model: model || known.model || chat.model || "", host: known.host ?? null, locality: known.locality ?? "loopback" };
      ai.queue = turnEvents(conversation, turn, text, route, ai.scripts.shift() ?? {});
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
