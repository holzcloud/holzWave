// The tab of the browser build calls the person back while the page is hidden. The title of the tab starts with the glyphs of the
// bar's marks (design 6.23) in their fixed order, `? \u25B2 ! \u2713`, and the icon changes to the one with the rose dot; both are put back
// the moment the tab is visible again and are never touched while it is. What happened comes from the same events as the marks of
// the bar (`bell`, `command-end`) and from the attention store's waiting questions; only the kind crosses, never a pane's title, a
// directory, a command or output.
import { attention } from "./attentionstate.js";
import type { Kind } from "./attentionstate.js";
import { listen } from "./bridge.js";
import { el, loadStyles, setText } from "./dom.js";
import { state } from "./state.js";
import {
  ICON_PLAIN, NOTIFY_FLAG_KEY, NOTIFY_GROUP, NotifyGate, Notices, commandKind, noticeIcon, noticeTitle, permissionState,
} from "./webstate.js";
import type { NotifyWord } from "./webstate.js";

const notices = new Notices();

/** The title the page had at start; the marks are put in front of it and it is put back unchanged. */
let base = "";
/** The one icon link, made by `initNotice` (the page file has none). */
let link: HTMLLinkElement | null = null;
/** The panes that waited for an answer when the store last told, so a question that began can be told from one that went on. */
let asking = new Set<number>();
/** What else wants to know that a kind occurred while the page was hidden (the notifications). */
const hooks: ((kind: Kind) => void)[] = [];

/** Whether the page is hidden: the tab is in the background or the window is minimised. */
export const isHidden = (): boolean => document.hidden === true || document.visibilityState === "hidden";

/** Be told of every occurrence of a kind while the page is hidden, repeats included. */
export function onNotice(fn: (kind: Kind) => void): void {
  hooks.push(fn);
}

/** Put the title and the icon where the set says they belong; touches nothing that is already right. */
function render(): void {
  const kinds = notices.list();
  const title = noticeTitle(base, kinds);
  if (document.title !== title) document.title = title;
  const icon = noticeIcon(kinds);
  if (link !== null && link.getAttribute("href") !== icon) link.setAttribute("href", icon);
}

/** A kind occurred. Only a hidden page records it; every occurrence is passed on, a repeat too. */
function occurred(kind: Kind): void {
  if (!isHidden()) return;
  if (notices.note(kind, true)) render();
  for (const fn of hooks) fn(kind);
}

/** Take the questions that wait now: a question that began while hidden occurs; one that is gone leaves the title. */
function followQuestions(): void {
  const now = attention.waiting();
  const began = now.some((pane) => !asking.has(pane));
  asking = new Set(now);
  if (!isHidden()) return;
  if (now.length === 0) {
    if (notices.drop("ask")) render();
    return;
  }
  if (began) occurred("ask");
  else if (notices.note("ask", true)) render();
}

const isPaneEvent = (p: unknown): p is { pane: number; code?: unknown } =>
  typeof p === "object" && p !== null && Number.isSafeInteger((p as { pane?: unknown }).pane);

/** A pane that exited or left this window: the bar ignores its events, and so does the tab. */
const isGone = (pane: number): boolean => state.exited.has(pane) || state.away.has(pane);

// ---- notifications ----------------------------------------------------------------------------------------------

const gate = new NotifyGate();

/** Whether the browser has a notification API at all. */
export const canNotify = (): boolean => typeof window.Notification === "function";

/** The person's choice, read from the one key of local storage; a browser that refuses storage reads as off. */
function readFlag(): boolean {
  try {
    return localStorage.getItem(NOTIFY_FLAG_KEY) === "1";
  } catch {
    return false;
  }
}

/** The person's choice as the page holds it: the key is read at start and again when another tab changes it. */
let flag = false;

function writeFlag(on: boolean): void {
  flag = on;
  try {
    if (on) localStorage.setItem(NOTIFY_FLAG_KEY, "1");
    else localStorage.removeItem(NOTIFY_FLAG_KEY);
  } catch {
    // the choice then lasts until the page is closed
  }
}

/** The state of the notifications now: the browser's permission and the person's choice together. */
const wordNow = (): NotifyWord => permissionState(canNotify() ? window.Notification.permission : "denied", flag);

/** Make the notification for `kind` when the rule allows it: the words of the mark, quiet, one per kind in five seconds. */
function notify(kind: Kind): void {
  if (!canNotify()) return;
  const text = gate.next(kind, performance.now(), isHidden(), wordNow());
  if (text === null) return;
  try {
    const n = new window.Notification(text, { tag: kind, silent: true });
    n.onclick = () => {
      window.focus();
      n.close();
    };
  } catch {
    // a browser that cannot make one here (some phones) shows the title and the icon only
  }
}

