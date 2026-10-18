// The connection manager dialog: the saved connections on the left, the form of the chosen one on the right. It
// shows what the core listed (`connections()`), sends `connection_save` and `connection_delete`, and opens a
// connection with `connect`. The password or key passphrase is typed into a field that is never filled in again:
// the form only says whether one is saved, and a new value replaces it. Every string goes in with `textContent`.
// While it is open it consumes the keys (the key handler stack).
import { call } from "./bridge.js";
import { connect, connections, refreshConnections } from "./connections.js";
import type { Connection } from "./connstate.js";
import { el, loadStyles, setText } from "./dom.js";
import { pushKeyHandler } from "./keystack.js";
import { INTERACTIVE_HINT } from "./loginpromptstate.js";

let opened: Promise<void> | null = null;

/** Open the dialog. A second ask while it is open returns the same promise. Resolves when it is closed. */
export function openConnections(): Promise<void> {
  opened ??= show().finally(() => {
    opened = null;
  });
  return opened;
}

/** A connection that has not been saved yet: the form's starting values. */
export function blankConnection(): Connection {
  return {
    id: "",
    name: "",
    kind: "ssh",
    host: "",
    port: 22,
    user: "",
    auth: "key",
    key_path: DEFAULT_KEY_FILE,
    jump: null,
    startup: "",
    reconnect_attempts: 5,
    reconnect_delay_secs: 3,
    shell: "",
    wsh: false,
    has_secret: false,
  };
}

/** The words under the `wsh` checkbox: what ticking it allows and what it never allows. */
export const WSH_NOTE =
  "A program on this host can then open files and rename this pane in holzWave through a private tunnel. It can never use the AI or approve anything. The host must allow Unix socket forwarding.";

/** The words under the form of a connection that logs in through the ssh-agent. */
export const AGENT_HINT = "The ssh-agent signs the login. Nothing is stored. The public key file picks one of its keys.";

/** The key file a new key login starts with. */
const DEFAULT_KEY_FILE = "~/.ssh/id_ed25519";

/** The label of the host field for each kind of connection. */
export function hostLabel(kind: Connection["kind"]): string {
  return kind === "wsl" ? "Distribution" : kind === "docker" ? "Container" : "Host";
}

