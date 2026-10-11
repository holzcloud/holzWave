// The status line below the stage, tmux style: one segment per workspace (the shown one solid), then the pane
// count, the renderer with its draw time, the encoding and the version. It is rendered from `state`; every text is
// put in with `textContent`.
import { el, fitNodes, setAttr, setText } from "./dom.js";
import { paneCountText, rendererText, tabLabel } from "./labels.js";
import { state } from "./state.js";

const strip = el("div", "", document.getElementById("app") ?? document.body);
strip.id = "status";
strip.setAttribute("aria-label", "Status");

const segments: HTMLElement[] = [];
const seg = (cls: string): HTMLElement => el("span", `seg label ${cls}`);
const panes = seg("panes");
const renderer = seg("renderer");
const encoding = seg("encoding");
const version = seg("version");
strip.append(panes, renderer, encoding, version);

/** Bring the strip in line with the view, the renderer, the draw statistics and `app_info`. */
export function renderStatus(): void {
  const view = state.view;
  const workspaces = view?.workspaces ?? [];
  fitNodes(strip, segments, workspaces.length, () => seg("ws"), panes);
  workspaces.forEach((w, i) => {
    const node = segments[i] as HTMLElement;
    setText(node, tabLabel(i, w.name));
    node.classList.toggle("active", w.active);
    setAttr(node, "aria-current", w.active ? "true" : null);
  });
  setText(panes, paneCountText(view?.panes.length ?? 0));
  setText(renderer, rendererText(state.renderer, state.stats.avgMs));
  setText(encoding, "UTF-8");
  setText(version, state.version);
}
