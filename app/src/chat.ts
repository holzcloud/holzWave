// The AI chat as a pane (docs/design.md section 6.12): sessions over the page mirror of the core's chat (`ChatFeed`), the
// transcript, the composer, the provider chip and picker, the title-bar button `✦ AI` and the actions that open a chat.
// The core owns the chat; this module draws what the core's events and snapshots say, and sends the typed text with
// `ai_send`, nothing else.
//
// Every string goes onto the screen with `textContent` or a text node: model text and user text are never markup here.
// The one exception is a finished assistant reply, which the core renders with the sanitiser the previews use
// (`markdown_render`) and which the page copies a second time through its own allow-list (`appendSafe`), so a broken
// sanitiser still cannot put a script, a handler, an image or a `javascript:` link on the page. No image is ever made. A
// link opens in the system browser by a click only, and one whose address carries data is text followed by the address.
// Nothing in this module decides, grants or approves anything; the parts of a message that are not text (tool calls,
// context, pending approvals) draw nothing until the plans that build them. Where a request goes is told by the core's
// route and the profile list, never guessed; the profile list is read only when the user acts (the composer gets the
// keyboard, the picker opens, a send needs it), so a chat restored at start-up reads no keychain. This module never
// imports `blocks.ts`: the opener of a block is handed to `initChat`, and `blocks.ts` mounts a chat pane through
// `mountChat` and tells `chatClosed` when a chat pane left the core's list.
import { registerAction } from "./actions.js";
import { onProfilesChanged, openProviders } from "./aisettings.js";
import { reviewIds } from "./aistate.js";
import { call, listen } from "./bridge.js";
import { paneBlocks, setChatTitle } from "./blockstate.js";
import { ChatFeed, isChatId, messageText, readChatEvent, readChatState } from "./chatstate.js";
import type { ChatMessage, Route } from "./chatstate.js";
import {
  MAX_TEXT_BYTES, chipView, endNote, faultText, linkKind, outcomeWord, pickProfile, readProfiles, roleWord, setupLabel,
  statusLabel, statusWord, titleWords, tokenWords, usageWords, utf8Bytes,
} from "./chatview.js";
import type { ChipView, EndNote, ProfileChoice } from "./chatview.js";
import { el, loadStyles } from "./dom.js";
import { focusInput } from "./keyboard.js";
import { onOverlayRender, renderOverlay } from "./overlay.js";
import { appendSafe } from "./safehtml.js";
import { applyView } from "./scene.js";
import { state } from "./state.js";
import { showNotice } from "./status.js";
import type { View } from "./types.js";

/** Opens a block pane; `blocks.ts` hands its `openBlock` to `initChat`. Answers null when done, else one sentence. */
export type BlockOpener = (
  kind: "chat",
  source: "local",
  path: string,
  placement: "tab" | "right" | "down",
) => Promise<string | null>;

/** A chat drawn in a pane (or, later, in the sidebar). */
export interface ChatView {
  readonly conversation: string;
  /** Give the keyboard to the composer. */
  focus(): void;
  /** Show another conversation in this view. */
  show(conversation: string): void;
  /** Remove the view's nodes and listeners; the session stays, so a tab switch costs nothing. */
  dispose(): void;
}

/** The most snapshot requests in a row for one hole: a snapshot older than the hole asks again, up to this many times. */
const MAX_SNAPSHOTS = 3;
/** A scroll position this close to the bottom (in pixels) counts as being at the bottom. */
const BOTTOM_SLACK = 24;
/** An Enter this soon after an input method committed text is the commit's, not a send. */
const ENTER_AFTER_COMPOSITION_MS = 80;
/** The keys that the composer shows, as the design says. */
const HINT = "⏎ send · ⇧⏎ new line · Esc stop";
/** How many rendered replies are kept, so a redraw, a snapshot or a tab switch never asks the core again. */
const MARKDOWN_KEPT = 64;
/** The most characters of rendered replies that are kept together. */
const MARKDOWN_BUDGET = 8 * 1024 * 1024;
/** The longest rendered reply the page copies; a longer answer leaves the reply as plain text. */
const MAX_RENDERED = 8 * 1024 * 1024;
/** The longest address written after the text of a link that is not one, in characters. */
const SHOWN_HREF = 300;

/** One line under the transcript: a refusal or a hint of the page (`error`, `note`). */
interface Notice {
  text: string;
  kind: "error" | "note";
}

