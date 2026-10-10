// Page hygiene (DESK-17): the page carries no inline style, its stylesheet is a file, later plans load their own
// stylesheets through one helper, and the start-up does not need `unsafe-inline` in the policy.
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
