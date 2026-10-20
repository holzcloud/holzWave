// Shared helpers of the AI chat browser tests (ai*.test.mjs): a booted page with panes, saved profiles and a block layer
// that opens chat panes, the profiles of the tests, and the two things a person does most, opening a chat and sending.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { APP_DIR, callsOf } from "./lib.mjs";

/** The settings view of the protocol golden: the core's answer to `get_settings`. */
const SETTINGS_VIEW = JSON.parse(readFileSync(join(APP_DIR, "tests/fixtures/protocol.json"), "utf8")).settings_view;

/** One saved profile as `ai_profiles` lists it, plus the mock-only member `locality` that the route of a turn reports. */
export const PROFILE = {
  id: "local-1",
  name: "Local",
  preset: "ollama",
  wire: "ollama",
  base_url: `${"http"}://localhost:11434`,
  model: "llama3.2",
  allow_lan_http: false,
  host: "localhost",
  key: "not_set",
  gateway_key: null,
  disclosure: null,
  locality: "loopback",
};

/** A cloud profile for the cases that need a second one. */
export const OTHER_PROFILE = {
  id: "other-1",
  name: "Cloud",
  preset: "anthropic",
  wire: "anthropic",
  base_url: `${"https"}://api.example.org`,
  model: "big-model",
  allow_lan_http: false,
  host: "api.example.org",
  key: "set",
  gateway_key: null,
  disclosure: null,
  locality: "cloud",
};

/** The text the mock's panes hold and the key drawn on a screen. Neither may occur in any call, node, attribute or store. */
export const PANE_TEXT = "zq-consent-text-71b9e4";
export const PANE_KEY = "sk-ant-api03-zqconsent5e90b3aXYZ0123456789";

/** The path and the text of a file the mock's tool turns read. Neither may occur in any call, attribute or store. */
export const TOOL_PATH = "/work/zq-approval-path-3f7b02/notes.txt";
export const TOOL_TEXT = "zq-approval-text-6a41d9";

/**
 * The file the mock's write tools change, the text it holds before (three lines) and the text the model writes over it. The
 * two canaries occur only in the files of the tests and in the text nodes of a card; neither may be in any call, attribute or
 * store of the page.
 */
export const TOOL_FILE = "/work/proj/todo.txt";
export const FILE_OLD = "alpha\nbeta zq-old-text-4d2e91\ngamma\n";
export const FILE_NEW = "alpha\nbeta zq-new-text-b83f17\ngamma\n";

/** Two terminal panes side by side. */
export const VIEW = {
  workspaces: [{ name: "main", active: true, panes: 2 }],
  tabs: [{ title: "shell", active: true, focus: 2, panes: 2 }],
  focus: 2,
  panes: [[1, { x: 0, y: 0, w: 0.5, h: 1 }], [2, { x: 0.5, y: 0, w: 0.5, h: 1 }]],
  dividers: [{ split: 10, axis: "Row", area: { x: 0, y: 0, w: 1, h: 1 }, ratio: 0.5 }],
  pane_names: [],
  windows: 1,
};

/** The same two panes with the focus on pane 1, so a chat opens beside it. */
export const VIEW_P1 = { ...VIEW, tabs: [{ title: "shell", active: true, focus: 1, panes: 2 }], focus: 1 };

/**
 * Open the page with the saved profiles `profiles`, the panes of `view` and the block layer of the core played by the
 * mock: `block_open` splits the focused pane to the right, focuses the new pane and lists its block in `pane_blocks`.
 * `blocks` are the blocks that exist at the start. `context` is members of `__MOCK__.ai.context` to set (`panes`,
 * `localContext`, `localOnly`, `ttl`, `persist`) before the page loads. `device` is the state of the Mac's on-device model, `{ state,
 * advice }` (default: no such model, so the answer of `ai_profiles` has no `on_device` member).
 *
 * `auto` is the state of the toggle `Auto` the case starts with. The product starts with it on. A case that is not about Auto
 * leaves this false: an observer then presses each toggle once, the first time it is drawn pressed, so the case sends plain
 * messages as it did before Auto existed. With `auto: true` nothing is installed and the page keeps its own default.
 */