/** One chat of this window: the feed that mirrors the core, what the user has chosen and typed, and the views that show it. */
interface Session {
  readonly id: string;
  readonly feed: ChatFeed;
  readonly views: Set<Mounted>;
  /** The route of the latest turn this window has seen start. */
  route: Route | null;
  routeTurn: number;
  /** The profiles the core listed when the user last acted; null until it is read. */
  profiles: ProfileChoice[] | null;
  /** A read of the profile list is on its way. */
  loadingProfiles: Promise<void> | null;
  /** The profile the user chose for this chat. */
  choice: string | null;
  draft: string;
  /** A snapshot request is on its way. */
  fetching: boolean;
  /** A send is on its way. */
  sending: boolean;
  /** The refusal or hint under the transcript; cleared when the user types. */
  notice: Notice | null;
  /** Stop was pressed and the turn has not ended yet. */
  stopping: boolean;
  /** The turn Stop was pressed for, so one turn is cancelled once; -1 for none. */
  cancelled: number;
  /** A turn started in this window, so its outcome may be said; cleared words stay cleared until the next turn. */
  sawTurn: boolean;
  dismissed: boolean;
}

/** The rendered HTML of finished replies by message id, with the text it was made from; the oldest goes first. */
const rendered = new Map<string, { text: string; html: string }>();
let renderedChars = 0;

/** Keep the rendered `html` of the reply `id` made from `text`, within the count and the budget of the cache. */
function keepRendered(id: string, text: string, html: string): void {
  const before = rendered.get(id);
  if (before !== undefined) {
    renderedChars -= before.html.length;
    rendered.delete(id);
  }
  rendered.set(id, { text, html });
  renderedChars += html.length;
  for (const [oldest, entry] of rendered) {
    if (rendered.size <= MARKDOWN_KEPT && renderedChars <= MARKDOWN_BUDGET) break;
    if (oldest === id) continue;
    renderedChars -= entry.html.length;
    rendered.delete(oldest);
  }
}

const sessions = new Map<string, Session>();
/** The chats the user just opened: their composer takes the keyboard when it is mounted. */
const wantFocus = new Set<string>();
const dirty = new Set<Session>();
let frame = 0;
let opener: BlockOpener | null = null;
let button: HTMLButtonElement | null = null;
/** The profile chosen last in this window: a new chat starts with it. */
let lastChoice: string | null = null;
/** The providers that wait for the user's review (a gateway or a server of the user's own that was not allowed yet). */
let review = new Set<string>();

// ---- sessions and the core's events -----------------------------------------------------------------------------------

function sessionFor(conversation: string): Session {
  let s = sessions.get(conversation);
  if (!s) {
    s = {
      id: conversation,
      feed: new ChatFeed(conversation),
      views: new Set(),
      route: null,
      routeTurn: 0,
      profiles: null,
      loadingProfiles: null,
      choice: lastChoice,
      draft: "",
      fetching: false,
      sending: false,
      notice: null,
      stopping: false,
      cancelled: -1,
      sawTurn: false,
      dismissed: false,
    };
    sessions.set(conversation, s);
  }
  return s;
}

/** Draw every view of `s` on the next animation frame; any number of changes in between are one draw. */
function schedule(s: Session): void {
  dirty.add(s);
  if (frame !== 0) return;
  frame = requestAnimationFrame(() => {
    frame = 0;
    const now = [...dirty];
    dirty.clear();
    for (const session of now) {
      if (setChatTitle(session.id, session.feed.state.title)) renderOverlay();
      for (const view of session.views) view.draw();
    }
  });
}

/** An `ai` event of the core: one feed per chat; a gap asks for one snapshot. */
function onAi(raw: unknown): void {
  const event = readChatEvent(raw);
  if (event === null) return;
  const s = sessions.get(event.conversation);
  if (s === undefined) return;
  if (event.kind === "turn_started") {
    if (event.turn >= s.routeTurn) {
      s.route = event.route;
      s.routeTurn = event.turn;
    }
    s.sawTurn = true;
    s.dismissed = false;
    s.stopping = false;
    s.notice = null;
  }
  const result = s.feed.receive(event);
  if (result === "applied") schedule(s);
  else if (result === "snapshot") void fetchSnapshot(s);
}

/**
 * Ask the core for the state of a chat and hand it to the feed. A snapshot older than the hole asks again, at most
 * three times in a row, then the feed stops waiting. One request is on its way at a time.
 */
async function fetchSnapshot(s: Session): Promise<void> {
  if (s.fetching) return;
  s.fetching = true;
  try {
    for (let round = 0; round < MAX_SNAPSHOTS; round++) {
      let snapshot = null;
      try {
        snapshot = readChatState(await call<unknown>("ai_turn_snapshot", { conversation: s.id }));
      } catch {
        // a rejected request is handled below like an unreadable answer
      }
      if (snapshot === null) {
        s.feed.failed();
        schedule(s);
        return;
      }
      if (s.feed.install(snapshot) !== "again") {
        schedule(s);
        return;
      }
    }
    s.feed.failed();
    schedule(s);
  } finally {
    s.fetching = false;
  }
}

/**
 * Keep the profiles an answer of `ai_profiles` lists. The session that asked takes them (`all` false); the providers dialog
 * hands every answer it reads to every session (`all` true), so the chip, the picker and the button follow a saved
 * provider without a reload.
 */
