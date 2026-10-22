// Shared set-up of the integration block tests (not a test itself: the runner only runs *.test.mjs). A page with a shell
// on the left and an integration pane (pane 2) on the right, and a mock core that answers from `window.__IG__`, so a test
// can change what the core says while the page runs.

export const HOSTILE = "<img src=x onerror=alert(1)>";

export const LAYOUT = {
  workspaces: [{ name: "main", active: true, panes: 2 }],
  tabs: [{ title: "pods", active: true, focus: 1, panes: 2 }],
  focus: 1,
  panes: [[1, { x: 0, y: 0, w: 0.5, h: 1 }], [2, { x: 0.5, y: 0, w: 0.5, h: 1 }]],
  dividers: [{ split: 10, axis: "Row", area: { x: 0, y: 0, w: 1, h: 1 }, ratio: 0.5 }],
  pane_names: [],
  windows: 1,
};

const verb = (id, label, effect, output, refresh_secs = null, args = []) => ({ id, label, effect, output, refresh_secs, exit_is_state: false, args });

export const KUBE = {
  id: "kube", label: "Kubernetes", state: "found", path: "/opt/homebrew/bin/kubectl", place: "path", binary: null, reason: null,
  version: "v1.31.0", searched: [], selects: ["context", "namespace"],
  verbs: [
    verb("contexts", "Contexts", "read", "view"),
    verb("namespaces", "Namespaces", "read", "view"),
    verb("pods", "Pods", "read", "view", 10),
    verb("use_context", "Use context", "local", "done", null, [{ kind: "context", name: "context", required: true }]),
    verb("delete_pod", "Delete pod", "write", "done", null, [{ kind: "pod", name: "pod", required: true }]),
    verb("exec", "Exec", "exec", "terminal", null, [{ kind: "pod", name: "pod", required: true }]),
    verb("logs", "Logs", "read", "terminal", null, [{ kind: "pod", name: "pod", required: true }]),
  ],
};

export const logs = (pod) => ({ verb: "logs", label: "Logs", effect: "read", args: { pod } });

export const podsView = (over = {}) => ({
  kind: "table",
  title: "Pods",
  columns: ["NAME", "READY", "STATUS", "RESTARTS", "AGE"].map((l) => ({ id: l.toLowerCase(), label: l })),
  rows: [
    { key: "api-0", cells: ["api-0", "1/1", "Running", "0", "3d"], tone: "normal", actions: [logs("api-0")] },
    { key: HOSTILE, cells: [HOSTILE, "0/1", "Pending", "0", "4s"], tone: "warn", actions: [] },
  ],
  truncated: null,
  note: null,
  fetched: Math.floor(Date.now() / 1000),
  ...over,
});

export const listView = (title, names, over = {}) => ({
  kind: "table",
  title,
  columns: [{ id: "name", label: "NAME" }],
  rows: names.map((n) => ({ key: n, cells: [n], tone: "normal", actions: [] })),
  truncated: null,
  note: null,
  fetched: null,
  ...over,
});

export const answer = (view, selection = { context: "prod", namespace: "payments" }, production = "Production") => ({
  view,
  selection: { selection: { profile: null, subscription: null, section: null, node: null, ...selection }, production },
  fetched: view.fetched,
});

export const BLOCK = { pane: 2, kind: "integration", source: "kube", path: "pods", source_label: "Kubernetes" };

/**
 * Open the page with the integration pane. `config` is merged into `window.__IG__`:
 *   tools     the answer of integration_tools
 *   blocks    the answer of pane_blocks
 *   runs      the answer of integration_run by verb id
 *   faults    a fault by verb id, thrown instead of an answer
 *   delay     milliseconds every run waits
 *   selectProduction  what the core says about the production flag after a selection (default: unchanged)
 *   launch    the answer of integration_launch; launchFault is thrown instead
 * The mock also counts the runs on the way (`inflight`, `maxInflight`) and the runs started (`started`).
 */
export async function openBlock(page, url, config = {}, layout = LAYOUT) {
  const base = {
    tools: [KUBE],
    blocks: [BLOCK],
    runs: { pods: answer(podsView()), contexts: answer(listView("Contexts", ["prod", "dev"])), namespaces: answer(listView("Namespaces", ["payments", "default"])) },
    faults: {},
    delay: 0,
    inflight: 0,
    maxInflight: 0,
    started: 0,
    selectFault: null,
    launchFault: null,
    launch: null,
  };
  await page.addInitScript(([layout, cfg]) => {
    const M = window.__MOCK__;
    window.__IG__ = cfg;
    M.setView(layout);
    M.on("pane_blocks", () => window.__IG__.blocks);
    M.on("integration_tools", () => window.__IG__.tools);
    M.on("integration_run", async (a) => {
      const ig = window.__IG__;
      ig.started += 1;
      ig.inflight += 1;
      ig.maxInflight = Math.max(ig.maxInflight, ig.inflight);
      try {
        if (ig.delay > 0) await new Promise((r) => setTimeout(r, ig.delay));
        if (ig.faults[a.verb]) throw ig.faults[a.verb];
        return ig.runs[a.verb] ?? null;
      } finally {
        ig.inflight -= 1;
      }
    });
    M.on("integration_select", (a) => {
      const ig = window.__IG__;
      if (ig.selectFault) throw ig.selectFault;
      const selection = { profile: null, context: null, namespace: null, subscription: null, section: null, node: null, ...a.selection };
      // The core now answers with this pane's new selection, and whether it is a production one.
      for (const r of Object.values(ig.runs)) {
        if (!r) continue;
        r.selection.selection = selection;
        if (ig.selectProduction !== undefined) r.selection.production = ig.selectProduction;
      }
      return { selection, production: ig.selectProduction ?? null };
    });
    M.on("integration_launch", () => {
      if (window.__IG__.launchFault) throw window.__IG__.launchFault;
      return window.__IG__.launch;
    });
    M.on("block_open", () => M.view());
    M.on("block_update", () => M.view());
  }, [layout, { ...base, ...config }]);
  await page.goto(`${url}index.html?renderer=canvas`);
}

/** Wait until the block shows rows. */
export async function rowsShown(page) {
  await page.waitForSelector(".block-integration .ig-table tbody tr");
}

/** The layout with the integration pane focused (the keys of a block go to the focused pane). */
export const FOCUSED = { ...LAYOUT, focus: 2, tabs: [{ ...LAYOUT.tabs[0], focus: 2 }] };

/** The layout after a launch: a third pane, the terminal that shows the logs. */
export const LAUNCHED = {
  ...LAYOUT,
  panes: [[1, { x: 0, y: 0, w: 0.5, h: 1 }], [2, { x: 0.5, y: 0, w: 0.5, h: 0.5 }], [3, { x: 0.5, y: 0.5, w: 0.5, h: 0.5 }]],
  focus: 3,
  tabs: [{ title: "pods", active: true, focus: 3, panes: 3 }],
  workspaces: [{ name: "main", active: true, panes: 3 }],
  dividers: [
    { split: 10, axis: "Row", area: { x: 0, y: 0, w: 1, h: 1 }, ratio: 0.5 },
    { split: 11, axis: "Column", area: { x: 0.5, y: 0, w: 0.5, h: 1 }, ratio: 0.5 },
  ],
};
