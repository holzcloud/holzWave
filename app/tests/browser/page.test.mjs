// Page hygiene (DESK-17): the page carries no inline style, its stylesheet is a file, later plans load their own
// stylesheets through one helper, and the start-up does not need `unsafe-inline` in the policy. Also the renderer
// fallback: a WebGL2 failure after the context was handed out must not break start-up (DESK-26).
import assert from "node:assert/strict";

/** Remember what the document looked like when the parser finished, before any module script ran. */
const recordAtLoad = (page) =>
  page.addInitScript(() => {
    document.addEventListener("readystatechange", () => {
      if (document.readyState !== "interactive") return;
      window.__atLoad = {
        styleElements: document.querySelectorAll("style").length,
        styleAttributes: document.querySelectorAll("[style]").length,
        stylesheetLinks: [...document.querySelectorAll('link[rel="stylesheet"]')].map((l) => l.getAttribute("href")),
      };
    });
  });

export default async function ({ page, url, errors }) {
  await stylesheetCase(page, url);
  await loadStylesCase(page, url);
  await strictPolicyCase(page, url, errors);
  await defaultRendererCase(page, url, errors);
  await fallbackCase(page, url, errors);
}

/** The stylesheet is app.css, applies, and nothing in the page is styled inline. */
async function stylesheetCase(page, url) {
  await recordAtLoad(page);
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");

  // The shipped source, fetched outside the page (its policy has no connect-src for the page's own origin) and
  // parsed without running anything.
  const html = await (await fetch(`${url}index.html`)).text();
  const source = await page.evaluate((text) => {
    const doc = new DOMParser().parseFromString(text, "text/html");
    return {
      styleElements: doc.querySelectorAll("style").length,
      styleAttributes: doc.querySelectorAll("[style]").length,
      links: [...doc.querySelectorAll('link[rel="stylesheet"]')].map((l) => l.getAttribute("href")),
    };
  }, html);
  assert.equal(source.styleElements, 0, "index.html still has a <style> element");
  assert.equal(source.styleAttributes, 0, "index.html still has a style attribute");
  assert.deepEqual(source.links, ["./app.css"], "index.html links exactly the app stylesheet");

  // The live document at the moment parsing ended.
  const atLoad = await page.evaluate(() => window.__atLoad);
  assert.ok(atLoad, "the document never reached readyState interactive");
  assert.equal(atLoad.styleElements, 0, "a <style> element exists at load");
  assert.equal(atLoad.styleAttributes, 0, "an element has a style attribute at load");
  assert.deepEqual(atLoad.stylesheetLinks, ["./app.css"]);

  assert.equal(await page.locator("style").count(), 0, "a <style> element exists");

  const sheets = await page.evaluate(() =>
    [...document.styleSheets].map((s) => ({ href: s.href, rules: s.cssRules.length })),
  );
  const app = sheets.find((s) => s.href?.endsWith("/app.css"));
  assert.ok(app, `document.styleSheets has no app.css sheet: ${JSON.stringify(sheets)}`);
  assert.ok(app.rules > 0, "the app.css sheet has no rules");

  // The rules really apply: the layout tests depend on them.
  const computed = await page.evaluate(() => {
    const css = (sel, prop) => getComputedStyle(document.querySelector(sel))[prop];
    return {
      bodyBg: css("body", "backgroundColor"),
      barHeight: css("#bar", "height"),
      overlayPosition: css("#overlay", "position"),
      overlayEvents: css("#overlay", "pointerEvents"),
      dividerPosition: css(".divider", "position"),
      dividerCursor: css(".divider", "cursor"),
    };
  });
  assert.equal(computed.bodyBg, "rgb(20, 9, 26)", "the ground colour is not applied");
  assert.equal(computed.barHeight, "32px", "the bar rules are not applied");
  assert.equal(computed.overlayPosition, "absolute");
  assert.equal(computed.overlayEvents, "none");
  assert.equal(computed.dividerPosition, "absolute");
  assert.equal(computed.dividerCursor, "col-resize");
}

