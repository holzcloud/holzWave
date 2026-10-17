// Connections on the page (design.md section 6.3, "Connections"): the saved list becomes palette entries, the dialog
// edits and opens them, the pane header and the status line show the state of a connection, and a connection that
// is being made again is a shape, not only a colour. The browser stub plays the core.
import assert from "node:assert/strict";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await listCase(page, url);
  await wslCase(page, url);
  await stateCase(page, url);
  await dialogCase(page, url);
  await wshCase(page, url);
  await agentCase(page, url);
  await installCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

const TALOS = {
  id: "talos", name: "Talos", kind: "ssh", host: "10.0.4.12", port: 22, user: "holz", auth: "key",
  key_path: "~/.ssh/id_ed25519", jump: null, startup: "", reconnect_attempts: 5, reconnect_delay_secs: 3, shell: "",
  has_secret: true,
};
const WEB = { ...TALOS, id: "web", name: "Web", kind: "docker", host: "web-1", has_secret: false };

/** Open the page against the stub; `list` is what `connections_list` answers, `conns` what `pane_connections` answers. */
async function open(page, url, list = [TALOS, WEB], conns = [], os = null) {
  await page.addInitScript(
    ([l, c, o]) => {
      if (o !== null) window.__MOCK__.on("app_info", () => ({ version: "0.0.1-alpha1", os: o, hostname: "woodbox" }));
      window.__MOCK__.on("connections_list", () => structuredClone(window.__LIST ?? l));
      window.__MOCK__.on("pane_connections", () => structuredClone(window.__CONNS ?? c));
    },
    [list, conns, os],
  );
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await page.evaluate(async () => {
    window.__registry = await import(new URL("actions.js", location.href).href);
  });
}

async function listCase(page, url) {
  await open(page, url);
  const titles = await page.evaluate(() => window.__registry.listActions().map((a) => a.title));
  assert.ok(titles.includes("Connections…"), "the dialog has a palette entry");
  assert.ok(titles.includes("Connect: Talos (ssh)"), "each saved connection has one");
  assert.ok(titles.includes("Connect: Web (docker)"));
  // Running the entry asks the core to open it.
  await page.evaluate(() => window.__registry.runAction("connect.talos"));
  const asked = await page.evaluate(() => window.__MOCK__.callsOf("connect"));
  assert.deepEqual(asked, [{ id: "talos" }]);
}

const BOX = { ...TALOS, id: "box", name: "Box", kind: "wsl", host: "Ubuntu", has_secret: false };

async function wslCase(page, url) {
  const wslTitle = "Connect: Box (wsl)";
  const titlesAt = async (os) => {
    await open(page, url, [TALOS, WEB, BOX], [], os);
    return page.evaluate(() => window.__registry.listActions().map((a) => a.title));
  };
  for (const os of ["macos", "linux"]) {
    const titles = await titlesAt(os);
    assert.ok(!titles.includes(wslTitle), `a WSL connection has no connect entry on ${os}`);
    assert.ok(titles.includes("Connect: Talos (ssh)") && titles.includes("Connect: Web (docker)"), `the others stay on ${os}`);
    // The dialog still lists it: the profile stays in the list and in the sync.
    await page.evaluate(() => window.__registry.runAction("connections.open"));
    await page.waitForSelector(".dlg.conns");
    const names = await page.locator(".conns .cx-list button .cx-name").allTextContents();
    assert.deepEqual(names, ["Talos", "Web", "Box"], `the dialog lists the WSL connection on ${os}`);
  }
  const win = await titlesAt("windows");
  assert.ok(win.includes(wslTitle) && win.includes("Connect: Talos (ssh)") && win.includes("Connect: Web (docker)"), "all three on windows");
  // The platform is not known yet (an empty answer): the entry is offered; it is hidden only when the platform is known.
  const unknown = await titlesAt("");
  assert.ok(unknown.includes(wslTitle), "an unknown platform offers the entry");
}

