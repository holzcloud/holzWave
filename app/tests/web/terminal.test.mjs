// The first administrator, made in the page from the setup token the server printed, and then a shell: the test types a
// command in the page and reads what the shell printed from the server's own protocol, on a second socket of the same
// session (the oracle). With HW_SELF_CHECK=1 it also runs two deliberately wrong expectations and requires that both fail.
import { Authenticator } from "./totp.mjs";
import { runTest, openPage, oracle, poll, check, equal, focusedPane } from "./lib.mjs";
import { secretOfLink } from "./totp.mjs";

const SELF_CHECK = process.env.HW_SELF_CHECK === "1";

await runTest("terminal", {}, async ({ engine, harness, open }) => {
  const stranger = await open();
  const page = await openPage(stranger, harness, "/");

  // The sign-in page is what an anonymous person gets at `/`, and with no administrator it offers the set-up.
  await page.waitForSelector(".lg-box");
  equal(await page.title(), "holzWave", "the title of the sign-in page");
  equal(await page.locator("h2").first().textContent(), "Set up this server", "the first screen with no administrator");
  equal(new URL(page.url()).origin, harness.origin, "the page comes from the harness");
  check(harness.bootstrap !== null && harness.bootstrap !== undefined, "the harness printed a setup token");

  const username = "first-admin";
  const password = `pw-${Math.random().toString(16).slice(2, 14)}-${Math.random().toString(16).slice(2, 14)}`;
  await page.getByLabel("Setup token").fill(harness.bootstrap);
  await page.getByLabel("Username").fill(username);
  await page.getByLabel("Display name").fill("First Admin");
  await page.getByLabel("Password and authenticator app").check();
  await page.getByLabel("Password", { exact: true }).fill(password);
  await page.getByRole("button", { name: "Continue" }).click();

  // The authenticator step: the key is shown, the test is the authenticator app.
  await page.waitForSelector(".lg-secret");
  const link = (await page.locator(".lg-tx").textContent()).trim();
  const secret = secretOfLink(link);
  const authenticator = new Authenticator(secret);
  await page.getByLabel("Code").fill(await authenticator.next());
  await page.getByRole("button", { name: "Continue" }).click();

  // The recovery step: shown once, and the box has to be ticked before the person can go on.
  await page.waitForSelector(".lg-code");
  equal(await page.locator("h2").textContent(), "Save your recovery code", "the recovery step");
  equal(await page.getByRole("button", { name: "Continue", exact: true }).isDisabled(), true, "Continue before the box is ticked");
  await page.getByLabel("I have saved this code").check();
  equal(await page.getByRole("button", { name: "Continue", exact: true }).isEnabled(), true, "Continue after the box is ticked");
  await page.getByRole("button", { name: "Continue", exact: true }).click();

  // The application: one pane, known to the server.
  await page.waitForSelector("canvas#term");
  const socket = await oracle(page);
  const view = await poll(async () => {
    const layout = await socket.layout();
    return layout.panes?.length > 0 ? layout : undefined;
  }, "the application shows a pane", 20_000);
  const panes = view.panes.length;
  const pane = await focusedPane(socket);
  check(Number.isInteger(pane), "the layout names the focused pane");

  // The tracer: type a command whose output differs from its text, read the output from the pane's screen.
  await page.locator("canvas#term").click();
  await page.keyboard.type("printf 'hw-%s\\n' roundtrip");
  await page.keyboard.press("Enter");
  const screen = await poll(async () => {
    const text = await socket.screen(pane);
    return text.split("\n").some((line) => line.trim() === "hw-roundtrip") ? text : undefined;
  }, "the shell's output on the pane's screen", 15_000);
  check(screen.includes("printf"), "the typed command is on the screen too, as the shell echoes it");
  check(!screen.split("\n").some((line) => line.includes("hw-roundtrip") && line.includes("printf")), "the typed text never contains the joined word");

  // ---- reattach: the pane outlives the page ---------------------------------------------------------------------------------------
  const lines = (text) => text.split("\n").map((l) => l.trim());
  const typeLine = async (p, text) => {
    await p.locator("canvas#term").click();
    await p.keyboard.type(text);
    await p.keyboard.press("Enter");
  };
  const sawLines = (socketOfPage, paneId, wanted, what, ms = 15_000) =>
    poll(async () => {
      const seen = lines(await socketOfPage.screen(paneId));
      return wanted.every((w) => seen.includes(w)) ? seen : undefined;
    }, what, ms);

  await typeLine(page, "printf 'mark-%s\\n' one two three; sh -c 'sleep 2; printf \"late-%s\\n\" done'");
  await sawLines(socket, pane, ["mark-one", "mark-two", "mark-three"], "the markers are on the screen before the page goes");
  const goneAt = Date.now();
  await socket.close();
  await page.close();

  const again = await stranger.context.newPage();
  await again.goto(`${harness.origin}/`);
  await again.waitForSelector("canvas#term");
  const socketAgain = await oracle(again);
  const afterwards = await poll(async () => {
    const layout = await socketAgain.layout();
    return layout.panes?.length > 0 ? layout : undefined;
  }, "the new page finds the layout", 20_000);
  check(afterwards.panes.some((p) => (p.id ?? p) === pane) || afterwards.focus === pane, "the pane is still there after the page was closed");
  const reattached = await sawLines(socketAgain, pane, ["mark-one", "mark-two", "mark-three", "late-done"], "the late line appears on the reattached pane", 10_000);
  const order = ["mark-one", "mark-two", "mark-three", "late-done"].map((w) => reattached.indexOf(w));
  check(order.every((at, i) => at >= 0 && (i === 0 || at > order[i - 1])), `the markers are in order and none is missing (${order.join(" < ")})`);
  const lateAfter = (Date.now() - goneAt) / 1000;

  // ---- a second tab of the same person --------------------------------------------------------------------------------------------------
  const second = await stranger.context.newPage();
  await second.goto(`${harness.origin}/`);
  await second.waitForSelector("canvas#term");
  const socketSecond = await oracle(second);
  const layoutSecond = await poll(async () => {
    const layout = await socketSecond.layout();
    return layout.panes?.length > 0 ? layout : undefined;
  }, "the second tab finds the layout", 20_000);
  equal(JSON.stringify(layoutSecond.panes.map((p) => p.id ?? p)), JSON.stringify((await socketAgain.layout()).panes.map((p) => p.id ?? p)), "both tabs show the same pane ids");
  await typeLine(second, "printf 'tab-%s\\n' two");
  await sawLines(socketAgain, pane, ["tab-two"], "text typed in the second tab appears on the first tab's screen");
  await typeLine(again, "printf 'tab-%s\\n' one");
  await sawLines(socketSecond, pane, ["tab-one"], "text typed in the first tab appears on the second tab's screen");

  // ---- a second browser of the same person: the same cookies in another context -------------------------------------------------------
  const other = await open({ storageState: await stranger.context.storageState() });
  const third = await other.context.newPage();
  await third.goto(`${harness.origin}/`);
  await third.waitForSelector("canvas#term");
  const socketThird = await oracle(third);
  await sawLines(socketThird, pane, ["mark-one", "late-done", "tab-one", "tab-two"], "a second browser sees the same output");
  other.expectClean("the second browser");

  // ---- the chords of the web table -------------------------------------------------------------------------------------------------------
  await again.bringToFront();
  await again.locator("canvas#term").click();
  const panesBefore = (await socketAgain.layout()).panes.length;
  await again.keyboard.press("Control+W");
  equal(again.isClosed(), false, "Control+W does not close the page");
  equal((await socketAgain.layout()).panes.length, panesBefore, "Control+W is not a chord of the page: the layout is unchanged");
  await again.keyboard.press("Control+Shift+D");
  const split = await poll(async () => {
    const layout = await socketAgain.layout();
    return layout.panes.length === panesBefore + 1 ? layout : undefined;
  }, "the split chord adds a pane");
  await again.keyboard.press("Alt+Shift+W");
  await poll(async () => ((await socketAgain.layout()).panes.length === panesBefore ? true : undefined), "the close-pane chord of the web table closes the new pane");
  const chordPanes = `${panesBefore} -> ${split.panes.length} -> ${panesBefore}`;
  await socketAgain.close();
  await socketSecond.close();
  await socketThird.close();
  await second.close();
  stranger.expectClean("the first administrator's context, reattached");

  if (SELF_CHECK) {
    // Two expectations that cannot hold. Each must fail; the runner counts the lines.
    const expectFailure = async (name, attempt) => {
      try {
        await attempt();
      } catch (e) {
        console.log(`selfcheck.fail ${name}: ${String(e.message).split("\n")[0].slice(0, 120)}`);
        return;
      }
      console.log(`selfcheck.pass ${name}`);
    };
    await expectFailure("a pane contains a string nobody printed", async () => {
      await poll(async () => ((await socket.screen(pane)).includes("hw-this-was-never-printed") ? true : undefined), "the pane shows a string nobody printed", 3_000);
    });
    await expectFailure("a sign-in with a wrong password succeeds", async () => {
      const other = await open();
      const again = await openPage(other, harness, "/");
      await again.waitForSelector(".lg-box");
      await again.getByLabel("Username").fill(username);
      await again.getByLabel("Password", { exact: true }).fill(`${password}-wrong`);
      await again.getByRole("button", { name: "Continue" }).click();
      await again.getByLabel("Code").waitFor({ timeout: 3_000 });
      await again.getByLabel("Code").fill(await authenticator.next());
      await again.getByRole("button", { name: "Sign in" }).click();
      await again.waitForSelector("canvas#term", { timeout: 3_000 });
    });
  }

  stranger.expectClean("the first administrator's context");
  const { recorder } = stranger;
  console.log(
    `tracer: screens passed: set-up, authenticator, recovery, application; panes in the layout: ${panes}; ` +
      `the screen has the line hw-roundtrip; requests: ${recorder.requests}, hosts: ${[...recorder.hosts].join(", ")}; ` +
      `sockets: ${recorder.sockets.length}; engine: ${engine}; reattach: markers in order, late line after ${lateAfter.toFixed(1)} s; second tab and second browser see the same output; chords: Control+W ignored, panes ${chordPanes}`,
  );
});
