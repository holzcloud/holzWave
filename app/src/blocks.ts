// Block panes: a pane that shows a file browser or the preview of a file instead of a terminal (docs/design.md
// sections 6.3 and 6.5). The core owns what each block shows (`pane_blocks`) and reads the files; this module draws it
// over the empty terminal of the pane and sends what the person does back. Every name from a file system goes onto the
// screen with `textContent`; the one exception is the HTML of a Markdown document, which the core has already passed
// through an allow-list sanitiser (core/src/preview.rs). Images and video are shown from `blob:` URLs, never from a
// path. While a block pane has the focus it takes the navigation keys (the key handler stack); everything with a
// modifier stays with the shortcuts.
import { registerAction } from "./actions.js";
import { call } from "./bridge.js";
import { el, loadStyles, setText } from "./dom.js";
import { showPdf } from "./pdfview.js";
import type { PdfView } from "./pdfview.js";
import { appendSafe } from "./safehtml.js";
import { paneRect } from "./geometry.js";
import { focusInput } from "./keyboard.js";
import { pushKeyHandler } from "./keystack.js";
import { onOverlayRender, renderOverlay } from "./overlay.js";
import { state } from "./state.js";
import {
  entryIcon, humanSize, joinPath, paneBlocks, readBlocks, readListing, setPaneBlocks, shortDate, visibleEntries,
} from "./blockstate.js";
import type { BlockInfo, FileEntry, Listing, Opened } from "./blockstate.js";
import type { View } from "./types.js";

/** The most lines of a text file the viewer draws; the rest is said to be cut. */
const MAX_LINES = 5000;

let layer: HTMLElement | null = null;
let applyView: (v: View) => void = () => {};
let onBlocks: () => void = () => {};

/** One drawn block. */
interface Instance {
  pane: number;
  root: HTMLElement;
  info: BlockInfo;
  /** The block's path changed in the core (a preview was pointed at another file). */
  update(info: BlockInfo): void;
  /** A key while this pane has the focus; true when it was used. */
  key(e: KeyboardEvent): boolean;
  dispose(): void;
}

const instances = new Map<number, Instance>();
/** The panes the core was asked about, so a new pane is asked about once. */
const asked = new Set<number>();
/** Which preview pane a browser pane opens files in, so a second file reuses it. */
const previewOf = new Map<number, number>();
let refreshing = false;

// ---- asking the core ---------------------------------------------------------------------------------------------

/** Ask the core which panes are blocks and draw them. */
export async function refreshBlocks(): Promise<void> {
  if (refreshing) return;
  refreshing = true;
  try {
    let raw: unknown = null;
    try {
      raw = await call<unknown>("pane_blocks");
    } catch {
      // no answer is no blocks
    }
    if (setPaneBlocks(readBlocks(raw))) onBlocks();
    renderOverlay();
  } finally {
    refreshing = false;
  }
}

/** Open a file browser (`files`) or a preview (`preview`) in a new tab or beside the focused pane. */
export async function openBlock(
  kind: "files" | "preview",
  source: string,
  path: string,
  placement: "tab" | "right" | "down",
): Promise<string | null> {
  try {
    const view = await call<View>("block_open", { kind, source, path, placement });
    applyView(view);
    await refreshBlocks();
    return null;
  } catch (e) {
    return String(e);
  }
}

/** What `file_grant` answers: where the file is served from, and what it is. */
interface Granted {
  url: string;
  token: string;
  mime: string;
  size: number;
}

// ---- the file browser --------------------------------------------------------------------------------------------

