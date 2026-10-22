// The status line below the stage, tmux style: the pane count, the renderer with its draw time, the encoding and the
// version, with the connections segment before them. The workspaces are not repeated here: they are the pills of the
// title bar. The mode segment names copy mode (`COPY`, or `SELECT` while a selection is being extended) and is empty,
// so not shown, otherwise: the word is the cue, not a colour. A notice (a refused tear-off) takes the segment after
// it for a few seconds and goes by itself. It is rendered from `state`; every text is put in with `textContent`.
import { getCopyMode } from "./copymode.js";
import { connSummary, paneConns } from "./connstate.js";
import { el, setText } from "./dom.js";
import { followLayout } from "./integratecontext.js";
import { NOTICE_MS, paneCountText, rendererText } from "./labels.js";
import { state } from "./state.js";

const strip = el("div", "", document.getElementById("app") ?? document.body);
strip.id = "status";
strip.setAttribute("aria-label", "Status");

const seg = (cls: string): HTMLElement => el("span", `seg label ${cls}`);
const modeSeg = seg("mode");
const noticeSeg = seg("notice");
noticeSeg.setAttribute("role", "status");
const conns = seg("conns");
const panes = seg("panes");
const renderer = seg("renderer");
const encoding = seg("encoding");
const version = seg("version");
strip.append(modeSeg, noticeSeg, conns, panes, renderer, encoding, version);

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
  const copy = getCopyMode();
  setText(modeSeg, copy === "select" ? "SELECT" : copy === "copy" ? "COPY" : "");
  setText(noticeSeg, notice);
  setText(conns, connSummary(paneConns.values()));
  setText(panes, paneCountText(view?.panes.length ?? 0));
  setText(renderer, rendererText(state.renderer, state.stats.avgMs));
  setText(encoding, "UTF-8");
  setText(version, state.version);
  followLayout(); // asks the core only when the focus or the panes changed
}