async function stateCase(page, url) {
  await open(page, url, [TALOS], [{ pane: 2, profile: "talos", name: "Talos", state: "connected", attempt: 0, of: 0, secs: 0 }]);
  const head = (pane) => page.locator(`.panehead[data-pane="${pane}"]`);
  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="2"] .ph-ctx')?.textContent === "Talos");
  assert.equal(await page.locator("#status .conns").textContent(), "ssh 1", "the status line counts the connection");
  assert.equal(await head(1).locator(".ph-ctx").textContent(), "", "a pane without a connection shows its directory (none yet)");

  // The connection drops: the header says so with the counter, the dot changes shape, the status line says it is down.
  await page.evaluate(() => {
    window.__CONNS = [{ pane: 2, profile: "talos", name: "Talos", state: "reconnecting", attempt: 2, of: 5, secs: 3 }];
  });
  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="2"] .ph-ctx')?.textContent === "Talos · ⟳ 2/5 in 3 s", null, { timeout: 4000 });
  assert.equal(await head(2).locator(".ph-dot").getAttribute("data-state"), "wait", "the dot is dotted while it reconnects");
  const border = await head(2).locator(".ph-dot").evaluate((n) => getComputedStyle(n).borderStyle);
  assert.equal(border, "dotted", "a dotted ring is a shape, so the state needs no colour");
  assert.equal(await page.locator("#status .conns").textContent(), "ssh 1 · 1 down");

  await page.evaluate(() => {
    window.__CONNS = [{ pane: 2, profile: "talos", name: "Talos", state: "failed", attempt: 0, of: 0, secs: 0 }];
  });
  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="2"] .ph-ctx')?.textContent === "Talos · failed, Enter retries", null, { timeout: 4000 });
  assert.equal(await head(2).locator(".ph-dot").getAttribute("data-state"), "failed");
}

async function dialogCase(page, url) {
  await open(page, url);
  await page.evaluate((l) => { window.__LIST = l; }, [TALOS, WEB]);
  await page.evaluate(() => window.__registry.runAction("connections.open"));
  await page.waitForSelector(".dlg.conns");
  const names = await page.locator(".conns .cx-list button .cx-name").allTextContents();
  assert.deepEqual(names, ["Talos", "Web"], "the saved connections are listed");
  assert.equal(await page.locator("#cx-name").inputValue(), "Talos", "the first one is shown");
  assert.equal(await page.locator("#cx-secret").inputValue(), "", "a secret is never filled in again");
  assert.match(await page.locator(".conns .cx-hint:not(#cx-wsh-note)").textContent(), /A secret is saved in the system keychain/);

  // A Docker connection hides the SSH fields and names its host a container.
  await page.locator(".conns .cx-list button", { hasText: "Web" }).click();
  assert.equal(await page.locator("label[for=cx-host]").textContent(), "Container");
  assert.equal(await page.locator("#cx-user").isVisible(), false);
  assert.equal(await page.locator("#cx-shell").isVisible(), true);

  // New connection: fill, save, and what is sent has the typed secret as a `set`.
  await page.evaluate(() => {
    window.__MOCK__.on("connection_save", ({ profile }) => {
      window.__LIST = [...(window.__LIST ?? []).filter((c) => c.id !== "nas"), { ...profile, id: "nas", has_secret: true }];
      return { ...profile, id: "nas", has_secret: true };
    });
  });
  await page.locator(".conns .cx-new").click();
  assert.equal(await page.locator("#cx-name").inputValue(), "");
  await page.fill("#cx-name", "NAS");
  await page.fill("#cx-host", "nas.local");
  await page.fill("#cx-user", "holz");
  await page.check('input[name="cx-auth"][value="password"]');
  assert.equal(await page.locator("label[for=cx-secret]").textContent(), "Password");
  assert.equal(await page.locator("#cx-key").isVisible(), false, "a password login has no key file");
  await page.fill("#cx-secret", "hunter2");
  await page.getByRole("button", { name: "Save" }).click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("connection_save").length === 1);
  const saved = (await page.evaluate(() => window.__MOCK__.callsOf("connection_save")))[0];
  assert.equal(saved.profile.name, "NAS");
  assert.equal(saved.profile.host, "nas.local");
  assert.equal(saved.profile.auth, "password");
  assert.deepEqual(saved.secret, { kind: "set", value: "hunter2" });
  assert.equal(await page.locator("#cx-secret").inputValue(), "", "the field is emptied after the save");

  // Saving without typing a secret keeps it; the clear box removes it.
  await page.getByRole("button", { name: "Save" }).click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("connection_save").length === 2);
  assert.deepEqual((await page.evaluate(() => window.__MOCK__.callsOf("connection_save")))[1].secret, { kind: "keep" });
  await page.check("#cx-clear");
  await page.getByRole("button", { name: "Save" }).click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("connection_save").length === 3);
  assert.deepEqual((await page.evaluate(() => window.__MOCK__.callsOf("connection_save")))[2].secret, { kind: "clear" });

  // A refusal is shown and changes nothing.
  await page.evaluate(() => window.__MOCK__.on("connection_save", () => { throw "the host has a blank or starts with a dash"; }));
  await page.getByRole("button", { name: "Save" }).click();
  await page.waitForFunction(() => document.querySelector(".conns .set-status")?.textContent.includes("starts with a dash"));

  // Delete needs a second click.
  const deleteButton = page.locator(".conns .cx-danger");
  await deleteButton.click();
  assert.equal(await deleteButton.textContent(), "Click again to delete");
  assert.equal((await page.evaluate(() => window.__MOCK__.callsOf("connection_delete"))).length, 0, "one click deletes nothing");
  await deleteButton.click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("connection_delete").length === 1);

  // Connect saves, opens and closes the dialog.
  await page.evaluate(() => window.__MOCK__.on("connect", () => window.__MOCK__.view()));
  await page.evaluate(() => window.__MOCK__.on("connection_save", ({ profile }) => ({ ...profile, has_secret: true })));
  await page.locator(".conns .cx-list button", { hasText: "Talos" }).click();
  await page.getByRole("button", { name: "Connect", exact: true }).click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("connect").length === 1);
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.callsOf("connect")), [{ id: "talos" }]);
  await page.waitForSelector(".dlg.conns", { state: "detached" });

  // Escape closes the dialog and the terminal gets its keyboard back.
  await page.evaluate(() => window.__registry.runAction("connections.open"));
  await page.waitForSelector(".dlg.conns");
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.conns", { state: "detached" });
}

