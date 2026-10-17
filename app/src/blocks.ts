// Block panes: a pane that shows a file browser or the preview of a file instead of a terminal (docs/design.md
// sections 6.3 and 6.5). The core owns what each block shows (`pane_blocks`) and reads the files; this module draws it
// over the empty terminal of the pane and sends what the person does back. Every name from a file system goes onto the
// screen with `textContent`; the one exception is the HTML of a Markdown document, which the core has already passed
// through an allow-list sanitiser (core/src/preview.rs). Images and video are shown from `blob:` URLs, never from a
// path. While a block pane has the focus it takes the navigation keys (the key handler stack); everything with a
// modifier stays with the shortcuts.
import { registerAction } from "./actions.js";
import { call } from "./bridge.js";
import { chatClosed, mountChat } from "./chat.js";
import { el, loadStyles, setText } from "./dom.js";
import { bar, bytes, memory, noteWords, rate, readReading, uptime } from "./monitorstate.js";
import type { MonNoteKind, MonReading, MonSample } from "./monitorstate.js";
import { showEditor } from "./editorview.js";
import type { EditorHandle } from "./editorview.js";
import { showPdf } from "./pdfview.js";
import type { PdfView } from "./pdfview.js";
import { appendSafe } from "./safehtml.js";
import { paneRect } from "./geometry.js";
import { focusInput } from "./keyboard.js";
import { pushKeyHandler } from "./keystack.js";
import { onOverlayRender, renderOverlay } from "./overlay.js";
import { state } from "./state.js";
import {
  dirtyPanes, entryIcon, goneChats, humanSize, joinPath, paneBlocks, readBlocks, readListing, setPaneBlocks, shortDate,
  visibleEntries,
} from "./blockstate.js";
import type { BlockInfo, FileEntry, Listing, Opened } from "./blockstate.js";
import type { View } from "./types.js";

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
    // A chat pane that left the core's list is closed: its reply stops. A list that could not be read stops nothing, and
    // a pane that a tab switch only hides is still in the list.
    const list = readBlocks(raw);
    const gone = Array.isArray(raw) ? goneChats([...paneBlocks.values()], list) : [];
    if (setPaneBlocks(list)) onBlocks();
    renderOverlay();
    for (const conversation of gone) chatClosed(conversation);
  } finally {
    refreshing = false;
  }
}

