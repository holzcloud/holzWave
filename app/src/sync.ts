// The sync control: a button in the title bar that says the state (`☁ synced`), and the popover under it (docs/design.md
// section 6.9). The popover shows the account, the last run, the conflicts and what is synced, and runs the sync now;
// before an account exists it is the form that sets one up. The app password and the passphrase are typed into fields
// that are never filled in again; they go to the core and from there to the system keychain. Every string goes in with
// `textContent`. While the popover is open it takes the keys (the key handler stack).
import { registerAction } from "./actions.js";
import { backend, call } from "./bridge.js";
import { el, loadStyles, setText } from "./dom.js";
import { pushKeyHandler } from "./keystack.js";
import { NOT_SIGNED_IN, SIGNED_OUT, SIGN_IN_LINK, readServerView } from "./serverstate.js";
import type { ServerView } from "./serverstate.js";
import {
  CHANGE_SENTENCE, NO_ADDRESS, OFF, SERVER_ADDRESS, TRANSPORT, clock, conflictText, describeTarget, endpointText, lastText, readSync, removedText,
  syncLabel, targetTitle,
} from "./syncstate.js";
import { STRINGS, syncLine } from "./vaultstate.js";
import type { SyncStatus, Transport } from "./syncstate.js";

let status: SyncStatus = OFF;
let button: HTMLButtonElement | null = null;
let opened: Promise<void> | null = null;
let onChange: () => void = () => {};

/** Whether the store of secrets is locked as far as the last status says (a function, so that a check made before an `await` is not trusted after it). */
const paused = (): boolean => status.state === "paused";

/** What the person has typed in the set-up form; it lives in the popover and nowhere else, so a switch of the target keeps it. */
interface Draft {
  transport: Transport;
  endpoint: string;
  user: string;
  password: string;
  passphrase: string;
}

/** What the set-up form and the connected view need of the popover that holds them. */
interface Ctx {
  draft: Draft;
  /** The choice of the target is drawn: the desktop app and the phone, never the browser window (a server does not sync to a server). */
  desktop: boolean;
  /** The link to the server as the popover last read it. */
  link: () => ServerView;
  /** The state line of the server variant and its Connect button, so that an answer of the server redraws them in place. */
  target: { line: HTMLElement | null; connect: HTMLButtonElement | null; address: HTMLElement | null };
  /** Read the link (no request) and ask the server (one request) once; the answers redraw the state line. */
  load: () => void;
  /** Open the sign-in dialog; the popover is refreshed when it ends signed in. */
  signIn: () => void;
  /** Read the link again after a refused Connect, so that a sign-in that ended is said by the state line. */
  relink: () => Promise<void>;
}

function draw(): void {
  if (!button) return;
  setText(button, syncLabel(status));
  button.dataset.state = status.state;
  button.title = status.error ?? (status.configured ? `Last sync ${lastText(status)}` : "Set up encrypted sync");
  onChange();
}

/** Ask the core for the state. */
export async function refreshSync(): Promise<SyncStatus> {
  try {
    status = readSync(await call<unknown>("sync_status"));
  } catch {
    status = OFF;
  }
  draw();
  return status;
}

/** Run the sync now; the answer, or why it failed. */
export async function syncNow(): Promise<string | null> {
  try {
    status = readSync(await call<unknown>("sync_now"));
    draw();
    return null;
  } catch (e) {
    await refreshSync();
    return String(e);
  }
}

/** Open the popover. A second ask while it is open returns the same promise. */
export function openSync(): Promise<void> {
  opened ??= show().finally(() => {
    opened = null;
  });
  return opened;
}

