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
import { Approvals } from "./approval.js";
import { canUndo, indexResults, rowsOf, waitingLabel, waitingName } from "./approvalstate.js";
import type { ToolResult } from "./approvalstate.js";
import { mountApprovals, toolRowNode } from "./approvalcard.js";
import type { CardView as ApprovalView } from "./approvalcard.js";
import { reviewIds } from "./aistate.js";
import { call, listen } from "./bridge.js";
import { paneBlocks, setChatTitle } from "./blockstate.js";
import { ChatFeed, isChatId, messageText, readChatEvent, readChatState } from "./chatstate.js";
import type { ChatMessage, PendingCall, Route } from "./chatstate.js";
import {
  MAX_TEXT_BYTES, chipView, endNote, faultText, leavesComputer, linkKind, localityWord, outcomeWord, pickProfile, readProfiles, roleWord,
  setupLabel, statusLabel, statusWord, titleWords, tokenWords, usageWords, utf8Bytes,
} from "./chatview.js";
import type { ChipView, EndNote, ProfileChoice } from "./chatview.js";
import { Consent } from "./consent.js";
import { mountCard } from "./consentcard.js";
import type { CardView } from "./consentcard.js";
import { NOTES, contextArguments, ctxButtonView, paneWords, sentLine } from "./consentstate.js";
import type { CtxKind, Route as ConsentRoute } from "./consentstate.js";
import { el, loadStyles } from "./dom.js";
import { focusInput } from "./keyboard.js";
import { onOverlayRender, renderOverlay } from "./overlay.js";
import { paneLabel } from "./panehead.js";
import { appendSafe } from "./safehtml.js";
import { applyView } from "./scene.js";
import { state } from "./state.js";
import { showNotice } from "./status.js";
import { Undos } from "./undo.js";
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

/** The two sentences of the page that a send and a press of `Context…` share. */
const CHOOSE_FIRST = "Choose a provider first.";
const REVIEW_FIRST = "Review this provider first.";

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
  /** The sharing of this chat (`consent.ts`); made at the first user act that needs it, never at mount. */
  consent: Consent | null;
  /** The calls of this chat that wait for a decision (`approval.ts`); making it asks the core nothing. */
  readonly approvals: Approvals;
  /** The list of waiting calls the controller was last given, so an unchanged list is not read again. */
  pendingSeen: readonly PendingCall[] | null;
  /** The calls whose result the person opened. */
  readonly openResults: Set<string>;
  /** The presses of Undo on finished writes (`undo.ts`); making it asks the core nothing. */
  readonly undos: Undos;
}

/** The rendered HTML of finished replies by message id, with the text it was made from; the oldest goes first. */
const rendered = new Map<string, { text: string; html: string }>();
let renderedChars = 0;
/** Numbers the strips of the chats, so each button names its own. */
let stripNumber = 0;

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
/**
 * The pane the chats act for: the last pane the user focused that shows a terminal or a preview, in the shown tab. Every
 * message names it, with context or without; a chat, a file browser or a monitor never counts.
 */
let acting: number | null = null;
/** The panes an earlier render has shown, so a pane whose block the core has not listed yet is not taken for a terminal. */
let shownPanes = new Set<number>();

/** What the page offers the chat besides the bridge: the settings watch. */
export interface ChatHost {
  /** Call `fn` after every accepted settings view; the answer stops it. */
  watchSettings(fn: () => void): () => void;
}

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
      consent: null,
      approvals: new Approvals({
        invoke: (cmd, args) => call(cmd, args),
        conversation: () => conversation,
        now: () => Date.now(),
        say: (text, kind) => {
          const mine = sessions.get(conversation);
          if (mine !== undefined) sayTo(mine, text, kind);
        },
        refresh: () => {
          const mine = sessions.get(conversation);
          if (mine === undefined) return;
          mine.feed.awaitSnapshot();
          void fetchSnapshot(mine);
        },
      }),
      pendingSeen: null,
      openResults: new Set(),
      undos: new Undos({ invoke: (cmd, args) => call(cmd, args), conversation: () => conversation }),
    };
    sessions.set(conversation, s);
    // The number on the title-bar button follows every chat of this window.
    s.approvals.subscribe(updateBadge);
    // A press of Undo, its answer and its refusal draw the rows again.
    const made = s;
    s.undos.subscribe(() => schedule(made));
  }
  return s;
}

