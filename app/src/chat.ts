// The AI chat as a pane (docs/design.md section 6.12): sessions over the page mirror of the core's chat (`ChatFeed`), the
// transcript, the composer, the title-bar button `✦ AI` and the actions that open a chat. The core owns the chat; this
// module draws what the core's events and snapshots say, and sends the typed text with `ai_send`, nothing else.
//
// Every string goes onto the screen with `textContent` or a text node: model text and user text are never markup here.
// Nothing in this module decides, grants or approves anything; the parts of a message that are not text (tool calls,
// context, pending approvals) draw nothing until the plans that build them. This module never imports `blocks.ts`: the
// opener of a block is handed to `initChat`, and `blocks.ts` mounts a chat pane through `mountChat`.
import { registerAction } from "./actions.js";
import { call, listen } from "./bridge.js";
import { paneBlocks, setChatTitle } from "./blockstate.js";
import { ChatFeed, isChatId, messageText, readChatEvent, readChatState } from "./chatstate.js";
import type { ChatMessage, Route } from "./chatstate.js";
import { faultText, pickProfile, readProfiles } from "./chatview.js";
import type { ProfileChoice } from "./chatview.js";
import { el, loadStyles } from "./dom.js";
import { onOverlayRender, renderOverlay } from "./overlay.js";
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
  /** The profile the user chose for this chat. */
  choice: string | null;
  draft: string;
  /** A snapshot request is on its way. */
  fetching: boolean;
  /** A send is on its way. */
  sending: boolean;
  /** The line of the status row: a refusal or a hint; empty for none. */
  status: string;
}