async function show(): Promise<void> {
  try {
    await loadStyles("./dialogs.css");
    await loadStyles("./settings.css");
    await loadStyles("./sync.css");
  } catch {
    // the popover still works without its style sheets
  }
  await refreshSync();
  return new Promise<void>((resolve) => {
    const backdrop = el("div", "dlg-backdrop sync-backdrop", document.body);
    const box = el("div", "dlg sync", backdrop);
    box.setAttribute("role", "dialog");
    box.setAttribute("aria-modal", "true");
    box.setAttribute("aria-labelledby", "sy-title");
    let pop: () => void = () => {};
    let unlockTimer: ReturnType<typeof setInterval> | null = null;
    let gone = false;
    const finish = (): void => {
      gone = true;
      if (unlockTimer !== null) clearInterval(unlockTimer);
      unlockTimer = null;
      pop();
      backdrop.remove();
      resolve();
    };

    // The link to the server, for the choice of the target (desktop and phone only). The popover reads it without a request when the
    // server variant is shown and asks the server once; an answer redraws the state line in place, so nothing the person typed moves.
    let link: ServerView = SIGNED_OUT;
    let refreshed = false;
    let loaded = false;
    const ctx: Ctx = {
      draft: { transport: "webdav", endpoint: "", user: "", password: "", passphrase: "" },
      desktop: backend === "tauri",
      link: () => link,
      target: { line: null, connect: null, address: null },
      load: () => {
        if (loaded || !ctx.desktop) return;
        loaded = true;
        void (async () => {
          link = readServerView(await call<unknown>("server_status").catch(() => null));
          drawTarget();
          if (link.locked || gone) return;
          link = readServerView(await call<unknown>("server_refresh").catch(() => null));
          refreshed = true;
          if (!gone) drawTarget();
        })();
      },
      signIn: () => {
        void (async () => {
          const { openServerSignIn } = await import("./serversignin.js");
          // The link that opened the dialog may be redrawn or gone when it ends, so the focus returns to the field that comes next.
          const back = box.querySelector<HTMLElement>("#sy-passphrase") ?? box.querySelector<HTMLElement>(".sy-link");
          const view = await openServerSignIn(back);
          if (gone || view === null) return;
          link = view;
          refreshed = false;
          loaded = false;
          ctx.load();
          if (status.configured && status.state === "signed_out") {
            const why = await syncNow();
            if (!gone) render(why ?? (paused() ? "" : "Synced."), why !== null);
          } else if (!status.configured) {
            drawTarget();
            box.querySelector<HTMLElement>("#sy-passphrase")?.focus();
          }
        })();
      },
      relink: async () => {
        if (!ctx.desktop) return;
        link = readServerView(await call<unknown>("server_status").catch(() => null));
        drawTarget();
      },
    };

    /** Draw the state line of the server variant and the state of Connect from the link; the line is a status region. */
    const drawTarget = (): void => {
      const { line, connect, address } = ctx.target;
      if (!line || !connect || !line.isConnected) return;
      const d = describeTarget(link, refreshed);
      if (address) setText(address, link.url === "" ? NO_ADDRESS : new URL(link.url).host);
      fillTargetLine(line, d, ctx.signIn);
      connect.disabled = !d.connect;
      connect.dataset.reason = d.connect ? "" : "true";
      if (d.connect) connect.removeAttribute("aria-describedby");
      else connect.setAttribute("aria-describedby", line.id);
      // While the secrets are locked the dialog waits for the unlock by itself, as the sign-in dialog does.
      if (link.locked && unlockTimer === null && !gone) {
        unlockTimer = setInterval(() => {
          void call<unknown>("server_status").then(
            (raw) => {
              const now = readServerView(raw);
              if (gone || now.locked) return;
              if (unlockTimer !== null) clearInterval(unlockTimer);
              unlockTimer = null;
              loaded = false;
              refreshed = false;
              link = now;
              ctx.load();
            },
            () => {},
          );
        }, 2000);
      }
    };
    backdrop.addEventListener("pointerdown", (e) => {
      if (e.target === backdrop) finish();
    });

    const render = (message = "", failed = false): void => {
      box.replaceChildren();
      const head = el("div", "sy-head", box);
      const title = el("h2", "set-title", head);
      title.id = "sy-title";
      title.textContent = targetTitle(status.configured ? status.transport : ctx.draft.transport);
      const chip = el("span", "sy-chip", head);
      chip.dataset.state = status.state;
      chip.textContent = status.configured
        ? status.state === "error"
          ? "error"
          : status.state === "syncing"
            ? "syncing"
            : status.state === "paused"
              ? "paused"
              : status.state === "signed_out"
                ? NOT_SIGNED_IN
                : "connected"
        : "not set up";
      const note = el("p", "set-status", box);
      note.setAttribute("role", "status");
      note.id = "sy-note";
      note.dataset.failed = failed ? "true" : "false";
      if (!message && status.state === "paused") {
        // The store of secrets is locked: not an error, so the line is quiet and has a pause mark before it (docs/design.md 6.6).
        note.dataset.quiet = "true";
        const mark = el("span", "sy-pause", note);
        mark.setAttribute("aria-hidden", "true");
        mark.textContent = STRINGS.syncMark;
        note.append(syncLine(status.state));
      } else {
        note.textContent = message || status.error || "";
      }
      if (status.configured) configured(box, ctx);
      else form(box, note, render, ctx);
      const actions = el("div", "dlg-actions", box);
      actionsFor(actions, finish, note, render);
      box.querySelector<HTMLElement>("input, button")?.focus();
    };

    const actionsFor = (actions: HTMLElement, done: () => void, note: HTMLElement, again: (m?: string, f?: boolean) => void): void => {
      const closeBtn = el("button", "", actions);
      closeBtn.type = "button";
      closeBtn.textContent = "Close";
      closeBtn.addEventListener("click", done);
      if (status.configured) {
        const off = el("button", "sy-off", actions);
        off.type = "button";
        off.textContent = "Turn off";
        // While the secrets are locked the sync's own secrets cannot be removed from the vault, so turning the sync off would leave them
        // behind: the button waits, and its reason is the paused line above it, the same words as the title-bar button's (docs/design.md 6.6).
        const locked = status.state === "paused";
        off.disabled = locked;
        if (locked) off.setAttribute("aria-describedby", note.id);
        let armed = false;
        off.addEventListener("click", () => {
          if (status.state === "paused") return;
          if (!armed) {
            armed = true;
            off.textContent = "Click again to turn off";
            off.dataset.armed = "true";
            return;
          }
          void (async () => {
            status = readSync(await call<unknown>("sync_off").catch(() => null));
            draw();
            again("Sync is off. The files on the server and here stay.");
          })();
        });
        const now = el("button", "primary", actions);
        now.type = "button";
        now.textContent = "Sync now";
        now.addEventListener("click", () => {
          now.disabled = true;
          setText(note, "Syncing…");
          void syncNow().then((why) => again(why ?? (status.state === "paused" ? "" : "Synced."), why !== null));
        });
      }
    };

    pop = pushKeyHandler((e) => {
      if (e.key === "Escape") {
        e.preventDefault();
        finish();
      } else if (e.key === "Tab") {
        e.preventDefault();
        // Inside a group of radios Tab reaches the chosen one only; the arrows move between them.
        const items = Array.from(box.querySelectorAll<HTMLElement>("input, button")).filter(
          (n) => !(n as HTMLInputElement).disabled && n.offsetParent !== null && !(n instanceof HTMLInputElement && n.type === "radio" && !n.checked),
        );
        if (items.length === 0) return true;
        const at = items.indexOf(document.activeElement as HTMLElement);
        const next = e.shiftKey ? (at <= 0 ? items.length - 1 : at - 1) : at < 0 || at === items.length - 1 ? 0 : at + 1;
        items[next]!.focus();
      } else if (e.key === "Enter" && e.target instanceof HTMLInputElement) {
        e.preventDefault();
        box.querySelector<HTMLButtonElement>(".sy-connect")?.click();
      }
      return true;
    });
    render();
  });
}

