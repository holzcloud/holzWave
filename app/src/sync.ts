// The sync control: a button in the title bar that says the state (`☁ synced`), and the popover under it (docs/design.md
// section 6.9). The popover shows the account, the last run, the conflicts and what is synced, and runs the sync now;
// before an account exists it is the form that sets one up. The app password and the passphrase are typed into fields
// that are never filled in again; they go to the core and from there to the system keychain. Every string goes in with
// `textContent`. While the popover is open it takes the keys (the key handler stack).
import { registerAction } from "./actions.js";
import { call } from "./bridge.js";
import { el, loadStyles, setText } from "./dom.js";
import { pushKeyHandler } from "./keystack.js";
import { OFF, clock, conflictText, endpointText, lastText, readSync, removedText, syncLabel } from "./syncstate.js";
import type { SyncStatus } from "./syncstate.js";

let status: SyncStatus = OFF;
let button: HTMLButtonElement | null = null;
let opened: Promise<void> | null = null;
let onChange: () => void = () => {};

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
    const finish = (): void => {
      pop();
      backdrop.remove();
      resolve();
    };
    backdrop.addEventListener("pointerdown", (e) => {
      if (e.target === backdrop) finish();
    });

    const render = (message = "", failed = false): void => {
      box.replaceChildren();
      const head = el("div", "sy-head", box);
      const title = el("h2", "set-title", head);
      title.id = "sy-title";
      title.textContent = "Nextcloud";
      const chip = el("span", "sy-chip", head);
      chip.dataset.state = status.state;
      chip.textContent = status.configured ? (status.state === "error" ? "error" : status.state === "syncing" ? "syncing" : "connected") : "not set up";
      const note = el("p", "set-status", box);
      note.setAttribute("role", "status");
      note.dataset.failed = failed ? "true" : "false";
      note.textContent = message || status.error || "";
      if (status.configured) configured(box, note, render);
      else form(box, note, render);
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
        let armed = false;
        off.addEventListener("click", () => {
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
          void syncNow().then((why) => again(why ?? "Synced.", why !== null));
        });
      }
    };

    pop = pushKeyHandler((e) => {
      if (e.key === "Escape") {
        e.preventDefault();
        finish();
      } else if (e.key === "Tab") {
        e.preventDefault();
        const items = Array.from(box.querySelectorAll<HTMLElement>("input, button")).filter(
          (n) => !(n as HTMLInputElement).disabled && n.offsetParent !== null,
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

function configured(box: HTMLElement, _note: HTMLElement, _render: (m?: string, f?: boolean) => void): void {
  el("p", "sy-sub", box).textContent = endpointText(status);
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

function form(box: HTMLElement, note: HTMLElement, render: (m?: string, f?: boolean) => void): void {
  el("p", "sy-sub", box).textContent =
    "Keeps settings, themes, connections and layouts the same on your computers, through your Nextcloud or any WebDAV server. Everything is encrypted here with your passphrase before it leaves; the server only sees ciphertext.";
  const fields: [string, string, "text" | "password", string][] = [
    ["sy-endpoint", "Folder address", "text", "https://cloud.example.com/remote.php/dav/files/you/holzwave"],
    ["sy-user", "User", "text", ""],
    ["sy-password", "App password", "password", ""],
    ["sy-passphrase", "Passphrase", "password", "at least 10 characters"],
  ];
  const inputs: HTMLInputElement[] = [];
  for (const [id, label, type, hint] of fields) {
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
    inputs.push(input);
  }
  el("p", "sy-sub", box).textContent =
    "A new passphrase is made for a new folder. For a folder that exists, type the one it was made with: it cannot be recovered if lost. The app password and the passphrase go to the system keychain.";
  const connect = el("button", "primary sy-connect", el("div", "sy-connect-row", box));
  connect.type = "button";
  connect.textContent = "Connect";
  connect.addEventListener("click", () => {
    connect.disabled = true;
    setText(note, "Connecting…");
    note.dataset.failed = "false";
    void (async () => {
      try {
        status = readSync(
          await call<unknown>("sync_save", {
            endpoint: inputs[0]!.value,
            user: inputs[1]!.value,
            password: inputs[2]!.value,
            passphrase: inputs[3]!.value,
          }),
        );
        draw();
        render(`Connected. Last sync ${clock(status.last ?? Math.floor(Date.now() / 1000))}.`);
      } catch (e) {
        connect.disabled = false;
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
