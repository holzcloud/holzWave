// The tab of the browser build calls the person back while the page is hidden. The title of the tab starts with the glyphs of the
// bar's marks (design 6.23) in their fixed order, `? \u25B2 ! \u2713`, and the icon changes to the one with the rose dot; both are put back
// the moment the tab is visible again and are never touched while it is. What happened comes from the same events as the marks of
// the bar (`bell`, `command-end`) and from the attention store's waiting questions; only the kind crosses, never a pane's title, a
// directory, a command or output.
import { attention } from "./attentionstate.js";
import type { Kind } from "./attentionstate.js";
import { listen } from "./bridge.js";
import { state } from "./state.js";
import { ICON_PLAIN, Notices, commandKind, noticeIcon, noticeTitle } from "./webstate.js";

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