function filesBlock(info: BlockInfo, root: HTMLElement): Instance {
  root.classList.add("block-files");
  const bar = el("div", "bk-bar", root);
  const up = el("button", "bk-up", bar);
  up.type = "button";
  up.textContent = "↑ Up";
  up.title = "Parent folder (Backspace)";
  const path = el("span", "bk-path", bar);
  const hiddenToggle = el("button", "bk-toggle", bar);
  hiddenToggle.type = "button";
  hiddenToggle.title = "Show or hide hidden files (.)";
  const listEl = el("div", "bk-list", root);
  listEl.setAttribute("role", "listbox");
  listEl.setAttribute("aria-label", "Files");
  const status = el("div", "bk-status", root);
  status.setAttribute("role", "status");

  let current = info;
  let listing: Listing | null = null;
  let shown: FileEntry[] = [];
  let selected = 0;
  let showHidden = false;
  let loading = 0;
  let failure = "";

  const draw = (): void => {
    setText(path, listing?.path ?? current.path);
    setText(hiddenToggle, showHidden ? "hidden: shown" : "hidden: off");
    hiddenToggle.setAttribute("aria-pressed", String(showHidden));
    up.disabled = !listing?.parent;
    listEl.replaceChildren();
    shown = listing ? visibleEntries(listing.entries, showHidden) : [];
    selected = Math.max(0, Math.min(selected, shown.length - 1));
    const frag = document.createDocumentFragment();
    shown.forEach((entry, i) => {
      const row = el("div", "bk-row");
      row.setAttribute("role", "option");
      row.setAttribute("aria-selected", String(i === selected));
      row.dataset.kind = entry.kind;
      el("span", "bk-icon", row).textContent = entryIcon(entry);
      const name = el("span", "bk-name", row);
      name.textContent = entry.link ? `${entry.name} →` : entry.name;
      el("span", "bk-size", row).textContent = entry.kind === "dir" ? "" : humanSize(entry.size);
      el("span", "bk-date", row).textContent = shortDate(entry.modified);
      row.addEventListener("click", () => {
        selected = i;
        mark();
      });
      row.addEventListener("dblclick", () => void activate(entry));
      frag.append(row);
    });
    listEl.append(frag);
    const parts: string[] = [];
    if (failure) parts.push(failure);
    else if (loading > 0) parts.push("Loading…");
    else if (listing) {
      const folders = shown.filter((e) => e.kind === "dir").length;
      parts.push(`${shown.length} ${shown.length === 1 ? "entry" : "entries"}${folders ? `, ${folders} folders` : ""}`);
      if (listing.truncated) parts.push("the list is cut: only the first entries are shown");
    }
    setText(status, parts.join(" · "));
    status.dataset.failed = failure ? "true" : "false";
    mark();
  };

  /** Move the highlight without redrawing the list. */
  const mark = (): void => {
    const rows = listEl.children;
    for (let i = 0; i < rows.length; i++) rows[i]?.setAttribute("aria-selected", String(i === selected));
    (rows[selected] as HTMLElement | undefined)?.scrollIntoView({ block: "nearest" });
  };

  const load = async (target: string): Promise<void> => {
    const mine = ++loading;
    failure = "";
    draw();
    try {
      const answer = readListing(await call<unknown>("files_list", { source: current.source, path: target }));
      if (!answer) throw new Error("the list could not be read");
      listing = answer;
      selected = 0;
      void call("block_update", { pane: current.pane, path: answer.path }).catch(() => {});
    } catch (e) {
      failure = String(e);
    } finally {
      if (mine === loading) loading = 0;
      else loading = Math.max(0, loading - 1);
    }
    draw();
  };

  const activate = async (entry: FileEntry): Promise<void> => {
    if (!listing) return;
    const full = joinPath(listing.path, entry.name);
    if (entry.kind === "dir") {
      await load(full);
      return;
    }
    if (entry.kind !== "file") return;
    // A second file goes into the preview this browser opened before, while that pane is still there.
    const known = previewOf.get(current.pane);
    if (known !== undefined && state.view?.panes.some(([id]) => id === known)) {
      try {
        await call("block_update", { pane: known, path: full });
        await refreshBlocks();
        return;
      } catch {
        // fall through: a new preview
      }
    }
    const before = new Set(state.view?.panes.map(([id]) => id) ?? []);
    const reason = await openBlock("preview", current.source, full, "right");
    if (reason) {
      failure = reason;
      draw();
      return;
    }
    const made = state.view?.panes.map(([id]) => id).find((id) => !before.has(id));
    if (made !== undefined) previewOf.set(current.pane, made);
  };

  up.addEventListener("click", () => {
    if (listing?.parent) void load(listing.parent);
  });
  hiddenToggle.addEventListener("click", () => {
    showHidden = !showHidden;
    draw();
  });

  void load(current.path);

  return {
    pane: info.pane,
    root,
    get info() {
      return current;
    },
    update(next) {
      if (next.path !== current.path && next.path !== listing?.path) void load(next.path);
      current = next;
    },
    key(e) {
      const page = Math.max(1, Math.floor(listEl.clientHeight / 24) - 1);
      switch (e.key) {
        case "ArrowDown":
          selected = Math.min(shown.length - 1, selected + 1);
          break;
        case "ArrowUp":
          selected = Math.max(0, selected - 1);
          break;
        case "PageDown":
          selected = Math.min(shown.length - 1, selected + page);
          break;
        case "PageUp":
          selected = Math.max(0, selected - page);
          break;
        case "Home":
          selected = 0;
          break;
        case "End":
          selected = Math.max(0, shown.length - 1);
          break;
        case "Enter":
        case "ArrowRight":
        case " ": {
          const entry = shown[selected];
          if (entry) void activate(entry);
          break;
        }
        case "Backspace":
        case "ArrowLeft":
          if (listing?.parent) void load(listing.parent);
          break;
        case ".":
          showHidden = !showHidden;
          draw();
          return true;
        default:
          return false;
      }
      mark();
      return true;
    },
    dispose() {
      root.replaceChildren();
    },
  };
}

