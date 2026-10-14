// The header strip above every visible pane: a state dot, the title the program set (the tab's title before one
// did) and the context, the working directory with the home prefix as `~`, right-aligned. The focused pane's strip
// has the 1 px rose top line, and only while more than one pane is visible. State is never colour alone: the dot
// is filled for the focused pane, hollow for the others and hollow with a dash for a pane that exited, which also
// says "exited" in the context slot. A × at the right end closes the pane after a confirmation (chrome.ts).
//
// The strips live in the overlay (pointer events none, so the terminal below still gets every click) and are
// keyed by pane id like the dividers: the same element is moved on every render. Everything is `textContent`.
import { el, fitNodes, setAttr, setRect, setText } from "./dom.js";
import { paneRect } from "./geometry.js";
import { shortenHome } from "./labels.js";
import { startRename } from "./rename.js";
import { state } from "./state.js";

interface Head {
  root: HTMLElement;
  dot: HTMLElement;
  title: HTMLElement;
  /** The program's title, shown after a name the user gave. */
  sub: HTMLElement;
  /** The text of the context lives in a child, so the cut can be made from the left (see chrome.css). */
  ctx: HTMLElement;
}

const heads = new Map<number, Head>();
let layer: HTMLElement | null = null;

/** Hand over the overlay layer the strips go into. Call once, before the first render. */
export function initPaneHeads(container: HTMLElement): void {
  layer = container;
}

function create(pane: number, parent: HTMLElement): Head {
  const root = el("div", "panehead", parent);
  root.setAttribute("data-pane", String(pane));
  const dot = el("span", "ph-dot", root);
  dot.setAttribute("aria-hidden", "true");
  const title = el("span", "label ph-title", root);
  const sub = el("span", "label ph-sub", root);
  const ctx = el("span", "ph-ctx", root);
  el("span", "", ctx);
  title.addEventListener("dblclick", () => renamePane(pane));
  // The × is the only part of the strip, besides the title, that takes the pointer. It is a button of its own, so a
  // press on it is neither a drag of the pane (dragdrop.ts leaves `.close` alone) nor a click on the terminal below.
  const close = el("button", "close ph-close", root);
  close.type = "button";
  close.title = "Close pane";
  close.setAttribute("aria-label", "Close pane");
  close.addEventListener("click", (e) => {
    e.stopPropagation();
    closeHandler(pane);
  });
  return { root, dot, title, sub, ctx };
}

/** What a confirmed name is sent to; the chrome sets it, so this module does not import it. */
let renameHandler: (pane: number, name: string) => void = () => {};

/** Hand over the function that sends a pane's new name to the core. */
export function setPaneRenameHandler(handler: (pane: number, name: string) => void): void {
  renameHandler = handler;
}

/** What a × is sent to; the chrome sets it (it asks first), so this module does not import it. */
let closeHandler: (pane: number) => void = () => {};

/** Hand over the function that asks and closes a pane. */
export function setPaneCloseHandler(handler: (pane: number) => void): void {
  closeHandler = handler;
}

/** The name the user gave to `pane`, or the empty string. */
function nameOf(pane: number): string {
  return state.view?.pane_names?.find(([id]) => id === pane)?.[1] ?? ""; // a view without the field has no names
}

/** Open the name editor over the pane's header title. */
export function renamePane(pane: number): void {
  const head = heads.get(pane);
  if (!head) return;
  startRename(head.title, nameOf(pane), automaticTitle(pane), (name) => renameHandler(pane, name));
}

/** What the pane's header shows as its title: the name the user gave, else the program's title. */
export function paneLabel(pane: number): string {
  return nameOf(pane) || automaticTitle(pane);
}

/** The title a pane has without a name: what its program set, else the title of its tab. */
function automaticTitle(pane: number): string {
  const tab = state.view?.tabs.find((t) => t.active)?.title ?? "";
  return titleOf(pane, tab);
}

/** The title of a pane's strip: what its program set, else the title of the tab the pane is in. */
function titleOf(pane: number, tabTitle: string): string {
  return state.titles.get(pane) || tabTitle;
}

/**
 * Bring the strips in line with `state.view`: one per visible pane, over the pane's top edge and as high as
 * `state.headerPx`. A strip whose pane left the layout is removed.
 */
export function renderPaneHeads(): void {
  const view = state.view;
  if (!layer) return;
  const live = new Set<number>();
  if (view) {
    const multiple = view.panes.length > 1;
    const tabTitle = view.tabs.find((t) => t.active)?.title ?? "";
    for (const [id, unit] of view.panes) {
      live.add(id);
      let head = heads.get(id);
      if (!head) {
        head = create(id, layer);
        heads.set(id, head);
      }
      const exited = state.exited.has(id);
      const focused = id === view.focus;
      const r = paneRect(state.metrics, unit);
      setRect(head.root, { x: r.x, y: r.y, w: r.w, h: state.headerPx });
      head.root.classList.toggle("focused", focused);
      head.root.classList.toggle("focus-line", focused && multiple && !exited);
      head.root.classList.toggle("exited", exited);
      setAttr(head.dot, "data-state", exited ? "exited" : focused ? "focused" : "idle");
      const name = nameOf(id);
      const program = titleOf(id, tabTitle);
      // A name the user gave comes first; the program's title stays visible after it, lighter.
      setText(head.title, name === "" ? program : name);
      setText(head.sub, name !== "" && program !== name ? program : "");
      const ctx = exited ? "exited" : shortenHome(state.cwds.get(id) ?? "", state.os);
      setText(head.ctx.firstElementChild as HTMLElement, ctx);
      setAttr(head.ctx, "title", ctx === "" ? null : ctx);
    }
  }
  for (const [id, head] of heads) {
    if (live.has(id)) continue;
    head.root.remove();
    heads.delete(id);
  }
}