/**
 * Build the group `Notifications` in `parent`, before `before` (the end when null), and return the function that removes it.
 * Nothing is built where the browser has no notification API. The browser is asked for its permission only inside the press of
 * `Turn on`, never when the group is shown; the one button changes with the state (`Turn on`, `Turn off`, `Check again`).
 */
export function mountNotificationsGroup(parent: Element, before: Element | null): () => void {
  if (!canNotify()) return () => {};
  void loadStyles("./web.css").catch(() => {});
  const set = el("fieldset", "set-group set-notify");
  parent.insertBefore(set, before);
  set.setAttribute("role", "group");
  set.setAttribute("aria-labelledby", "set-notify-legend");
  const legend = el("legend", "", set);
  legend.id = "set-notify-legend";
  legend.textContent = NOTIFY_GROUP.legend;

  const row = el("div", "set-notify-row", set);
  el("span", "set-notify-name", row).textContent = NOTIFY_GROUP.name;
  el("span", "set-notify-meta", row).textContent = NOTIFY_GROUP.meta;
  const wordNode = el("span", "set-notify-state", row);
  const adviceNode = el("p", "set-notify-advice", row);
  const buttons = el("div", "set-notify-buttons", row);
  const button = el("button", "", buttons);
  button.type = "button";
  const line = el("p", "set-notify-line", set);
  line.setAttribute("role", "status");

  let disposed = false;
  /** The state the row shows, which is what the button in front of the person means; the browser may have changed since. */
  let shownWord: NotifyWord = "off";
  const draw = (): void => {
    const word = wordNow();
    shownWord = word;
    const shown = NOTIFY_GROUP.rows[word];
    row.dataset.state = word === "blocked by the browser" ? "blocked" : word;
    row.dataset.tone = shown.tone;
    setText(wordNode, word);
    setText(adviceNode, shown.advice);
    setText(button, shown.button);
  };

  /** `Turn on`: the request is made in this press and nowhere else. A permission the browser already gave needs none. */
  const turnOn = (): void => {
    const permission = window.Notification.permission;
    if (permission === "granted") {
      writeFlag(true);
      draw();
      return;
    }
    if (permission === "denied") {
      draw();
      return;
    }
    let asked: Promise<unknown>;
    try {
      asked = Promise.resolve(window.Notification.requestPermission());
    } catch {
      setText(line, NOTIFY_GROUP.failed);
      return;
    }
    void asked.then(
      (result) => {
        if (result === "granted") writeFlag(true);
        if (!disposed) draw();
      },
      () => {
        if (!disposed) {
          setText(line, NOTIFY_GROUP.failed);
          draw();
        }
      },
    );
  };

  button.addEventListener("click", () => {
    setText(line, "");
    if (shownWord === "off") turnOn();
    else if (shownWord === "on") {
      writeFlag(false);
      draw();
    } else {
      draw();
      setText(line, NOTIFY_GROUP.checked);
    }
  });
  draw();
  return () => {
    disposed = true;
    set.remove();
  };
}

let started = false;

/** Start the tab marks. Called once, by `web.ts`. */
export async function initNotice(): Promise<void> {
  if (started) return;
  started = true;
  base = document.title === "" ? "holzWave" : document.title;
  link = document.createElement("link");
  link.rel = "icon";
  link.type = "image/svg+xml";
  link.setAttribute("href", ICON_PLAIN);
  document.head.append(link);

  flag = readFlag();
  window.addEventListener("storage", (e) => {
    if (e.key === NOTIFY_FLAG_KEY || e.key === null) flag = readFlag();
  });
  onNotice(notify);
  asking = new Set(attention.waiting());
  attention.subscribe(followQuestions);
  document.addEventListener("visibilitychange", () => {
    if (isHidden()) {
      // A question that waited before the tab went away is there at once; nothing is announced for it.
      if (asking.size > 0 && notices.note("ask", true)) render();
    } else if (notices.clear()) {
      render();
    }
  });
  await listen<{ pane: number }>("bell", (p) => {
    if (isPaneEvent(p) && !isGone(p.pane)) occurred("bell");
  });
  await listen<{ pane: number; code: number | null }>("command-end", (p) => {
    if (isPaneEvent(p) && !isGone(p.pane)) occurred(commandKind(p.code));
  });
}