export async function boot(
  page, url,
  { profiles = [PROFILE], view = VIEW, blocks = [], states = {}, search = "", starts = 2, settings = false, context = {}, clock = false, ask = {}, device = null, auto = false } = {},
) {
  await page.addInitScript(
    ([saved, shown, existing, chats, shared, frozen, asked, onDevice, autoOn]) => {
      window.__MOCK__.reset();
      if (!autoOn) {
        const handled = new WeakSet();
        const settle = () => {
          for (const button of document.querySelectorAll("button.chat-ctx")) {
            const pressed = button.getAttribute("aria-pressed");
            if (pressed === null || handled.has(button)) continue;
            handled.add(button);
            if (pressed === "true") button.click();
          }
        };
        new MutationObserver(settle).observe(document, { childList: true, subtree: true, attributes: true, attributeFilter: ["aria-pressed"] });
      }
      // A frozen clock: `Date.now` stands still and only `tickClock` moves it, so a window of the page that is measured in
      // milliseconds is the same on every machine. Off by default, so the other cases keep their page as it was.
      window.__REAL_NOW ??= Date.now;
      if (frozen) {
        const base = window.__REAL_NOW();
        window.__CLOCK = 0;
        Date.now = () => base + window.__CLOCK;
      } else {
        Date.now = window.__REAL_NOW;
      }
      Object.assign(window.__MOCK__.ai.context, shared);
      Object.assign(window.__MOCK__.ai.ask, asked);
      window.__MOCK__.ai.profiles = saved;
      if (onDevice) window.__MOCK__.ai.setDevice(onDevice.state, onDevice.advice);
      for (const [id, partial] of Object.entries(chats)) window.__MOCK__.ai.setState(id, partial);
      window.__MOCK__.setView(shown);
      window.__BLOCKS = existing;
      window.__MOCK__.on("pane_blocks", () => structuredClone(window.__BLOCKS));
      window.__MOCK__.on("block_open", ({ kind, source, path }) => {
        const v = window.__MOCK__.view();
        const at = v.panes.find(([id]) => id === v.focus) ?? v.panes[v.panes.length - 1];
        const made = Math.max(...v.panes.map(([id]) => id)) + 1;
        const half = at[1].w / 2;
        at[1].w = half;
        v.panes.push([made, { x: at[1].x + half, y: at[1].y, w: half, h: at[1].h }]);
        v.focus = made;
        v.tabs[0].focus = made;
        v.tabs[0].panes = v.panes.length;
        v.workspaces[0].panes = v.panes.length;
        window.__MOCK__.setView(v);
        window.__BLOCKS.push({ pane: made, kind, source, path, source_label: "This computer" });
        return window.__MOCK__.view();
      });
    },
    [profiles, view, blocks, states, context, clock, ask, device, auto],
  );
  // The settings of the core are played only when asked for, so the chat cases keep their page exactly as it was.
  if (settings) await settingsCore(page, typeof settings === "object" ? settings : {});
  await page.goto(`${url}index.html?renderer=canvas${search}`);
  if (view.dividers.length > 0) await page.waitForSelector(".divider");
  await page.waitForFunction((n) => window.__MOCK__.callsOf("start_pane").length >= n, starts);
  await page.evaluate(async () => {
    window.__registry = await import(new URL("actions.js", location.href).href);
  });
}

/**
 * Open the consent card the way the palette action Explain error does (the exported `attachContext` of chat.js with no kinds)
 * and wait for it. The toggle `Auto` no longer opens the card. Answers the sentence the call gave (null when the card opened).
 */
export async function openContext(page, conversation = "chat-mock-1") {
  const sentence = await page.evaluate(async (id) => {
    const chat = await import(new URL("chat.js", location.href).href);
    return chat.attachContext(id, []);
  }, conversation);
  if (sentence === null) await page.waitForSelector(".chat-consent:not([aria-busy])");
  return sentence;
}

/** The calls of the three context commands, oldest first, as `[command, args]`. */
export const ctxCalls = (page) =>
  page.evaluate(() => window.__MOCK__.calls.filter((c) => c[0].startsWith("ai_context_")).map((c) => [c[0], structuredClone(c[1])]));

/**
 * Change the two privacy settings: the knobs of the mock and, when the in-page settings core is installed, the same keys
 * of its view, announced the way `set_setting` does. A key left out is unchanged.
 */
