// The header strip above every visible pane: a state dot, the title the program set (the tab's title before one
// did) and the context, the working directory with the home prefix as `~`, right-aligned. The focused pane's strip
// has the 1 px rose top line, and only while more than one pane is visible. State is never colour alone: the dot
// is filled for the focused pane, hollow for the others and hollow with a dash for a pane that exited, which also
// says "exited" in the context slot.
//
// The strips live in the overlay (pointer events none, so the terminal below still gets every click) and are
// keyed by pane id like the dividers: the same element is moved on every render. Everything is `textContent`.
import { el, fitNodes, setAttr, setRect, setText } from "./dom.js";
import { paneRect } from "./geometry.js";
import { shortenHome } from "./labels.js";
import { state } from "./state.js";

interface Head {
  root: HTMLElement;
  dot: HTMLElement;
  title: HTMLElement;
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
  const ctx = el("span", "ph-ctx", root);
  el("span", "", ctx);
  return { root, dot, title, ctx };
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
      setText(head.title, titleOf(id, tabTitle));
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
