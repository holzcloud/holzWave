// Connections on the page (design.md section 6.3, "Connections"): the saved list becomes palette entries, the dialog
// edits and opens them, the pane header and the status line show the state of a connection, and a connection that
// is being made again is a shape, not only a colour. The browser stub plays the core.
import assert from "node:assert/strict";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await listCase(page, url);
  await stateCase(page, url);
  await dialogCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

const TALOS = {
  id: "talos", name: "Talos", kind: "ssh", host: "10.0.4.12", port: 22, user: "holz", auth: "key",
  key_path: "~/.ssh/id_ed25519", jump: null, startup: "", reconnect_attempts: 5, reconnect_delay_secs: 3, shell: "",
  has_secret: true,
};
const WEB = { ...TALOS, id: "web", name: "Web", kind: "docker", host: "web-1", has_secret: false };

/** Open the page against the stub; `list` is what `connections_list` answers, `conns` what `pane_connections` answers. */
async function open(page, url, list = [TALOS, WEB], conns = []) {
  await page.addInitScript(
    ([l, c]) => {
      window.__MOCK__.on("connections_list", () => structuredClone(window.__LIST ?? l));
      window.__MOCK__.on("pane_connections", () => structuredClone(window.__CONNS ?? c));
    },
    [list, conns],
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
  assert.match(await page.locator(".conns .cx-hint").textContent(), /A secret is saved in the system keychain/);

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