function takeProfiles(raw: unknown, all: boolean, asking: Session | null): void {
  const list = readProfiles(raw);
  // The providers whose address waits for the user's review: replaced by every answer, so the guard and the button follow.
  review = new Set(reviewIds(raw));
  const targets = all ? [...sessions.values()] : asking === null ? [] : [asking];
  for (const t of targets) {
    t.profiles = list;
    schedule(t);
  }
}

/** The profiles the core lists. Called when the user acts, never when a chat is mounted. */
function loadProfiles(s: Session): Promise<void> {
  if (s.loadingProfiles !== null) return s.loadingProfiles;
  const reading = call("ai_profiles")
    .then((raw: unknown) => {
      takeProfiles(raw, false, s);
    })
    .finally(() => {
      s.loadingProfiles = null;
    });
  s.loadingProfiles = reading;
  return reading;
}

/** The line under the transcript for `s`: the page's own notice, else what the running turn does, else how it ended. */
function statusFor(s: Session): { text: string; kind: "error" | "note" | "" } {
  if (s.notice !== null) return s.notice;
  const st = s.feed.state;
  if (st.running) return { text: s.stopping ? "stopping" : statusWord(st), kind: "" };
  s.stopping = false;
  if (s.sawTurn && !s.dismissed && st.ended !== null) return { text: outcomeWord(st.ended.end), kind: "" };
  return { text: "", kind: "" };
}

// ---- the transcript and the composer ----------------------------------------------------------------------------------

/** One message drawn: its node and the plain text it shows. */
interface Article {
  node: HTMLElement;
  who: HTMLElement;
  text: HTMLElement;
  /** The plain text drawn now; null before the first draw. */
  plain: string | null;
  /** The word after the role that is drawn now. */
  label: string | null;
  /** `none`: plain text, not asked yet; `asked`: the core renders it; `done`: nothing more to do (rendered or kept plain). */
  markdown: "none" | "asked" | "done";
}

/** Whether `parent` holds exactly `nodes`, in order. */
function holds(parent: HTMLElement, nodes: readonly HTMLElement[]): boolean {
  if (parent.childElementCount !== nodes.length) return false;
  return nodes.every((n, i) => parent.children[i] === n);
}

/** Put the label row of a message right: the role, and after it the word of a message that did not complete. */
function drawWho(a: Article, role: string, label: string): void {
  if (a.label === label) return;
  a.label = label;
  const roleNode = el("span", "chat-role");
  roleNode.textContent = roleWord(role);
  a.who.replaceChildren(roleNode);
  if (label !== "") {
    const stat = el("span", "chat-stat");
    stat.textContent = label;
    a.who.append(document.createTextNode(" · "), stat);
  }
}

/**
 * Make every link of a rendered reply safe to show: one without an address is its text, one whose address carries data
 * is its text followed by the address in brackets, and the rest keep the address as a tooltip and the class `chat-link`.
 * A link never opens a window by itself: the click handler of the log opens it through the shell.
 */
function fixLinks(root: Element): void {
  for (const link of Array.from(root.querySelectorAll("a"))) {
    const href = link.getAttribute("href");
    if (href === null || href === "") {
      link.replaceWith(document.createTextNode(link.textContent ?? ""));
    } else if (linkKind(href) === "inert") {
      const shown = Array.from(href).slice(0, SHOWN_HREF).join("");
      link.replaceWith(document.createTextNode(`${link.textContent ?? ""} [${shown}]`));
    } else {
      link.removeAttribute("target");
      link.classList.add("chat-link");
      link.title = href;
    }
  }
}

/** The chat drawn into one host element. */
class Mounted implements ChatView {
  private session: Session;
  private readonly bar: HTMLElement;
  private chip: HTMLElement;
  private readonly where: HTMLElement;
  private readonly usage: HTMLElement;
  private readonly setup: HTMLButtonElement;
  private picker: HTMLElement | null = null;
  private pickerKey = "";
  private readonly log: HTMLElement;
  private readonly statusLine: HTMLElement;
  private readonly input: HTMLTextAreaElement;
  private readonly count: HTMLElement;
  private readonly sendButton: HTMLButtonElement;
  private readonly articles = new Map<string, Article>();
  private streaming: Article | null = null;
  private streamed: Text | null = null;
  private endRow: HTMLElement | null = null;
  private endKey = "";
  private composingEnded = Number.NEGATIVE_INFINITY;
  /** The text the field's height was measured for. */
  private sizedFor: string | null = null;
  private alive = true;
  private readonly onMouseDown: (e: MouseEvent) => void;