function row(parent: HTMLElement, label: string, value: string, cls = ""): void {
  const r = el("div", "sy-row", parent);
  el("span", "sy-label", r).textContent = label;
  el("span", `sy-value ${cls}`.trim(), r).textContent = value;
}

function configured(box: HTMLElement, ctx: Ctx): void {
  el("p", "sy-sub", box).textContent = endpointText(status);
  if (status.transport === "server" && status.state === "signed_out") {
    // A run found the sign-in ended (revoked or run out): the state line says so, with the link, and not a generic error.
    const line = el("div", "sy-target", box);
    line.id = "sy-target";
    line.setAttribute("role", "status");
    fillTargetLine(line, { dot: "hollow", word: NOT_SIGNED_IN, bad: false, link: true, connect: false }, ctx.signIn);
  }
  row(box, "Last sync", lastText(status));
  row(box, "Encryption", "end-to-end · passphrase", "sy-ok");
  row(box, "Conflicts", conflictText(status), status.conflicts > 0 ? "sy-warn" : "");
  el("p", "label sy-group", box).textContent = "Synced";
  el("p", "sy-sub", box).textContent = "Settings · themes · connections · layouts";
  el("p", "sy-sub", box).textContent = "Secrets only inside the encrypted files";
  const removed = removedText(status);
  if (removed !== "") el("p", "sy-sub sy-removed", box).textContent = removed;
  if (status.restart) el("p", "sy-restart", box).textContent = "Changes from another computer arrived. They apply at the next start.";
}