// ---- the preview -------------------------------------------------------------------------------------------------

function previewBlock(info: BlockInfo, root: HTMLElement): Instance {
  root.classList.add("block-preview");
  const body = el("div", "bk-body", root);
  const status = el("div", "bk-status", root);
  status.setAttribute("role", "status");
  let current = info;
  /** The grant of the file shown now; the core stops serving it when it is revoked. */
  let grant: string | null = null;
  let token = 0;
  /** The PDF viewer, while a PDF is shown: it takes the keys of the pane. */
  let pdf: PdfView | null = null;

  const release = (): void => {
    pdf?.dispose();
    pdf = null;
    if (grant) void call("file_revoke", { token: grant }).catch(() => {});
    grant = null;
  };

  const message = (text: string, failed = false): void => {
    release();
    body.replaceChildren();
    const p = el("p", "bk-message", body);
    p.textContent = text;
    status.dataset.failed = failed ? "true" : "false";
    setText(status, failed ? text : "");
  };

  const load = async (): Promise<void> => {
    const mine = ++token;
    message("Loading…");
    let opened: Opened | null = null;
    try {
      opened = (await call<Opened>("file_open", { source: current.source, path: current.path })) ?? null;
    } catch (e) {
      if (mine === token) message(String(e), true);
      return;
    }
    if (mine !== token || !opened || typeof opened.content !== "string") return;
    release();
    body.replaceChildren();
    status.dataset.failed = "false";
    const facts = `${opened.mime} · ${humanSize(opened.size)}`;
    setText(status, facts);
    try {
      switch (opened.content) {
        case "markdown": {
          const doc = el("div", "md", body);
          // The core cleaned this HTML; the page copies it through an allow-list of its own and never uses innerHTML.
          if (opened.html) appendSafe(opened.html, doc);
          else doc.textContent = opened.text ?? "";
          break;
        }
        case "text": {
          const text = opened.text;
          if (text === null) {
            message(`This file is larger than the editor opens (${humanSize(opened.size)}).`);
            break;
          }
          const lines = text.split("\n");
          const cut = lines.length > MAX_LINES;
          const code = el("div", "code", body);
          const gutter = el("div", "code-gutter", code);
          const content = el("pre", "code-text", code);
          const shownLines = cut ? lines.slice(0, MAX_LINES) : lines;
          gutter.textContent = shownLines.map((_, i) => String(i + 1)).join("\n");
          content.textContent = shownLines.join("\n");
          if (opened.language) setText(status, `${facts} · ${opened.language}${cut ? " · cut after " + MAX_LINES + " lines" : ""}`);
          else if (cut) setText(status, `${facts} · cut after ${MAX_LINES} lines`);
          break;
        }
        case "image":
        case "video":
        case "audio": {
          // The file is not loaded into the page: it is served by the core in ranges, from a URL that is good for this
          // one file (the `holzfile` scheme), so a video starts at once and seeks, and a large file costs no memory.
          const granted = await call<Granted>("file_grant", { source: current.source, path: current.path });
          if (mine !== token) {
            void call("file_revoke", { token: granted.token }).catch(() => {});
            return;
          }
          grant = granted.token;
          const url = granted.url;
          if (opened.content === "image") {
            const img = el("img", "bk-image", body);
            img.alt = opened.name;
            img.decoding = "async";
            img.addEventListener("load", () => setText(status, `${facts} · ${img.naturalWidth}×${img.naturalHeight}`));
            img.src = url;
          } else {
            const media = el(opened.content === "video" ? "video" : "audio", "bk-media", body);
            media.controls = true;
            media.preload = "metadata";
            media.src = url;
          }
          break;
        }
        case "pdf": {
          // Read in ranges through the core with a grant of its own; PDF.js is loaded only now.
          const granted = await call<Granted>("file_grant", { source: current.source, path: current.path });
          if (mine !== token) {
            void call("file_revoke", { token: granted.token }).catch(() => {});
            return;
          }
          grant = granted.token;
          const view = await showPdf(body, granted.token, opened.size, (t) => setText(status, `${facts} · ${t}`));
          if (mine !== token) {
            view.dispose();
            return;
          }
          pdf = view;
          break;
        }
        default:
          message(`No preview for this kind of file yet (${opened.mime}, ${humanSize(opened.size)}).`);
      }
    } catch (e) {
      if (mine === token) message(String(e), true);
    }
  };

  void load();

  return {
    pane: info.pane,
    root,
    get info() {
      return current;
    },
    update(next) {
      const changed = next.path !== current.path || next.source !== current.source;
      current = next;
      if (changed) void load();
    },
    key(e) {
      if (pdf) return pdf.key(e);
      const step = 48;
      switch (e.key) {
        case "ArrowDown":
          body.scrollBy({ top: step });
          return true;
        case "ArrowUp":
          body.scrollBy({ top: -step });
          return true;
        case "PageDown":
        case " ":
          body.scrollBy({ top: body.clientHeight * 0.9 });
          return true;
        case "PageUp":
          body.scrollBy({ top: -body.clientHeight * 0.9 });
          return true;
        case "Home":
          body.scrollTo({ top: 0 });
          return true;
        case "End":
          body.scrollTo({ top: body.scrollHeight });
          return true;
        default:
          return false;
      }
    },
    dispose() {
      token += 1;
      release();
      root.replaceChildren();
    },
  };
}