  constructor(
    private readonly host: HTMLElement,
    conversation: string,
    readonly pane: () => number | null,
  ) {
    this.session = sessionFor(conversation);
    this.bar = el("div", "chat-bar", host);
    this.chip = this.makeChip("span");
    this.where = el("span", "chat-where", this.bar);
    this.where.hidden = true;
    this.usage = el("span", "chat-usage", this.bar);
    this.setup = el("button", "chat-setup", this.bar);
    this.setup.type = "button";
    this.setup.dataset.field = "";
    this.setup.setAttribute("aria-haspopup", "dialog");
    this.setup.addEventListener("click", () => {
      // The dialog gives the keyboard back to this chat's composer when it closes.
      void openProviders().then(() => {
        if (this.alive) this.focus();
      });
    });
    this.log = el("div", "chat-log", host);
    this.log.setAttribute("role", "log");
    this.log.setAttribute("aria-label", "Conversation");
    this.log.setAttribute("aria-live", "off");
    this.log.tabIndex = 0;
    this.log.dataset.field = "";
    this.statusLine = el("div", "chat-status", host);
    this.statusLine.setAttribute("role", "status");
    const composer = el("div", "chat-composer", host);
    this.input = el("textarea", "chat-input", composer);
    this.input.dataset.field = "";
    this.input.rows = 1;
    this.input.setAttribute("aria-label", "Message");
    this.input.setAttribute("placeholder", "Ask a question");
    this.input.spellcheck = false;
    const foot = el("div", "chat-foot", composer);
    this.count = el("span", "chat-count", foot);
    el("span", "chat-hint", foot).textContent = HINT;
    this.sendButton = el("button", "chat-send", foot);
    this.sendButton.type = "button";
    this.sendButton.dataset.field = "";
    this.sendButton.dataset.mode = "send";
    this.sendButton.textContent = "Send";

    this.input.addEventListener("input", () => {
      const s = this.session;
      s.draft = this.input.value;
      s.notice = null;
      s.dismissed = true;
      this.drawStatus();
      this.drawComposer();
    });
    // The profile list is read when the user gets to type, not before.
    this.input.addEventListener("focus", () => {
      const s = this.session;
      if (s.profiles === null) void loadProfiles(s).catch(() => {});
    });
    this.input.addEventListener("compositionend", () => {
      this.composingEnded = performance.now();
    });
    this.input.addEventListener("keydown", (e) => this.onKey(e, true));
    this.log.addEventListener("keydown", (e) => this.onKey(e, false));
    this.sendButton.addEventListener("click", () => {
      if (this.session.feed.state.running) this.stop();
      else void this.submit();
    });
    // The page never follows a link by itself: a click on a safe link is opened by the shell in the system browser, any
    // other click or middle click on a link does nothing.
    this.log.addEventListener("click", (e) => {
      const link = e.target instanceof Element ? e.target.closest("a") : null;
      if (link === null) return;
      e.preventDefault();
      const url = link.classList.contains("chat-link") ? link.getAttribute("href") : null;
      if (url !== null && linkKind(url) === "open") void call("open_url", { url }).catch(() => {});
    });
    this.log.addEventListener("auxclick", (e) => {
      if (e.target instanceof Element && e.target.closest("a") !== null) e.preventDefault();
    });
    // A click on the pane's empty parts gives the keyboard to the composer, not to the terminal behind the pane.
    this.onMouseDown = (e: MouseEvent): void => {
      if (e.target instanceof Element && e.target.closest("[data-field]")) return;
      e.preventDefault();
      this.focus();
    };
    host.addEventListener("mousedown", this.onMouseDown);

    this.attach(this.session);
    if (wantFocus.delete(conversation)) this.focus();
  }

  get conversation(): string {
    return this.session.id;
  }

  focus(): void {
    this.input.focus({ preventScroll: true });
  }

  show(conversation: string): void {
    if (conversation === this.session.id) return;
    this.session.views.delete(this);
    this.closePicker(false);
    this.articles.clear();
    this.streaming = null;
    this.streamed = null;
    this.endRow = null;
    this.endKey = "";
    this.log.replaceChildren();
    this.session = sessionFor(conversation);
    this.attach(this.session);
  }

  dispose(): void {
    this.alive = false;
    this.session.views.delete(this);
    this.host.removeEventListener("mousedown", this.onMouseDown);
    this.host.replaceChildren();
  }

  /** Show `s` here: its draft, what it holds so far, and a fresh snapshot of the core's state. */
  private attach(s: Session): void {
    s.views.add(this);
    this.input.value = s.draft;
    this.draw();
    s.feed.awaitSnapshot();
    void fetchSnapshot(s);
  }

  // ---- drawing ----------------------------------------------------------------------------------------------------

  /** Bring everything in line with the feed's state. */
  draw(): void {
    if (!this.alive) return;
    this.drawBar();
    this.drawTranscript();
    this.drawStatus();
    this.drawComposer();
  }

  private makeChip(tag: "span" | "button"): HTMLElement {
    const chip = el(tag, "chat-chip");
    if (chip instanceof HTMLButtonElement) {
      chip.type = "button";
      chip.dataset.field = "";
      chip.setAttribute("aria-haspopup", "true");
      chip.addEventListener("click", () => void this.togglePicker());
    } else {
      chip.setAttribute("role", "group");
    }
    this.bar.prepend(chip);
    return chip;
  }