/** The state line of the server variant: a shape, the words, and the link `Sign in…` while the device is not signed in. */
function fillTargetLine(line: HTMLElement, d: ReturnType<typeof describeTarget>, signIn: () => void): void {
  line.replaceChildren();
  line.dataset.bad = d.bad ? "true" : "false";
  const dot = el("span", `sy-dot sy-${d.dot}`, line);
  dot.setAttribute("aria-hidden", "true");
  if (d.dot === "mark") dot.textContent = "!";
  el("span", "sy-word", line).textContent = d.word;
  if (d.link) {
    const go = el("button", "sy-link", line);
    go.type = "button";
    go.textContent = SIGN_IN_LINK;
    go.addEventListener("click", signIn);
  }
}

/** The choice of the target: a group of two native radios (the arrows move between them), and the sentence that says what a change does. */
function choice(box: HTMLElement, ctx: Ctx, render: (m?: string, f?: boolean) => void): void {
  const group = el("div", "sy-choice", box);
  group.setAttribute("role", "radiogroup");
  group.setAttribute("aria-label", TRANSPORT.group);
  for (const value of ["webdav", "server"] as const) {
    const item = el("label", "sy-option", group);
    const radio = el("input", "", item);
    radio.type = "radio";
    radio.name = "sy-transport";
    radio.value = value;
    radio.checked = ctx.draft.transport === value;
    el("span", "", item).textContent = TRANSPORT[value];
    radio.addEventListener("change", () => {
      if (!radio.checked || ctx.draft.transport === value) return;
      keep(box, ctx.draft);
      ctx.draft.transport = value;
      render();
      box.querySelector<HTMLInputElement>('input[name="sy-transport"]:checked')?.focus();
    });
  }
  el("p", "sy-sub sy-change", box).textContent = CHANGE_SENTENCE;
}

/** Take what is typed into the draft, so that a redraw puts it back. */
function keep(box: HTMLElement, draft: Draft): void {
  const read = (id: string): string | null => box.querySelector<HTMLInputElement>(`#${id}`)?.value ?? null;
  draft.endpoint = read("sy-endpoint") ?? draft.endpoint;
  draft.user = read("sy-user") ?? draft.user;
  draft.password = read("sy-password") ?? draft.password;
  draft.passphrase = read("sy-passphrase") ?? draft.passphrase;
}

