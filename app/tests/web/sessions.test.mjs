// The sessions of the server in a real browser against the real server: the window lists the server it was loaded from as `This
// server`, `New session` opens a pane whose header says who writes, typing reaches the program, the × detaches and `Attach` brings
// the screen back with what was printed meanwhile, a second tab of the same person shares the pane as it shares every pane of the
// window, another person sees and reaches only their own sessions, and the operator's switch decides what the row offers. The page
// renders nothing of its own here: the person's worker draws the session for the browser, as it draws every pane of the window.
import { runTest, signIn, setSwitches, oracle, poll, check, equal, runEntry } from "./lib.mjs";

const REMOTE_OFF = "Remote sessions are off on this server.";
const NOT_FOUND = "That session is not on the server any more.";

/** The lines of a pane's screen as the worker draws it, read on a second socket of the same person. */
const lines = async (socket, pane) => (await socket.screen(pane)).split("\n").map((l) => l.trim());

const sawLine = (socket, pane, line, what) =>
  poll(async () => ((await lines(socket, pane)).includes(line) ? true : undefined), what, 20_000);

/** Open the Connections dialog from the palette and wait for the row of the server to say `word`. */
async function openConnections(page, word = "signed in") {
  await runEntry(page, "Connections…");
  await page.waitForSelector(".dlg.conns");
  await poll(async () => ((await page.locator(".conns .cx-srv-state").textContent()) === word ? true : undefined), `the row says ${word}`);
}

async function closeConnections(page) {
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.conns", { state: "detached" });
}

/** Type a line into the pane that has the keyboard. */
async function typeLine(page, text) {
  await page.locator("canvas#term").click();
  await page.keyboard.type(text);
  await page.keyboard.press("Enter");
}

const writerWord = (page, pane) => page.locator(`.panehead[data-pane="${pane}"] .ph-writer`).textContent();

