// Signing this app in to a holzWave server (docs/design.md 6.35, "The step in the app"): a small dialog of two screens. The first asks
// for the server's address once; the second shows the code to compare, has the system browser opened for the approval, and ends in
// one of the record's states. The dialog is opened by the sync popover (6.9) and by the server row (6.34); it is built once here.
//
//   server_status()                    the link as the host last knew it, no request
//   server_sign_in_start({address})    the code and the address to open (the host builds it from the origin the person typed)
//   server_sign_in_poll()              one poll: the state of the wait and the seconds to leave before the next
//   server_sign_in_cancel()            drop the sign-in in progress; nothing was stored
//   server_sign_out()                  forget the token, keep the address
//   open_url({url, pane: null})        the system browser, with the address the host returned and no other
//
// The page never holds a secret: the host's views have no member for a token or a device code, and the code on the screen is the user
// code only. Every string is put in with `textContent`; the host's own sentences are bounded first. A timer lives only while the wait
// does, and every way out (Cancel, Escape, a failure, an approval) clears it. A click outside does nothing, as in the other layers.
import { call } from "./bridge.js";
import { el } from "./dom.js";
import {
  ADDRESS_HELP,
  ADDRESS_LABEL,
  ADDRESS_REFUSED,
  CANCEL,
  CLOSE,
  CONTINUE,
  COMPARE,
  DEVICE_LABEL,
  LOCKED_LINE,
  OPENING,
  OPEN_AGAIN,
  Pacer,
  SIGN_IN_TITLE,
  SIGN_OUT,
  SIGN_OUT_ARMED,
  START_AGAIN,
  checkAddress,
  isFailure,
  plain,
  readPoll,
  readServerView,
  readSignIn,
  refusalOf,
  waitWords,
  MAX_SENTENCE,
} from "./serverstate.js";
import type { ServerView, SignInView, WaitState } from "./serverstate.js";
import { openLayer } from "./weblayer.js";

/** The style sheets of the dialog: the web dialogs' box, field and buttons, and the rules of this file. */
const SHEETS = ["./websession.css", "./serversignin.css"];
/** How often the dialog looks again while the store of secrets is locked, in milliseconds. */
const UNLOCK_POLL_MS = 2000;
const SERVER_LABEL = "Server";

/** The dialog that is open, so that a second ask returns the same promise. */
let current: Promise<ServerView | null> | null = null;

/** Read the host's answer to a command that answers a link; a failure to ask is the safe view. */
async function status(): Promise<ServerView> {
  try {
    return readServerView(await call<unknown>("server_status"));
  } catch {
    return readServerView(null);
  }
}

/**
 * Open the dialog. It resolves when the dialog closes: with the link after a sign-in that was approved, and with null after Cancel,
 * Escape or a failure the person left. A second ask while it is open returns the same promise. The focus goes back to `returnTo`
 * (the element that was focused when the dialog opened without one), so a caller whose opener is about to be redrawn names its own.
 */
export function openServerSignIn(returnTo?: HTMLElement | null): Promise<ServerView | null> {
  current ??= run(returnTo).finally(() => {
    current = null;
  });
  return current;
}