// ---- placing the blocks ------------------------------------------------------------------------------------------

/** Bring the drawn blocks in line with the view: one per visible block pane, over the pane below its header. */
function renderBlocks(): void {
  const view = state.view;
  if (!layer || !view) return;
  const live = new Set<number>();
  for (const [id, unit] of view.panes) {
    // A pane the core has not been asked about yet may be a block: ask once.
    if (!asked.has(id)) {
      asked.add(id);
      void refreshBlocks();
    }
    const info = paneBlocks.get(id);
    if (!info) continue;
    live.add(id);
    let inst = instances.get(id);
    if (inst && inst.info.kind !== info.kind) {
      inst.dispose();
      inst.root.remove();
      instances.delete(id);
      inst = undefined;
    }
    if (!inst) {
      const root = el("div", "block", layer);
      root.dataset.pane = String(id);
      root.setAttribute("tabindex", "-1");
      root.addEventListener("pointerdown", () => {
        if (state.view && state.view.focus !== id) void call<View>("focus_pane", { pane: id }).then(applyView);
        focusInput();
      });
      inst = info.kind === "files" ? filesBlock(info, root) : previewBlock(info, root);
      instances.set(id, inst);
    } else {
      inst.update(info);
    }
    const r = paneRect(state.metrics, unit);
    const head = state.headerPx;
    const style = inst.root.style;
    style.left = `${r.x}px`;
    style.top = `${r.y + head}px`;
    style.width = `${r.w}px`;
    style.height = `${Math.max(0, r.h - head)}px`;
    inst.root.classList.toggle("focused", id === view.focus);
  }
  for (const [id, inst] of instances) {
    if (live.has(id)) continue;
    inst.dispose();
    inst.root.remove();
    instances.delete(id);
  }
}

/**
 * Start block panes: the layer they are drawn in, the actions that open them, and the key handler. `apply` shows a view,
 * `changed` is called when the blocks changed (so the pane headers draw again).
 */
export async function initBlocks(container: HTMLElement, apply: (v: View) => void, changed: () => void): Promise<void> {
  await loadStyles("./blocks.css").catch(() => {
    // the blocks still work without their style sheet
  });
  layer = container;
  applyView = apply;
  onBlocks = changed;
  onOverlayRender(renderBlocks);
  registerAction({
    id: "files.open",
    title: "Files: This computer",
    run: () => void openBlock("files", "local", "", "tab"),
  });
  // The navigation keys of a block pane that has the focus; a key with a modifier stays with the shortcuts.
  pushKeyHandler((e) => {
    if (e.metaKey || e.ctrlKey || e.altKey) return false;
    const focus = state.view?.focus;
    const inst = focus === undefined ? undefined : instances.get(focus);
    if (!inst) return false;
    if (!inst.key(e)) return false;
    e.preventDefault();
    return true;
  });
  void refreshBlocks();
}