const WSH_NOTE =
  "A program on this host can then open files and rename this pane in holzWave through a private tunnel. It can never use the AI or approve anything. The host must allow Unix socket forwarding.";

const AGENT_HINT = "The ssh-agent signs the login. Nothing is stored. The public key file picks one of its keys.";

/** The Agent login: a third radio after Key file and Password, with its own rows and words. */
async function agentCase(page, url) {
  await open(page, url);
  const AGENT = { ...TALOS, id: "ag", name: "Agent host", auth: "agent", key_path: "", has_secret: false };
  await page.evaluate((l) => { window.__LIST = l; }, [TALOS, WEB, AGENT]);
  await page.evaluate(() => {
    window.__MOCK__.on("connection_save", ({ profile }) => ({ ...profile, has_secret: false }));
  });
  await page.evaluate(() => window.__registry.runAction("connections.open"));
  await page.waitForSelector(".dlg.conns");

  const login = page.locator('.conns [role="radiogroup"][aria-label="Login"]');
  assert.deepEqual(await login.locator("label.cx-option").allTextContents(), ["Key file", "Password", "Agent"], "three logins in this order");
  const keyLabel = page.locator("label[for=cx-key]");
  const hint = page.locator(".conns .cx-hint:not(#cx-wsh-note)");
  assert.equal(await keyLabel.textContent(), "Key file");
  assert.equal(await page.locator("#cx-key").inputValue(), "~/.ssh/id_ed25519");

  // A secret typed for the key file is not sent once the login is the agent.
  await page.fill("#cx-secret", "typed-before-the-switch");
  await page.check('input[name="cx-auth"][value="agent"]');
  assert.equal(await page.locator("#cx-secret").isVisible(), false, "no passphrase row for the agent");
  assert.equal(await page.locator("#cx-clear").isVisible(), false, "no removal row for the agent");
  assert.equal(await keyLabel.textContent(), "Public key (optional)");
  assert.equal(await page.locator("#cx-key").isVisible(), true, "the row stays, as the optional public key");
  assert.equal(await page.locator("#cx-key").inputValue(), "", "the private key path is not shown as a public key");
  assert.equal(await hint.textContent(), AGENT_HINT);
  assert.doesNotMatch(await hint.textContent(), /password|passphrase|keychain/i, "no word about a secret that is not stored");

  await page.fill("#cx-key", "~/.ssh/id_work.pub");
  await page.getByRole("button", { name: "Save" }).click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("connection_save").length === 1);
  const sent = (await page.evaluate(() => window.__MOCK__.callsOf("connection_save")))[0];
  assert.equal(sent.profile.auth, "agent");
  assert.equal(sent.profile.key_path, "~/.ssh/id_work.pub");
  assert.deepEqual(sent.secret, { kind: "keep" }, "an agent login never sends a secret");

  // Choosing the key file again restores its words, its row and its path.
  await page.check('input[name="cx-auth"][value="key"]');
  assert.equal(await keyLabel.textContent(), "Key file");
  assert.equal(await page.locator("#cx-key").inputValue(), "~/.ssh/id_ed25519");
  assert.equal(await page.locator("#cx-secret").isVisible(), true);
  assert.equal(await page.locator("label[for=cx-secret]").textContent(), "Passphrase");
  // And the password login has no key row at all, as before.
  await page.check('input[name="cx-auth"][value="password"]');
  assert.equal(await page.locator("#cx-key").isVisible(), false);
  assert.equal(await page.locator("label[for=cx-secret]").textContent(), "Password");

  // A saved agent connection shows the Agent radio, its label and its (empty) public key.
  await page.locator(".conns .cx-list button", { hasText: "Agent host" }).click();
  assert.equal(await page.locator('input[name="cx-auth"][value="agent"]').isChecked(), true);
  assert.equal(await keyLabel.textContent(), "Public key (optional)");
  assert.equal(await page.locator("#cx-key").inputValue(), "");
  assert.equal(await page.locator("#cx-secret").isVisible(), false);
  assert.equal(await hint.textContent(), AGENT_HINT);

  // The group is a radio group: the arrow keys walk Key file, Password, Agent.
  await page.focus('input[name="cx-auth"][value="key"]');
  await page.keyboard.press("ArrowRight");
  assert.equal(await page.locator('input[name="cx-auth"][value="password"]').isChecked(), true, "right from Key file is Password");
  await page.keyboard.press("ArrowRight");
  assert.equal(await page.locator('input[name="cx-auth"][value="agent"]').isChecked(), true, "right from Password is Agent");
  assert.equal(await keyLabel.textContent(), "Public key (optional)");
  await page.keyboard.press("ArrowLeft");
  assert.equal(await page.locator('input[name="cx-auth"][value="password"]').isChecked(), true);

  // Not an SSH connection: the login rows are gone, whatever login was chosen.
  await page.locator(".conns .cx-list button", { hasText: "Agent host" }).click();
  await page.check('input[name="cx-kind"][value="docker"]');
  assert.equal(await page.locator("#cx-key").isVisible(), false);
  assert.equal(await login.isVisible(), false);
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.conns", { state: "detached" });
}