async function run(returnTo?: HTMLElement | null): Promise<ServerView | null> {
  let view = await status();
  return new Promise<ServerView | null>((resolve) => {
    void (async () => {
      let closed = false;
      let approved = false;
      let locked = view.locked;
      /** Counts every start of a sign-in: an answer that comes back after a newer start or an exit is dropped. */
      let generation = 0;
      let grant: SignInView | null = null;
      let wait: WaitState | null = null;
      let sentence = "";
      let timer: ReturnType<typeof setTimeout> | null = null;
      let unlockTimer: ReturnType<typeof setInterval> | null = null;
      let pacer: Pacer | null = null;
      let screen: 1 | 2 = 1;
      let typed = view.url;
      let openNote = "";
      let started = false;
      /** Starts of a sign-in whose answer has not come back yet. */
      let starting = 0;

      const layer = await openLayer({
        cls: "server-signin",
        labelledBy: "ss-title",
        sheets: SHEETS,
        returnTo,
        onEscape: () => leave(),
        onEnter: () => {
          if (screen === 1) submit();
        },
      });
      const box = layer.box;

      function stopTimers(): void {
        if (timer !== null) clearTimeout(timer);
        timer = null;
        if (unlockTimer !== null) clearInterval(unlockTimer);
        unlockTimer = null;
      }

      /** Close the dialog and give the result back. Nothing stays behind: the host's pending request is dropped unless it was approved. */
      function done(result: ServerView | null): void {
        if (closed) return;
        closed = true;
        generation++;
        stopTimers();
        layer.close();
        resolve(result);
      }

      /** Cancel, Escape and the way out of a failure: drop the host's request and close. With the store locked and nothing started, nothing is called. */
      function leave(): void {
        if (closed) return;
        if (approved) {
          void status().then((v) => done(v));
          return;
        }
        if (grant === null && locked && !started) return done(null);
        void call("server_sign_in_cancel").catch(() => {});
        done(null);
      }

      // ---- the lock: the buttons that need the store give way to a line, and return after the unlock ---------------------------

      function watchLock(): void {
        if (!locked || unlockTimer !== null || closed) return;
        unlockTimer = setInterval(() => {
          void status().then((now) => {
            if (closed || !locked || now.locked) return;
            locked = false;
            view = now;
            // The locked sentence of a refused approval is not true any more: the grant it ended is gone, so the state is the plain
            // failure with `Start again` (a start refused as locked has no state and gets `Start again` from drawActions).
            if (sentence === LOCKED_LINE) sentence = "";
            if (unlockTimer !== null) clearInterval(unlockTimer);
            unlockTimer = null;
            draw();
          });
        }, UNLOCK_POLL_MS);
      }

      const lockedLine = (parent: HTMLElement): void => {
        const line = el("p", "ss-locked", parent);
        line.setAttribute("role", "status");
        line.textContent = LOCKED_LINE;
      };

      const button = (parent: HTMLElement, text: string, solid: boolean, on: () => void): HTMLButtonElement => {
        const b = el("button", solid ? "web-btn web-solid" : "web-btn", parent);
        b.type = "button";
        b.textContent = text;
        b.addEventListener("click", on);
        return b;
      };

      // ---- the first screen --------------------------------------------------------------------------------------------------

      /** What is under the field: the refusal in a failure's style, empty when there is none. */
      let refusal = "";

      function screenOne(): void {
        box.replaceChildren();
        const title = el("h2", "set-title", box);
        title.id = "ss-title";
        title.textContent = SIGN_IN_TITLE;
        const field = el("div", "web-field", box);
        const label = el("label", "label web-label", field);
        label.htmlFor = "ss-address";
        label.textContent = ADDRESS_LABEL;
        const input = el("input", "web-input ss-address", field);
        input.id = "ss-address";
        input.type = "text";
        input.value = typed;
        input.maxLength = 253;
        input.setAttribute("autocomplete", "off");
        input.setAttribute("autocapitalize", "none");
        input.setAttribute("autocorrect", "off");
        input.spellcheck = false;
        input.inputMode = "url";
        const help = el("p", "ss-help", box);
        help.id = "ss-help";
        help.textContent = ADDRESS_HELP;
        const err = el("p", "web-err ss-refusal", box);
        err.id = "ss-refusal";
        err.setAttribute("role", "alert");
        err.textContent = refusal;
        const described = refusal === "" ? "ss-help" : "ss-help ss-refusal";
        input.setAttribute("aria-describedby", described);
        if (refusal !== "") input.setAttribute("aria-invalid", "true");
        input.addEventListener("input", () => {
          typed = input.value;
          if (refusal !== "") {
            refusal = "";
            err.textContent = "";
            input.removeAttribute("aria-invalid");
            input.setAttribute("aria-describedby", "ss-help");
          }
        });
        const kv = el("div", "ss-kv", box);
        el("span", "ss-k", kv).textContent = DEVICE_LABEL;
        el("span", "ss-v ss-mono", kv).textContent = view.name === "" ? "–" : view.name;
        const actions = el("div", "dlg-actions", box);
        button(actions, CANCEL, false, leave);
        if (locked) lockedLine(actions);
        else button(actions, CONTINUE, true, submit);
        input.focus();
        input.setSelectionRange(input.value.length, input.value.length);
      }

      function submit(): void {
        // A second Enter (or a held key) while the start is out would ask the server for a second code.
        if (closed || locked || screen !== 1 || starting > 0) return;
        const input = box.querySelector<HTMLInputElement>("#ss-address");
        const checked = checkAddress(input?.value ?? typed);
        if (!checked.ok) {
          refusal = ADDRESS_REFUSED;
          screenOne();
          return;
        }
        typed = checked.address;
        void begin(checked.address);
      }

      // ---- starting a sign-in -------------------------------------------------------------------------------------------------

      /** Start (or start again) at `address`. A failure is said on the screen the person is on. */
      async function begin(address: string): Promise<void> {
        starting++;
        try {
          await start(address);
        } finally {
          starting--;
        }
      }

      async function start(address: string): Promise<void> {
        const mine = ++generation;
        started = true;
        stopTimers();
        grant = null;
        wait = null;
        sentence = "";
        openNote = "";
        if (screen === 1) {
          const go = box.querySelector<HTMLButtonElement>(".web-solid");
          if (go) go.disabled = true;
        } else {
          draw();
        }
        let answer: unknown;
        try {
          answer = await call<unknown>("server_sign_in_start", { address });
        } catch (failure) {
          if (mine !== generation) return;
          fail(refusalOf(failure));
          return;
        }
        if (mine !== generation) {
          // Closed (or replaced) while the request was out: the host keeps a pending request now, so drop it again.
          if (closed) void call("server_sign_in_cancel").catch(() => {});
          return;
        }
        const read = readSignIn(answer);
        if (read === null) {
          fail({ kind: "sentence", text: "" });
          return;
        }
        grant = read;
        wait = "waiting";
        screen = 2;
        view = { ...view, url: address, state: "signing_in" };
        draw();
        openBrowser(mine, true);
        pacer = new Pacer(performance.now(), read.interval);
        schedule(mine);
      }

      /** A failure to start: on the first screen the refusal line, on the second the failed state. */
      function fail(refused: ReturnType<typeof refusalOf>): void {
        if (refused.kind === "locked") {
          locked = true;
          watchLock();
          if (screen === 2) {
            wait = null;
            draw();
          } else {
            refusal = "";
            screenOne();
          }
          return;
        }
        const state: WaitState = refused.kind === "state" ? refused.state : "failed";
        const text = refused.kind === "sentence" ? refused.text : "";
        if (screen === 1) {
          refusal = waitWords(state, text).word;
          screenOne();
          return;
        }
        wait = state;
        sentence = text;
        draw();
      }

      /** Ask the system browser for the address the host returned. Once at the start, and again only on `Open again`. */
      function openBrowser(mine: number, first: boolean): void {
        if (grant === null) return;
        if (first) openNote = OPENING;
        drawNote();
        const url = grant.openUrl;
        void call("open_url", { url, pane: null }).then(
          () => {
            if (mine !== generation || closed) return;
            openNote = "";
            drawNote();
          },
          (failure: unknown) => {
            if (mine !== generation || closed) return;
            openNote = plain(typeof failure === "string" ? failure : "", MAX_SENTENCE) || "The browser could not be opened.";
            drawNote();
          },
        );
      }

      // ---- the wait -----------------------------------------------------------------------------------------------------------

      function schedule(mine: number): void {
        if (mine !== generation || closed || pacer === null) return;
        timer = setTimeout(() => void tick(mine), pacer.wait(performance.now()));
      }

      async function tick(mine: number): Promise<void> {
        timer = null;
        if (mine !== generation || closed || pacer === null || wait !== "waiting") return;
        if (!pacer.due(performance.now())) return schedule(mine);
        pacer.polled(performance.now(), pacer.seconds);
        let poll: ReturnType<typeof readPoll>;
        try {
          poll = readPoll(await call<unknown>("server_sign_in_poll"));
        } catch (failure) {
          if (mine !== generation || closed) return;
          const refused = refusalOf(failure);
          if (refused.kind === "locked") {
            locked = true;
            wait = "failed";
            sentence = LOCKED_LINE;
            watchLock();
          } else {
            wait = refused.kind === "state" ? refused.state : "failed";
            sentence = refused.kind === "sentence" ? refused.text : "";
          }
          draw();
          return;
        }
        if (mine !== generation || closed) return;
        pacer.polled(performance.now(), poll.interval);
        if (poll.state === "waiting") return schedule(mine);
        wait = poll.state;
        if (poll.state === "approved") {
          approved = true;
          view = await status();
          if (mine !== generation || closed) return;
        }
        draw();
      }

      // ---- the second screen ----------------------------------------------------------------------------------------------------

      let note: HTMLElement | null = null;
      let stateLine: HTMLElement | null = null;
      let actions2: HTMLElement | null = null;
      let codeNode: HTMLElement | null = null;

      function screenTwo(): void {
        box.replaceChildren();
        const title = el("h2", "set-title", box);
        title.id = "ss-title";
        title.textContent = SIGN_IN_TITLE;
        title.tabIndex = -1;
        codeNode = el("div", "ss-code", box);
        codeNode.setAttribute("aria-label", "Code");
        const kv = el("div", "ss-kv", box);
        el("span", "ss-k", kv).textContent = SERVER_LABEL;
        el("span", "ss-v ss-mono", kv).textContent = view.url === "" ? "–" : new URL(view.url).host;
        note = el("p", "ss-note", box);
        el("p", "ss-note ss-compare", box).textContent = COMPARE;
        stateLine = el("div", "ss-state", box);
        stateLine.setAttribute("role", "status");
        actions2 = el("div", "dlg-actions", box);
        title.focus();
      }

      function drawNote(): void {
        if (note === null) return;
        note.textContent = openNote;
        note.hidden = openNote === "";
        if (openNote !== "" && openNote !== OPENING) note.dataset.bad = "true";
        else delete note.dataset.bad;
      }

      function drawState(): void {
        if (stateLine === null) return;
        stateLine.replaceChildren();
        delete stateLine.dataset.bad;
        if (wait === null) {
          stateLine.hidden = true;
          return;
        }
        stateLine.hidden = false;
        const words = waitWords(wait, sentence);
        stateLine.dataset.state = wait;
        const dot = el("span", `ss-dot ss-${words.dot}`, stateLine);
        dot.setAttribute("aria-hidden", "true");
        if (words.dot === "mark") dot.textContent = "!";
        const word = el("span", "ss-word", stateLine);
        word.textContent = words.word;
        if (words.bad) stateLine.dataset.bad = "true";
      }

      function drawActions(): void {
        if (actions2 === null) return;
        const had = actions2.contains(document.activeElement);
        actions2.replaceChildren();
        let first: HTMLButtonElement | null = null;
        if (wait === "approved") {
          first = button(actions2, CLOSE, true, () => leave());
        } else {
          // The buttons that need the store of secrets give way to the dim line while it is locked; Cancel stays.
          if (locked) {
            if (sentence !== LOCKED_LINE) lockedLine(actions2);
          } else if (wait === "waiting" && grant !== null) {
            first = button(actions2, OPEN_AGAIN, false, () => openBrowser(generation, false));
          } else if ((wait !== null && isFailure(wait)) || (wait === null && starting === 0)) {
            // After a failure, and after a start that was refused before it had a state (the store was locked) once it is unlocked.
            first = button(actions2, START_AGAIN, false, () => void begin(view.url !== "" ? view.url : typed));
          }
          button(actions2, CANCEL, false, leave);
        }
        // A button that was under the focus is gone, and a state that ends the wait is where the person looks next: the keyboard goes
        // to the first of the new buttons (or Cancel), so it is never lost.
        const ended = wait !== null && wait !== "waiting";
        if (had || ended) (first ?? actions2.querySelector<HTMLButtonElement>("button"))?.focus();
      }

      function draw(): void {
        if (closed) return;
        if (screen === 1) {
          screenOne();
          return;
        }
        if (codeNode === null || !box.contains(codeNode)) screenTwo();
        if (codeNode !== null) {
          codeNode.textContent = grant?.code ?? "";
          codeNode.hidden = grant === null;
        }
        drawNote();
        drawState();
        drawActions();
      }

      // ---- the start ----------------------------------------------------------------------------------------------------------

      watchLock();
      if (view.url !== "" && !locked) {
        // The address is known (a sign-out keeps it), so the first screen is not shown again: the request starts at once.
        screen = 2;
        void begin(view.url); // draws the second screen before its first wait
      } else {
        draw();
      }
    })();
  });
}

