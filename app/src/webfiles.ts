// Files in the browser build (docs/design.md 6.30). Files live on the server and not on the person's computer, so a file dropped on a
// pane is sent to the server and its path is typed into that pane, like a paste. `web.ts` starts this module on the web platform
// only; the desktop page never loads it.
//
// The drop. While a file is dragged over a pane that runs a shell, the pane shows a dashed edge and the label `Drop to upload`. A drop
// sends each file, one after the other (16 at most), as the raw body of `PUT /api/upload?name=<file name>` through the page's one
// request function (`http` of the bridge: the session cookie, the CSRF token, no redirect, same origin only), says what it does in the
// status line, and types the returned path, quoted for the shell, through the paste path of the pane under the drop. A refusal says why
// and types nothing. A drag that carries files is taken everywhere on the page, so the browser never navigates to a dropped file.
//
// The download. A file that the core grants (it grants images, video, audio and PDF files) can be taken out by an explicit act only: the
// entry `Download` of the row menu of the file browser (opened by the context-menu gesture, the context-menu key or Shift+F10) or of the
// palette. The act asks the core for a short-lived grant (`file_grant`), whose address is `/files/<token>` on the page's own origin, and
// follows it with a link that has the `download` attribute. The page keeps at most 16 grants it made, ends the oldest beyond that and all of
// them when the page is hidden for good. Selecting a row, opening the menu or listing the palette entry grants nothing.
//
// Nothing of a file (its name, its path) is kept: the upload folder shown on the label is held in memory until the page goes.
import { registerAction } from "./actions.js";
import { HttpError, call, http } from "./bridge.js";
import { focusedFile } from "./blocks.js";
import type { FileRef } from "./blocks.js";
import { el, loadStyles } from "./dom.js";
import { paneBlocks } from "./blockstate.js";
import { paneAt, paneRect } from "./geometry.js";
import { focusInput, pasteText } from "./keyboard.js";
import { pushKeyHandler } from "./keystack.js";
import { onOverlayRender } from "./overlay.js";
import { state } from "./state.js";
import { showNotice } from "./status.js";
import { sessionEnded } from "./websession.js";
import {
  DOWNLOAD_ONLY_SENTENCE, DROP_LABEL, GrantList, MAX_DROP_FILES, baseName, batchText, readGrant, readUploaded, refusalText, shellQuote,
  takeFiles, uploadFolder, uploadUrl, uploadingText,
} from "./webstate.js";

/** One file waiting to be sent, with the pane under its drop and the count of paths already typed for that drop. */
interface Job {
  file: File;
  pane: number;
  drop: { typed: number };
}

/** What the status line says when the run of uploads ends. */
interface Batch {
  ok: number;
  failed: string[];
  over: number;
  folders: number;
}

const fresh = (): Batch => ({ ok: 0, failed: [], over: 0, folders: 0 });

const queue: Job[] = [];
let running = false;
let batch: Batch = fresh();

/** The folder the server named in an earlier answer; empty until then. Memory only. */
let folder = "";

let started = false;
let target: HTMLElement | null = null;
let targetPane = -1;
let targetFolder = "";
let watchdog: ReturnType<typeof setTimeout> | undefined;

/** How long a drag may go without an event before its edge is taken away (a drag that was cancelled in some way that sent no event). */
const WATCHDOG_MS = 1500;

/** The canvas of the panes. */
const canvas = (): HTMLElement | null => document.getElementById("term");

/** Whether the drag carries files (the transfer lists `Files`). */
const carriesFiles = (dt: DataTransfer | null): boolean => dt !== null && Array.from(dt.types ?? []).includes("Files");

/** Whether the drag carries a link and no file: a browser follows a dropped link unless the page cancels it. */
const carriesLink = (dt: DataTransfer | null): boolean => dt !== null && !carriesFiles(dt) && Array.from(dt.types ?? []).includes("text/uri-list");

/** A text field of the page keeps its own drop. */
function inField(node: EventTarget | null): boolean {
  return (
    node instanceof HTMLInputElement ||
    node instanceof HTMLTextAreaElement ||
    node instanceof HTMLSelectElement ||
    (node instanceof HTMLElement && (node.isContentEditable || node.dataset.field !== undefined))
  );
}