async function show(): Promise<void> {
  try {
    await loadStyles("./dialogs.css");
    await loadStyles("./settings.css");
    await loadStyles("./connections.css");
  } catch {
    // the dialog still works without its style sheets
  }
  await refreshConnections();

  return new Promise<void>((resolve) => {
    const backdrop = el("div", "dlg-backdrop", document.body);
    const box = el("div", "dlg conns", backdrop);
    box.setAttribute("role", "dialog");
    box.setAttribute("aria-modal", "true");
    box.setAttribute("aria-labelledby", "cx-title");

    const title = el("h2", "set-title", box);
    title.id = "cx-title";
    title.textContent = "Connections";
    const statusLine = el("p", "set-status", box);
    statusLine.setAttribute("role", "status");

    const body = el("div", "cx-body", box);
    const list = el("div", "cx-list", body);
    list.setAttribute("role", "group");
    list.setAttribute("aria-label", "Saved connections");
    const form = el("form", "cx-form", body);
    form.addEventListener("submit", (e) => e.preventDefault());

    // ---- the form: one row per field; a row says for which kinds of connection it shows ----

    const rows: { row: HTMLElement; kinds: string }[] = [];
    const textRow = (id: string, label: string, kinds: string, type: "text" | "password" | "number" = "text") => {
      const row = el("div", "set-row", form);
      const l = el("label", "", row);
      l.htmlFor = id;
      l.textContent = label;
      const input = el("input", "", row);
      input.id = id;
      input.type = type;
      input.autocomplete = "off";
      input.spellcheck = false;
      rows.push({ row, kinds });
      return { input, label: l };
    };
    const choiceRow = (name: string, label: string, kinds: string, options: [string, string][]) => {
      const row = el("div", "cx-row-choice", form);
      row.setAttribute("role", "radiogroup");
      row.setAttribute("aria-label", label);
      el("span", "cx-label", row).textContent = label;
      const radios: HTMLInputElement[] = [];
      for (const [value, text] of options) {
        const wrap = el("label", "cx-option", row);
        const radio = el("input", "", wrap);
        radio.type = "radio";
        radio.name = name;
        radio.value = value;
        el("span", "", wrap).textContent = text;
        radios.push(radio);
      }
      rows.push({ row, kinds });
      return { row, radios };
    };

    const name = textRow("cx-name", "Name", "ssh wsl docker");
    const kind = choiceRow("cx-kind", "Type", "ssh wsl docker", [["ssh", "SSH"], ["wsl", "WSL"], ["docker", "Docker"]]);
    const host = textRow("cx-host", "Host", "ssh wsl docker");
    const port = textRow("cx-port", "Port", "ssh", "number");
    const user = textRow("cx-user", "User", "ssh");
    const auth = choiceRow("cx-auth", "Login", "ssh", [["key", "Key file"], ["password", "Password"], ["agent", "Agent"], ["interactive", "Code or password prompt"]]);
    const keyPath = textRow("cx-key", "Key file", "ssh");
    const secret = textRow("cx-secret", "Passphrase", "ssh", "password");
    const clearRow = el("div", "cx-row-choice", form);
    const clearWrap = el("label", "cx-option", clearRow);
    const clear = el("input", "", clearWrap);
    clear.type = "checkbox";
    clear.id = "cx-clear";
    const clearText = el("span", "", clearWrap);
    rows.push({ row: clearRow, kinds: "ssh" });
    const jump = choiceRow("cx-jump", "Jump host", "ssh", [["", "None"]]);
    const startup = textRow("cx-startup", "Startup line", "ssh");
    const attempts = textRow("cx-attempts", "Reconnects", "ssh", "number");
    const delay = textRow("cx-delay", "Seconds between", "ssh", "number");
    const shell = textRow("cx-shell", "Shell", "docker");
    const wshRow = el("div", "cx-row-choice", form);
    const wshWrap = el("label", "cx-option", wshRow);
    const wsh = el("input", "", wshWrap);
    wsh.type = "checkbox";
    wsh.id = "cx-wsh";
    wsh.setAttribute("aria-describedby", "cx-wsh-note");
    el("span", "", wshWrap).textContent = "Use wsh on this host";
    rows.push({ row: wshRow, kinds: "ssh" });
    const wshNote = el("p", "cx-hint", form);
    wshNote.id = "cx-wsh-note";
    wshNote.textContent = WSH_NOTE;
    rows.push({ row: wshNote, kinds: "ssh" });
    const hint = el("p", "cx-hint", form);

    // ---- the state of the dialog ----

    /** The connection the form shows; `blank` for one that is not saved. */
    let current: Connection = blankConnection();
    let armed = false;

    const kindValue = (): Connection["kind"] => (kind.radios.find((r) => r.checked)?.value ?? "ssh") as Connection["kind"];
    const authValue = (): Connection["auth"] => {
      const value = auth.radios.find((r) => r.checked)?.value;
      return value === "password" || value === "agent" || value === "interactive" ? value : "key";
    };

    // The row below the login holds a private key file for a key or a password login and an optional public key file for
    // the agent; each keeps what was typed while the other is chosen.
    let keyFile = DEFAULT_KEY_FILE;
    let publicFile = "";
    let shownAuth: Connection["auth"] = "key";
    const swapKeyRow = (): void => {
      const next = authValue();
      if (next === shownAuth) return;
      if (shownAuth === "agent") publicFile = keyPath.input.value;
      else keyFile = keyPath.input.value;
      if (next === "agent") keyPath.input.value = publicFile;
      else if (shownAuth === "agent") keyPath.input.value = keyFile;
      shownAuth = next;
    };

    const syncRows = (): void => {
      const k = kindValue();
      for (const { row, kinds } of rows) row.hidden = !kinds.split(" ").includes(k);
      setText(host.label, hostLabel(k));
      const login = authValue();
      const password = login === "password";
      const agent = login === "agent";
      // The agent and the prompt hold no secret of the connection: no passphrase row and nothing to remove.
      const noSecret = agent || login === "interactive";
      keyPath.input.parentElement!.hidden = k !== "ssh" || password || login === "interactive";
      setText(keyPath.label, agent ? "Public key (optional)" : "Key file");
      secret.input.parentElement!.hidden = k !== "ssh" || noSecret;
      setText(secret.label, password ? "Password" : "Passphrase");
      setText(clearText, password ? "Remove the saved password" : "Remove the saved passphrase");
      clearRow.hidden = k !== "ssh" || !current.has_secret || noSecret;
      setText(
        hint,
        k === "ssh"
          ? login === "interactive"
            ? INTERACTIVE_HINT
            : agent
              ? AGENT_HINT
              : current.has_secret
              ? "A secret is saved in the system keychain. Type a new one to replace it, or leave the field empty."
              : "The password or passphrase goes to the system keychain, never to a file."
          : k === "wsl"
            ? "Opens a shell in this WSL distribution. Windows only."
            : "Opens a shell in this running container with docker exec.",
      );
    };

    const fill = (c: Connection): void => {
      current = c;
      armed = false;
      for (const r of kind.radios) r.checked = r.value === c.kind;
      for (const r of auth.radios) r.checked = r.value === c.auth;
      name.input.value = c.name;
      host.input.value = c.host;
      port.input.value = String(c.port);
      user.input.value = c.user;
      shownAuth = c.auth;
      keyFile = c.auth === "agent" ? DEFAULT_KEY_FILE : c.key_path;
      publicFile = c.auth === "agent" ? c.key_path : "";
      keyPath.input.value = c.key_path;
      secret.input.value = "";
      clear.checked = false;
      startup.input.value = c.startup;
      attempts.input.value = String(c.reconnect_attempts);
      delay.input.value = String(c.reconnect_delay_secs);
      shell.input.value = c.shell;
      wsh.checked = c.wsh;
      // The jump host choices: every other SSH connection.
      for (const old of Array.from(jump.row.querySelectorAll("label.cx-option")).slice(1)) old.remove();
      jump.radios.splice(1);
      for (const other of connections().filter((o) => o.kind === "ssh" && o.id !== c.id)) {
        const wrap = el("label", "cx-option", jump.row);
        const radio = el("input", "", wrap);
        radio.type = "radio";
        radio.name = "cx-jump";
        radio.value = other.id;
        el("span", "", wrap).textContent = other.name;
        jump.radios.push(radio);
      }
      for (const r of jump.radios) r.checked = r.value === (c.jump ?? "");
      syncRows();
      renderButtons();
      renderList();
    };

    const read = (): Connection => ({
      id: current.id,
      name: name.input.value.trim(),
      kind: kindValue(),
      host: host.input.value.trim(),
      port: Number.parseInt(port.input.value, 10) || 22,
      user: user.input.value.trim(),
      auth: authValue(),
      key_path: keyPath.input.value.trim(),
      jump: jump.radios.find((r) => r.checked)?.value || null,
      startup: startup.input.value,
      reconnect_attempts: Math.max(0, Number.parseInt(attempts.input.value, 10) || 0),
      reconnect_delay_secs: Math.max(0, Number.parseInt(delay.input.value, 10) || 0),
      shell: shell.input.value.trim(),
      // Only an SSH connection can carry it: a hidden box that was ticked before the type changed sends false.
      wsh: kindValue() === "ssh" && wsh.checked,
      has_secret: current.has_secret,
    });

    // ---- the list ----

    const renderList = (): void => {
      list.replaceChildren();
      const saved = connections();
      if (saved.length === 0) el("p", "cx-empty", list).textContent = "No connections yet.";
      for (const c of saved) {
        const b = el("button", "", list);
        b.type = "button";
        b.setAttribute("aria-current", c.id === current.id ? "true" : "false");
        el("span", "cx-name", b).textContent = c.name;
        el("span", "cx-tag", b).textContent = c.kind;
        b.addEventListener("click", () => fill(c));
      }
      const add = el("button", "cx-new", list);
      add.type = "button";
      add.textContent = "＋ New connection";
      add.addEventListener("click", () => {
        fill(blankConnection());
        name.input.focus();
      });
    };

    // ---- the buttons ----

    const actions = el("div", "dlg-actions", box);
    const del = el("button", "cx-danger", actions);
    del.type = "button";
    const save = el("button", "", actions);
    save.type = "button";
    save.textContent = "Save";
    const open = el("button", "dlg-ok", actions);
    open.type = "button";
    open.textContent = "Connect";
    const close = el("button", "", actions);
    close.type = "button";
    close.textContent = "Close";

    const renderButtons = (): void => {
      del.disabled = current.id === "";
      del.dataset.armed = armed ? "true" : "false";
      del.textContent = armed ? "Click again to delete" : "Delete";
    };

    // An agent or an interactive login has no secret: whatever was typed before the login was chosen is not sent.
    const secretChange = (): { kind: string; value?: string } =>
      authValue() === "agent" || authValue() === "interactive"
        ? { kind: "keep" }
        : secret.input.value !== ""
          ? { kind: "set", value: secret.input.value }
          : clear.checked
            ? { kind: "clear" }
            : { kind: "keep" };

    /** Save the form; resolves to the saved connection, or null after showing why not. */
    const doSave = async (): Promise<Connection | null> => {
      setText(statusLine, "");
      try {
        const saved = await call<Connection>("connection_save", { profile: read(), secret: secretChange() });
        await refreshConnections();
        const found = connections().find((c) => c.id === saved.id) ?? null;
        if (found) fill(found);
        setText(statusLine, "Saved.");
        return found;
      } catch (e) {
        setText(statusLine, String(e));
        return null;
      }
    };

    save.addEventListener("click", () => void doSave());
    del.addEventListener("click", () => {
      if (!armed) {
        armed = true;
        renderButtons();
        return;
      }
      void (async () => {
        try {
          await call("connection_delete", { id: current.id });
          await refreshConnections();
          fill(connections()[0] ?? blankConnection());
          setText(statusLine, "Deleted.");
        } catch (e) {
          setText(statusLine, String(e));
        }
      })();
    });
    open.addEventListener("click", () => {
      void (async () => {
        const target = await doSave();
        if (!target) return;
        const reason = await connect(target.id);
        if (reason) setText(statusLine, reason);
        else finish();
      })();
    });

    // ---- closing and keys ----

    let pop: () => void = () => {};
    const finish = (): void => {
      pop();
      backdrop.remove();
      resolve();
    };
    close.addEventListener("click", finish);
    backdrop.addEventListener("pointerdown", (e) => {
      if (e.target === backdrop) finish();
    });
    for (const r of kind.radios) r.addEventListener("change", syncRows);
    for (const r of auth.radios) {
      r.addEventListener("change", () => {
        swapKeyRow();
        syncRows();
      });
    }

    const stops = (): HTMLElement[] =>
      Array.from(box.querySelectorAll<HTMLElement>("input, button")).filter(
        (n) => !(n as HTMLInputElement).disabled && n.offsetParent !== null,
      );
    pop = pushKeyHandler((e) => {
      if (e.key === "Escape") {
        e.preventDefault();
        finish();
      } else if (e.key === "Tab") {
        e.preventDefault();
        const items = stops();
        if (items.length === 0) return true;
        const at = items.indexOf(document.activeElement as HTMLElement);
        const next = e.shiftKey ? (at <= 0 ? items.length - 1 : at - 1) : at < 0 || at === items.length - 1 ? 0 : at + 1;
        items[next]!.focus();
      } else if (e.key === "Enter" && e.target instanceof HTMLInputElement && e.target.type !== "radio" && e.target.type !== "checkbox") {
        e.preventDefault();
        void doSave();
      }
      return true;
    });

    fill(connections()[0] ?? blankConnection());
    (connections().length > 0 ? list.querySelector("button") : name.input)?.focus();
  });
}