export async function setPrivacy(page, { localOnly, localContext } = {}) {
  await page.evaluate(
    ([only, mode]) => {
      const context = window.__MOCK__.ai.context;
      if (only !== undefined) context.localOnly = only;
      if (mode !== undefined) context.localContext = mode;
      if (window.__core) {
        const view = window.__core.view();
        if (only !== undefined) view.settings.ai_local_only = only;
        if (mode !== undefined) view.settings.ai_local_context = mode;
        window.__core.set(view);
        window.__MOCK__.emit("settings", view);
      }
    },
    [localOnly, localContext],
  );
}

/** Change the state of the Mac's on-device model as the core will report it from now on (`null` removes the model). */
export async function setDevice(page, state, advice) {
  await page.evaluate(([s, a]) => window.__MOCK__.ai.setDevice(s, a), [state, advice]);
}

/** Click `✦ AI` and wait for the chat pane. */
export async function openAi(page) {
  await page.locator(".ai-btn").click();
  await page.waitForSelector(".block-chat");
}

/** Type a message into the composer and press Enter. */
export async function sendText(page, text) {
  await page.locator(".chat-input").fill(text);
  await page.locator(".chat-input").press("Enter");
}

/** A view with three panes: the two terminals and, as pane 3, a chat that was open when the page loaded. */
export const VIEW3 = {
  ...VIEW,
  tabs: [{ title: "shell", active: true, focus: 2, panes: 3 }],
  workspaces: [{ name: "main", active: true, panes: 3 }],
  panes: [[1, { x: 0, y: 0, w: 0.5, h: 1 }], [2, { x: 0.5, y: 0, w: 0.25, h: 1 }], [3, { x: 0.75, y: 0, w: 0.25, h: 1 }]],
};

/** One pane, the chat: the screen of a phone. */
export const VIEW_CHAT_ONLY = {
  ...VIEW,
  workspaces: [{ name: "main", active: true, panes: 1 }],
  tabs: [{ title: "chat", active: true, focus: 3, panes: 1 }],
  focus: 3,
  panes: [[3, { x: 0, y: 0, w: 1, h: 1 }]],
  dividers: [],
};

/** The chat block of pane 3 as the core lists it. */
export const chatBlock = (path) => ({ pane: 3, kind: "chat", source: "local", path, source_label: "This computer" });

/** Write down every text the status row of the chat shows, in order, in `window.__STATUS`. */
export async function watchStatus(page) {
  await page.evaluate(() => {
    window.__STATUS = [];
    const row = document.querySelector(".chat-status");
    const note = () => {
      const text = row.textContent;
      if (window.__STATUS[window.__STATUS.length - 1] !== text) window.__STATUS.push(text);
    };
    new MutationObserver(note).observe(row, { childList: true, characterData: true, subtree: true });
    note();
  });
}

/** The args of every call the page made to `cmd`, oldest first. */
export const calls = callsOf;

/** Wait for the providers dialog to be on screen with its list read. */
export async function openDialog(page) {
  await page.waitForSelector(".dlg.ai-settings");
  await page.waitForSelector('.dlg.ai-settings .aiset-rows:not([aria-busy="true"])', { state: "attached" });
}

/** Choose a provider type in the open dialog by clicking the text of its radio. */
export async function choosePreset(page, label) {
  await page.locator("fieldset.aiset-presets").getByText(label, { exact: true }).click();
}

/** Replace the text of a field of the open dialog. */
export async function fillField(page, selector, text) {
  await page.locator(selector).fill(text);
}

/**
 * Install an in-page core for the settings: `get_settings`, `reload_settings` and `set_setting` over a copy of the golden
 * view, as the settings tests do. `set_setting` checks the type of the two AI keys, changes its copy, announces the
 * `settings` event and answers the whole view; `window.__core.fail = { key, message }` makes one key fail with a text, and
 * `window.__core.set(view)` replaces the copy. With `unavailable` the core cannot answer `get_settings`. Call it after the
 * page's `__MOCK__.reset()` (`boot` does): a reset removes the handlers.
 */
