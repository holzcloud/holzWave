// The server as a source of sessions in the Connections dialog (docs/design.md 6.34): one row above the saved connections with the
// server's host, its address in mono, a state dot with its word and the buttons of the state, the help line, and under a signed-in
// row the running sessions with `Attach` and `End`. The words and the decisions are in sessionstate.ts; this module draws them and
// calls the source. The source is a parameter: the desktop wires it to the host's commands (`desktopSource`), and the browser build
// to the same commands of its own worker (`webSource`), whose row is `This server` with no sign-in (6.30). Every value is set with
// `textContent`.
import { armed, el, setText } from "./dom.js";
import { armSignOut, openServerSignIn } from "./serversignin.js";
import { LOCKED_LINE, MAX_SENTENCE, SIGN_IN_LINK, SIGN_OUT, plain, readServerView } from "./serverstate.js";
import type { ServerView } from "./serverstate.js";
import {
  ATTACH,
  END,
  END_ARMED,
  NEW_SESSION,
  REMOTE_OFF,
  SERVER_LEGEND,
  THIS_SERVER,
  helpLine,
  isRemoteOff,
  readSessions,
  rowPlan,
  rowWords,
  serverName,
  webView,
} from "./sessionstate.js";
import type { SessionRow } from "./sessionstate.js";
import { isLockedFailure } from "./vaultstate.js";
import type { View } from "./types.js";

/** How often the list is read again while the dialog is open, in milliseconds. */
export const REFRESH_MS = 5000;

/** Where the row gets its answers. Every function may reject with the host's fixed sentence. */
export interface ServerSource {
  /**
   * The browser build: the row is the server the page was loaded from, named `This server` with `host` as its address, signed in
   * by the page's own sign-in. Whether remote sessions are on is read from the list's answer (the host's sentence when they are off).
   */
  web?: { host: string };
  /** The link; `fresh` asks the server again (once per opening), otherwise the host answers what it knows. */
  status(fresh: boolean): Promise<unknown>;
  list(): Promise<unknown>;
  create(): Promise<View>;
  open(session: number): Promise<View>;
  end(session: number): Promise<unknown>;
  /** The pane of this window that already shows `session`, if one does: `Attach` moves the keyboard there instead. */
  shownIn?(session: number): number | null;
  /** Move the keyboard to `pane`; answers the new view. */
  focus?(pane: number): Promise<View>;
}

/** What the row tells the dialog. */
export interface RowHooks {
  /** Say a sentence in the dialog's status line (empty clears it); `quiet` for the locked line. */
  say(text: string, quiet?: boolean): void;
  /** A session was opened or made: show the view, close the dialog, focus the pane. */
  opened(view: View): void;
}

export interface ServerRowHandle {
  refresh(): Promise<void>;
  dispose(): void;
}

/** A refusal as the status line shows it: the locked line for a locked store, else the host's sentence, bounded and plain. */
function refusal(failure: unknown): { text: string; quiet: boolean } {
  if (isLockedFailure(failure)) return { text: LOCKED_LINE, quiet: true };
  const text = plain(failure instanceof Error ? failure.message : String(failure), MAX_SENTENCE);
  return { text, quiet: false };
}

/** The source of the desktop: the host's commands. */
export function desktopSource(call: <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>): ServerSource {
  return {
    status: (fresh) => call<unknown>(fresh ? "server_refresh" : "server_status"),
    list: () => call<unknown>("server_sessions"),
    create: () => call<View>("server_session_new"),
    open: (session) => call<View>("server_session_open", { session }),
    end: (session) => call<unknown>("server_session_end", { session }),
    focus: (pane) => call<View>("focus_pane", { pane }),
  };
}

/** The source of the browser build: the same commands, answered by the person's own worker; the page's sign-in is the link. */
export function webSource(call: <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>, host: string): ServerSource {
  return {
    web: { host },
    status: () => Promise.resolve(null),
    list: () => call<unknown>("server_sessions"),
    create: () => call<View>("server_session_new"),
    open: (session) => call<View>("server_session_open", { session }),
    end: (session) => call<unknown>("server_session_end", { session }),
    focus: (pane) => call<View>("focus_pane", { pane }),
  };
}

