// Pictures in a real browser against the real server over TLS (POL-01, D-03): a shell in a real pane prints a Kitty picture, the page pulls
// the pixels over its socket and draws them, and the colours are read back from the canvas at the cells the picture covers. The page is the
// product's own, served with its shipped Content-Security-Policy; nothing here is a switch of the product (D-03: the page treats the server as
// it treats the desktop). Every positive case has its control in the same test: the colours of the same cells before the command, and the
// animation that runs without the preference before it is held with it.
//
// The last case of each renderer is the page's memory: a picture the program deleted is let go after the grace, and with WebGL its texture
// is deleted on the GPU (the textures made minus the textures deleted, counted by wrapping the context's `createTexture` and `deleteTexture`
// before the page's scripts run, is one higher with the picture on screen and back to what it was after the grace).
//
// With HW_SELF_CHECK=1 two deliberately wrong expectations run too and must fail.
//
// The Kitty fields used: `a=T` transmit and display, `f=32` straight RGBA, `s`/`v` the size in pixels, `i` the image id, `c`/`r` the cells it is
// shown over, `q=2` no reply to the program, `m=1` more chunks follow; for the animation `a=f` a further frame with `z` its gap in
// milliseconds, `a=a` with `r=1` and `z` the gap of the root frame and with `s=3` run and loop.
import {
  runTest, signIn, poll, check, equal, sleep, oracle, watchSocket, gridOf, cellCentre, readColours, isColour, hex, typeInto,
  RED, GREEN, BLUE, WHITE, RESET, cursorTo, marker, kittyPicture, kittyWhite, kittyAnimation, line,
} from "./lib.mjs";

const SELF_CHECK = process.env.HW_SELF_CHECK === "1";

/** Run an expectation that cannot hold; the runner counts the `selfcheck.fail` lines (two are due, no `selfcheck.pass`). */
async function expectFailure(name, attempt) {
  try {
    await attempt();
  } catch (e) {
    console.log(`selfcheck.fail ${name}: ${String(e.message).split("\n")[0].slice(0, 120)}`);
    return;
  }
  console.log(`selfcheck.pass ${name}`);
}
const RED_GREEN = [255, 0, 0, 255, 0, 255, 0, 255];

/** Where the small picture goes: two cells at column 10, row 3. The shell's prompt is on row 0, so these cells hold no text before or after. */
const SMALL = { col: 10, row: 3 };
/** The big picture's cells: 12 by 6 from column 30, row 8. */
const BIG = { col: 30, row: 8, cols: 12, rows: 6 };

