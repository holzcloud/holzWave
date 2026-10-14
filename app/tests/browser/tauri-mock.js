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
//     reset()          clear calls and handlers and go back to the default layout (listeners are kept)
//
// Defaults: one workspace, one tab, panes 1 and 2 side by side and one Row divider at ratio 0.5. The layout
// commands (the close commands too) return the current view, `set_ratio` clamps to 0.1..0.9, `release_outside` answers `{outcome: "cancelled"}`,
// every other command returns null. The commands between windows (`tear_off_pane`, `tear_off_tab`, `attach_pane`,
// `attach_tab`) answer the current view like the other layout commands; `window_ready` answers null.
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
    },
  };

  window.__MOCK__ = mock;
  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args) => {
        calls.push([cmd, args]);
        const handler = handlers.get(cmd);
        if (handler) return (await handler(args)) ?? null;
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