const sessions = new Map<string, Session>();
/** The chats the user just opened: their composer takes the keyboard when it is mounted. */
const wantFocus = new Set<string>();
const dirty = new Set<Session>();
let frame = 0;
let opener: BlockOpener | null = null;
let button: HTMLButtonElement | null = null;

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
      choice: null,
      draft: "",
      fetching: false,
      sending: false,
      status: "",
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
  if (event.kind === "turn_started" && event.turn >= s.routeTurn) {
    s.route = event.route;
    s.routeTurn = event.turn;
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

/** The profiles the core lists. Called when the user acts, never when a chat is mounted. */
async function loadProfiles(s: Session): Promise<void> {
  s.profiles = readProfiles(await call<unknown>("ai_profiles"));
}

// ---- the transcript and the composer ----------------------------------------------------------------------------------

/** One message drawn: its node and the plain text it shows. */
interface Article {
  node: HTMLElement;
  text: HTMLElement;
  /** The plain text drawn now; null before the first draw. */
  plain: string | null;
}

/** Whether `parent` holds exactly `nodes`, in order. */
function holds(parent: HTMLElement, nodes: readonly HTMLElement[]): boolean {
  if (parent.childElementCount !== nodes.length) return false;
  return nodes.every((n, i) => parent.children[i] === n);
}

function roleWord(role: ChatMessage["role"]): string {
  return role === "user" ? "you" : "assistant";
}

/** The chat drawn into one host element. */
class Mounted implements ChatView {
  private session: Session;
  private readonly log: HTMLElement;
  private readonly statusLine: HTMLElement;
  private readonly input: HTMLTextAreaElement;
  private readonly sendButton: HTMLButtonElement;
  private readonly articles = new Map<string, Article>();
  private streaming: Article | null = null;
  private streamed: Text | null = null;
  private composingEnded = Number.NEGATIVE_INFINITY;
  private alive = true;

  constructor(
    private readonly host: HTMLElement,
    conversation: string,
    readonly pane: () => number | null,
  ) {
    this.session = sessionFor(conversation);
    el("div", "chat-bar", host);
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
    this.sendButton = el("button", "chat-send", composer);
    this.sendButton.type = "button";
    this.sendButton.dataset.field = "";
    this.sendButton.dataset.mode = "send";
    this.sendButton.textContent = "Send";

    this.input.addEventListener("input", () => {
      this.session.draft = this.input.value;
    });
    this.input.addEventListener("compositionend", () => {
      this.composingEnded = performance.now();
    });
    this.input.addEventListener("keydown", (e) => {
      if (e.key !== "Enter" || e.shiftKey || e.isComposing) return;
      e.preventDefault();
      if (performance.now() - this.composingEnded < ENTER_AFTER_COMPOSITION_MS) return;
      void this.submit();
    });
    this.sendButton.addEventListener("click", () => void this.submit());
    // A click on the pane's empty parts gives the keyboard to the composer, not to the terminal behind the pane.
    host.addEventListener("mousedown", (e) => {
      if (e.target instanceof Element && e.target.closest("[data-field]")) return;
      e.preventDefault();
      this.focus();
    });

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
    this.articles.clear();
    this.streaming = null;
    this.streamed = null;
    this.log.replaceChildren();
    this.session = sessionFor(conversation);
    this.attach(this.session);
  }

  dispose(): void {
    this.alive = false;
    this.session.views.delete(this);
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

  /** Bring the transcript in line with the feed's state. */
  draw(): void {
    if (!this.alive) return;
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
    if (st.running && st.partial.text !== "") {
      if (this.streaming === null || this.streamed === null) {
        const node = el("article", "chat-msg chat-partial");
        node.dataset.role = "assistant";
        node.dataset.status = "streaming";
        const who = el("div", "chat-who label", node);
        el("span", "chat-role", who).textContent = roleWord("assistant");
        const text = el("div", "chat-text", node);
        this.streamed = document.createTextNode("");
        text.append(this.streamed);
        this.streaming = { node, text, plain: null };
      }
      if (this.streamed.data !== st.partial.text) this.streamed.data = st.partial.text;
      nodes.push(this.streaming.node);
    } else {
      this.streaming = null;
      this.streamed = null;
    }

    if (!holds(this.log, nodes)) this.log.replaceChildren(...nodes);
    if (atBottom) this.log.scrollTop = this.log.scrollHeight;
    if (this.statusLine.textContent !== s.status) this.statusLine.textContent = s.status;
  }

  private makeArticle(m: ChatMessage): Article {
    const node = el("article", "chat-msg");
    node.dataset.role = m.role;
    node.dataset.id = m.id;
    node.dataset.status = m.status;
    const who = el("div", "chat-who label", node);
    el("span", "chat-role", who).textContent = roleWord(m.role);
    const text = el("div", "chat-text", node);
    return { node, text, plain: null };
  }

  private updateArticle(a: Article, m: ChatMessage): void {
    a.node.dataset.status = m.status;
    const plain = messageText(m);
    if (a.plain === plain) return;
    a.plain = plain;
    a.text.textContent = plain;
  }

  /** Send the typed text. Nothing but the text, the chat, the pane and the profile leaves. */
  private async submit(): Promise<void> {
    const s = this.session;
    const text = this.input.value.trim();
    if (text === "" || s.sending) return;
    const pane = this.pane();
    if (pane === null) {
      this.say("This chat has no pane to act for.");
      return;
    }
    s.sending = true;
    try {
      let pick = pickProfile(s.profiles, s.choice, s.feed.state.profile);
      if (pick.via === "unknown") {
        try {
          await loadProfiles(s);
        } catch (e) {
          this.say(faultText(e));
          return;
        }
        pick = pickProfile(s.profiles, s.choice, s.feed.state.profile);
      }
      if (pick.via === "ask") {
        this.say("Choose a provider first.");
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
        this.say(faultText(e));
        return;
      }
      s.draft = "";
      s.status = "";
      for (const view of s.views) view.input.value = "";
      s.feed.awaitSnapshot();
      void fetchSnapshot(s);
      schedule(s);
    } finally {
      s.sending = false;
    }
  }

  /** Say one line in the status row of every view of this chat. */
  private say(text: string): void {
    this.session.status = text;
    schedule(this.session);
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

/** The pane of a chat is gone: forget its session. */
export function chatClosed(conversation: string): void {
  sessions.delete(conversation);
  wantFocus.delete(conversation);
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
  // The button is pressed while the shown tab has a chat pane.
  onOverlayRender(() => {
    const pressed = shownChatPane() !== null;
    button?.setAttribute("aria-pressed", pressed ? "true" : "false");
  });
}