await runTest("images", {}, async ({ engine, harness, open }) => {
  const person = await harness.account({ role: "admin", name: "im-owner" });
  const owned = await open();

  // Sign in once; the pane the page starts outlives every page of this test.
  const first = await owned.context.newPage();
  await signIn(first, harness, person);
  await first.close();

  /** A page of the application with the renderer asked for. Resolves with `null` when the engine has no WebGL2 for `webgl`. */
  const appPage = async (renderer, { reduced, counters = false } = {}) => {
    const page = await owned.context.newPage();
    if (reduced !== undefined) await page.emulateMedia({ reducedMotion: reduced });
    if (counters) {
      await page.addInitScript(() => {
        const count = { made: 0, deleted: 0 };
        window.__TEX__ = count;
        const proto = window.WebGL2RenderingContext?.prototype;
        if (!proto) return;
        const make = proto.createTexture;
        const del = proto.deleteTexture;
        proto.createTexture = function (...args) {
          const texture = make.apply(this, args);
          if (texture) count.made++;
          return texture;
        };
        proto.deleteTexture = function (...args) {
          if (args[0]) count.deleted++;
          return del.apply(this, args);
        };
      });
    }
    const net = watchSocket(page);
    await page.goto(`${harness.origin}/?renderer=${renderer}`);
    await page.waitForSelector("canvas#term");
    const grid = await poll(async () => {
      const g = await gridOf(page);
      return g && g.cols >= 60 && g.rows >= 20 ? g : undefined;
    }, "the page lays out a pane that is big enough for the pictures", 20_000);
    const wanted = renderer === "webgl" ? "webgl2" : "canvas2d";
    if (grid.renderer !== wanted) {
      check(renderer === "webgl", `the page used ${grid.renderer} where ${wanted} was asked for`);
      await page.close();
      return null;
    }
    const sock = await oracle(page);
    return { page, net, sock, grid, pane: grid.pane };
  };

  /** Wait until the pane's screen shows `word` as a line of its own (the typed text never holds the joined word). */
  const shows = (app, word) =>
    poll(async () => ((await app.sock.screen(app.pane)).split("\n").some((l) => l.trim() === word) ? true : undefined), `the screen shows ${word}`, 20_000);

  /** Run a shell line and wait for its marker. */
  const run = async (app, name, ...parts) => {
    await typeInto(app.sock, app.pane, line(...parts, marker(name)));
    await shows(app, `hw-${name}`);
  };

  const cells = (grid, at, count) => Array.from({ length: count }, (_, i) => cellCentre(grid, at.col + i, at.row));
  const colours = async (app, points) => readColours(app.page, points);
  const untilColours = (app, points, wanted, what, ms = 15_000) =>
    poll(async () => {
      const got = await colours(app, points);
      return wanted.every((w, i) => isColour(got[i], w)) ? got : undefined;
    }, what, ms);

  const report = { pixels: [], motion: [], budget: [], textures: [], renderers: [] };

  for (const renderer of ["canvas", "webgl"]) {
    const name = renderer === "canvas" ? "canvas2d" : "webgl2";
    let app = await appPage(renderer);
    if (app === null) {
      // Not a pass: the output line says so.
      report.renderers.push(`${name} NOT RUN (no WebGL2 in this engine)`);
      continue;
    }
    report.renderers.push(`${name} ok`);
    const image = renderer === "canvas" ? 11 : 21;

    // ---- the picture on the canvas, with its control, and the reattach ------------------------------------------------------------
    await run(app, `clear-${renderer}`, RESET);
    const points = cells(app.grid, SMALL, 2);
    const before = await colours(app, points);
    check(before.every((c) => !isColour(c, RED, 40) && !isColour(c, GREEN, 40)), `the control cells hold the background before the command (${before.map(hex)})`);
    await run(app, `picture-${renderer}`, RESET, cursorTo(SMALL.col, SMALL.row), kittyPicture({ id: image, width: 2, height: 1, rgba: RED_GREEN, cols: 2, rows: 1 }));
    const after = await untilColours(app, points, [RED, GREEN], `the picture is on the ${name} canvas: red then green at its two cells`);
    check(app.net.sent("image_data").some((a) => a.image === image && a.offset === 0), "the page pulled the pixels with image_data");
    report.pixels.push(`${name}: before ${before.map(hex).join(" ")}, after ${after.map(hex).join(" ")}`);
    if (SELF_CHECK && renderer === "canvas") await selfCheck(app, points, image);

    // The page is closed and a new page of the same session attaches to the same pane: the picture is pulled again.
    await app.page.close();
    app = await appPage(renderer);
    const again = await untilColours(app, cells(app.grid, SMALL, 2), [RED, GREEN], `the picture is back on the ${name} canvas after the page was closed and a new one attached`);
    const pulls = app.net.sent("image_data").filter((a) => a.image === image);
    check(pulls.length >= 1, "the new page pulled the pixels again");
    check(app.net.binaryAnswers().length >= 1, "the new page received a binary answer");
    report.pixels.push(`${name} reattached: ${again.map(hex).join(" ")} after ${pulls.length} image_data call(s)`);
    await app.page.close();

    // ---- animation: it runs, it is held under reduced motion, and it runs again -----------------------------------------------------
    const motionImage = image + 1;
    const running = await appPage(renderer, { reduced: "no-preference" });
    const spot = [cellCentre(running.grid, SMALL.col, SMALL.row)];
    await run(running, `clear-motion-${renderer}`, RESET);
    await run(
      running,
      `animation-${renderer}`,
      RESET,
      cursorTo(SMALL.col, SMALL.row),
      kittyAnimation({ id: motionImage, cols: 1, rows: 1, first: [255, 0, 0, 255], second: [0, 255, 0, 255], gap: 100 }),
    );
    // The control: without the preference the frame changes, from one colour of the picture to the other.
    await untilColours(running, spot, [RED], "the first frame of the animation (red) is shown", 15_000);
    const startedAt = Date.now();
    const seenGreen = await poll(async () => (isColour((await colours(running, spot))[0], GREEN) ? Date.now() - startedAt : undefined), "the animation moves on to green with no reduced-motion preference", 3_000);
    const generationsRunning = await poll(() => (running.net.generations(motionImage) >= 4 ? running.net.generations(motionImage) : undefined), "frames told about several generations of the running animation", 3_000);
    await running.page.close();

    // Reduced motion, set before the page loads: the page tells the core, and the picture stays on one frame for the same time.
    const reduced = await appPage(renderer, { reduced: "reduce" });
    const spotR = [cellCentre(reduced.grid, SMALL.col, SMALL.row)];
    await poll(
      async () => (reduced.net.sent("set_image_animation").some((a) => a.enabled === false) ? true : undefined),
      "the page reports reduced motion to the core",
      15_000,
    );
    const held = await poll(async () => {
      const c = (await colours(reduced, spotR))[0];
      return isColour(c, RED) || isColour(c, GREEN) ? c : undefined;
    }, "the picture is shown under reduced motion", 15_000);
    await sleep(700); // a frame already on its way when the switch reached the core may still arrive; after that none does
    const generationsHeld = reduced.net.generations(motionImage);
    const samples = new Set();
    const heldFor = Date.now();
    while (Date.now() - heldFor < 3_000) {
      samples.add(hex((await colours(reduced, spotR))[0]));
      await sleep(40);
    }
    equal(samples.size, 1, `the pixel stays on one frame for 3 seconds under reduced motion (saw ${[...samples].join(", ")}, first ${hex(held)})`);
    equal(reduced.net.generations(motionImage), generationsHeld, "no frame told about a new generation of the picture during those 3 seconds");

    // The preference back: it runs again.
    const resumedAt = Date.now();
    await reduced.page.emulateMedia({ reducedMotion: "no-preference" });
    const moved = await poll(async () => {
      const c = (await colours(reduced, spotR))[0];
      return (isColour(c, RED) || isColour(c, GREEN)) && !isColour(c, held) ? Date.now() - resumedAt : undefined;
    }, "the animation runs again when the preference is switched back", 3_000);
    check(reduced.net.sent("set_image_animation").at(-1)?.enabled === true, "the page told the core that motion is allowed again");
    report.motion.push(`${name}: changed to green after ${(seenGreen / 1000).toFixed(2)} s without the preference; held ${samples.size} colour over 3.0 s and ${generationsHeld} generations with it; moved again after ${(moved / 1000).toFixed(2)} s (${generationsRunning} generations seen while it ran)`);
    await reduced.page.close();

    // ---- the page's memory budget -----------------------------------------------------------------------------------------------
    const budget = await appPage(renderer);
    check(budget.grid.cols >= BIG.col + BIG.cols && budget.grid.rows >= BIG.row + BIG.rows, `the pane is big enough for the big picture (${budget.grid.cols} by ${budget.grid.rows})`);
    const smallId = image + 2;
    const bigId = image + 3;
    const setBudget = (mib) =>
      budget.page.evaluate(async (value) => {
        const { change } = await import(new URL("settings.js", location.href).href);
        const failure = await change("image_memory_mib", () => value);
        const { state } = await import(new URL("state.js", location.href).href);
        return { failure, now: state.settings?.settings.image_memory_mib };
      }, mib);
    equal(JSON.stringify(await setBudget(1)), JSON.stringify({ failure: null, now: 1 }), "the page's picture budget is set to 1 MiB through set_setting");
    const smallPoints = cells(budget.grid, SMALL, 2);
    const bigPoint = [cellCentre(budget.grid, BIG.col + BIG.cols / 2, BIG.row + BIG.rows / 2)];
    await run(
      budget,
      `budget-${renderer}`,
      RESET,
      cursorTo(SMALL.col, SMALL.row),
      kittyPicture({ id: smallId, width: 2, height: 1, rgba: RED_GREEN, cols: 2, rows: 1 }),
      cursorTo(BIG.col, BIG.row),
      kittyWhite({ id: bigId, width: 1024, height: 512, cols: BIG.cols, rows: BIG.rows }),
    );
    await untilColours(budget, smallPoints, [RED, GREEN], "the small picture still draws with a budget of 1 MiB");
    await poll(() => (budget.net.sent("image_data").some((a) => a.image === bigId) ? true : undefined), "the page asked for the big picture's first piece", 15_000);
    await sleep(1_500); // the answer to that request and the draw after it need this window to show up if the budget were ignored
    const refused = (await colours(budget, bigPoint))[0];
    check(!isColour(refused, WHITE, 40), `the 2 MiB picture is not drawn within a budget of 1 MiB (centre ${hex(refused)})`);
    const askedSmall = budget.net.sent("image_data").filter((a) => a.image === bigId);
    equal(JSON.stringify(askedSmall.map((a) => a.offset)), JSON.stringify([0]), "the page asked for no more than the first piece of the big picture");

    // The control: with the budget back at 64 MiB the same picture draws and all four pieces are pulled.
    equal(JSON.stringify(await setBudget(64)), JSON.stringify({ failure: null, now: 64 }), "the budget is back at 64 MiB");
    await budget.sock.ok("request_full", { pane: budget.pane });
    await poll(async () => (isColour((await colours(budget, bigPoint))[0], WHITE) ? true : undefined), "the big picture draws with a budget of 64 MiB", 20_000);
    const offsets = [...new Set(budget.net.sent("image_data").filter((a) => a.image === bigId).map((a) => a.offset))].sort((a, b) => a - b);
    equal(JSON.stringify(offsets), JSON.stringify([0, 524_288, 1_048_576, 1_572_864]), "the big picture is pulled in four pieces of 512 KiB");
    report.budget.push(`${name}: with 1 MiB image_data calls for the 2 MiB picture ${JSON.stringify(askedSmall.map((a) => a.offset))} (drawn: no); with 64 MiB offsets ${JSON.stringify(offsets)} (drawn: yes)`);
    await budget.page.close();

    // ---- the memory of a picture the program deleted: the page lets go of the bitmap, and with WebGL the GPU lets go of the texture -----
    const mem = await appPage(renderer, { counters: true });
    const grace = await mem.page.evaluate(async () => (await import(new URL("images.js", location.href).href)).UNUSED_GRACE_MS);
    check(Number.isFinite(grace) && grace > 0, `the page names its grace (${grace})`);
    const store = () =>
      mem.page.evaluate(async (pane) => (await import(new URL("images.js", location.href).href)).images.stats(pane), mem.pane);
    const liveTextures = () => mem.page.evaluate(() => window.__TEX__.made - window.__TEX__.deleted);
    const memImage = image + 4;
    const memPoints = cells(mem.grid, SMALL, 2);
    // What the earlier cases left on the screen is let go of after the grace; the count is taken once the page holds nothing.
    await run(mem, `clear-memory-${renderer}`, RESET);
    await poll(async () => {
      const held = await store();
      return held.held === 0 && held.jobs === 0 ? true : undefined;
    }, "the page holds no bitmap once the screen is clear", grace + 15_000);
    await sleep(700); // the draw after the sweep deletes the textures
    const baseline = await liveTextures();
    await run(mem, `memory-${renderer}`, RESET, cursorTo(SMALL.col, SMALL.row), kittyPicture({ id: memImage, width: 2, height: 1, rgba: RED_GREEN, cols: 2, rows: 1 }));
    await untilColours(mem, memPoints, [RED, GREEN], `the picture is on the ${name} canvas for the memory case`);
    equal((await store()).held, 1, "the page holds the picture's bitmap while it is on screen");
    const withPicture = renderer === "webgl" ? await poll(async () => ((await liveTextures()) === baseline + 1 ? baseline + 1 : undefined), "one live texture more with the picture on screen", 5_000) : null;
    // The control for the sweep: a placed picture stays however long it is on screen.
    await sleep(grace + 1_000);
    equal((await store()).held, 1, `a picture that is still placed is not let go of after the grace (${grace} ms)`);
    await untilColours(mem, memPoints, [RED, GREEN], "and it is still drawn");
    if (renderer === "webgl") equal(await liveTextures(), baseline + 1, "and its texture is still alive");
    // The program clears the screen: within the grace the bitmap is kept, after it the bitmap and the texture are gone.
    await run(mem, `clear-memory-after-${renderer}`, RESET);
    const clearedAt = Date.now();
    equal((await store()).held, 1, "the bitmap is still held right after the screen was cleared (the grace has not passed)");
    await poll(async () => ((await store()).held === 0 ? true : undefined), "the bitmap is let go of after the grace", grace + 15_000);
    const letGoAfter = Date.now() - clearedAt;
    check(letGoAfter > grace - 2_500, `the bitmap was not let go of before the grace (${letGoAfter} ms after the clear, grace ${grace} ms)`);
    let afterGrace = null;
    if (renderer === "webgl") afterGrace = await poll(async () => ((await liveTextures()) === baseline ? baseline : undefined), "the texture is deleted on the GPU in the draw after the sweep", 5_000);
    equal(JSON.stringify(await store()), JSON.stringify({ held: 0, bytes: 0, jobs: 0 }), "the pane holds nothing");
    report.textures.push(
      renderer === "webgl"
        ? `${name}: live textures before ${baseline}, with the picture ${withPicture}, still ${baseline + 1} after the grace with the picture placed, after the clear and the grace ${afterGrace}; bitmap let go of ${letGoAfter} ms after the clear`
        : `${name}: bitmap let go of ${letGoAfter} ms after the clear (grace ${grace} ms); no texture to count`,
    );
    await mem.page.close();
  }

  owned.expectClean("the context of the pictures test");
  console.log(
    `images [${engine}]: renderers: ${report.renderers.join(", ")}; Pixels: ${report.pixels.join(" | ")}; Motion: ${report.motion.join(" | ")}; Budget: ${report.budget.join(" | ")}; Textures: ${report.textures.join(" | ")}`,
  );

  /** Two expectations that cannot hold. Each must fail; the runner counts the lines. */
  async function selfCheck(app, points, image) {
    await expectFailure("the first cell of the picture is blue", async () => {
      await poll(async () => (isColour((await readColours(app.page, points))[0], BLUE) ? true : undefined), "the first cell of the picture is blue", 2_500);
    });
    await expectFailure("the page holds five pictures", async () => {
      const count = await app.page.evaluate(async (pane) => {
        const { state } = await import(new URL("state.js", location.href).href);
        return state.frames.get(pane)?.images.length;
      }, app.pane);
      equal(count, 5, `the number of pictures the page holds for image ${image}`);
    });
  }
});
