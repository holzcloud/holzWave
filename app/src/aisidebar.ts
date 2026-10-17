// The second form of the sidebar column (docs/design.md section 6.18): the AI chat that takes the place of the sidebar in the
// same column, with the list of stored conversations in it. Every string goes in with `textContent`; a stored title, a model
// name or a note of the core is text and nothing else. The chat in the column is the module of the chat pane (`mountChat`),
// fed by the same events, so the consent card, the approval cards and the acting-pane rule are the pane chat's own; nothing
// here decides or grants anything and the column starts no turn and sends no text of its own. No command is called before a
// press: not at launch, not when the column opens as the sidebar, not when the chat mounts or redraws.
import { registerAction } from "./actions.js";
import { call } from "./bridge.js";
import { chatTitles } from "./blockstate.js";
import { mountChat } from "./chat.js";
import type { ChatView } from "./chat.js";
import { isChatId } from "./chatstate.js";
import { faultText } from "./chatview.js";
import { headTitle, listRows, readChatList } from "./chatlist.js";
import type { RowView } from "./chatlist.js";
import { el, loadStyles, setText } from "./dom.js";
import { focusInput } from "./keyboard.js";
import { onOverlayRender, renderOverlay } from "./overlay.js";
import { state } from "./state.js";
import { showNotice } from "./status.js";

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
let listBox: HTMLElement | null = null;
let listEl: HTMLElement | null = null;
let form: Form = "sidebar";
let view: ChatView | null = null;
let current: string | null = null;
let listOpen = false;
let reading = false;
let opening: Promise<string | null> | null = null;

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

  listBox = el("div", "ai-side-chats", root);
  listBox.id = LIST_ID;
  listBox.hidden = true;
  listEl = el("ul", "ai-side-list", listBox);
  listEl.setAttribute("aria-label", "Chats");

  // The block classes put the chat's buttons and rules to work inside the column: its style sheets are written for a
  // block root (`.block button` is the reset the chat's controls start from).
  host = el("div", "block block-chat side-chat", root);

  for (const node of [back, titleButton]) {
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
    view?.dispose();
    view = null;
    if (hooks.shown()) hooks.refresh();
  }
  hooks.fit();
  renderOverlay();
}

/** Show `id` in the chat host: mount the chat module the first time, else switch its view. */
function showChat(id: string): void {
  if (host === null) return;
  if (view === null) view = mountChat(host, id, { pane: focusedPane });
  else if (view.conversation !== id) view.show(id);
  current = id;
  if (titleText !== null) setText(titleText, headTitle(chatTitles.get(id) ?? ""));
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

/** The column shows or hides. */
export function columnChanged(_shown: boolean): void {
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

// ---- the list ----------------------------------------------------------------------------------------------------------

function closeList(): void {
  listOpen = false;
  if (listBox !== null) listBox.hidden = true;
  titleButton?.setAttribute("aria-expanded", "false");
  if (caret !== null) setText(caret, "▾");
}

async function toggleList(): Promise<void> {
  if (listOpen) {
    closeList();
    return;
  }
  if (reading || current === null) return;
  reading = true;
  let raw: unknown = null;
  try {
    raw = await call<unknown>("ai_conversation_list");
  } catch {
    raw = null;
  } finally {
    reading = false;
  }
  if (form !== "chat" || current === null || listBox === null) return;
  const chats = typeof raw === "object" && raw !== null ? readChatList(raw).chats : [];
  drawList(listRows(chats, current, chatTitles.get(current) ?? "", new Map(), Math.floor(Date.now() / 1000)));
  listOpen = true;
  listBox.hidden = false;
  titleButton?.setAttribute("aria-expanded", "true");
  if (caret !== null) setText(caret, "▴");
}

function drawList(rows: readonly RowView[]): void {
  if (listEl === null) return;
  listEl.replaceChildren();
  for (const row of rows) {
    const item = el("li", "ai-side-row", listEl);
    item.dataset.id = row.id;
    item.dataset.current = row.current ? "true" : "false";
    if (row.pane !== null) item.dataset.pane = String(row.pane);
    const pick = control("ai-side-pick", item, "");
    if (row.current) pick.setAttribute("aria-current", "true");
    el("span", "ai-side-mark", pick).textContent = row.mark;
    el("span", "ai-side-name", pick).textContent = row.title;
    el("span", "ai-side-meta", pick).textContent = row.meta;
    pick.addEventListener("click", () => choose(row));
  }
}

/** A press on the pick button of a row. */
function choose(row: RowView): void {
  closeList();
  if (row.id !== current) showChat(row.id);
  view?.focus();
}
