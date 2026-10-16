// Shared helpers of the AI chat browser tests (ai*.test.mjs): a booted page with panes, saved profiles and a block layer
// that opens chat panes, the profiles of the tests, and the two things a person does most, opening a chat and sending.
import { callsOf } from "./lib.mjs";

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

/**
 * Open the page with the saved profiles `profiles`, the panes of `view` and the block layer of the core played by the
 * mock: `block_open` splits the focused pane to the right, focuses the new pane and lists its block in `pane_blocks`.
 * `blocks` are the blocks that exist at the start.
 */
export async function boot(page, url, { profiles = [PROFILE], view = VIEW, blocks = [], search = "" } = {}) {
  await page.addInitScript(
    ([saved, shown, existing]) => {
      window.__MOCK__.reset();
      window.__MOCK__.ai.profiles = saved;
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
    [profiles, view, blocks],
  );
  await page.goto(`${url}index.html?renderer=canvas${search}`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await page.evaluate(async () => {
    window.__registry = await import(new URL("actions.js", location.href).href);
  });
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

/** The args of every call the page made to `cmd`, oldest first. */
export const calls = callsOf;
