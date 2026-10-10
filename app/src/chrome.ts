// The bar above the canvas: one button per workspace, the plus button, a separator, one button per tab and the
// plus button. Names come from the core and are put in with `textContent` only, never parsed as markup.
import { call } from "./bridge.js";
import { el } from "./dom.js";
import type { View } from "./types.js";

const bar = document.getElementById("bar") as HTMLDivElement;

function button(label: string, active: boolean, onClick: () => void, cls = ""): HTMLButtonElement {
  const b = el("button", `${active ? "active" : ""} ${cls}`.trim());
  b.textContent = label;
  b.addEventListener("click", () => {
    onClick();
    document.getElementById("term")?.focus();
  });
  return b;
}

/** Rebuild the bar for `view`; every command a button sends answers with a view that goes to `apply`. */
export function renderBar(view: View, apply: (v: View) => void): void {
  const show = (p: Promise<View>) => void p.then(apply);
  bar.replaceChildren();
  view.workspaces.forEach((w, i) => bar.append(button(w.name, w.active, () => show(call("select_workspace", { index: i })))));
  bar.append(button("+", false, () => show(call("new_workspace", { name: `ws ${view.workspaces.length + 1}` })), "plus"));
  el("span", "sep", bar);
  view.tabs.forEach((t, i) => bar.append(button(t.title, t.active, () => show(call("select_tab", { index: i })))));
  bar.append(button("+", false, () => show(call("new_tab")), "plus"));
}