/** A rectangle in window pixels. */
interface Box {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** The pane that takes an upload at a window point: a pane that runs a shell, with no layer open and a session that is alive. Null otherwise. */
function acceptingPane(x: number, y: number): { id: number; box: Box } | null {
  const c = canvas();
  if (c === null || sessionEnded() || document.querySelector(".dlg-backdrop, .palette-backdrop.open") !== null) return null;
  const box = c.getBoundingClientRect();
  const at = paneAt(state.metrics, state.view, x - box.left, y - box.top);
  if (at === null || paneBlocks.has(at[0])) return null;
  const r = paneRect(state.metrics, at[1]);
  return { id: at[0], box: { left: box.left + r.x, top: box.top + r.y, width: r.w, height: r.h } };
}

/** Take the edge away. */
function hideTarget(): void {
  clearTimeout(watchdog);
  watchdog = undefined;
  target?.remove();
  target = null;
  targetPane = -1;
}

/** Draw the label and, once the page knows it, the folder. */
function fill(node: HTMLElement): void {
  node.replaceChildren();
  const box = el("div", "web-drop-box", node);
  el("b", "web-drop-label label", box).textContent = DROP_LABEL;
  if (folder !== "") {
    const to = el("span", "web-drop-to", box);
    to.append("to ");
    el("span", "web-drop-folder", to).textContent = folder;
  }
  targetFolder = folder;
}

/** Show the edge on the pane `id` at `box` (window pixels), once, moving it when the pane changes. */
function showTarget(id: number, box: Box): void {
  if (target === null) {
    target = el("div", "web-drop", document.body);
    target.setAttribute("role", "status");
  }
  if (targetPane !== id || targetFolder !== folder) {
    fill(target);
    targetPane = id;
  }
  const s = target.style;
  s.left = `${box.left + 2}px`;
  s.top = `${box.top + 2}px`;
  s.width = `${Math.max(0, box.width - 4)}px`;
  s.height = `${Math.max(0, box.height - 4)}px`;
  clearTimeout(watchdog);
  watchdog = setTimeout(hideTarget, WATCHDOG_MS);
}

/** A drag over the page: take it when it carries files (or a link, so the page is not left), and show where it would land. */
function onDrag(e: DragEvent): void {
  const dt = e.dataTransfer;
  const files = carriesFiles(dt);
  if (!files) {
    // A dropped link would take the page away; a text field keeps its own drop, and so does everything else that is not a file.
    if (carriesLink(dt) && !inField(e.target)) {
      e.preventDefault();
      if (dt) dt.dropEffect = "none";
    }
    return;
  }
  e.preventDefault(); // every drag of files, anywhere: the browser must not open the file
  if (e.type === "dragleave") {
    if (e.relatedTarget === null) hideTarget();
    return;
  }
  const accepting = acceptingPane(e.clientX, e.clientY);
  if (dt) dt.dropEffect = accepting === null ? "none" : "copy";
  if (accepting === null) hideTarget();
  else showTarget(accepting.id, accepting.box);
}

/** The files of a drop with a note of which items are folders. Read in the event: the browser empties the transfer afterwards. */
function filesOf(dt: DataTransfer): { file: File; directory: boolean }[] {
  const out: { file: File; directory: boolean }[] = [];
  const items = Array.from(dt.items ?? []).filter((i) => i.kind === "file");
  if (items.length > 0) {
    for (const item of items) {
      const file = item.getAsFile();
      if (file === null) continue;
      let directory = false;
      try {
        directory = item.webkitGetAsEntry?.()?.isDirectory === true;
      } catch {
        // an entry that cannot be read is a file as far as the page can tell
      }
      out.push({ file, directory });
    }
    return out;
  }
  for (const file of Array.from(dt.files ?? [])) out.push({ file, directory: false });
  return out;
}

/** A drop: send what it holds to the pane under it, when there is one. */
function onDrop(e: DragEvent): void {
  const dt = e.dataTransfer;
  if (!carriesFiles(dt)) {
    if (carriesLink(dt) && !inField(e.target)) e.preventDefault();
    return;
  }
  e.preventDefault();
  hideTarget();
  const accepting = acceptingPane(e.clientX, e.clientY);
  if (dt === null || accepting === null) return;
  take(filesOf(dt), accepting.id);
}

/** Queue the files of one drop for `pane`, at most 16 waiting at a time; say what was left out. */
function take(list: { file: File; directory: boolean }[], pane: number): void {
  const taken = takeFiles(list);
  const pending = queue.length + (running ? 1 : 0);
  const room = Math.max(0, MAX_DROP_FILES - pending);
  const files = taken.files.slice(0, room);
  batch.over += taken.over + (taken.files.length - files.length);
  batch.folders += taken.folders;
  const drop = { typed: 0 };
  for (const item of files) queue.push({ file: item.file, pane, drop });
  if (files.length === 0 && !running) {
    showNotice(batchText(batch));
    batch = fresh();
    return;
  }
  void run();
}

/** Send one file and type its path. A refusal or a failure is counted, never thrown. */
async function send(job: Job): Promise<void> {
  showNotice(uploadingText(job.file.name));
  try {
    const answer = readUploaded(await http("PUT", uploadUrl(job.file.name), job.file));
    if (answer === null) throw new Error("the answer is not an upload");
    folder = uploadFolder(answer.path);
    batch.ok++;
    const text = (job.drop.typed > 0 ? " " : "") + shellQuote(answer.path);
    job.drop.typed++;
    await pasteText(job.pane, text);
    focusInput();
  } catch (e) {
    batch.failed.push(e instanceof HttpError ? refusalText(e.status, e.body) : "upload failed");
  }
}

/** Send the queue one file after the other; when it is empty say how it went. */
async function run(): Promise<void> {
  if (running) return;
  running = true;
  try {
    for (let job = queue.shift(); job !== undefined; job = queue.shift()) {
      if (sessionEnded()) {
        // The session is over: nothing more is sent, and the rest of the run says why.
        queue.length = 0;
        batch.failed.push("sign in again");
        break;
      }
      await send(job);
    }
  } finally {
    running = false;
  }
  const done = batch;
  batch = fresh();
  showNotice(batchText(done));
}

// ---- download ---------------------------------------------------------------------------------------------------------

/** The grants made for downloads, oldest first. */
const grants = new GrantList();

/** End a grant in the core; a failure is of no use to anyone. */
function revoke(token: string): void {
  void call("file_revoke", { token }).catch(() => {});
}

/** The grants end with the page: every one that is left is revoked, oldest first. */
function revokeAll(): void {
  for (const token of grants.clear()) revoke(token);
}

/** Take `file` out of the server: ask for a grant, follow its address with a download link, keep the grant in the bounded list. */
export async function downloadFile(file: FileRef): Promise<void> {
  let raw: unknown;
  try {
    raw = await call<unknown>("file_grant", { source: file.source, path: file.path });
  } catch {
    showNotice(DOWNLOAD_ONLY_SENTENCE); // the core refuses a kind it does not grant
    return;
  }
  const granted = readGrant(raw);
  if (granted === null) {
    // An answer that is not a same-origin grant is not followed, and what it granted ends.
    const token = typeof raw === "object" && raw !== null ? (raw as { token?: unknown }).token : undefined;
    if (typeof token === "string") revoke(token);
    showNotice(DOWNLOAD_ONLY_SENTENCE);
    return;
  }
  const old = grants.add(granted.token);
  if (old !== null) revoke(old);
  const link = el("a", "web-download", document.body);
  link.href = granted.url;
  link.download = baseName(file.path);
  link.rel = "noopener";
  link.hidden = true;
  link.click();
  link.remove();
}

let offered = false;

/** Add the palette entry `Download` (listed only while the focused pane has a file). Called by the file browser or preview when the first one is made. */
export function offerDownload(): void {
  if (offered) return;
  offered = true;
  registerAction({
    id: "files.download",
    title: "Download",
    run: () => {
      const file = focusedFile();
      if (file !== null) void downloadFile(file);
    },
    available: () => focusedFile() !== null,
  });
}

// ---- the row menu -------------------------------------------------------------------------------------------------------

/** The open menu: its element, the way out of everything it listens to, and the layout it was opened over. */
let menu: { node: HTMLElement; off: () => void } | null = null;

/** The panes' rectangles and the canvas size as one text: a change of it is a change of layout (the focus moving is not). */
function layoutKey(): string {
  const m = state.metrics;
  return JSON.stringify([state.view?.panes ?? [], m.width, m.height]);
}

/** Take the menu away. With `back` the keyboard returns to the terminal. */
function closeMenu(back: boolean): void {
  if (menu === null) return;
  const open = menu;
  menu = null;
  open.off();
  open.node.remove();
  if (back) focusInput();
}

/**
 * Open the row menu of `file` at a point (the pointer) or under an element (the selected row): a floating layer with the one entry
 * `Download`, which has the keyboard. Escape, a press elsewhere, a blur and a change of layout close it and the keyboard goes back to the
 * terminal; while it is open it takes every key.
 */
export function openRowMenu(file: FileRef, at: { x: number; y: number } | HTMLElement): void {
  closeMenu(false);
  const node = el("div", "web-menu", document.body);
  node.setAttribute("role", "menu");
  node.setAttribute("aria-label", `Actions for ${baseName(file.path)}`);
  const item = el("button", "web-menu-item", node);
  item.type = "button";
  item.setAttribute("role", "menuitem");
  item.textContent = "Download";
  // A press on the entry must not take the focus from it first: some engines blur a button on press, and the blur would close the menu before the click.
  node.addEventListener("mousedown", (e) => e.preventDefault());
  item.addEventListener("click", () => {
    closeMenu(true);
    void downloadFile(file);
  });

  // Place it: at the pointer, or just under the left part of the row; then keep it inside the window.
  let x: number;
  let y: number;
  if (at instanceof HTMLElement) {
    const r = at.getBoundingClientRect();
    x = r.left + 2 * (parseFloat(getComputedStyle(document.documentElement).fontSize) || 12);
    y = r.bottom - 2;
  } else {
    x = at.x;
    y = at.y;
  }
  const box = node.getBoundingClientRect();
  const edge = 4;
  node.style.left = `${Math.max(edge, Math.min(x, window.innerWidth - box.width - edge))}px`;
  node.style.top = `${Math.max(edge, Math.min(y, window.innerHeight - box.height - edge))}px`;

  const layout = layoutKey();
  const press = (e: PointerEvent): void => {
    if (!(e.target instanceof Node && node.contains(e.target))) closeMenu(true);
  };
  const blur = (e: FocusEvent): void => {
    if (!(e.relatedTarget instanceof Node && node.contains(e.relatedTarget))) closeMenu(true);
  };
  const resize = (): void => closeMenu(true);
  const stopRender = onOverlayRender(() => {
    if (layoutKey() !== layout) closeMenu(true);
  });
  const stopKeys = pushKeyHandler((e) => {
    if (e.key === "Escape") {
      e.preventDefault();
      closeMenu(true);
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp" || e.key === "Home" || e.key === "End") {
      e.preventDefault();
      item.focus(); // the one entry keeps the keyboard
    }
    return true; // every key belongs to the menu while it is open; Enter and Space reach the entry as clicks
  });
  document.addEventListener("pointerdown", press, true);
  node.addEventListener("focusout", blur);
  window.addEventListener("resize", resize);
  menu = {
    node,
    off: () => {
      document.removeEventListener("pointerdown", press, true);
      node.removeEventListener("focusout", blur);
      window.removeEventListener("resize", resize);
      stopRender();
      stopKeys();
    },
  };
  item.focus();
}

/** Start the file code of the page: the listeners for dragged files and the end of the grants. Called once by `web.ts`, on the web platform only. */
export function initFiles(): void {
  if (started) return;
  started = true;
  window.addEventListener("pagehide", revokeAll);
  void loadStyles("./web.css").catch(() => {
    // the target still works without its edge: the label is text
  });
  for (const type of ["dragenter", "dragover", "dragleave"]) document.addEventListener(type, (e) => onDrag(e as DragEvent), true);
  document.addEventListener("drop", (e) => onDrop(e as DragEvent), true);
}
