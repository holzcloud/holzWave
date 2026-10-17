// The second form of the sidebar column (docs/design.md section 6.18): the AI chat that takes the place of the sidebar in the
// same column, with the list of stored conversations in it. Every string goes in with `textContent`; a stored title, a model
// name or a note of the core is text and nothing else. The chat in the column is the module of the chat pane (`mountChat`),
// fed by the same events, so the consent card, the approval cards and the acting-pane rule are the pane chat's own; nothing
// here decides or grants anything and the column starts no turn and sends no text of its own. No command is called before a
// press: not at launch, not when the column opens as the sidebar, not when the chat mounts or redraws.
import { registerAction } from "./actions.js";
import { call } from "./bridge.js";
import { chatTitles, paneBlocks } from "./blockstate.js";
import { chatClosed, mountChat, releaseChat } from "./chat.js";
import type { ChatView, SidebarHandle } from "./chat.js";
import { isChatId } from "./chatstate.js";
import { faultText } from "./chatview.js";
import { LIST_NOTES, armWords, headTitle, listRows, readChatList } from "./chatlist.js";
import type { RowView } from "./chatlist.js";
import { el, loadStyles, setText } from "./dom.js";
import { focusInput } from "./keyboard.js";
import { onOverlayRender, renderOverlay } from "./overlay.js";
import { applyView } from "./scene.js";
import { state } from "./state.js";
import { showNotice } from "./status.js";
import type { View } from "./types.js";

/** What the column hands to the AI form: whether it is shown, how to show it, how to measure the stage again, how to redraw the sidebar. */
export interface ColumnHooks {
  shown(): boolean;
  show(): void;
  fit(): void;
  /** Draw the sidebar's hosts and load its widgets again. */
  refresh(): void;
}

type Form = "sidebar" | "chat";

let column: HTMLElement | null = null;
let mainBox: HTMLElement | null = null;
let hooks: ColumnHooks | null = null;
let root: HTMLElement | null = null;
let host: HTMLElement | null = null;
let titleButton: HTMLButtonElement | null = null;
let titleText: HTMLElement | null = null;
let caret: HTMLElement | null = null;
let statusLine: HTMLElement | null = null;
let listBox: HTMLElement | null = null;
let notesBox: HTMLElement | null = null;
let listEl: HTMLElement | null = null;
let form: Form = "sidebar";
let view: ChatView | null = null;
let current: string | null = null;
let listOpen = false;
let reading = false;
let making = false;
let opening: Promise<string | null> | null = null;
/** The delete button that the first press armed, or null. */
let armed: HTMLButtonElement | null = null;
/** The chats this form has shown, oldest first: each one that is not on screen is let go of when it is idle. */
const visited: string[] = [];
/** The most chats the sweep remembers. */
const MAX_VISITED = 64;
/** Numbers the names of the rows, so each delete button is described by its own row. */
let rowNumber = 0;

/** The id of the list region, named by the title button. */
const LIST_ID = "ai-side-chats";

/** A control of the form: a button the page's own keys leave alone. */
function control(cls: string, parent: HTMLElement, text: string): HTMLButtonElement {
  const button = el("button", cls, parent);
  button.type = "button";
  button.dataset.field = "";
  button.textContent = text;
  return button;
}

/** The pane the chat acts for when the chat module's own rule has none: the focused pane. */
const focusedPane = (): number | null => state.view?.focus ?? null;

