// Shared set-up of the context tests (not a test itself: the runner only runs *.test.mjs). The page is shown with the sidebar
// open, and a mock core answers `integration_context` from `window.__CX__`, so a test can change what the core says while the
// page runs. The core resolves the rows for the pane it is asked about; the mock does it by `rowsByPane` (a pane id to rows),
// with `rows` for any other pane.
import { KUBE, LAYOUT, listView } from "./integrate-lib.mjs";

export { KUBE, LAYOUT, listView };

const verb = (id, label, effect, output, refresh_secs = null, args = []) => ({ id, label, effect, output, refresh_secs, exit_is_state: false, args });

const tool = (id, label, selects, extra = {}, verbs = []) => ({
  id, label, state: "found", path: `/usr/local/bin/${id}`, place: "path", binary: null, reason: null, version: "1.0", searched: [], selects, verbs, ...extra,
});

export const KUBE_FULL = {
  ...KUBE,
  verbs: [...KUBE.verbs, verb("shell", "Shell", "read", "terminal")],
};
export const TALOS = tool("talos", "Talos", ["context", "node"], {}, [verb("contexts", "Contexts", "read", "view"), verb("nodes", "Nodes", "read", "view")]);
export const AZURE = tool("azure", "Azure", ["subscription"], {}, [verb("subscriptions", "Subscriptions", "read", "view")]);
export const GAM = tool("gam", "GAM", ["section"], {}, [verb("sections", "Sections", "read", "view")]);
export const ALL_TOOLS = [KUBE_FULL, TALOS, AZURE, GAM];

export const row = (toolId, over = {}) => ({
  tool: toolId, state: "found", context: null, namespace: null, node: null, production: null, section: null, stale: false, subscription: null, ...over,
});

export const KUBE_ROW = row("kube", { context: "prod-eu", namespace: "payments" });
export const ROWS = [KUBE_ROW, row("talos", { context: "talos-prod", node: "node-1" }), row("azure", { subscription: "holz-prod" }), row("gam", { section: "work" })];

export const paneInfo = (pane, over = {}) => ({
  pane, tool: "kube", view: "shell", context: "prod-eu", namespace: "payments", production: null, held: false, status: null, ...over,
});

/** A layout with panes 1 and 2 and the focus where `focus` says. */
export const layout = (focus = 1, ids = [1, 2]) => ({
  workspaces: [{ name: "main", active: true, panes: ids.length }],
  tabs: [{ title: "shell", active: true, focus, panes: ids.length }],
  focus,
  panes: ids.map((id, i) => [id, { x: i / ids.length, y: 0, w: 1 / ids.length, h: 1 }]),
  dividers: ids.slice(1).map((_, i) => ({ split: 10 + i, axis: "Row", area: { x: 0, y: 0, w: 1, h: 1 }, ratio: (i + 1) / ids.length })),
  pane_names: [],
  windows: 1,
});

/**
 * Open the page. `config` is merged into `window.__CX__`:
 *   tools        the answer of integration_tools (default: all four, found)
 *   rows         the rows the core answers (default: ROWS)
 *   rowsByPane   rows by pane id, used when the call names that pane
 *   panes        the panes the core lists (default none)
 *   blocks       the answer of pane_blocks (default none)
 *   runs         the answer of integration_run by verb id
 *   contextFault a fault thrown by integration_context instead of an answer
 *   selectFault  a fault thrown by integration_select
 *   launch / launchFault   the answer or the fault of integration_launch
 *   toolsAfterSet  the answer of integration_binary_set
 *   os           what app_info names (default macos)
 *   sidebar      whether the sidebar starts shown (default true)
 */
export async function openContext(page, url, config = {}, view = LAYOUT) {
  const base = {
    tools: ALL_TOOLS,
    rows: ROWS,
    rowsByPane: {},
    panes: [],
    blocks: [],
    runs: {},
    contextFault: null,
    selectFault: null,
    launch: null,
    launchFault: null,
    toolsAfterSet: null,
    os: "macos",
    sidebar: true,
    delay: 0,
  };
  await page.addInitScript(([view, cfg]) => {
    const M = window.__MOCK__;
    window.__CX__ = cfg;
    M.setView(view);
    M.on("app_info", () => ({ version: "0.0.1-alpha1", os: window.__CX__.os, hostname: "woodbox" }));
    M.on("pane_blocks", () => window.__CX__.blocks);
    M.on("integration_tools", () => window.__CX__.tools);
    M.on("integration_context", async (a) => {
      const cx = window.__CX__;
      if (cx.delay > 0) await new Promise((r) => setTimeout(r, cx.delay));
      if (cx.contextFault) throw cx.contextFault;
      const rows = (a.pane !== null && cx.rowsByPane[a.pane]) || cx.rows;
      return { rows: structuredClone(rows), panes: structuredClone(cx.panes) };
    });
    M.on("integration_run", (a) => {
      const cx = window.__CX__;
      if (cx.faults?.[a.verb]) throw cx.faults[a.verb];
      return cx.runs[a.verb] ?? null;
    });
    M.on("integration_select", (a) => {
      const cx = window.__CX__;
      if (cx.selectFault) throw cx.selectFault;
      return { selection: { profile: null, context: null, namespace: null, subscription: null, section: null, node: null, ...a.selection }, production: null };
    });
    M.on("integration_launch", () => {
      const cx = window.__CX__;
      if (cx.launchFault) throw cx.launchFault;
      return cx.launch;
    });
    M.on("integration_binary_set", () => {
      const cx = window.__CX__;
      if (cx.setFault) throw cx.setFault;
      return cx.toolsAfterSet ?? cx.tools;
    });
    M.on("block_open", () => M.view());
    M.on("block_update", () => M.view());
    try {
      localStorage.setItem("holzwave.sidebar", cfg.sidebar ? "1" : "0");
    } catch {
      // a page without storage keeps the sidebar hidden
    }
  }, [view, { ...base, ...config }]);
  await page.goto(`${url}index.html?renderer=canvas`);
}

/** Wait until the section has its rows. */
export async function sectionShown(page) {
  await page.waitForSelector(".cx-row");
}

/** The number of calls of a command so far. */
export const callCount = (page, cmd) => page.evaluate((c) => window.__MOCK__.callsOf(c).length, cmd);

/** The calls of a command so far. */
export const callsOf = (page, cmd) => page.evaluate((c) => window.__MOCK__.callsOf(c), cmd);