/** Build the group `Server` as the first child of `parent`. */
export function buildServerRow(parent: HTMLElement, source: ServerSource, hooks: RowHooks): ServerRowHandle {
  const group = el("fieldset", "cx-server");
  parent.prepend(group);
  const legend = el("legend", "label cx-legend", group);
  legend.textContent = SERVER_LEGEND;
  // The legend is never redrawn, so it is where the focus comes back to from the sign-in dialog: the button that opened it is gone
  // by then (the row has no buttons while signing in), and a lost focus would fall to the terminal behind this dialog. It is not an
  // ancestor of the buttons, so a click that does not focus its button (WebKit on a Mac) never lands on it.
  legend.tabIndex = -1;
  const row = el("div", "cx-srv", group);
  const dot = el("span", "cx-dot", row);
  dot.setAttribute("aria-hidden", "true");
  const name = el("span", "cx-srv-name", row);
  const address = el("span", "cx-srv-addr", row);
  const word = el("span", "cx-srv-state", row);
  word.setAttribute("role", "status");
  const acts = el("span", "cx-srv-acts", row);
  const below = el("div", "cx-srv-below", group);

  /** Each drawing of the buttons and the list has its own signal, so the listeners of an old drawing go with it. */
  let drawn = new AbortController();
  const web = source.web ?? null;
  let view: ServerView = web === null ? readServerView(null) : webView(true);
  let sessions: SessionRow[] = [];
  let listFailed = false;
  let drawnKey = "";
  let busy = false;
  let timer: ReturnType<typeof setInterval> | null = null;
  let disposed = false;

  /** Run one action; a second press while one runs does nothing (two presses of Attach open one pane). */
  const act = async (work: () => Promise<void>): Promise<void> => {
    if (busy) return;
    busy = true;
    hooks.say("");
    try {
      await work();
    } catch (failure) {
      const { text, quiet } = refusal(failure);
      hooks.say(text, quiet);
    } finally {
      busy = false;
    }
  };

  const showView = (next: View): void => {
    dispose();
    hooks.opened(next);
  };

  const button = (parentNode: HTMLElement, text: string, cls = ""): HTMLButtonElement => {
    const b = el("button", `cx-sv-btn${cls === "" ? "" : ` ${cls}`}`, parentNode);
    b.type = "button";
    b.textContent = text;
    return b;
  };

  const draw = (): void => {
    const plan = rowPlan(view, listFailed, web !== null);
    const key = JSON.stringify([plan, view.url, sessions]);
    if (key === drawnKey) return;
    drawnKey = key;
    // A button that is armed is redrawn only when something changed; the focus stays where it was when that button is drawn again.
    const focused = document.activeElement instanceof HTMLElement && group.contains(document.activeElement) ? document.activeElement.dataset.key : undefined;
    drawn.abort();
    drawn = new AbortController();
    dot.dataset.s = plan.dot;
    setText(name, web === null ? serverName(view.url) : THIS_SERVER);
    setText(address, web === null ? view.url : web.host);
    setText(word, plan.word);
    acts.replaceChildren();
    below.replaceChildren();
    if (plan.locked) {
      el("span", "cx-quiet", acts).textContent = LOCKED_LINE;
    }
    for (const text of plan.buttons) {
      if (text === SIGN_OUT) {
        const b = el("button", "cx-sv-btn", acts);
        b.dataset.key = "signout";
        armSignOut(b, group, drawn.signal, (result) => {
          if (result.ok) {
            // A read that started before the sign-out would bring the signed-in link back: its answer is dropped.
            seq++;
            view = result.view;
            sessions = [];
            listFailed = false;
            draw();
          } else hooks.say(result.sentence);
        });
      } else if (text === SIGN_IN_LINK) {
        const b = button(acts, text);
        b.dataset.key = "signin";
        b.addEventListener(
          "click",
          () =>
            void act(async () => {
              await openServerSignIn(legend);
              toRow();
              await refresh();
              toRow();
            }),
          { signal: drawn.signal },
        );
      } else if (text === NEW_SESSION) {
        const b = button(acts, text);
        b.dataset.key = "new";
        b.addEventListener("click", () => void act(async () => showView(await source.create())), { signal: drawn.signal });
      }
    }
    if (plan.help) el("p", "cx-help", below).textContent = helpLine(null);
    if (plan.off) el("p", "cx-help", below).textContent = REMOTE_OFF;
    if (plan.list && sessions.length > 0) {
      const list = el("div", "cx-sessions", below);
      list.setAttribute("role", "list");
      list.setAttribute("aria-label", "Sessions");
      const now = new Date();
      sessions.forEach((s, at) => {
        const words = rowWords(s, at + 1, now);
        const item = el("div", "cx-sess", list);
        item.setAttribute("role", "listitem");
        item.dataset.session = String(s.id);
        el("span", "cx-sess-title", item).textContent = words.title;
        // The directory, the size, the start and the attached count: columns of the row on the desktop, one wrapped line on a phone.
        const meta = el("span", "cx-sess-meta", item);
        el("span", "cx-sess-dir", meta).textContent = words.dir;
        el("span", "cx-sess-size", meta).textContent = words.size;
        el("span", "cx-sess-start", meta).textContent = words.start;
        el("span", "cx-sess-att", meta).textContent = words.attached;
        const writer = el("span", "cx-sess-writer", item);
        const sq = el("span", "cx-sq", writer);
        sq.setAttribute("aria-hidden", "true");
        sq.dataset.shape = s.held ? "filled" : "dashed";
        el("span", "", writer).textContent = words.writer;
        const buttons = el("span", "cx-sess-acts", item);
        const attach = button(buttons, ATTACH);
        attach.dataset.key = `attach-${s.id}`;
        attach.setAttribute("aria-label", `${ATTACH} ${words.title}`);
        attach.addEventListener(
          "click",
          () =>
            void act(async () => {
              const pane = source.shownIn?.(s.id) ?? null;
              showView(pane !== null && source.focus ? await source.focus(pane) : await source.open(s.id));
            }),
          { signal: drawn.signal },
        );
        const end = el("button", "cx-sv-btn", buttons);
        end.dataset.key = `end-${s.id}`;
        armed(end, END, END_ARMED, group, drawn.signal, () =>
          act(async () => {
            await source.end(s.id);
            await refresh();
          }),
        );
        end.setAttribute("aria-label", `${END} ${words.title}`);
      });
    }
    if (focused !== undefined) group.querySelector<HTMLElement>(`[data-key="${CSS.escape(focused)}"]`)?.focus();
    toRow();
  };

  /** The focus that rests on the legend (after the sign-in dialog) moves on to the first button of the row once there is one. */
  function toRow(): void {
    if (document.activeElement === legend) acts.querySelector<HTMLElement>("button")?.focus();
  }

  /**
   * Every read has a number, and only the newest may write what it learnt: an older answer that comes back late (the host blocks on
   * the network for both calls) is dropped, and so is one that started before a sign-out. `inFlight` counts the reads still out, so
   * the timer skips a tick instead of piling calls up in the host behind a slow server.
   */
  let seq = 0;
  let inFlight = 0;

  /** The browser build: the list says it all. The host's sentence is `off`, any other failure or an unreadable answer `unreachable`. */
  const readWeb = async (mine: number): Promise<void> => {
    let raw: unknown = null;
    let off = false;
    try {
      raw = await source.list();
    } catch (failure) {
      off = isRemoteOff(failure);
    }
    if (disposed || mine !== seq) return;
    view = webView(!off);
    const read = off ? [] : readSessions(raw);
    listFailed = read === null;
    sessions = read ?? [];
    draw();
  };

  /** The desktop: read the link and, when it shows a list, the list; draw what changed. */
  const readLink = async (mine: number, fresh: boolean): Promise<void> => {
    let next: ServerView;
    try {
      next = readServerView(await source.status(fresh));
    } catch {
      next = readServerView(null);
    }
    if (disposed || mine !== seq) return;
    let rows: SessionRow[] = [];
    let failed = false;
    if (rowPlan(next, false).list) {
      let raw: unknown = null;
      let locked = false;
      try {
        raw = await source.list();
      } catch (failure) {
        // The store locked after the link was read: that is the locked row, not a server that cannot be reached.
        locked = isLockedFailure(failure);
        raw = null;
      }
      if (disposed || mine !== seq) return;
      if (locked) next = { ...next, locked: true };
      const read = locked ? [] : readSessions(raw);
      failed = read === null;
      rows = read ?? [];
    }
    view = next;
    listFailed = failed;
    sessions = rows;
    draw();
  };

  const read = async (fresh: boolean): Promise<void> => {
    const mine = ++seq;
    inFlight++;
    try {
      await (web !== null ? readWeb(mine) : readLink(mine, fresh));
    } finally {
      inFlight--;
    }
  };

  /** Read again now; a read that is still out is overtaken, and its answer dropped. */
  const refresh = (): Promise<void> => read(false);

  /** The timer: nothing while a read is out, so a server that answers slowly gets one call at a time. */
  const tick = (): void => {
    if (inFlight === 0) void read(false);
  };

  function dispose(): void {
    if (disposed) return;
    disposed = true;
    if (timer !== null) clearInterval(timer);
    timer = null;
    drawn.abort();
  }

  draw();
  void read(true);
  timer = setInterval(tick, REFRESH_MS);
  return { refresh, dispose };
}