/** Make the form beside the sidebar's sections. Nothing is asked of the core. */
export async function initAiSidebar(box: HTMLElement, main: HTMLElement, handed: ColumnHooks): Promise<void> {
  column = box;
  mainBox = main;
  hooks = handed;
  try {
    await loadStyles("./aisidebar.css");
  } catch {
    // the form still works without its style sheet
  }
  box.dataset.form = "sidebar";
  root = el("div", "sb-ai", box);
  root.hidden = true;

  const head = el("div", "ai-side-head sb-head label", root);
  el("span", "", head).textContent = "✦ AI chat";
  const back = control("ai-side-back", head, "◂ Sidebar");
  back.addEventListener("click", () => {
    setForm("sidebar");
    focusInput();
  });

  const bar = el("div", "ai-side-bar", root);
  titleButton = control("ai-side-title", bar, "");
  titleButton.setAttribute("aria-expanded", "false");
  titleButton.setAttribute("aria-controls", LIST_ID);
  titleText = el("span", "ai-side-text", titleButton);
  titleText.textContent = headTitle("");
  caret = el("span", "ai-side-caret", titleButton);
  caret.textContent = "▾";
  titleButton.addEventListener("click", () => void toggleList());
  const fresh = control("ai-side-new", bar, "＋ New chat");
  fresh.addEventListener("click", () => void pressNew());

  statusLine = el("p", "ai-side-status", root);
  statusLine.setAttribute("role", "status");
  statusLine.hidden = true;

  listBox = el("div", "ai-side-chats", root);
  listBox.id = LIST_ID;
  listBox.hidden = true;
  notesBox = el("div", "ai-side-notes", listBox);
  listEl = el("ul", "ai-side-list", listBox);
  listEl.setAttribute("aria-label", "Chats");
  listBox.addEventListener("keydown", onListKey);

  for (const node of [back, titleButton, fresh]) {
    node.addEventListener("keydown", (e) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      focusInput();
    });
  }
  registerAction({
    id: "ai.sidebar",
    title: "AI chat in the sidebar",
    run: () => {
      void openSidebarChat(false).then((sentence) => {
        if (sentence !== null) showNotice(sentence);
      });
    },
  });
  // The title button follows the title the chat module keeps for the chat on screen.
  onOverlayRender(() => {
    if (form !== "chat" || titleText === null || current === null) return;
    setText(titleText, headTitle(chatTitles.get(current) ?? ""));
  });
}

/** Show the form `next`: the two forms take turns inside the column. */
function setForm(next: Form): void {
  if (column === null || mainBox === null || root === null || hooks === null || form === next) return;
  form = next;
  column.dataset.form = next;
  mainBox.hidden = next === "chat";
  root.hidden = next !== "chat";
  column.setAttribute("aria-label", next === "chat" ? "AI chat" : "Sidebar");
  if (next === "sidebar") {
    // The chat module keeps the session, so coming back costs one snapshot.
    closeList();
    say("", "note");
    view?.dispose();
    view = null;
    host?.remove();
    host = null;
    if (hooks.shown()) hooks.refresh();
  }
  hooks.fit();
  renderOverlay();
}

/** Show `id` in the chat host: mount the chat module the first time, else switch its view. */
function showChat(id: string): void {
  if (root === null) return;
  if (view === null) {
    // The block classes put the chat's buttons and rules to work inside the column: its style sheets are written for a block
    // root (`.block button` is the reset the chat's controls start from). The host exists only while the chat is mounted.
    host = el("div", "block block-chat side-chat", root);
    view = mountChat(host, id, { pane: focusedPane });
  } else if (view.conversation !== id) view.show(id);
  current = id;
  if (titleText !== null) setText(titleText, headTitle(chatTitles.get(id) ?? ""));
  sweep(id);
}

/**
 * Remember that `id` was shown, and let go of every other chat this form showed that is idle (`releaseChat` keeps a chat that
 * replies, waits for a decision or holds a draft). A switch, a new chat, a delete and the opening of the list all end here.
 */
function sweep(on: string): void {
  const at = visited.indexOf(on);
  if (at >= 0) visited.splice(at, 1);
  visited.push(on);
  while (visited.length > MAX_VISITED) visited.shift();
  for (const id of [...visited]) {
    if (id !== on && releaseChat(id)) visited.splice(visited.indexOf(id), 1);
  }
}

async function open(newChat: boolean): Promise<string | null> {
  if (column === null || hooks === null) return "the sidebar is not started";
  let id = current;
  if (id === null || newChat) {
    let made: unknown;
    try {
      made = await call<unknown>("ai_conversation_new");
    } catch (e) {
      return faultText(e);
    }
    if (!isChatId(made)) return "the chat could not be started";
    id = made;
  }
  // The form first, then the column: the column never draws the sidebar's hosts or reads its widgets on the way.
  setForm("chat");
  hooks.show();
  showChat(id);
  view?.focus();
  return null;
}

/**
 * Show the column in the AI form on the current chat (a new one when `newChat` or none exists) and give the composer the
 * keyboard. Answers null when done, else one sentence. One call at a time: a second one while a chat is being made gets the
 * first one's answer.
 */
export function openSidebarChat(newChat: boolean): Promise<string | null> {
  if (opening !== null) return opening;
  const running = open(newChat).finally(() => {
    opening = null;
  });
  opening = running;
  return running;
}