/** `loadStyles` adds one link per href, resolves when it loaded and rejects when it cannot. */
async function loadStylesCase(page, url) {
  await page.route("**/probe.css", (route) =>
    route.fulfill({ status: 200, contentType: "text/css", body: "#stage { --probe: 41; }" }),
  );
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");

  const result = await page.evaluate(async () => {
    const { loadStyles } = await import(new URL("dom.js", location.href).href);
    const links = (name) => document.querySelectorAll(`link[rel="stylesheet"][href$="${name}"]`).length;
    const before = links("probe.css");
    // Twice at once and once more afterwards: still one link, every promise settles.
    await Promise.all([loadStyles("./probe.css"), loadStyles("./probe.css")]);
    await loadStyles("./probe.css");
    const probe = getComputedStyle(document.getElementById("stage")).getPropertyValue("--probe").trim();
    // The sheet the page already links does not get a twin.
    const appBefore = links("app.css");
    await loadStyles("./app.css");
    let missing = "resolved";
    try {
      await loadStyles("./missing.css");
    } catch (e) {
      missing = `rejected: ${e instanceof Error ? e.message : e}`;
    }
    return { before, after: links("probe.css"), probe, appBefore, appAfter: links("app.css"), missing };
  });

  assert.equal(result.before, 0, "the probe stylesheet was linked before the test asked for it");
  assert.equal(result.after, 1, `loadStyles created ${result.after} links for one href`);
  assert.equal(result.probe, "41", "the loaded stylesheet does not apply when the promise resolved");
  assert.equal(result.appBefore, 1);
  assert.equal(result.appAfter, 1, "loadStyles duplicated the stylesheet that index.html links");
  assert.match(result.missing, /^rejected/, "a stylesheet that cannot be loaded must reject");
}

/**
 * The page starts under a policy without `unsafe-inline` in style-src: the harness serves the policy of
 * tauri.conf.json, so this strips it for one load. Any inline style would be reported as a violation.
 */
async function strictPolicyCase(page, url, errors) {
  await page.route(
    (u) => u.pathname.endsWith("/index.html"),
    async (route) => {
      const response = await route.fetch();
      const headers = response.headers();
      const csp = headers["content-security-policy"] ?? "";
      headers["content-security-policy"] = csp.replace(/\s*'unsafe-inline'/g, "");
      await route.fulfill({ response, headers });
    },
  );
  const response = await page.goto(`${url}index.html?renderer=canvas`);
  const policy = response?.headers()["content-security-policy"] ?? "";
  assert.ok(policy.includes("style-src"), `no style-src in the policy: "${policy}"`);
  assert.ok(!policy.includes("unsafe-inline"), `the strict case still allows unsafe-inline: "${policy}"`);

  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await page.waitForTimeout(100);
  assert.deepEqual(errors, [], "the page reported errors under the strict policy");
  assert.equal(await page.locator("#bar button").count() >= 1, true, "the bar did not render");
}

