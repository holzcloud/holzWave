// The status line below the stage, tmux style: one segment per workspace (the shown one solid), then the pane
// count, the renderer with its draw time, the encoding and the version. The mode segment names copy mode (`COPY`,
// or `SELECT` while a selection is being extended) and is empty, so not shown, otherwise: the word is the cue, not
// a colour. A notice (a refused tear-off) takes the segment after it for a few seconds and goes by itself. It is
// rendered from `state`; every text is put in with `textContent`.
import { getCopyMode } from "./copymode.js";
import { el, fitNodes, setAttr, setText } from "./dom.js";
import { NOTICE_MS, paneCountText, rendererText, tabLabel } from "./labels.js";
import { state } from "./state.js";

const strip = el("div", "", document.getElementById("app") ?? document.body);
strip.id = "status";
strip.setAttribute("aria-label", "Status");

const segments: HTMLElement[] = [];
const seg = (cls: string): HTMLElement => el("span", `seg label ${cls}`);
const modeSeg = seg("mode");
const noticeSeg = seg("notice");
noticeSeg.setAttribute("role", "status");
const panes = seg("panes");
const renderer = seg("renderer");
const encoding = seg("encoding");
const version = seg("version");
strip.append(modeSeg, noticeSeg, panes, renderer, encoding, version);

let notice = "";
let noticeTimer: ReturnType<typeof setTimeout> | undefined;

/** Say `text` in the status line for [`NOTICE_MS`]; a newer notice replaces it and restarts the time. */
export function showNotice(text: string): void {
  clearTimeout(noticeTimer);
  notice = text;
  noticeTimer = setTimeout(() => {
    notice = "";
    renderStatus();
  }, NOTICE_MS);
  renderStatus();
}

/** Bring the strip in line with the view, the renderer, the draw statistics and `app_info`. */
export function renderStatus(): void {
  const view = state.view;
  const workspaces = view?.workspaces ?? [];
  fitNodes(strip, segments, workspaces.length, () => seg("ws"), modeSeg);
  workspaces.forEach((w, i) => {
    const node = segments[i] as HTMLElement;
    setText(node, tabLabel(i, w.name));
    node.classList.toggle("active", w.active);
    setAttr(node, "aria-current", w.active ? "true" : null);
  });
  const copy = getCopyMode();
  setText(modeSeg, copy === "select" ? "SELECT" : copy === "copy" ? "COPY" : "");
  setText(noticeSeg, notice);
  setText(panes, paneCountText(view?.panes.length ?? 0));
  setText(renderer, rendererText(state.renderer, state.stats.avgMs));
  setText(encoding, "UTF-8");
  setText(version, state.version);
}