export async function settingsCore(page, { unavailable = false } = {}) {
  await page.addInitScript(
    ([initial, cannot]) => {
      let view = structuredClone(initial);
      window.__core = {
        fail: null,
        view: () => structuredClone(view),
        set(v) {
          view = structuredClone(v);
        },
      };
      const answer = () => {
        if (cannot) throw "the settings are not available";
        return structuredClone(view);
      };
      window.__MOCK__.on("get_settings", answer);
      window.__MOCK__.on("reload_settings", answer);
      window.__MOCK__.on("set_setting", ({ key, value }) => {
        const fail = window.__core.fail;
        if (fail && fail.key === key) throw fail.message;
        if (!(key in view.settings)) throw `there is no setting called "${key}"`;
        if (key === "ai_local_only" && typeof value !== "boolean") throw "invalid value for ai_local_only: expected true or false";
        if (key === "ai_local_context" && value !== "ask" && value !== "auto") throw "invalid value for ai_local_context: expected ask or auto";
        view.settings[key] = value;
        const out = structuredClone(view);
        window.__MOCK__.emit("settings", out);
        return out;
      });
    },
    [SETTINGS_VIEW, unavailable],
  );
}

/**
 * Queue a turn in which the model asks for tools. `spec` is a call (`id`, `tool`, `path`, `pattern`, `rows`, `payload`,
 * `result`, `error`, `target`, `remote`, `tainted`, `taint`) or a list of calls in `calls`, and `answer` is the text after
 * the last result. A call without a result text returns the file text of the tests.
 */
export async function scriptTool(page, spec) {
  const { calls: list, answer, ...single } = spec;
  const tools = (list ?? [single]).map((c) => ({ result: TOOL_TEXT, ...c }));
  await page.evaluate(([queued, text]) => window.__MOCK__.ai.script({ tools: queued, ...(text === undefined ? {} : { answer: text }) }), [tools, answer]);
}

/** Wait for the first approval card; answers its locator. */
export async function openCard(page, call = null) {
  const sel = call === null ? "section.chat-approval" : `section.chat-approval[data-call="${call}"]`;
  await page.waitForSelector(sel);
  return page.locator(sel).first();
}

/** The arguments of every decision the page made, oldest first. */
export const decisionCalls = (page) => callsOf(page, "ai_decide");

/** Move the page's frozen clock forward (the page must have been booted with the `clock` option). */
export async function tickClock(page, ms) {
  await page.evaluate((by) => {
    window.__CLOCK += by;
  }, ms);
}

/**
 * Reload the page and keep what the core would keep: the chats and their open turns, the layout and the block list. The
 * mock is seeded again before the page asks for its first snapshot.
 */
export async function reloadKeeping(page) {
  const saved = await page.evaluate(() => ({
    mock: window.__MOCK__.ai.approval.save(), view: window.__MOCK__.view(), blocks: structuredClone(window.__BLOCKS),
  }));
  await page.addInitScript((keep) => {
    window.__MOCK__.ai.approval.restore(keep.mock);
    window.__MOCK__.setView(keep.view);
    window.__BLOCKS.length = 0;
    window.__BLOCKS.push(...keep.blocks);
  }, saved);
  await page.reload();
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_turn_snapshot").length >= 1);
}

/**
 * Queue a turn in which the model asks for a write tool. `spec` is a call (`id`, `tool` as `write_file` or `edit_file`,
 * `path`, `content`, `old_text`, `new_text`, `replace_all`, `target`, `remote`, `tainted`, `taint`, `diff` and `payload` to
 * replace what the mock builds) or a list of calls in `calls`, and `answer` is the text after the last result.
 */
export async function scriptWrite(page, spec) {
  const { calls: list, answer, ...single } = spec;
  const tools = (list ?? [single]).map((c) => ({ tool: "write_file", ...c }));
  await page.evaluate(([queued, text]) => window.__MOCK__.ai.script({ tools: queued, ...(text === undefined ? {} : { answer: text }) }), [tools, answer]);
}

/** A file exists in the mock with this text. */
export const seedFile = (page, path, text) => page.evaluate(([p, t]) => window.__MOCK__.ai.write.seed(p, t), [path, text]);

/** The file changes without a call (a text of `null` removes it), as if another program wrote it. */
export const changeFile = (page, path, text) => page.evaluate(([p, t]) => window.__MOCK__.ai.write.change(p, t), [path, text]);