  private drawBar(): void {
    const s = this.session;
    const st = s.feed.state;
    const view: ChipView = chipView(s.route, s.profiles, { profile: st.profile, model: st.model }, s.choice);
    const tag = view.kind === "button" ? "button" : "span";
    if (this.chip.localName !== tag) {
      const next = this.makeChip(tag);
      const focused = document.activeElement === this.chip;
      this.chip.remove();
      this.chip = next;
      if (focused) next.focus({ preventScroll: true });
    }
    if (this.chip.textContent !== view.text) this.chip.textContent = view.text;
    this.chip.hidden = view.text === "";
    if (view.aria === "") this.chip.removeAttribute("aria-label");
    else this.chip.setAttribute("aria-label", view.aria);
    if (view.kind === "button") {
      this.chip.setAttribute("aria-expanded", this.picker !== null ? "true" : "false");
    } else {
      this.chip.removeAttribute("aria-expanded");
      this.closePicker(false);
    }
    this.where.hidden = view.where === "";
    if (this.where.textContent !== view.where) this.where.textContent = view.where;
    this.where.dataset.leaves = view.leaves ? "true" : "false";
    const used = usageWords(!st.running && st.ended !== null ? st.ended.usage : st.usage);
    if (this.usage.textContent !== used) this.usage.textContent = used;
    const way = setupLabel(s.profiles, review.size);
    if (this.setup.textContent !== way) this.setup.textContent = way;
    if (this.picker !== null) this.drawPicker();
  }

  private drawTranscript(): void {
    const s = this.session;
    const st = s.feed.state;
    const atBottom = this.log.scrollHeight - this.log.scrollTop - this.log.clientHeight <= BOTTOM_SLACK;

    const nodes: HTMLElement[] = [];
    const seen = new Set<string>();
    for (const m of st.messages) {
      if (m.role === "tool") continue;
      seen.add(m.id);
      let a = this.articles.get(m.id);
      if (a === undefined) {
        a = this.makeArticle(m);
        this.articles.set(m.id, a);
      }
      this.updateArticle(a, m);
      nodes.push(a.node);
    }
    for (const id of this.articles.keys()) {
      if (!seen.has(id)) this.articles.delete(id);
    }

    // The reply that is streaming has no message yet: it is plain text in a node of its own.
    if (st.running && (st.partial.text !== "" || st.partial.reasoning !== "")) {
      if (this.streaming === null || this.streamed === null) {
        const node = el("article", "chat-msg chat-partial");
        node.dataset.role = "assistant";
        node.dataset.status = "streaming";
        const who = el("div", "chat-who label", node);
        const text = el("div", "chat-text", node);
        this.streamed = document.createTextNode("");
        text.append(this.streamed);
        this.streaming = { node, who, text, plain: null, label: null, markdown: "done" };
      }
      drawWho(this.streaming, "assistant", statusWord(st));
      if (this.streamed.data !== st.partial.text) this.streamed.data = st.partial.text;
      nodes.push(this.streaming.node);
    } else {
      this.streaming = null;
      this.streamed = null;
    }

    const note = st.running ? null : endNote(st);
    if (note === null) {
      this.endRow = null;
      this.endKey = "";
    } else {
      const key = `${note.kind}|${note.label}|${note.text}`;
      if (this.endRow === null || this.endKey !== key) {
        this.endRow = this.makeEnd(note);
        this.endKey = key;
      }
      nodes.push(this.endRow);
    }

    if (!holds(this.log, nodes)) this.log.replaceChildren(...nodes);
    const title = titleWords(st.title);
    this.log.setAttribute("aria-label", title === "" ? "Conversation" : `Conversation: ${title}`);
    if (atBottom) this.log.scrollTop = this.log.scrollHeight;
  }

  private makeArticle(m: ChatMessage): Article {
    const node = el("article", "chat-msg");
    node.dataset.role = m.role;
    node.dataset.id = m.id;
    node.dataset.status = m.status;
    const who = el("div", "chat-who label", node);
    const text = el("div", "chat-text", node);
    return { node, who, text, plain: null, label: null, markdown: "none" };
  }

  private updateArticle(a: Article, m: ChatMessage): void {
    a.node.dataset.status = m.status;
    drawWho(a, m.role, statusLabel(m.status));
    const plain = messageText(m);
    if (a.plain !== plain) {
      a.plain = plain;
      a.markdown = "none";
      a.text.classList.remove("chat-md");
      a.text.textContent = plain;
    }
    // Only a finished reply of the assistant is Markdown: not a user's message, not one that is streaming, and not one
    // that stopped, failed or was cut.
    if (m.role === "assistant" && m.status === "complete" && plain !== "" && a.markdown === "none") this.render(a, m.id, plain);
  }