/**
 * The column shows or hides. It is told before the column is hidden, so the keyboard goes back to the terminal while the focus
 * is still inside it (a hidden control keeps no focus), and the title-bar button follows.
 */
export function columnChanged(shown: boolean): void {
  if (column === null) return;
  if (!shown && column.contains(document.activeElement)) focusInput();
  renderOverlay();
}

/** Whether the column is in the AI form (it may be hidden). */
export function aiFormShown(): boolean {
  return form === "chat";
}

/** The chat the form shows; null while it shows the sidebar or nothing. */
export function sidebarConversation(): string | null {
  return form === "chat" ? current : null;
}

/** What the chat module's title-bar button reaches the column through. */
export const sidebarChat: SidebarHandle = {
  showing: () => form === "chat" && (hooks?.shown() ?? false),
  columnShown: () => hooks?.shown() ?? false,
  open: () => openSidebarChat(false),
  conversation: sidebarConversation,
  reveal(conversation: string): boolean {
    // Only the chat the column holds is brought back: a call that waits in it must reach the person in either form, hidden or not.
    if (hooks === null || current === null || current !== conversation) return false;
    setForm("chat");
    hooks.show();
    showChat(conversation);
    view?.focus();
    return true;
  },
};

// ---- the list ----------------------------------------------------------------------------------------------------------

/** Say one sentence about the list under the head, where it is seen with the list closed; an empty text clears it. */
function say(text: string, kind: "note" | "error"): void {
  if (statusLine === null) return;
  setText(statusLine, text);
  statusLine.hidden = text === "";
  if (text === "") statusLine.removeAttribute("data-kind");
  else statusLine.dataset.kind = kind;
}

function disarm(): void {
  const button = armed;
  if (button === null) return;
  armed = null;
  button.removeAttribute("data-armed");
  setText(button, armWords(false));
  document.removeEventListener("pointerdown", onOutsidePress, true);
  document.removeEventListener("keydown", onOutsideKey, true);
}

/** The first press on a delete button: it says `Delete?` and waits for a second press on the same button. */
function arm(button: HTMLButtonElement): void {
  disarm();
  armed = button;
  button.dataset.armed = "true";
  setText(button, armWords(true));
  document.addEventListener("pointerdown", onOutsidePress, true);
  document.addEventListener("keydown", onOutsideKey, true);
}

/** A press anywhere but on the armed button disarms it. */
function onOutsidePress(e: Event): void {
  if (armed !== null && e.target !== armed) disarm();
}

/** A key anywhere but on the armed button disarms it; `Escape` inside the list is the list's own and disarms first. */
function onOutsideKey(e: Event): void {
  if (armed === null) return;
  const key = (e as KeyboardEvent).key;
  const inList = listBox !== null && e.target instanceof Node && listBox.contains(e.target);
  if (key === "Escape" && inList) return;
  if (e.target === armed && (key === "Enter" || key === " ")) return;
  disarm();
}

function closeList(): void {
  disarm();
  listOpen = false;
  if (listBox !== null) listBox.hidden = true;
  titleButton?.setAttribute("aria-expanded", "false");
  if (caret !== null) setText(caret, "▾");
}

/** The pane of this window that shows each chat, from the core's list of blocks. */
function chatPanes(): Map<string, number> {
  const panes = new Map<string, number>();
  for (const block of paneBlocks.values()) if (block.kind === "chat") panes.set(block.path, block.pane);
  return panes;
}

async function toggleList(): Promise<void> {
  say("", "note");
  if (listOpen) {
    closeList();
    return;
  }
  if (reading || current === null) return;
  reading = true;
  let raw: unknown = null;
  let sentence = "";
  try {
    raw = await call<unknown>("ai_conversation_list");
  } catch (e) {
    sentence = faultText(e);
  } finally {
    reading = false;
  }
  if (form !== "chat" || current === null || listBox === null) return;
  const understood = typeof raw === "object" && raw !== null && !Array.isArray(raw);
  const list = understood ? readChatList(raw) : { chats: [], notes: [] };
  if (!understood && sentence === "") sentence = LIST_NOTES.unreadable;
  say(sentence, "error");
  drawList(list.notes, listRows(list.chats, current, chatTitles.get(current) ?? "", chatPanes(), Math.floor(Date.now() / 1000)));
  disarm();
  listOpen = true;
  listBox.hidden = false;
  titleButton?.setAttribute("aria-expanded", "true");
  if (caret !== null) setText(caret, "▴");
  sweep(current);
}