// ---- Sign out ---------------------------------------------------------------------------------------------------------------------

/** The result of Sign out: the link afterwards, or the sentence that says why it was refused. */
export type SignOutResult = { ok: true; view: ServerView } | { ok: false; sentence: string };

/** Sign this device out of the server: the token goes, the address stays. The host refuses with a fixed sentence while the secrets are locked. */
export async function signOut(): Promise<SignOutResult> {
  try {
    return { ok: true, view: readServerView(await call<unknown>("server_sign_out")) };
  } catch (failure) {
    const refused = refusalOf(failure);
    return { ok: false, sentence: refused.kind === "locked" ? LOCKED_LINE : refused.kind === "state" ? waitWords(refused.state).word : refused.text };
  }
}

/**
 * Make `button` the `Sign out` of a row: the first press changes its words and its border (`Click again to sign out`), a press or the
 * focus anywhere else in `scope` (the dialog the row is in), or Escape, puts it back (that Escape goes no further, so it does not also
 * close the dialog), and only the second press signs out, in the manner of `Turn off` of the sync
 * popover. `finished` gets the result. It is armed by a press and never by the focus, so the first press never calls the command.
 */
export function armSignOut(
  button: HTMLButtonElement,
  scope: HTMLElement,
  signal: AbortSignal,
  finished: (result: SignOutResult) => void,
): void {
  button.type = "button";
  button.textContent = SIGN_OUT;
  button.dataset.armed = "false";
  const off = (): void => {
    button.dataset.armed = "false";
    button.textContent = SIGN_OUT;
  };
  const away = (e: Event): void => {
    if (e.target !== button) off();
  };
  scope.addEventListener("focusin", away, { signal });
  scope.addEventListener("pointerdown", away, { signal });
  // On the document, not the scope: a button that a press did not focus (as in WebKit on a Mac) leaves the key on the page.
  document.addEventListener(
    "keydown",
    (e) => {
      if (e.key !== "Escape" || button.dataset.armed !== "true") return;
      // This Escape only puts the button back: the dialog around it stays open.
      e.preventDefault();
      e.stopPropagation();
      off();
    },
    { signal, capture: true },
  );
  button.addEventListener(
    "click",
    () => {
      if (button.dataset.armed !== "true") {
        button.dataset.armed = "true";
        button.textContent = SIGN_OUT_ARMED;
        return;
      }
      off();
      button.disabled = true;
      void signOut().then((result) => {
        button.disabled = false;
        finished(result);
      });
    },
    { signal },
  );
}