/** The renderer choice without `?renderer=`: whichever renderer the engine gives, the page starts and draws. */
async function defaultRendererCase(page, url, errors) {
  await page.goto(`${url}index.html`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await page.waitForTimeout(100);
  assert.deepEqual(errors, [], "the page reported errors with the default renderer");
}

const PANE_1_BG = 0x6b2fa0;
const PANE_2_BG = 0x2a6fdb;

const stubFrame = (pane, bg) => ({
  pane, seq: 1, full: true, cols: 20, rows: 4, fg: 0xffffff, bg, cursor: null, scrollbar: [0, 4, 4],
  selection: [], matches: [], search: null, copy_cursor: null,
  modes: { alt: false, bracketed_paste: false, mouse: false }, changed: [],
});

/** One pixel of the 2D canvas as 0xRRGGBB; (x, y) are CSS pixels inside the canvas. */
const pixel = (page, x, y) =>
  page.evaluate(
    ([px, py]) => {
      const c = document.getElementById("term");
      const ctx = c.getContext("2d");
      if (!ctx) throw new Error("the canvas has no 2D context");
      const k = c.width / c.clientWidth;
      const [r, g, b] = ctx.getImageData(Math.floor(px * k), Math.floor(py * k), 1, 1).data;
      return (r << 16) | (g << 8) | b;
    },
    [x, y],
  );

/**
 * WebGL2 hands out its context and then the program fails to link. A canvas that gave out a WebGL context never
 * returns a 2D one, so the scene must draw on a fresh element, and input must still reach the page.
 */
async function fallbackCase(page, url, errors) {
  await page.addInitScript(() => {
    const getContext = HTMLCanvasElement.prototype.getContext;
    window.__forced = { requests: 0, linkCalls: 0 };
    HTMLCanvasElement.prototype.getContext = function (type, ...rest) {
      const ctx = getContext.call(this, type, ...rest);
      if (type === "webgl2" && ctx) {
        window.__forced.requests++;
        this.__original = true; // an expando: it is not an attribute, so a copy of the element does not carry it
      }
      return ctx;
    };
    WebGL2RenderingContext.prototype.linkProgram = function () {
      window.__forced.linkCalls++;
      throw new Error("forced link failure");
    };
  });
  await page.goto(`${url}index.html`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);

  const forced = await page.evaluate(() => window.__forced);
  assert.ok(forced.requests >= 1, "the page never asked for a WebGL2 context: the failure path was not exercised");
  assert.ok(forced.linkCalls >= 1, "the program link was never attempted: the failure path was not exercised");

  const canvas = await page.evaluate(() => {
    const all = document.querySelectorAll("canvas");
    const c = document.getElementById("term");
    return {
      count: all.length,
      original: c.__original === true,
      tabindex: c.getAttribute("tabindex"),
      parent: c.parentElement?.id,
      focused: document.activeElement === c,
    };
  });
  assert.equal(canvas.count, 1, `the page has ${canvas.count} canvas elements, expected the replacement only`);
  assert.equal(canvas.original, false, "the canvas that handed out the WebGL context is still in the page");
  assert.equal(canvas.tabindex, "0", "the fresh canvas lost its tabindex");
  assert.equal(canvas.parent, "stage", "the fresh canvas is not where the old one was");
  assert.equal(canvas.focused, true, "the fresh canvas does not have the keyboard");

  // The Canvas 2D renderer draws: one stub frame per pane, a pixel inside each pane's grid has the pane colour.
  const size = await page.evaluate(() => {
    const c = document.getElementById("term");
    return { w: c.clientWidth, h: c.clientHeight };
  });
  await page.evaluate(
    ([a, b]) => {
      window.__MOCK__.emit("frame", a);
      window.__MOCK__.emit("frame", b);
    },
    [stubFrame(1, PANE_1_BG), stubFrame(2, PANE_2_BG)],
  );
  await page.waitForFunction(
    async () => {
      await new Promise((done) => requestAnimationFrame(() => done()));
      const c = document.getElementById("term");
      const [r, g, b] = c.getContext("2d").getImageData(10, 10, 1, 1).data;
      return ((r << 16) | (g << 8) | b) === 0x6b2fa0;
    },
    undefined,
    { timeout: 5000 },
  );
  assert.equal(await pixel(page, size.w * 0.25, size.h * 0.5), PANE_1_BG, "pane 1 did not draw its colour");
  assert.equal(await pixel(page, size.w * 0.75, size.h * 0.5), PANE_2_BG, "pane 2 did not draw its colour");

  // Pointer, wheel, key and window listeners still reach the page after the swap.
  const origin = await page.evaluate(() => {
    const r = document.getElementById("term").getBoundingClientRect();
    return { left: r.left, top: r.top };
  });
  await page.mouse.click(origin.left + size.w * 0.25, origin.top + size.h * 0.5);
  await page.waitForFunction(() => window.__MOCK__.callsOf("focus_pane").length >= 1, undefined, { timeout: 5000 });
  assert.deepEqual((await page.evaluate(() => window.__MOCK__.callsOf("focus_pane")))[0], { pane: 1 });

  await page.mouse.move(origin.left + size.w * 0.75, origin.top + size.h * 0.5);
  await page.mouse.wheel(0, 120);
  await page.waitForFunction(() => window.__MOCK__.callsOf("scroll").length >= 1, undefined, { timeout: 5000 });
  assert.equal((await page.evaluate(() => window.__MOCK__.callsOf("scroll")))[0].pane, 2);

  await page.keyboard.press("a");
  await page.waitForFunction(
    () => window.__MOCK__.callsOf("input").some((a) => a.data === "a"),
    undefined,
    { timeout: 5000 },
  );

  // The resize listener follows the fresh element.
  await page.setViewportSize({ width: 800, height: 520 });
  await page.waitForFunction(() => document.getElementById("term").width === 800, undefined, { timeout: 5000 });
  assert.equal(await page.evaluate(() => document.getElementById("term").clientWidth), 800);

  assert.deepEqual(errors, [], "the page reported errors after the renderer fallback");
}