/** The palette action that installs `wsh` on the host of the focused pane: one call with the pane and nothing else. */
async function installCase(page, url) {
  await open(page, url);
  const entry = await page.evaluate(() => window.__registry.listActions().find((a) => a.id === "wsh.install"));
  assert.equal(entry?.title, "Install wsh on this host", "the action has its words in the palette");
  const focus = await page.evaluate(async () => (await import(new URL("state.js", location.href).href)).state.view.focus);
  assert.equal(typeof focus, "number", "the page knows which pane is in focus");

  assert.equal(await page.evaluate(() => window.__registry.runAction("wsh.install")), true);
  await page.waitForFunction(() => window.__MOCK__.callsOf("wsh_install").length === 1);
  const calls = await page.evaluate(() => window.__MOCK__.callsOf("wsh_install"));
  assert.deepEqual(calls, [{ pane: focus }], "exactly the focused pane and nothing else: no host, no path, no connection");

  // The core refuses a pane that is not an SSH pane with a sentence on the pane; a rejected call is ignored, no page error.
  await page.evaluate(() => window.__MOCK__.on("wsh_install", () => {
    throw "wsh was not installed: this pane does not run a saved SSH connection";
  }));
  await page.evaluate(() => window.__registry.runAction("wsh.install"));
  await page.waitForFunction(() => window.__MOCK__.callsOf("wsh_install").length === 2);

  // With no view there is no focused pane: nothing is sent.
  const sent = await page.evaluate(async () => {
    const { state } = await import(new URL("state.js", location.href).href);
    const view = state.view;
    state.view = null;
    const before = window.__MOCK__.callsOf("wsh_install").length;
    window.__registry.runAction("wsh.install");
    await new Promise((resolve) => setTimeout(resolve, 50));
    const after = window.__MOCK__.callsOf("wsh_install").length;
    state.view = view;
    return after - before;
  });
  assert.equal(sent, 0, "with no focused pane the action sends nothing");
}