/** The text of a file in the mock, or null when there is none. */
export const fileText = (page, path) => page.evaluate((p) => window.__MOCK__.ai.write.text(p), path);

/** `count` added rows of a scripted diff, as the core would send them for a text of that many new lines. */
export const addRows = (count) => Array.from({ length: count }, (_, i) => ({ op: "add", old: null, new: i + 1, text: `new line ${i + 1}` }));

/** A diff of the core's shape over the rows `list`; the counts are the rows' own. */
export const diffOf = (list, kind = "replace") => ({
  kind,
  added: list.filter((r) => r.op === "add").length,
  removed: list.filter((r) => r.op === "del").length,
  before_bytes: 1,
  after_bytes: 1,
  rows: list,
});

/** A text of `count` characters. */
export const chars = (count) => "q".repeat(count);

/** The payload of a scripted card: the tool row and the fields `rest` (a list of `[name, value]`), each with its length. */
export const payloadOf = (tool, rest) =>
  [["tool", tool], ...rest].map(([name, value]) => ({ name, value, bytes: new TextEncoder().encode(value).length }));

/** The argument object of every `ai_undo` call the page made, oldest first. */
export const undoCalls = (page) => page.evaluate(() => structuredClone(window.__MOCK__.callsOf("ai_undo")));

/** The golden of the command card (written by the core's tests): the cards, the limits and the texts the page is held to. */
export const COMMAND_GOLDEN = JSON.parse(readFileSync(join(APP_DIR, "tests/fixtures/ai_command_card.json"), "utf8"));

/** The command the mock's command turns run unless the spec names another: it holds a real line break, which the card writes as `\n`. */
export const COMMAND_TEXT = "printf 'build ok\n'";

/**
 * Queue a turn in which the model asks for a command. `spec` is a call (`id`, `command`, `cwd`, `timeout_secs`, `target`, `remote`,
 * `tainted`, `taint`, `output` (the result of an allow, the golden's `ok` report by default), `failed` (the result is an error),
 * `overrides` (a payload value by name, to replace what the mock builds) and `payload`) or a list of calls in `calls`, and `answer`
 * is the text after the last result.
 */
export async function scriptCommand(page, spec) {
  const { calls: list, answer, ...single } = spec;
  const tools = (list ?? [single]).map((c) => ({ tool: "run_command", command: COMMAND_TEXT, output: COMMAND_GOLDEN.texts.report.ok, ...c }));
  await page.evaluate(([queued, text]) => window.__MOCK__.ai.script({ tools: queued, ...(text === undefined ? {} : { answer: text }) }), [tools, answer]);
}

/** From now on an allowed command keeps running until `releaseCommand`: the card says `Allowed · running…`. */
export const holdCommand = (page) => page.evaluate(() => window.__MOCK__.ai.exec.hold());

/** Let the held commands finish with their scripted result. */
export const releaseCommand = (page) => page.evaluate(() => window.__MOCK__.ai.exec.release());

/** Every command the mock ran, oldest first. */
export const commandRuns = (page) => page.evaluate(() => window.__MOCK__.ai.exec.runs);

/** Two stored chats of the sidebar tests: a saved list as the core would keep it, with the texts of their messages. */
export const SAVED_A = {
  id: "chat-saved-a",
  title: "Why is talos-3 NotReady?",
  agoMs: 2 * 3600 * 1000,
  profile: "local-1",
  model: "llama3.2",
  messages: [["user", "Why is talos-3 NotReady?"], ["assistant", "Check talosctl health."]],
};
export const SAVED_B = {
  id: "chat-saved-b",
  title: "Plan the backup job",
  agoMs: 3 * 86400 * 1000,
  profile: "local-1",
  model: "llama3.2",
  messages: [["user", "Plan the backup job"], ["assistant", "Start with a snapshot schedule."]],
};

/** Store `list` (members of `SAVED_A`'s shape) in the mock, as the core keeps chats that were sent. */
export async function saveChats(page, list) {
  await page.evaluate((chats) => {
    for (const { id, ...rest } of chats) window.__MOCK__.ai.save(id, rest);
  }, list);
}