function drawList(notes: readonly string[], rows: readonly RowView[]): void {
  if (listEl === null || notesBox === null) return;
  notesBox.replaceChildren();
  for (const note of notes) el("p", "ai-side-note", notesBox).textContent = note;
  listEl.replaceChildren();
  for (const row of rows) {
    const item = el("li", "ai-side-row", listEl);
    item.dataset.id = row.id;
    item.dataset.current = row.current ? "true" : "false";
    if (row.pane !== null) item.dataset.pane = String(row.pane);
    const pick = control("ai-side-pick", item, "");
    if (row.current) pick.setAttribute("aria-current", "true");
    el("span", "ai-side-mark", pick).textContent = row.mark;
    const name = el("span", "ai-side-name", pick);
    name.textContent = row.title;
    name.id = `ai-side-name-${++rowNumber}`;
    el("span", "ai-side-meta", pick).textContent = row.meta;
    pick.addEventListener("click", () => void choose(row));
    if (row.canDelete) {
      const del = control("ai-side-del", item, armWords(false));
      del.setAttribute("aria-label", "Delete");
      del.setAttribute("aria-describedby", name.id);
      del.addEventListener("click", () => {
        say("", "note");
        if (armed === del) void remove(row.id);
        else arm(del);
      });
    }
  }
}

/** A press on the pick button of a row: show that chat, or focus the pane that shows it. */
async function choose(row: RowView): Promise<void> {
  say("", "note");
  if (row.pane !== null) {
    const pane = row.pane;
    if (!(state.view?.panes.some(([id]) => id === pane) ?? false)) {
      disarm();
      say(LIST_NOTES.otherTab, "note");
      return;
    }
    try {
      applyView(await call<View>("focus_pane", { pane }));
    } catch (e) {
      say(faultText(e), "error");
      return;
    }
    closeList();
    return;
  }
  closeList();
  if (row.id !== current) showChat(row.id);
  view?.focus();
}

/** The second press on an armed delete button: the core deletes the chat with its file and stops a reply of it. */
async function remove(id: string): Promise<void> {
  disarm();
  try {
    await call("ai_conversation_delete", { conversation: id });
  } catch (e) {
    say(faultText(e), "error");
    return;
  }
  listEl?.querySelector(`li.ai-side-row[data-id="${id}"]`)?.remove();
  say(LIST_NOTES.deleted, "note");
  const wasOnScreen = id === current;
  if (wasOnScreen) {
    // The chat on screen is gone: a new empty one takes its place, and the list is read again when it is next opened.
    closeList();
    const sentence = await openSidebarChat(true);
    if (sentence !== null) say(sentence, "error");
  }
  const at = visited.indexOf(id);
  if (at >= 0) visited.splice(at, 1);
  // The core has stopped a reply and written nothing back; the page forgets the session, and later events of the chat find none.
  chatClosed(id);
}

async function pressNew(): Promise<void> {
  if (making) return;
  making = true;
  try {
    say("", "note");
    const sentence = await openSidebarChat(true);
    if (sentence !== null) {
      say(sentence, "error");
      return;
    }
    closeList();
  } finally {
    making = false;
  }
}

/** The keys of the list: the arrows move between the rows, `Escape` disarms first and then closes. */
function onListKey(e: KeyboardEvent): void {
  if (listBox === null) return;
  if (e.key === "Escape") {
    e.preventDefault();
    e.stopPropagation();
    if (armed !== null) {
      disarm();
      return;
    }
    closeList();
    titleButton?.focus({ preventScroll: true });
    return;
  }
  if (e.key !== "ArrowDown" && e.key !== "ArrowUp" && e.key !== "Home" && e.key !== "End") return;
  const picks = Array.from(listBox.querySelectorAll<HTMLButtonElement>("button.ai-side-pick"));
  if (picks.length === 0) return;
  e.preventDefault();
  const here = e.target instanceof Element ? e.target.closest("li.ai-side-row")?.querySelector("button.ai-side-pick") : null;
  const at = picks.findIndex((p) => p === here);
  const to =
    e.key === "Home" ? 0 : e.key === "End" ? picks.length - 1 : Math.min(picks.length - 1, Math.max(0, at + (e.key === "ArrowDown" ? 1 : -1)));
  picks[to]?.focus({ preventScroll: true });
}
