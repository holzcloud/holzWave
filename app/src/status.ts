// The status line below the stage, tmux style: the pane count, the renderer with its draw time, the encoding and the
// version, with the connections segment before them. The workspaces are not repeated here: they are the pills of the
// title bar. The mode segment names copy mode (`COPY`, or `SELECT` while a selection is being extended) and is empty,
// so not shown, otherwise: the word is the cue, not a colour. A notice (a refused tear-off) takes the segment after
// it for a few seconds and goes by itself. It is rendered from `state`; every text is put in with `textContent`.
import { getCopyMode } from "./copymode.js";
import { connSummary, paneConns } from "./connstate.js";
import { el, setAttr, setText } from "./dom.js";
import { contextSegments, followLayout, showContext } from "./integratecontext.js";
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

/** The segments of the Kubernetes context and the Azure subscription, made when the core has a value and removed when it has not. */
const ctxSegs = new Map<string, HTMLButtonElement>();

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
  followLayout(); // asks the core only when the focus or the panes changed; the segments below are then already marked old
  const view = state.view;
  const copy = getCopyMode();
  setText(modeSeg, copy === "select" ? "SELECT" : copy === "copy" ? "COPY" : "");
  setText(noticeSeg, notice);
  setText(conns, connSummary(paneConns.values()));
  setText(panes, paneCountText(view?.panes.length ?? 0));
  setText(renderer, rendererText(state.renderer, state.stats.avgMs));
  setText(encoding, "UTF-8");
  setText(version, state.version);
  renderContext();
}

/** The segments of the context, after the pane count: a button with the text, and the words `PRODUCTION` and `stale` as parts of its own. */
function renderContext(): void {
  const want = contextSegments();
  const live = new Set(want.map((s) => s.tool));
  for (const [tool, node] of ctxSegs) {
    if (live.has(tool as "kube" | "azure")) continue;
    node.remove();
    ctxSegs.delete(tool);
  }
  let after: Element = panes;
  for (const seg of want) {
    let node = ctxSegs.get(seg.tool);
    if (!node) {
      node = el("button", "seg label ctx-seg");
      node.type = "button";
      node.dataset.tool = seg.tool;
      el("span", "ctx-text", node);
      node.addEventListener("click", showContext);
      ctxSegs.set(seg.tool, node);
    }
    if (node.previousElementSibling !== after) after.after(node);
    after = node;
    node.setAttribute("aria-label", seg.label);
    setAttr(node, "data-stale", seg.stale ? "true" : null);
    setText(node.firstElementChild as HTMLElement, seg.text);
    word(node, "ctx-prod", seg.production ? "PRODUCTION" : "");
    word(node, "ctx-stale", seg.stale ? "stale" : "");
  }
}

/** A word after the text of a segment, made when it is said and removed when it is not. */
function word(node: HTMLElement, cls: string, text: string): void {
  let span = node.querySelector<HTMLElement>(`.${cls}`);
  if (text === "") {
    span?.remove();
    return;
  }
  if (!span) span = el("span", cls, node);
  setText(span, text);
}
