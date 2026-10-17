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
 * `localContext`, `localOnly`, `ttl`, `persist`) before the page loads.
 */
export async function boot(
  page, url,
  { profiles = [PROFILE], view = VIEW, blocks = [], states = {}, search = "", starts = 2, settings = false, context = {} } = {},
) {
  await page.addInitScript(
    ([saved, shown, existing, chats, shared]) => {
      window.__MOCK__.reset();
      Object.assign(window.__MOCK__.ai.context, shared);
      window.__MOCK__.ai.profiles = saved;
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
    [profiles, view, blocks, states, context],
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

/** Press `Context…` and wait for the card. */
export async function openContext(page) {
  await page.locator("button.chat-ctx").click();
  await page.waitForSelector(".chat-consent:not([aria-busy])");
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