  /** Make the finished reply `text` Markdown: from the cache, or by asking the core once. */
  private render(a: Article, id: string, text: string): void {
    const kept = rendered.get(id);
    if (kept !== undefined && kept.text === text) {
      this.fill(a, kept.html);
      return;
    }
    a.markdown = "asked";
    call("markdown_render", { text })
      .then((answer: unknown) => {
        // A null answer (a text the renderer does not take), a long one and a reply that changed meanwhile keep the plain text.
        if (typeof answer !== "string" || answer.length > MAX_RENDERED) {
          if (a.plain === text) a.markdown = "done";
          return;
        }
        keepRendered(id, text, answer);
        if (this.alive && a.plain === text && a.markdown === "asked") this.fill(a, answer);
      })
      .catch(() => {
        if (a.plain === text) a.markdown = "done";
      });
  }

  /** Replace the plain text of a reply by the core's HTML, copied through the page's allow-list. */
  private fill(a: Article, html: string): void {
    a.markdown = "done";
    a.text.replaceChildren();
    appendSafe(html, a.text);
    a.text.classList.add("chat-md");
    fixLinks(a.text);
  }

  /** The row after the last message when a turn failed or was cut: a mark, the class in words and the core's line. */
  private makeEnd(note: EndNote): HTMLElement {
    const row = el("div", "chat-end");
    row.dataset.kind = note.kind;
    el("span", "chat-mark", row).textContent = "!";
    el("span", "chat-end-label label", row).textContent = note.label;
    if (note.text !== "") el("span", "chat-end-text", row).textContent = note.text;
    return row;
  }

  private drawStatus(): void {
    const line = statusFor(this.session);
    if (this.statusLine.textContent !== line.text) this.statusLine.textContent = line.text;
    if (line.kind === "") this.statusLine.removeAttribute("data-kind");
    else this.statusLine.dataset.kind = line.kind;
  }

  /** The count, the limit, the mode of the button and the height of the field. */
  private drawComposer(): void {
    const st = this.session.feed.state;
    const text = this.input.value;
    const bytes = utf8Bytes(text);
    const over = bytes > MAX_TEXT_BYTES;
    const words = over ? `too long · ${bytes} of ${MAX_TEXT_BYTES} bytes` : tokenWords(text);
    if (this.count.textContent !== words) this.count.textContent = words;
    if (over) this.count.dataset.over = "true";
    else this.count.removeAttribute("data-over");
    const mode = st.running ? "stop" : "send";
    if (this.sendButton.dataset.mode !== mode) {
      this.sendButton.dataset.mode = mode;
      this.sendButton.textContent = st.running ? "Stop" : "Send";
    }
    this.sendButton.disabled = !st.running && over;
    // Grow with the text; the style sheet stops it at eight rows. The height is measured again only for new text.
    if (this.sizedFor !== text) {
      this.sizedFor = text;
      this.input.style.height = "auto";
      const edge = this.input.offsetHeight - this.input.clientHeight;
      this.input.style.height = `${this.input.scrollHeight + edge}px`;
    }
  }

  // ---- the provider picker ----------------------------------------------------------------------------------------

  /** The profile the next send would use, for the checked row. */
  private chosenId(): string {
    const s = this.session;
    return pickProfile(s.profiles, s.choice, s.feed.state.profile).id;
  }

  private async togglePicker(): Promise<void> {
    if (this.picker !== null) this.closePicker(true);
    else await this.openPicker();
  }

  /** Open the picker under the bar; the list is read again, so a profile saved meanwhile is there. */
  private async openPicker(): Promise<void> {
    const s = this.session;
    if (this.picker !== null) return;
    try {
      await loadProfiles(s);
    } catch (e) {
      this.say(faultText(e), "error");
      return;
    }
    if (!this.alive || this.picker !== null || s.profiles === null || s.profiles.length < 2) return;
    const picker = el("div", "chat-picker");
    picker.setAttribute("role", "radiogroup");
    picker.setAttribute("aria-label", "Provider");
    picker.addEventListener("keydown", (e) => this.onPickerKey(e));
    picker.addEventListener("focusout", (e) => {
      const to = e.relatedTarget;
      if (to instanceof Node && (picker.contains(to) || this.chip.contains(to))) return;
      this.closePicker(false);
    });
    this.bar.after(picker);
    this.picker = picker;
    this.pickerKey = "";
    this.drawPicker();
    this.chip.setAttribute("aria-expanded", "true");
    const rows = this.rows();
    (rows.find((r) => r.getAttribute("aria-checked") === "true") ?? rows[0])?.focus({ preventScroll: true });
  }

  private rows(): HTMLElement[] {
    return this.picker === null ? [] : Array.from(this.picker.querySelectorAll<HTMLElement>(".chat-choice"));
  }