async function wshCase(page, url) {
  await open(page, url);
  await page.evaluate((l) => { window.__LIST = l; }, [TALOS, WEB]);
  // The stub plays the core: it keeps what was saved and lists it back, as `connections_list` does.
  await page.evaluate(() => {
    window.__MOCK__.on("connection_save", ({ profile }) => {
      window.__LIST = window.__LIST.map((c) => (c.id === profile.id ? { ...profile, has_secret: c.has_secret } : c));
      return window.__LIST.find((c) => c.id === profile.id);
    });
  });
  await page.evaluate(() => window.__registry.runAction("connections.open"));
  await page.waitForSelector(".dlg.conns");

  const box = page.locator("#cx-wsh");
  const note = page.locator("#cx-wsh-note");
  assert.equal(await page.locator('label:has(#cx-wsh)').textContent(), "Use wsh on this host");
  assert.equal(await box.isChecked(), false, "unchecked for a connection that never ticked it");
  assert.equal(await note.textContent(), WSH_NOTE, "the note says what it allows and never allows");
  assert.equal(await note.evaluate((n) => n.children.length), 0, "the note is text, no markup");
  assert.equal(await note.getAttribute("class"), "cx-hint");

  await box.check();
  await page.getByRole("button", { name: "Save" }).click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("connection_save").length === 1);
  const sent = (await page.evaluate(() => window.__MOCK__.callsOf("connection_save")))[0];
  assert.equal(sent.profile.wsh, true, "the profile sent to the core carries wsh");
  assert.equal(sent.profile.id, "talos");

  // Another connection, then back: the saved value is shown again.
  await page.locator(".conns .cx-list button", { hasText: "Web" }).click();
  assert.equal(await box.isVisible(), false, "a Docker connection has no wsh row");
  assert.equal(await note.isVisible(), false, "and no note");
  await page.locator(".conns .cx-list button", { hasText: "Talos" }).click();
  assert.equal(await box.isVisible(), true);
  assert.equal(await box.isChecked(), true, "ticked again when the connection is chosen");
  assert.equal(await note.isVisible(), true);

  // A new connection starts unchecked, and a ticked box does not leak into it.
  await page.locator(".conns .cx-new").click();
  assert.equal(await box.isChecked(), false, "a new connection starts unchecked");

  // A WSL connection hides the row and the note; a box ticked before the type changed is not sent.
  await page.fill("#cx-name", "Ubuntu");
  await page.fill("#cx-host", "Ubuntu-24.04");
  await box.check();
  await page.check('input[name="cx-kind"][value="wsl"]');
  assert.equal(await box.isVisible(), false);
  assert.equal(await note.isVisible(), false);
  await page.evaluate(() => window.__MOCK__.on("connection_save", ({ profile }) => profile));
  await page.getByRole("button", { name: "Save" }).click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("connection_save").length === 2);
  const wsl = (await page.evaluate(() => window.__MOCK__.callsOf("connection_save")))[1];
  assert.equal(wsl.profile.kind, "wsl");
  assert.equal(wsl.profile.wsh, false, "a connection that is not SSH never sends wsh true");

  // Keyboard: the box is a stop in the dialog's order (after "Seconds between"), Space toggles it, Escape closes.
  await page.locator(".conns .cx-list button", { hasText: "Talos" }).click();
  await page.locator("#cx-delay").focus();
  await page.keyboard.press("Tab");
  assert.equal(await page.evaluate(() => document.activeElement?.id), "cx-wsh", "Tab from the last text field lands on the box");
  const before = await box.isChecked();
  await page.keyboard.press("Space");
  assert.equal(await box.isChecked(), !before, "Space toggles it");
  await page.keyboard.press("Tab");
  const after = await page.evaluate(() => document.activeElement?.textContent);
  assert.equal(after, "Delete", "the next stop is the first button");
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.conns", { state: "detached" });
}