/** Show the column when it is hidden, turn it into the AI chat with the page action and wait for the composer. */
export async function openSide(page) {
  if (await page.locator("#sidebar").isHidden()) await page.keyboard.press("Meta+b");
  await page.evaluate(() => window.__registry.runAction("ai.sidebar"));
  await page.waitForSelector('#sidebar[data-form="chat"] textarea.chat-input');
}

/** Every row of the chat list as the DOM shows it. */
export const sideRows = (page) =>
  page.evaluate(() =>
    [...document.querySelectorAll("#sidebar li.ai-side-row")].map((row) => ({
      id: row.dataset.id,
      current: row.dataset.current === "true",
      pane: row.dataset.pane === undefined ? null : Number(row.dataset.pane),
      mark: row.querySelector(".ai-side-mark")?.textContent ?? "",
      title: row.querySelector(".ai-side-name")?.textContent ?? "",
      meta: row.querySelector(".ai-side-meta")?.textContent ?? "",
      armed: row.querySelector("button.ai-side-del")?.dataset.armed === "true",
      canDelete: row.querySelector("button.ai-side-del") !== null,
    })),
  );

/** Open the list of chats from the title button, press the pick button of `id` and wait for the list to close. */
export async function pickChat(page, id) {
  await page.locator(".ai-side-title").click();
  await page.waitForSelector("#ai-side-chats:not([hidden]) li.ai-side-row");
  await page.locator(`li.ai-side-row[data-id="${id}"] button.ai-side-pick`).click();
  await page.waitForFunction(() => document.querySelector("#ai-side-chats").hidden);
}

/**
 * How the column and its strip of cards fit: the scroll sizes of the column and of the strip, the largest right edge of any
 * descendant of the strip against the strip's own, and the heights of the strip and of the chat host it sits in.
 */
export const overflow = (page) =>
  page.evaluate(() => {
    const side = document.getElementById("sidebar");
    const strip = side.querySelector("div.chat-cards");
    const edge = strip.getBoundingClientRect().right;
    let reach = 0;
    for (const node of strip.querySelectorAll("*")) reach = Math.max(reach, node.getBoundingClientRect().right);
    return {
      sideScroll: side.scrollWidth, sideClient: side.clientWidth, stripScroll: strip.scrollWidth, stripClient: strip.clientWidth,
      stripScrollHeight: strip.scrollHeight, stripClientHeight: strip.clientHeight, stripHeight: strip.getBoundingClientRect().height,
      hostHeight: side.querySelector(".side-chat").getBoundingClientRect().height, overflowY: getComputedStyle(strip).overflowY, reach, edge,
    };
  });

/** Type a message into the composer of the sidebar column and press Enter (a chat pane may have a composer of its own). */
export async function sendSide(page, text) {
  await page.locator("#sidebar .chat-input").fill(text);
  await page.locator("#sidebar .chat-input").press("Enter");
}

/** Run the action `id` through the page's registry, as a chord or the palette would. It does not wait for what the action starts. */
export const runAction = (page, id) =>
  page.evaluate((action) => {
    window.__registry.runAction(action);
  }, id);

/** The text of the composer under `scope` (a selector; the first composer of the page by default). */
export const draftOf = (page, scope = "") => page.locator(`${scope} textarea.chat-input`.trim()).first().inputValue();

/** Deliver a full `frame` event of pane `pane` with the members of `over` laid over a blank one (`selection`, `modes`). */
export async function emitFrame(page, pane, over = {}) {
  await page.evaluate(
    ([p, extra]) => {
      window.__FRAME_SEQ = (window.__FRAME_SEQ ?? 1000) + 1;
      window.__MOCK__.emit("frame", {
        pane: p, seq: window.__FRAME_SEQ, full: true, cols: 20, rows: 4, fg: 0xffffff, bg: 0x101010, cursor: null,
        scrollbar: [0, 4, 4], selection: [], matches: [], search: null, copy_cursor: null,
        modes: { alt: false, bracketed_paste: false, mouse: false }, changed: [], ...extra,
      });
    },
    [pane, over],
  );
  await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
}

/** The `kinds` of every preview call the page made, oldest first. */
export const previewKinds = async (page) => (await callsOf(page, "ai_context_preview")).map((args) => args.kinds);