function form(box: HTMLElement, note: HTMLElement, render: (m?: string, f?: boolean) => void, ctx: Ctx): void {
  const server = ctx.desktop && ctx.draft.transport === "server";
  el("p", "sy-sub", box).textContent = server
    ? "Keeps settings, themes, connections and layouts the same on your computers, through your holzWave server. Everything is encrypted here with your passphrase before it leaves; the server only sees ciphertext."
    : "Keeps settings, themes, connections and layouts the same on your computers, through your Nextcloud or any WebDAV server. Everything is encrypted here with your passphrase before it leaves; the server only sees ciphertext.";
  if (ctx.desktop) choice(box, ctx, render);
  const field = (id: string, label: string, type: "text" | "password", hint: string, value: string): void => {
    const r = el("div", "set-row", box);
    const l = el("label", "", r);
    l.htmlFor = id;
    l.textContent = label;
    const input = el("input", "", r);
    input.id = id;
    input.type = type;
    input.autocomplete = "off";
    input.spellcheck = false;
    input.placeholder = hint;
    input.value = value;
  };
  ctx.target = { line: null, connect: null, address: null };
  if (server) {
    const r = el("div", "set-row sy-row-address", box);
    const label = el("span", "sy-label", r);
    label.id = "sy-address-label";
    label.textContent = SERVER_ADDRESS;
    const address = el("output", "sy-address", r);
    address.setAttribute("aria-labelledby", label.id);
    ctx.target.address = address;
    const line = el("div", "sy-target", box);
    line.id = "sy-target";
    line.setAttribute("role", "status");
    ctx.target.line = line;
  } else {
    field("sy-endpoint", "Folder address", "text", "https://cloud.example.com/remote.php/dav/files/you/holzwave", ctx.draft.endpoint);
    field("sy-user", "User", "text", "", ctx.draft.user);
    field("sy-password", "App password", "password", "", ctx.draft.password);
  }
  field("sy-passphrase", "Passphrase", "password", "at least 10 characters", ctx.draft.passphrase);
  el("p", "sy-sub", box).textContent =
    "A new passphrase is made for a new folder. For a folder that exists, type the one it was made with: it cannot be recovered if lost. The app password and the passphrase go to the system keychain.";
  const connect = el("button", "primary sy-connect", el("div", "sy-connect-row", box));
  connect.type = "button";
  connect.textContent = "Connect";
  if (server) {
    ctx.target.connect = connect;
    connect.disabled = true;
    ctx.load();
    // The link is drawn at once from what is known (no request); the answers of the server redraw it in place.
    void ctx.relink();
  }
  connect.addEventListener("click", () => {
    keep(box, ctx.draft);
    const typed = { ...ctx.draft };
    // Secrets are not put back after an answer, whatever it is; the choice and the folder address and user stay.
    ctx.draft.password = "";
    ctx.draft.passphrase = "";
    connect.disabled = true;
    setText(note, "Connecting…");
    note.dataset.failed = "false";
    void (async () => {
      try {
        status = readSync(
          await (server
            ? call<unknown>("sync_use_server", { passphrase: typed.passphrase })
            : call<unknown>("sync_save", {
                endpoint: typed.endpoint,
                user: typed.user,
                password: typed.password,
                passphrase: typed.passphrase,
              })),
        );
        draw();
        ctx.draft.endpoint = "";
        ctx.draft.user = "";
        render(`Connected. Last sync ${clock(status.last ?? Math.floor(Date.now() / 1000))}.`);
      } catch (e) {
        if (server) await ctx.relink();
        render(String(e), true);
      }
    })();
  });
}

/**
 * Add the button to the title bar, register the actions and start the poll. `changed` is called when the label changed
 * (so the bar fits again).
 */
export async function initSync(changed: () => void): Promise<void> {
  onChange = changed;
  const info = document.querySelector(".bar-info");
  if (info) {
    button = el("button", "sync-btn label");
    button.type = "button";
    button.addEventListener("click", () => void openSync());
    info.prepend(button);
  }
  registerAction({ id: "sync.open", title: "Sync…", run: () => void openSync() });
  registerAction({
    id: "sync.now",
    title: "Sync now",
    run: () => {
      if (status.configured) void syncNow();
      else void openSync();
    },
  });
  await refreshSync();
  setInterval(() => void refreshSync(), 5000);
}