await runTest("sessions", {}, async ({ harness, open }) => {
  const admin = await harness.account({ role: "admin", name: "ss-admin" });
  const member = await harness.account({ role: "member", name: "ss-member" });
  const other = await harness.account({ role: "member", name: "ss-other" });

  // ---- off, as a new server is: the row says so and offers nothing ------------------------------------------------------------
  const mine = await open();
  const page = await mine.context.newPage();
  await signIn(page, harness, member);
  await page.waitForSelector("canvas#term");
  await openConnections(page);
  equal(await page.locator(".conns .cx-srv-name").textContent(), "This server", "the row of the browser build");
  await poll(async () => ((await page.locator(".conns .cx-help").textContent().catch(() => "")) === REMOTE_OFF ? true : undefined), "the off line");
  equal((await page.locator(".conns .cx-srv-acts button").allTextContents()).join(","), "", "no New session, no Sign in…, no Sign out");
  await closeConnections(page);

  // ---- an administrator turns remote sessions on ------------------------------------------------------------------------------
  const adminContext = await open();
  const adminPage = await adminContext.context.newPage();
  await signIn(adminPage, harness, admin);
  await setSwitches(adminPage, { remote: true });

  // ---- New session: a pane that writes, typing reaches the program ----------------------------------------------------------
  await openConnections(page);
  equal(await page.locator(".conns .cx-srv-addr").textContent(), new URL(harness.origin).host, "the address is the page's own");
  equal((await page.locator(".conns .cx-srv-acts button").allTextContents()).join(","), "New session", "New session and nothing to sign in or out");
  await page.locator(".conns .cx-srv-acts button", { hasText: "New session" }).click();
  await page.waitForSelector(".dlg.conns", { state: "detached" });
  const socket = await oracle(page);
  const sessions = await socket.ok("server_sessions");
  equal(sessions.length, 1, "one session");
  const session = sessions[0].id;
  const pane = (await socket.layout()).focus;
  await poll(async () => ((await writerWord(page, pane).catch(() => "")) === "WRITING" ? true : undefined), "the header says WRITING");
  equal(await page.locator(`.panehead[data-pane="${pane}"] .ph-close`).getAttribute("title"), "Detach session", "the × detaches");
  await typeLine(page, "echo hello");
  await sawLine(socket, pane, "hello", "the program's answer on the pane's screen");
  await openConnections(page);
  await page.waitForSelector(".conns .cx-sess");
  equal((await page.locator(".conns .cx-sess-writer").allTextContents()).join(","), "held", "the list says somebody writes");
  await closeConnections(page);

  // ---- a second tab of the same person shows the same pane, and both type into the one session ----------------------------
  const second = await mine.context.newPage();
  await second.goto(`${harness.origin}/`);
  await second.waitForSelector("canvas#term");
  await poll(async () => ((await writerWord(second, pane).catch(() => "")) === "WRITING" ? true : undefined), "the second tab's header says WRITING");
  await typeLine(second, "echo two-$((3*3))");
  await sawLine(socket, pane, "two-9", "what the second tab typed reaches the same session");
  await typeLine(page, "echo one-$((2*2))");
  await sawLine(socket, pane, "one-4", "what the first tab typed reaches it too");
  equal(await writerWord(page, pane), "WRITING", "the shared pane still writes in the first tab");
  const attached = (await socket.ok("server_sessions"))[0].attached;
  equal(attached, 1, "two tabs share the one pane: one attachment, not two");
  await second.close();

  // ---- the × detaches; Attach brings the screen back with what was printed meanwhile ---------------------------------------
  await typeLine(page, "sleep 2; echo later-$((5*5))");
  await page.locator(`.panehead[data-pane="${pane}"] .ph-close`).click();
  await poll(async () => (!(await socket.layout()).panes.some(([id]) => id === pane) ? true : undefined), "the pane is gone");
  equal((await socket.ok("server_sessions")).map((row) => row.id).join(","), String(session), "the session stays in the list");
  await page.waitForTimeout(2500);
  await openConnections(page);
  await page.waitForSelector(".conns .cx-sess");
  await page.locator(".conns .cx-sess").first().locator("button", { hasText: "Attach" }).click();
  await page.waitForSelector(".dlg.conns", { state: "detached" });
  const again = (await socket.layout()).focus;
  check(again !== pane, "Attach shows the session in a new pane");
  await sawLine(socket, again, "later-25", "what the program printed while nobody was attached");

  // ---- another person: only their own sessions, and the first person's number reaches nothing ----------------------------
  const theirs = await open();
  const otherPage = await theirs.context.newPage();
  await signIn(otherPage, harness, other);
  await otherPage.waitForSelector("canvas#term");
  await openConnections(otherPage);
  await poll(async () => ((await otherPage.locator(".conns .cx-help").count()) > 0 ? true : undefined), "the help line");
  equal(await otherPage.locator(".conns .cx-sess").count(), 0, "no session of another person is listed");
  await closeConnections(otherPage);
  const otherSocket = await oracle(otherPage);
  const tabs = (await otherSocket.layout()).tabs.length;
  const reached = await otherPage.evaluate(async (id) => {
    const { call } = await import(new URL("bridge.js", location.href).href);
    try {
      await call("server_session_open", { session: id });
      return "opened";
    } catch (failure) {
      return String(failure instanceof Error ? failure.message : failure);
    }
  }, session);
  equal(reached, NOT_FOUND, "another person's session number is not found");
  equal((await otherSocket.layout()).tabs.length, tabs, "and no pane was opened");

  // ---- off again: the row says so and New session is gone -------------------------------------------------------------------
  await setSwitches(adminPage, { remote: false });
  await openConnections(otherPage);
  await poll(async () => ((await otherPage.locator(".conns .cx-help").textContent().catch(() => "")) === REMOTE_OFF ? true : undefined), "the off line");
  equal(await otherPage.locator(".conns .cx-srv-acts button", { hasText: "New session" }).count(), 0, "no New session while off");
  equal(await otherPage.locator(".conns .cx-sess").count(), 0, "no list while off");
  await closeConnections(otherPage);
});
