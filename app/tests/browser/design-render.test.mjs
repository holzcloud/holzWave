// The design page still draws the views of the server sync and the remote sessions: docs/design/render.cjs checks every
// view and every phones row, and dumps a SHA-256 of the markup of each of these views. It launches its own Chromium, so it
// runs once, on the Chromium pass of the desktop backend.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

export const backends = ["tauri"];
export const backendNote = "the design page is rendered by its own script, once; the backend does not matter to it.";

const ROOT = fileURLToPath(new URL("../../../", import.meta.url));
const VIEWS = ["server-sessions", "server-pane-states", "device-authorize", "device-lists", "sync-transport", "phones-server"];

const render = (...args) => spawnSync(process.execPath, ["docs/design/render.cjs", ...args], { cwd: ROOT, encoding: "utf8", timeout: 25_000 });

export default async function ({ engine }) {
  if (engine !== "chromium") return;
  const check = render("--check");
  assert.equal(check.status, 0, `render.cjs --check failed: ${check.stderr}`);
  for (const view of VIEWS) {
    const dump = render("--dump", view);
    assert.equal(dump.status, 0, `render.cjs --dump ${view} failed: ${dump.stderr}`);
    assert.match(dump.stdout.trim(), /^[0-9a-f]{64}$/, `render.cjs --dump ${view} printed ${JSON.stringify(dump.stdout)}`);
  }
  // A view the page does not have is refused, so the dumps above are not accepted by default.
  assert.notEqual(render("--dump", "no-such-view").status, 0, "an unknown view was dumped");
}