/** The title-bar button says how many calls wait for a decision, over every chat of this window. */
function updateBadge(): void {
  if (button === null) return;
  let n = 0;
  for (const s of sessions.values()) n += s.approvals.waiting();
  let count = button.querySelector<HTMLElement>(":scope > .ai-count");
  if (n > 0) {
    if (count === null) {
      count = el("span", "ai-count", button);
    }
    const words = waitingLabel(n);
    if (count.textContent !== words) count.textContent = words;
    button.dataset.waiting = String(n);
    button.setAttribute("aria-label", waitingName(n));
  } else {
    count?.remove();
    button.removeAttribute("data-waiting");
    button.removeAttribute("aria-label");
  }
}

/** Hand the list of waiting calls of `s` to its controller; a list that did not change is not read again. */
function syncApprovals(s: Session): void {
  const list = s.feed.state.pending;
  if (s.pendingSeen === list) return;
  s.pendingSeen = list;
  s.approvals.sync(list);
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
      syncApprovals(session);
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
    // A saved model or address that changed is another route: the share ends and the next press asks again.
    t.consent?.check();
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

/** Say one line under the transcript of every view of `s`, now. */
function sayTo(s: Session, text: string, kind: "error" | "note"): void {
  s.notice = { text, kind };
  s.stopping = false;
  for (const view of s.views) view.drawStatus();
}

/** The two privacy settings as one string for the route; empty while the settings are unknown. */
function privacyKey(): string {
  const view = state.settings;
  return view === null ? "" : `${view.settings.ai_local_context}|${view.settings.ai_local_only ? "only" : "open"}`;
}

/** Where the next message of `s` would go, as the controller of its sharing compares it. */
function routeOf(s: Session): ConsentRoute {
  const pick = pickProfile(s.profiles, s.choice, s.feed.state.profile);
  const row = pick.id === "" ? undefined : s.profiles?.find((p) => p.id === pick.id);
  return {
    ask: pick.via === "ask",
    pane: acting,
    profile: pick.id === "" ? null : pick.id,
    model: null,
    entry: row === undefined ? null : `${row.id}|${row.model}|${row.host}`,
    settings: privacyKey(),
  };
}

/** The controller of the sharing of `s`, made at the first user act that needs it; making it asks the core nothing. */
function consentFor(s: Session): Consent {
  if (s.consent === null) {
    s.consent = new Consent({
      invoke: (cmd, args) => call(cmd, args),
      conversation: () => s.id,
      route: () => routeOf(s),
      now: () => Date.now(),
      say: (text, kind) => sayTo(s, text, kind),
    });
    for (const view of s.views) view.drawConsent();
  }
  return s.consent;
}

/**
 * Follow the focus: the pane the chats act for is the focused pane when it shows a terminal or a preview, else the pane
 * it was while that is still in the tab, else none. A pane whose block the core has not listed yet is not taken for a
 * terminal: the render that the list brings decides.
 */
function followActing(): void {
  const view = state.view;
  if (!view) return;
  const live = new Set(view.panes.map(([id]) => id));
  const before = acting;
  if (acting !== null && !live.has(acting)) acting = null;
  const focus = view.focus;
  if (live.has(focus) && (paneBlocks.has(focus) || shownPanes.has(focus))) {
    const kind = paneBlocks.get(focus)?.kind;
    if (kind === undefined || kind === "preview") acting = focus;
  }
  shownPanes = live;
  for (const s of sessions.values()) {
    if (acting !== before) s.consent?.check();
    for (const v of s.views) v.drawContextButton();
  }
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
  /** The lines of context sent that are drawn under the text, joined; empty for none. */
  notes: string;
  /** What the tool rows under the text were drawn from; empty for none. */
  tools: string;
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
  /** `local only`: drawn while the switch is on; it opens the providers dialog. */
  private readonly local: HTMLButtonElement;
  private readonly usage: HTMLElement;
  private readonly setup: HTMLButtonElement;
  private picker: HTMLElement | null = null;
  private pickerKey = "";
  private readonly log: HTMLElement;
  private readonly statusLine: HTMLElement;
  private readonly input: HTMLTextAreaElement;
  private readonly count: HTMLElement;
  private readonly sendButton: HTMLButtonElement;
  /** The strip between the transcript and the status line that holds the consent card. */
  private readonly cards: HTMLElement;
  private readonly ctx: HTMLButtonElement;
  private cardFor: Consent | null = null;
  private cardOff: (() => void) | null = null;
  private approvalOff: (() => void) | null = null;
  private cardWatch: (() => void) | null = null;
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
    this.local = el("button", "chat-local", this.bar);
    this.local.type = "button";
    this.local.dataset.field = "";
    this.local.hidden = true;
    this.local.textContent = "local only";
    this.local.setAttribute(
      "aria-label",
      "Local only is on: requests that leave this computer are refused. Opens the provider settings.",
    );
    this.local.setAttribute("aria-haspopup", "dialog");
    this.local.addEventListener("click", () => {
      void openProviders().then(() => {
        if (this.alive) this.focus();
      });
    });
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
    this.cards = el("div", "chat-cards", host);
    this.cards.id = `chat-cards-${++stripNumber}`;
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
    this.ctx = el("button", "chat-ctx", foot);
    this.ctx.type = "button";
    this.ctx.dataset.field = "";
    this.ctx.setAttribute("aria-controls", this.cards.id);
    this.ctx.addEventListener("click", () => void this.pressContext());
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
    this.unmountCard();
    this.unmountApprovals();
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
    this.unmountCard();
    this.unmountApprovals();
    this.session.views.delete(this);
    this.host.removeEventListener("mousedown", this.onMouseDown);
    this.host.replaceChildren();
  }

  /** Show `s` here: its draft, what it holds so far, and a fresh snapshot of the core's state. */
  private attach(s: Session): void {
    s.views.add(this);
    this.input.value = s.draft;
    this.approvalOff = mountApprovals(this.cards, s.approvals, this.approvalView());
    this.draw();
    s.feed.awaitSnapshot();
    void fetchSnapshot(s);
  }

  // ---- drawing ----------------------------------------------------------------------------------------------------

  /** Bring everything in line with the feed's state. */
  draw(): void {
    if (!this.alive) return;
    syncApprovals(this.session);
    this.drawBar();
    this.drawTranscript();
    this.drawStatus();
    this.drawComposer();
    this.drawConsent();
  }

  /** Mount the card of the session's controller when it has one and draw the button that opens it. */
  drawConsent(): void {
    if (!this.alive) return;
    const c = this.session.consent;
    if (c !== this.cardFor) {
      this.unmountCard();
      if (c !== null) {
        this.cardOff = mountCard(this.cards, c, this.cardView());
        this.cardWatch = c.subscribe(() => this.drawContextButton());
      }
      this.cardFor = c;
    }
    this.drawContextButton();
  }

  /** The text, the open state and the availability of `Context…`. */
  drawContextButton(): void {
    if (!this.alive) return;
    const m = this.session.consent?.model;
    const picks = m !== undefined && m.offer !== null ? m.offer.items.length - m.off.length : 0;
    const view = ctxButtonView(m?.mode ?? "closed", picks, acting !== null);
    if (this.ctx.textContent !== view.text) this.ctx.textContent = view.text;
    this.ctx.setAttribute("aria-expanded", view.expanded ? "true" : "false");
    if (view.disabled) this.ctx.setAttribute("aria-disabled", "true");
    else this.ctx.removeAttribute("aria-disabled");
  }

  private unmountApprovals(): void {
    this.approvalOff?.();
    this.approvalOff = null;
  }

  /**
   * What the approval cards need to know of the page: where a result goes and the way back to the composer. The place is the
   * core's word for the route of the turn that is running; with no route of this turn (after a reload) it is not told.
   */
  private approvalView(): ApprovalView {
    return {
      destination: () => {
        const s = this.session;
        const route = s.route;
        if (route === null || s.routeTurn !== s.feed.state.turn) return null;
        return { model: route.model, host: route.host, place: localityWord(route.locality), leaves: leavesComputer(route.locality) };
      },
      focusComposer: () => this.focus(),
    };
  }

  private unmountCard(): void {
    this.cardOff?.();
    this.cardWatch?.();
    this.cardOff = null;
    this.cardWatch = null;
    this.cardFor = null;
  }

  /** What the card needs to know of the page: pane words, provider names and the way back to the composer. */
  private cardView(): CardView {
    return {
      paneTitle: (pane) => paneWords(pane, paneLabel(pane)),
      profileName: (id) => {
        const row = this.session.profiles?.find((p) => p.id === id);
        return row === undefined || row.name === "" ? id : row.name;
      },
      paneKind: (pane) => {
        const kind = paneBlocks.get(pane)?.kind;
        return kind === undefined ? "terminal" : kind === "preview" ? "preview" : "other";
      },
      providers: () => {
        void openProviders().then(() => {
          if (this.alive) this.focus();
        });
      },
      focusButton: () => this.ctx.focus({ preventScroll: true }),
    };
  }

  /**
   * The button `Context…`: closes an open card, else asks the core about the pane the chat acts for. A sentence that tells
   * why it cannot is said under the transcript.
   */
  private async pressContext(): Promise<void> {
    const s = this.session;
    const consent = consentFor(s);
    if (consent.model.mode !== "closed") {
      consent.close();
      return;
    }
    const sentence = await this.beginContext([]);
    if (sentence !== null) sayTo(s, sentence, sentence === REVIEW_FIRST ? "error" : "note");
  }

  /**
   * Ask the core about the pane the chat acts for and open the card, with `prefill` ticked once the pane may be read. The
   * profile list is read first when it is not known (a user act, like a send), and two providers with no choice open the
   * picker. Answers null, or the sentence that says why the card did not open.
   */
  async beginContext(prefill: CtxKind[]): Promise<string | null> {
    const s = this.session;
    const consent = consentFor(s);
    if (acting === null) return NOTES.noPane;
    let pick = pickProfile(s.profiles, s.choice, s.feed.state.profile);
    if (pick.via === "unknown") {
      try {
        await loadProfiles(s);
      } catch (e) {
        return faultText(e);
      }
      pick = pickProfile(s.profiles, s.choice, s.feed.state.profile);
    }
    if (pick.via === "ask") {
      await this.openPicker();
      return CHOOSE_FIRST;
    }
    if (review.has(pick.id)) {
      void openProviders(pick.id).then(() => {
        if (this.alive) this.focus();
      });
      return REVIEW_FIRST;
    }
    await consent.open(prefill);
    return null;
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
    // The switch as the core last answered it; drawn only then, never from a guess.
    this.local.hidden = state.settings?.settings.ai_local_only !== true;
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
    // A message of the role tool draws nothing of its own: its results are shown in the rows of the calls they answer.
    const results = indexResults(st.messages);
    const waiting = new Set(st.pending.map((p) => p.call_id));
    for (const m of st.messages) {
      if (m.role === "tool") continue;
      seen.add(m.id);
      let a = this.articles.get(m.id);
      if (a === undefined) {
        a = this.makeArticle(m);
        this.articles.set(m.id, a);
      }
      this.updateArticle(a, m);
      this.drawTools(a, m, results, waiting, st.running);
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
        this.streaming = { node, who, text, plain: null, label: null, markdown: "done", notes: "", tools: "" };
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
    return { node, who, text, plain: null, label: null, markdown: "none", notes: "", tools: "" };
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
    this.drawNotes(a, m);
  }

  /**
   * Under the text of an assistant message that asked for tools: one row per call. The rows are drawn again only when what
   * they show changed, and a toggle that had the keyboard keeps it.
   */
  private drawTools(a: Article, m: ChatMessage, results: ReadonlyMap<string, ToolResult>, waiting: ReadonlySet<string>, running: boolean): void {
    const open = this.session.openResults;
    let key = "";
    if (m.role === "assistant") {
      for (const part of m.parts) {
        if (part.type !== "tool_call") continue;
        const r = results.get(part.id);
        const undo = r === undefined || r.is_error ? null : this.session.undos.model(part.id);
        key += `${part.id}|${part.name}|${part.summary.length}|${r === undefined ? "-" : `${r.is_error ? "e" : "r"}${r.content.length}`}|${
          waiting.has(part.id) ? "w" : ""}${open.has(part.id) ? "o" : ""}|${undo === null ? "" : JSON.stringify([undo.state, undo.fault])}\n`;
      }
      if (key !== "") key += running ? "running" : "idle";
    }
    if (a.tools === key) return;
    a.tools = key;
    const old = a.node.querySelector(":scope > .chat-tools");
    const active = document.activeElement;
    const had = old !== null && active !== null && old.contains(active) ? (active.closest(".chat-tool") as HTMLElement | null)?.dataset.call : undefined;
    old?.remove();
    if (key === "") return;
    const box = el("div", "chat-tools", a.node);
    const undos = this.session.undos;
    for (const row of rowsOf(m, results, waiting, running)) {
      box.append(
        toolRowNode(
          row,
          open.has(row.id),
          () => {
            if (open.has(row.id)) open.delete(row.id);
            else open.add(row.id);
            schedule(this.session);
          },
          canUndo(row) ? { model: undos.model(row.id), press: () => void undos.undo(row.id) } : undefined,
        ),
      );
    }
    if (had !== undefined) {
      for (const node of box.querySelectorAll<HTMLElement>(".chat-tool")) {
        if (node.dataset.call === had) node.querySelector<HTMLElement>(".tool-toggle")?.focus({ preventScroll: true });
      }
    }
  }

  /** Under the text of a user message that carried context: one quiet line per approval, from the message's own parts. */
  private drawNotes(a: Article, m: ChatMessage): void {
    const lines: string[] = [];
    if (m.role === "user") {
      for (const part of m.parts) if (part.type === "context") lines.push(sentLine(part));
    }
    const key = lines.join("\n");
    if (a.notes === key) return;
    a.notes = key;
    for (const old of Array.from(a.node.querySelectorAll(":scope > .chat-ctxnote"))) old.remove();
    for (const line of lines) el("p", "chat-ctxnote", a.node).textContent = line;
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

  drawStatus(): void {
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
    s.consent?.check();
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
    const pane = acting ?? this.pane();
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
        this.say(CHOOSE_FIRST, "note");
        await this.openPicker();
        return;
      }
      // A provider that waits for its review is not sent to: the dialog opens on it and the draft stays. The core refuses
      // such a send itself; this only saves the round trip and says why.
      if (review.has(pick.id)) {
        this.say(REVIEW_FIRST, "error");
        void openProviders(pick.id).then(() => {
          if (this.alive) this.focus();
        });
        return;
      }
      const consent = s.consent;
      if (consent !== null) {
        const wait = consent.blocksSend();
        if (wait !== null) {
          this.say(wait, "error");
          return;
        }
        if (consent.expired()) {
          consent.close();
          this.say(NOTES.expired, "error");
          return;
        }
      }
      const held = consent?.held() ?? null;
      try {
        await call("ai_send", {
          conversation: s.id,
          text,
          pane,
          profile: pick.id === "" ? null : pick.id,
          model: null,
          ...contextArguments(held),
        });
      } catch (e) {
        // A refusal that ends the offer says why itself; any other keeps the offer and is told like every refusal.
        if (held === null || consent === null || !consent.refused(e)) this.say(faultText(e), "error");
        return;
      }
      consent?.sent();
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
    sayTo(this.session, text, kind);
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
 * Open the card of the chat `conversation` for the pane it acts for, tick the kinds the core lists among `kinds` and read
 * them once the pane is shared or sharing is not required. It never shares by itself and never sends. Answers null, or one
 * sentence that says why the card did not open (the chat is not on screen, no pane can be shared, no provider is chosen).
 */
export async function attachContext(conversation: string, kinds: CtxKind[]): Promise<string | null> {
  const s = sessions.get(conversation);
  const view = s === undefined ? undefined : [...s.views][0];
  if (view === undefined) return "The chat is not open.";
  return view.beginContext(kinds);
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
    s.consent?.dispose();
    s.approvals.dispose();
    s.undos.dispose();
    if (s.feed.state.running) void call("ai_cancel", { conversation }).catch(() => {});
  }
  sessions.delete(conversation);
  wantFocus.delete(conversation);
  setChatTitle(conversation, "");
  updateBadge();
}

/**
 * Let go of the page's session of an idle chat that nothing shows: the sidebar column calls this for a chat it left, so an
 * attached offer, a share's card and a finished transcript of one chat never wait behind another. The session stays while a
 * view shows it, while a reply runs or a send is on its way, while a call waits for a decision, while the person has a draft
 * in it and while a chat pane holds the chat. The core's chat and its files are not touched, no command is called and no reply
 * is stopped; the next view that shows the chat makes a fresh session and asks for a snapshot. Answers whether it let go.
 */
export function releaseChat(conversation: string): boolean {
  const s = sessions.get(conversation);
  if (s === undefined) return false;
  if (s.views.size > 0 || s.feed.state.running || s.sending || s.feed.state.pending.length > 0) return false;
  if (s.approvals.waiting() > 0 || s.draft !== "") return false;
  for (const block of paneBlocks.values()) {
    if (block.kind === "chat" && block.path === conversation) return false;
  }
  dirty.delete(s);
  s.consent?.dispose();
  s.approvals.dispose();
  s.undos.dispose();
  sessions.delete(conversation);
  wantFocus.delete(conversation);
  setChatTitle(conversation, "");
  updateBadge();
  return true;
}

/** Make the button, register the two actions and listen to the core's `ai` events. Call once at start-up. */
export async function initChat(open: BlockOpener, host?: ChatHost): Promise<void> {
  opener = open;
  try {
    await loadStyles("./chat.css");
  } catch {
    // the chat still works without its style sheet
  }
  try {
    await loadStyles("./consent.css");
  } catch {
    // the chat still works without the style sheet of the card
  }
  try {
    await loadStyles("./approval.css");
  } catch {
    // the chat still works without the style sheet of the approval card
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
    followActing();
  });
  // A privacy setting that changes ends what the page knew of a share: every card looks at its route again.
  host?.watchSettings(() => {
    for (const s of sessions.values()) {
      s.consent?.check();
      for (const v of s.views) v.draw();
    }
  });
}