/** Open a file browser (`files`), a preview (`preview`), the monitor or a chat in a new tab or beside the focused pane. */
export async function openBlock(
  kind: "files" | "preview" | "monitor" | "chat",
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
  /** The editor, while a text file (or a Markdown document being edited) is shown. */
  let editor: EditorHandle | null = null;
  /** The text as last read or saved, to know whether the editor holds changes. */
  let saved = "";
  /** The file's time as last read or saved; sent with a save so a change on disk is noticed. `null` overwrites. */
  let expected: number | null = null;
  let facts = "";

  const setDirty = (dirty: boolean): void => {
    if (dirty === dirtyPanes.has(info.pane)) return;
    if (dirty) dirtyPanes.add(info.pane);
    else dirtyPanes.delete(info.pane);
    status.dataset.dirty = dirty ? "true" : "false";
    setText(status, dirty ? `● unsaved · ${facts}` : facts);
    onBlocks();
  };

  const save = async (): Promise<void> => {
    if (!editor) return;
    const text = editor.getText();
    try {
      const modified = await call<number | null>("file_save", {
        source: current.source, path: current.path, text, expected_modified: expected,
      });
      saved = text;
      expected = modified ?? null;
      status.dataset.failed = "false";
      setDirty(editor.getText() !== saved);
      if (!dirtyPanes.has(info.pane)) setText(status, `saved · ${facts}`);
    } catch (e) {
      const why = String(e);
      status.dataset.failed = "true";
      setText(status, why);
      // A change on disk is said once; the next save overwrites it on purpose.
      if (why.includes("changed on disk")) expected = null;
    }
  };

  const edit = async (text: string, language: string | null, markdown: boolean): Promise<void> => {
    const mine = token;
    const host = el("div", "bk-editor", body);
    const made = await showEditor({
      parent: host,
      text,
      language,
      onChange: (t) => setDirty(t !== saved),
      onSave: () => void save(),
      onLeave: () => {
        // A Markdown document that has no changes goes back to its page; anything else gives the keys to the terminal.
        if (markdown && !dirtyPanes.has(info.pane)) void load();
        else focusInput();
      },
    });
    if (mine !== token) {
      made.destroy();
      return;
    }
    editor = made;
    if (state.view?.focus === info.pane) made.focus();
  };

  const release = (): void => {
    editor?.destroy();
    editor = null;
    setDirty(false);
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
    facts = `${opened.mime} · ${humanSize(opened.size)}`;
    saved = opened.text ?? "";
    expected = opened.modified ?? null;
    setText(status, facts);
    try {
      switch (opened.content) {
        case "markdown": {
          const doc = el("div", "md", body);
          // The core cleaned this HTML; the page copies it through an allow-list of its own and never uses innerHTML.
          doc.tabIndex = 0;
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
          if (opened.language) facts += ` · ${opened.language}`;
          setText(status, facts);
          await edit(text, opened.language, false);
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
      if (editor) return false; // the editor takes its own keys
      if (e.key === "e" && current.path && body.querySelector(".md")) {
        // Markdown: from the page to its source.
        body.replaceChildren();
        void edit(saved, "markdown", true);
        return true;
      }
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

// ---- the system monitor ------------------------------------------------------------------------------------------

/** How many processes the monitor lists. */
const MON_ROWS = 8;
const MON_SEGMENTS = 8;

/**
 * CPU and memory as bars with their numbers, the network rates, and the busiest processes; sampled every second. The
 * monitor of another computer also has one line above the bars that says in words whether it is connecting, has no
 * answer or the host is not Linux; its numbers stay, dimmed, while the host is silent.
 */
function monitorBlock(info: BlockInfo, root: HTMLElement): Instance {
  root.classList.add("block-monitor");
  let current = info;
  const body = el("div", "mon bk-body", root);
  body.setAttribute("role", "group");
  body.setAttribute("aria-label", "System monitor");
  // The line of words. Every word in it is set as text, never as markup.
  const note = el("p", "mon-note", body);
  note.setAttribute("role", "status");
  note.hidden = true;
  const meter = (label: string) => {
    const row = el("div", "mon-row", body);
    el("span", "mon-label", row).textContent = label;
    const b = el("span", "mon-bar", row);
    b.setAttribute("role", "img");
    const cells = Array.from({ length: MON_SEGMENTS }, () => el("i", "", b));
    const value = el("span", "mon-value", row);
    return { b, cells, value };
  };
  const cpu = meter("CPU");
  const mem = meter("MEM");
  const netRow = el("div", "mon-row", body);
  el("span", "mon-label", netRow).textContent = "NET";
  const down = el("span", "mon-down", netRow);
  const up = el("span", "mon-up", netRow);
  const upRow = el("div", "mon-row mon-dim", body);
  el("span", "mon-label", upRow).textContent = "UP";
  const upValue = el("span", "", upRow);
  const head = el("div", "mon-proc mon-dim", body);
  for (const t of ["PID", "NAME", "CPU", "MEM"]) el("span", "", head).textContent = t;
  const procs = Array.from({ length: MON_ROWS }, () => {
    const row = el("div", "mon-proc", body);
    return [el("span", "", row), el("span", "mon-name", row), el("span", "", row), el("span", "", row)];
  });
  const fill = (m: { b: HTMLElement; cells: HTMLElement[]; value: HTMLElement }, percent: number, text: string): void => {
    const { on, level } = bar(percent, MON_SEGMENTS);
    m.cells.forEach((c, i) => {
      c.dataset.on = i < on ? level : "";
    });
    m.b.setAttribute("aria-label", `${Math.round(percent)} percent`);
    m.value.dataset.level = level;
    setText(m.value, text);
  };
  const draw = (s: MonSample): void => {
    fill(cpu, s.cpu, `${Math.round(s.cpu)}%`);
    const memPercent = s.mem_total === 0 ? 0 : (s.mem_used / s.mem_total) * 100;
    fill(mem, memPercent, memory(s.mem_used, s.mem_total));
    setText(down, `↓ ${rate(s.net_rx)}`);
    setText(up, `↑ ${rate(s.net_tx)}`);
    setText(upValue, `${uptime(s.uptime)} · ${s.cores} cores`);
    procs.forEach((cells, i) => {
      const p = s.procs[i];
      setText(cells[0]!, p ? String(p.pid) : "");
      setText(cells[1]!, p ? p.name : "");
      setText(cells[2]!, p ? p.cpu.toFixed(1) : "");
      setText(cells[3]!, p ? bytes(p.mem) : "");
    });
  };
  /** Empty every number: the host is not one this monitor can read. */
  const clear = (): void => {
    for (const m of [cpu, mem]) {
      m.cells.forEach((c) => {
        c.dataset.on = "";
      });
      m.b.removeAttribute("aria-label");
      delete m.value.dataset.level;
      setText(m.value, "");
    }
    for (const node of [down, up, upValue, ...procs.flat()]) setText(node, "");
  };
  const remote = (): boolean => current.source !== "local";
  const label = (): string => current.source_label || current.source;
  const showNote = (kind: MonNoteKind, detail: string): void => {
    const words = noteWords(kind, label(), detail);
    setText(note, words.text);
    note.dataset.state = kind;
    note.dataset.tone = words.tone;
    note.hidden = false;
  };
  const hideNote = (): void => {
    note.hidden = true;
    setText(note, "");
    delete note.dataset.state;
    delete note.dataset.tone;
  };
  /** Whether numbers are on show, which a silent host dims instead of removing. */
  let numbers = false;
  const stale = (on: boolean): void => {
    if (on) body.dataset.stale = "true";
    else delete body.dataset.stale;
  };
  const show = (reading: MonReading | null): void => {
    if (reading?.state === "live" && reading.sample) {
      draw(reading.sample);
      numbers = true;
      stale(false);
      hideNote();
    } else if (reading?.state === "unsupported") {
      clear();
      numbers = false;
      stale(false);
      showNote("unsupported", reading.system);
    } else if (remote()) {
      failed("unknown reason");
    }
  };
  const failed = (reason: string): void => {
    stale(numbers);
    showNote("no_answer", reason);
  };
  const reasonOf = (e: unknown): string => (typeof e === "string" ? e : e instanceof Error ? e.message : "unknown reason");
  let alive = true;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const tick = async (): Promise<void> => {
    if (!alive) return;
    // A hidden window (or a tab that is not shown) costs nothing: the sample waits.
    if (!document.hidden && root.offsetParent !== null) {
      try {
        const reading = readReading(await call<unknown>("monitor_sample", { pane: info.pane }));
        if (alive) show(reading);
      } catch (e) {
        // The monitor of this computer asks again next second without a word; another computer says why it is silent.
        if (alive && remote()) failed(reasonOf(e));
      }
    }
    if (alive) timer = setTimeout(() => void tick(), 1000);
  };
  if (remote()) showNote("connecting", "");
  void tick();
  return {
    pane: info.pane,
    root,
    get info() {
      return current;
    },
    update(next) {
      current = next;
    },
    key() {
      return false;
    },
    dispose() {
      alive = false;
      clearTimeout(timer);
      root.replaceChildren();
    },
  };
}

// ---- the AI chat -------------------------------------------------------------------------------------------------

/** A chat in a pane: the chat module draws it into the block's root and acts for this pane. */
function chatBlock(info: BlockInfo, root: HTMLElement): Instance {
  root.classList.add("block-chat");
  let current = info;
  // The pane that has the focus when the chat is drawn is not "gaining" it: a mount never takes the keyboard by itself.
  let focused = state.view?.focus === info.pane;
  const view = mountChat(root, info.path, { pane: () => current.pane });
  return {
    pane: info.pane,
    root,
    get info() {
      return current;
    },
    update(next) {
      const moved = next.path !== current.path;
      current = next;
      if (moved) view.show(next.path);
      const now = state.view?.focus === next.pane;
      if (now && !focused) view.focus();
      focused = now;
    },
    key() {
      return false;
    },
    dispose() {
      view.dispose();
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
      root.addEventListener("pointerdown", (ev) => {
        if (state.view && state.view.focus !== id) void call<View>("focus_pane", { pane: id }).then(applyView);
        // The editor keeps the focus it is given; anything else gives it to the terminal input.
        if (!(ev.target instanceof Element && ev.target.closest("[data-field]"))) focusInput();
      });
      inst =
        info.kind === "files"
          ? filesBlock(info, root)
          : info.kind === "monitor"
            ? monitorBlock(info, root)
            : info.kind === "chat"
              ? chatBlock(info, root)
              : previewBlock(info, root);
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
  registerAction({
    id: "monitor.open",
    title: "System monitor",
    run: () => void openBlock("monitor", "local", "", "tab"),
  });
  // The navigation keys of a block pane that has the focus; a key with a modifier stays with the shortcuts.
  pushKeyHandler((e) => {
    if (e.metaKey || e.ctrlKey || e.altKey) return false;
    if (e.target instanceof Element && e.target.closest("[data-field]")) return false;
    const focus = state.view?.focus;
    const inst = focus === undefined ? undefined : instances.get(focus);
    if (!inst) return false;
    if (!inst.key(e)) return false;
    e.preventDefault();
    return true;
  });
  void refreshBlocks();
}