  /** Build the rows when the list changed, and mark the chosen one. */
  private drawPicker(): void {
    const picker = this.picker;
    const list = this.session.profiles;
    if (picker === null || list === null) return;
    const key = JSON.stringify(list);
    if (key !== this.pickerKey) {
      this.pickerKey = key;
      const focused = this.rows().findIndex((r) => r === document.activeElement);
      picker.replaceChildren();
      for (const p of list) {
        const row = el("button", "chat-choice", picker);
        row.type = "button";
        row.dataset.field = "";
        row.dataset.id = p.id;
        row.setAttribute("role", "radio");
        el("span", "chat-mark", row);
        el("span", "chat-choice-name", row).textContent = p.name === "" ? p.id : p.name;
        el("span", "chat-choice-model", row).textContent = p.model;
        el("span", "chat-choice-host", row).textContent = p.host;
        row.addEventListener("click", () => this.choose(p.id, true));
      }
      if (focused >= 0) this.rows()[Math.min(focused, list.length - 1)]?.focus({ preventScroll: true });
    }
    const chosen = this.chosenId();
    const rows = this.rows();
    const first = rows.find((r) => r.dataset.id === chosen) ?? rows[0];
    for (const row of rows) {
      const on = row.dataset.id === chosen;
      row.setAttribute("aria-checked", on ? "true" : "false");
      row.tabIndex = row === first ? 0 : -1;
      const mark = row.firstElementChild;
      if (mark !== null) mark.textContent = on ? "●" : "○";
    }
  }

  private closePicker(refocus: boolean): void {
    const picker = this.picker;
    if (picker === null) return;
    // Forgotten first: removing the focused row fires `focusout`, which must not close it a second time.
    this.picker = null;
    this.pickerKey = "";
    picker.remove();
    if (this.chip instanceof HTMLButtonElement) {
      this.chip.setAttribute("aria-expanded", "false");
      if (refocus) this.chip.focus({ preventScroll: true });
    }
  }

  /** The user chose a profile for this chat and for the chats opened next in this window. */
  private choose(id: string, close: boolean): void {
    const s = this.session;
    s.choice = id;
    lastChoice = id;
    this.drawBar();
    for (const view of s.views) if (view !== this) view.drawBar();
    if (close) this.closePicker(true);
  }

  private onPickerKey(e: KeyboardEvent): void {
    const rows = this.rows();
    const at = rows.findIndex((r) => r === document.activeElement);
    const step = (by: number): void => {
      e.preventDefault();
      const next = rows[(Math.max(at, 0) + by + rows.length) % rows.length];
      if (next === undefined) return;
      const id = next.dataset.id;
      if (id !== undefined) this.choose(id, false);
      next.focus({ preventScroll: true });
    };
    switch (e.key) {
      case "ArrowDown":
      case "ArrowRight":
        step(1);
        break;
      case "ArrowUp":
      case "ArrowLeft":
        step(-1);
        break;
      case "Enter":
      case " ": {
        e.preventDefault();
        const id = rows[at]?.dataset.id;
        if (id !== undefined) this.choose(id, true);
        else this.closePicker(true);
        break;
      }
      case "Escape":
        e.preventDefault();
        e.stopPropagation();
        this.closePicker(true);
        break;
      default:
    }
  }

  // ---- keys, sending and stopping ---------------------------------------------------------------------------------

  private onKey(e: KeyboardEvent, composer: boolean): void {
    if (e.isComposing) return;
    if (e.key === "Escape") {
      e.preventDefault();
      if (this.session.feed.state.running) this.stop();
      else focusInput();
      return;
    }
    if (!composer || e.key !== "Enter" || e.shiftKey) return;
    e.preventDefault();
    if (performance.now() - this.composingEnded < ENTER_AFTER_COMPOSITION_MS) return;
    void this.submit();
  }

  /** Send the typed text. Nothing but the text, the chat, the pane and the profile leaves. */
  private async submit(): Promise<void> {
    const s = this.session;
    const text = this.input.value.trim();
    if (text === "" || s.sending || utf8Bytes(text) > MAX_TEXT_BYTES) return;
    const pane = this.pane();
    if (pane === null) {
      this.say("This chat has no pane to act for.", "error");
      return;
    }
    s.sending = true;
    try {
      let pick = pickProfile(s.profiles, s.choice, s.feed.state.profile);
      if (pick.via === "unknown") {
        try {
          await loadProfiles(s);
        } catch (e) {
          this.say(faultText(e), "error");
          return;
        }
        pick = pickProfile(s.profiles, s.choice, s.feed.state.profile);
      }
      if (pick.via === "ask") {
        this.say("Choose a provider first.", "note");
        await this.openPicker();
        return;
      }
      // A provider that waits for its review is not sent to: the dialog opens on it and the draft stays. The core refuses
      // such a send itself; this only saves the round trip and says why.
      if (review.has(pick.id)) {
        this.say("Review this provider first.", "error");
        void openProviders(pick.id).then(() => {
          if (this.alive) this.focus();
        });
        return;
      }
      try {
        await call("ai_send", {
          conversation: s.id,
          text,
          pane,
          profile: pick.id === "" ? null : pick.id,
          model: null,
        });
      } catch (e) {
        this.say(faultText(e), "error");
        return;
      }
      s.draft = "";
      s.notice = null;
      for (const view of s.views) {
        view.input.value = "";
        view.drawComposer();
      }
      s.feed.awaitSnapshot();
      void fetchSnapshot(s);
      schedule(s);
    } finally {
      s.sending = false;
    }
  }