/** The names of the commands the page called, oldest first. */
export const commandNames = (page) => page.evaluate(() => window.__MOCK__.calls.map((c) => c[0]));

/**
 * Let the page see its layout twice, so the pane it was last on is the pane the chats act for (the rule follows the focus
 * between two draws), and give the keyboard to the terminal input.
 */
export async function settle(page) {
  for (let i = 0; i < 2; i += 1) {
    await page.evaluate(() => window.__MOCK__.emit("layout", window.__MOCK__.view()));
    await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  }
  await page.evaluate(() => document.getElementById("ime").focus());
}

/** The recorded paste calls, oldest first. */
export const pastesOf = (page) => callsOf(page, "paste");

/**
 * Queue a turn whose text is `text`, script the core's Markdown renderer to answer `html`, send `task` with `send` (the composer
 * of a chat pane by default) and wait until the finished reply of the assistant has been drawn as Markdown.
 */
export async function scriptReply(page, { text, html, task = "go", send = sendText }) {
  const before = await page.locator('.chat-msg[data-role="assistant"] .chat-text.chat-md').count();
  await page.evaluate(
    ([chunk, rendered]) => {
      window.__MOCK__.ai.script({ chunks: [chunk] });
      window.__MOCK__.on("markdown_render", () => rendered);
    },
    [text, html],
  );
  await send(page, task);
  await page.waitForFunction((n) => document.querySelectorAll('.chat-msg[data-role="assistant"] .chat-text.chat-md').length > n, before);
}

/** Every bar and note under `scope` (a selector) as `{ kind, text, before }`: what it is, its text and the tag the node before it has. */
export const codeBars = (page, scope = ".chat-log") =>
  page.evaluate(
    (selector) =>
      [...document.querySelectorAll(`${selector} .chat-codebar, ${selector} .chat-codenote`)].map((node) => ({
        kind: node.classList.contains("chat-codebar") ? "bar" : "note",
        text: node.textContent,
        before: node.previousElementSibling?.localName ?? null,
      })),
    scope,
  );

// ---- requests of `wsh ai` -------------------------------------------------------------------------------------------------

/** A word of the prompt of the cloud card that must never occur in any call, attribute or store of the page but as text. */
export const ASK_WORD = "zq-ask-prompt-8d1f3a";

/** The card of a request that goes to a cloud provider, from pane 2: the golden's members with the values of the tests. */
export const ASK = {
  ask: "a1b2c3d4e5f60718293a4b5c6d7e8f90",
  seal: "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00",
  pane: 2,
  prompt: "why does this fail",
  prompt_findings: [],
  input: { lines: 142, bytes: 6254, sent_bytes: 5939, findings: [{ rule: "anthropic_key", count: 1 }] },
  profile: "anthropic",
  model: "example-model",
  host: "api.example.org",
  locality: "cloud",
};

/** The card of a request with no piped text for a model on this computer, from pane 1. */
export const LOCAL_ASK = {
  ask: "0f1e2d3c4b5a69788796a5b4c3d2e1f0",
  seal: "beefbeefbeefbeefbeefbeefbeefbeefbeefbeefbeefbeefbeefbeefbeefbeef",
  pane: 1,
  prompt: "explain the last error",
  prompt_findings: [],
  input: null,
  profile: "ollama",
  model: "llama3.2",
  host: "localhost",
  locality: "loopback",
};

/** A card with some members replaced. */
export const askView = (changes = {}, base = ASK) => ({ ...structuredClone(base), ...changes });

/** The core tells the window `label` (the main window by default) that a request waits. */
export const emitAsk = (page, view, label = "main") =>
  page.evaluate(([name, v]) => window.__MOCK__.emitTo(name, "ai-ask", { kind: "waiting", view: v }), [label, view]);

/** The core tells every window that a request is over. */
export const closeAsk = (page, ask, why = "gone") =>
  page.evaluate(([id, reason]) => window.__MOCK__.emit("ai-ask", { kind: "closed", ask: id, why: reason }), [ask, why]);

/** The card on screen, as a locator. */
export const askCard = (page) => page.locator("section.ask");

/** The arguments of every answer the page made, oldest first. */
export const askCalls = (page) => callsOf(page, "ai_ask_answer");