  /** Stop the running reply: one call per turn, and the status says `stopping` at once. */
  private stop(): void {
    const s = this.session;
    const st = s.feed.state;
    if (!st.running || s.cancelled === st.turn) return;
    s.cancelled = st.turn;
    s.stopping = true;
    for (const view of s.views) view.drawStatus();
    call("ai_cancel", { conversation: s.id }).catch((e: unknown) => {
      s.cancelled = -1;
      this.say(faultText(e), "error");
    });
    this.focus();
  }

  /** Say one line under the transcript of every view of this chat, now. */
  private say(text: string, kind: "error" | "note"): void {
    const s = this.session;
    s.notice = { text, kind };
    s.stopping = false;
    for (const view of s.views) view.drawStatus();
  }
}

// ---- the public functions ---------------------------------------------------------------------------------------------

/**
 * Draw the chat `conversation` into `host`. `pane` names the pane the chat acts for: the block's own pane for a chat
 * pane, the focused terminal pane for a chat in the sidebar later.
 */
export function mountChat(host: HTMLElement, conversation: string, options: { pane: () => number | null }): ChatView {
  return new Mounted(host, conversation, options.pane);
}

/** The pane of the shown tab that holds a chat, the focused one first. */
function shownChatPane(): number | null {
  const view = state.view;
  if (!view) return null;
  const chats = view.panes.map(([id]) => id).filter((id) => paneBlocks.get(id)?.kind === "chat");
  if (chats.includes(view.focus)) return view.focus;
  return chats[0] ?? null;
}

/**
 * Open the chat: without `newChat`, focus the chat pane of the shown tab when there is one; otherwise make a new
 * conversation and open it as a pane to the right of the focused pane. Answers null when done, else one sentence.
 */
export async function openChat(newChat: boolean): Promise<string | null> {
  if (opener === null) return "the chat is not started";
  if (!newChat) {
    const pane = shownChatPane();
    if (pane !== null) {
      try {
        if (state.view?.focus !== pane) applyView(await call<View>("focus_pane", { pane }));
      } catch (e) {
        return faultText(e);
      }
      for (const s of sessions.values()) {
        for (const view of s.views) if (view.pane() === pane) view.focus();
      }
      return null;
    }
  }
  let id: unknown;
  try {
    id = await call<unknown>("ai_conversation_new");
  } catch (e) {
    return faultText(e);
  }
  if (!isChatId(id)) return "the chat could not be started";
  wantFocus.add(id);
  const reason = await opener("chat", "local", id, "right");
  if (reason !== null) wantFocus.delete(id);
  return reason;
}

/**
 * The pane of a chat left the core's list: stop its reply when one runs (the core keeps the text so far as a stopped
 * message) and forget its session and its title. A pane that is only hidden by a tab switch is still in the list and
 * never comes here.
 */
export function chatClosed(conversation: string): void {
  const s = sessions.get(conversation);
  if (s !== undefined) {
    dirty.delete(s);
    if (s.feed.state.running) void call("ai_cancel", { conversation }).catch(() => {});
  }
  sessions.delete(conversation);
  wantFocus.delete(conversation);
  setChatTitle(conversation, "");
}

/** Make the button, register the two actions and listen to the core's `ai` events. Call once at start-up. */
export async function initChat(open: BlockOpener): Promise<void> {
  opener = open;
  try {
    await loadStyles("./chat.css");
  } catch {
    // the chat still works without its style sheet
  }
  const run = (newChat: boolean): void => {
    void openChat(newChat).then((reason) => {
      if (reason !== null) showNotice(reason);
    });
  };
  const info = document.querySelector(".bar-info");
  if (info) {
    button = el("button", "ai-btn label");
    button.type = "button";
    button.textContent = "✦ AI";
    button.setAttribute("aria-pressed", "false");
    button.addEventListener("click", () => run(false));
    // Before the sidebar button, which is added after this one.
    info.append(button);
  }
  registerAction({ id: "ai.open", title: "AI chat", run: () => run(false) });
  registerAction({ id: "ai.new_chat", title: "New AI chat", run: () => run(true) });
  await listen<unknown>("ai", onAi);
  // Every list the providers dialog reads is the list of every chat of this window.
  onProfilesChanged((raw) => takeProfiles(raw, true, null));
  // A window that gains the focus may have missed events, and a last event that was lost cannot be seen from the
  // events: every chat on screen asks for a fresh snapshot.
  window.addEventListener("focus", () => {
    for (const s of sessions.values()) {
      if (s.views.size === 0) continue;
      s.feed.awaitSnapshot();
      void fetchSnapshot(s);
    }
  });
  // The button is pressed while the shown tab has a chat pane.
  onOverlayRender(() => {
    const pressed = shownChatPane() !== null;
    button?.setAttribute("aria-pressed", pressed ? "true" : "false");
  });
}
