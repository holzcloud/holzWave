// The count of tool calls that wait for a decision on a phone (docs/design.md 6.6, the phone layout, and 6.15, "Waiting"): the phone
// has no title bar, so while calls wait its top strip ends with the button `✦ AI · N`, at least 44 px, and a tap brings the waiting
// chat with its card to the screen as the title-bar button does on the desktop. The platform is mocked (`app_info` answers `android`)
// and the window is sized like a phone in the desktop engines; nothing here was run on a real phone or with a real model.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { APP_DIR } from "./lib.mjs";
import { PROFILE, TOOL_PATH, openCard, scriptTool, sendSide, sendText, tickClock } from "./ai-lib.mjs";

// The phone layout is the Android shell's; the web backend never reports `android`.
export const backends = ["tauri"];
export const backendNote = "the phone layout belongs to the Android shell, which the web backend never is";

const spec = readFileSync(join(APP_DIR, "../docs/design.md"), "utf8");

export default async function ({ engine, page, url, errors }) {
  page.setDefaultTimeout(5000);
  for (const [name, run] of [
    ["the count", countCase],
    ["a tap brings the chat", tapCase],
    ["a chat without a pane", sideCase],
    ["cues and modes", cuesCase],
    ["the fit", fitCase],
    ["elsewhere", elsewhereCase],
    ["words", wordsCase],
  ]) {
    try {
      await run(page, url, engine);
    } catch (e) {
      // The case is named, and the line of the test that failed is kept, because the runner prints one line.
      const where = String(e?.stack ?? "").split("\n").find((l) => l.includes("phone-ai.test.mjs"))?.trim() ?? "";
      throw new Error(`${name}: ${e?.message ?? e} ${where}`);
    }
  }
  assert.deepEqual(errors, [], `the page reported errors (${engine})`);
}

/** One tab with one terminal: the phone of most cases. */
const ONE_TAB = [{ title: "shell", panes: [1], focus: 1 }];

/** The status the vault reports while it is locked (the lock chip shows `LOCKED`). */
const LOCKED = { state: "locked", protection: "auth", has_screen_lock: true, strongbox: false, can_protect: false, notice: "none", reason: null };

/**
 * Open the page as a device would, with the local AI profile, a frozen clock and an in-page layout core that keeps every tab (the
 * mock's own keeps only the shown one): `focus_pane` shows the tab of the pane, `select_tab` shows a tab, and `block_open` splits the
 * focused pane of the shown tab to the right, focuses the new pane and lists its block. The page sees the tabs as the core sends them.
 */
async function boot(page, url, { os = "android", width = 390, height = 844, screen = [width, height], tabs = ONE_TAB, vault = null, shown = 1 } = {}) {
  await page.setViewportSize({ width, height });
  await page.addInitScript(([w, h]) => {
    for (const [key, value] of [["width", w], ["height", h], ["availWidth", w], ["availHeight", h]]) {
      Object.defineProperty(window.screen, key, { get: () => value, configurable: true });
    }
  }, screen);
  await page.addInitScript(
    ([info, start, profiles, locked]) => {
      const mock = window.__MOCK__;
      mock.reset();
      // The toggle `Auto` starts on; these cases send plain messages, so it is pressed once when it is first drawn pressed.
      if (!window.__settled) {
        window.__settled = true;
        const handled = new WeakSet();
        new MutationObserver(() => {
          for (const button of document.querySelectorAll("button.chat-ctx")) {
            if (button.getAttribute("aria-pressed") === null || handled.has(button)) continue;
            handled.add(button);
            if (button.getAttribute("aria-pressed") === "true") button.click();
          }
        }).observe(document, { childList: true, subtree: true, attributes: true, attributeFilter: ["aria-pressed"] });
      }
      // A frozen clock: only `tickClock` moves it, so the half second of the card is the same on every machine.
      window.__REAL_NOW ??= Date.now;
      const base = window.__REAL_NOW();
      window.__CLOCK = 0;
      Date.now = () => base + window.__CLOCK;
      mock.ai.profiles = profiles;
      mock.on("app_info", () => info);
      if (locked !== null) mock.on("vault_status", () => structuredClone(locked));
      const core = { tabs: structuredClone(start), active: 0, rev: 1 };
      window.__CORE = core;
      const publish = () => {
        const t = core.tabs[core.active];
        const all = core.tabs.flatMap((x) => x.panes);
        core.rev += 1;
        mock.setView({
          rev: core.rev,
          workspaces: [{ name: "main", active: true, panes: all.length, pane_ids: all }],
          tabs: core.tabs.map((x, i) => ({ title: x.title, active: i === core.active, focus: x.focus, panes: x.panes.length, pane_ids: [...x.panes] })),
          focus: t.focus,
          panes: t.panes.map((id, i) => [id, { x: i / t.panes.length, y: 0, w: 1 / t.panes.length, h: 1 }]),
          dividers: [],
          pane_names: [],
          windows: 1,
        });
        return mock.view();
      };
      publish();
      window.__BLOCKS = [];
      mock.on("pane_blocks", () => structuredClone(window.__BLOCKS));
      mock.on("focus_pane", ({ pane }) => {
        const at = core.tabs.findIndex((x) => x.panes.includes(pane));
        if (at >= 0) {
          core.active = at;
          core.tabs[at].focus = pane;
        }
        return publish();
      });
      mock.on("select_tab", ({ index }) => {
        if (index >= 0 && index < core.tabs.length) core.active = index;
        return publish();
      });
      mock.on("block_open", ({ kind, source, path }) => {
        const t = core.tabs[core.active];
        const made = Math.max(...core.tabs.flatMap((x) => x.panes)) + 1;
        t.panes.splice(t.panes.indexOf(t.focus) + 1, 0, made);
        t.focus = made;
        window.__BLOCKS.push({ pane: made, kind, source, path, source_label: "This computer" });
        return publish();
      });
    },
    [{ version: "0.0.1-alpha1", os, hostname: "pixel" }, tabs, [PROFILE], vault],
  );
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForFunction((n) => window.__MOCK__.callsOf("start_pane").length >= n, shown);
  await page.evaluate(async () => {
    window.__registry = await import(new URL("actions.js", location.href).href);
  });
  if (vault !== null) await page.waitForSelector("#vault-chip");
}

/** The phone's button in the top strip (there is at most one). */
const phoneButton = (page) => page.locator("#phone-top button.phone-ai");

/** Whether the strip shows the button: it is in the strip and drawn. */
const shows = (page) =>
  page.evaluate(() => {
    const n = document.querySelector("#phone-top button.phone-ai");
    return n !== null && !n.hidden && n.getBoundingClientRect().width > 0;
  });

/** Open a chat the way `✦ AI` does (the action `ai.open`; the phone has no title bar) and wait for its composer. */
async function openChat(page) {
  await page.evaluate(() => window.__registry.runAction("ai.open"));
  await page.waitForSelector(".block-chat textarea.chat-input");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profiles").length >= 1);
}

/** Make `count` calls wait in the chat that is on screen (ids call_1, call_2, …) and wait for their cards. */
async function makeWait(page, count, first = 1, side = false) {
  const calls = Array.from({ length: count }, (_, i) => ({ id: `call_${first + i}`, tool: "read_file", path: TOOL_PATH }));
  await scriptTool(page, { calls, answer: "Done." });
  if (side) await sendSide(page, "read my notes");
  else await sendText(page, "read my notes");
  await openCard(page, `call_${first + count - 1}`);
}

/** The commands the page sent since the mark `from` (an index into the call log). */
const sentSince = (page, from) => page.evaluate((at) => window.__MOCK__.calls.slice(at).map(([cmd, args]) => [cmd, structuredClone(args)]), from);
const callCount = (page) => page.evaluate(() => window.__MOCK__.calls.length);

async function countCase(page, url) {
  await boot(page, url);
  await page.waitForSelector("#phone-top");
  assert.equal(await shows(page), false, "no call waits: the strip draws no AI button");
  await openChat(page);
  assert.equal(await shows(page), false, "an open chat without a waiting call: still no button");

  await makeWait(page, 1);
  await page.waitForFunction(() => document.querySelector("#phone-top button.phone-ai")?.dataset.waiting === "1");
  const button = phoneButton(page);
  assert.equal(await shows(page), true, "one call waits: the strip shows the button");
  assert.equal(await button.textContent(), "✦ AI · 1");
  assert.equal(await button.getAttribute("aria-label"), "AI chat, 1 call waits for your decision");
  assert.equal(await button.getAttribute("type"), "button");
  const place = await button.evaluate((n) => {
    const top = document.getElementById("phone-top").getBoundingClientRect();
    const r = n.getBoundingClientRect();
    const chip = document.getElementById("phone-chip");
    return {
      w: r.width, h: r.height, right: top.right - r.right, inStrip: n.closest("#phone-top") !== null,
      afterChip: (chip.compareDocumentPosition(n) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0 && !chip.contains(n),
      chipLeft: chip.getBoundingClientRect().right <= r.left + 0.5,
    };
  });
  assert.ok(place.w >= 44 && place.h >= 44, `the button is ${place.w} by ${place.h}`);
  assert.ok(place.inStrip && place.afterChip && place.chipLeft, `the button follows the place of the status chip: ${JSON.stringify(place)}`);
  assert.ok(place.right <= 12, `the button is at the right end of the strip (${place.right} px from it)`);
  // The title-bar button of the hidden desktop bar says the same, from the same sum.
  assert.equal(await page.locator("#bar .ai-btn").getAttribute("data-waiting"), "1");

  // Decided: the button goes.
  await tickClock(page, 1000);
  await page.locator("section.chat-approval .approval-allow").click();
  await page.waitForFunction(() => !window.__MOCK__.ai.chat("chat-mock-1").running);
  await page.waitForFunction(() => document.querySelector("#phone-top button.phone-ai")?.dataset.waiting === undefined);
  assert.equal(await shows(page), false, "nothing waits: the button is gone");

  // Two calls: the number and the plural name.
  await makeWait(page, 2, 2);
  await page.waitForFunction(() => document.querySelector("#phone-top button.phone-ai")?.dataset.waiting === "2");
  assert.equal(await button.textContent(), "✦ AI · 2");
  assert.equal(await button.getAttribute("aria-label"), "AI chat, 2 calls wait for your decision");
  await tickClock(page, 1000);
  await page.locator('section[data-call="call_2"] .approval-deny').click();
  await page.waitForFunction(() => document.querySelector("#phone-top button.phone-ai")?.dataset.waiting === "1");
  assert.equal(await button.textContent(), "✦ AI · 1");
  await page.locator('section[data-call="call_3"] .approval-deny').click();
  await page.waitForFunction(() => document.querySelector("#phone-top button.phone-ai")?.dataset.waiting === undefined);
  assert.equal(await shows(page), false, "both decided: the button is gone");
}

async function tapCase(page, url) {
  await boot(page, url, { tabs: [{ title: "shell", panes: [1], focus: 1 }, { title: "work", panes: [2], focus: 2 }] });
  await openChat(page);
  const chatPane = await page.evaluate(() => window.__BLOCKS[0].pane);
  await makeWait(page, 1);
  // The person goes to the other tab: the chat leaves the screen, its call still waits.
  await page.locator("#phone-nav button.phone-tab-btn").nth(1).click();
  await page.waitForFunction(() => window.__CORE.active === 1 && document.querySelector(".block-chat") === null);
  await page.waitForFunction(() => document.querySelector("#phone-top button.phone-ai")?.dataset.waiting === "1");
  assert.equal(await page.locator("section.chat-approval").count(), 0, "the card is not on screen");

  const from = await callCount(page);
  await phoneButton(page).click();
  await page.waitForSelector('.block-chat section.chat-approval[data-call="call_1"]');
  const state = await page.evaluate(() => ({ active: window.__CORE.active, focus: window.__MOCK__.view().focus }));
  assert.deepEqual(state, { active: 0, focus: chatPane }, "the chat's tab is shown and its pane has the focus");
  const card = await page.locator('section.chat-approval[data-call="call_1"]').evaluate((n) => {
    const r = n.getBoundingClientRect();
    return { top: r.top, bottom: r.bottom, h: innerHeight, w: r.width };
  });
  assert.ok(card.w > 0 && card.top >= 0 && card.top < card.h, `the card is on screen: ${JSON.stringify(card)}`);
  assert.equal(await page.locator("#phone-top .phone-tab").textContent().then((t) => t.includes("shell")), true, "the strip names the chat's tab");
  const sent = await sentSince(page, from);
  const names = sent.map(([cmd]) => cmd);
  assert.deepEqual(sent.filter(([cmd]) => cmd === "focus_pane"), [["focus_pane", { pane: chatPane }]], `one focus_pane for the chat's pane: ${names}`);
  for (const bad of ["ai_decide", "block_open", "ai_conversation_new", "select_tab"]) assert.ok(!names.includes(bad), `the tap sent ${bad}: ${names}`);
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.ai.approval.decisions.length), 0, "nothing was decided by the tap");
}

async function sideCase(page, url) {
  // A tablet whose column holds the chat; the window then narrows to a phone (split screen), where there is no column.
  await boot(page, url, { width: 1100, height: 800, screen: [1100, 800] });
  await page.waitForFunction(() => document.documentElement.classList.contains("layout-tablet"));
  await page.evaluate(() => window.__registry.runAction("ai.sidebar"));
  await page.waitForSelector('#sidebar[data-form="chat"] textarea.chat-input');
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profiles").length >= 1);
  await makeWait(page, 1, 1, true);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.waitForFunction(() => document.documentElement.classList.contains("layout-phone"));
  await page.waitForFunction(() => document.querySelector("#phone-top button.phone-ai")?.dataset.waiting === "1");
  assert.equal(await shows(page), true, "the strip made for the phone shows the count");
  assert.equal(await page.locator(".block-chat").count(), 0, "the chat has no pane");

  const from = await callCount(page);
  await phoneButton(page).click();
  await page.waitForSelector('.block-chat section.chat-approval[data-call="call_1"]');
  const sent = await sentSince(page, from);
  const names = sent.map(([cmd]) => cmd);
  const opened = sent.filter(([cmd]) => cmd === "block_open").map(([, args]) => args);
  assert.equal(opened.length, 1, `one chat pane is opened: ${names}`);
  assert.deepEqual([opened[0].kind, opened[0].path], ["chat", "chat-mock-1"], "the pane shows the waiting chat");
  for (const bad of ["ai_decide", "ai_conversation_new"]) assert.ok(!names.includes(bad), `the tap sent ${bad}: ${names}`);
  const focus = await page.evaluate(() => window.__MOCK__.view().focus);
  assert.equal(focus, await page.evaluate(() => window.__BLOCKS.at(-1).pane), "the new chat pane has the focus");
}

/** The colour `token` resolves to on this page, read through an element as the browser computes it. */
const tokenColour = (page, token) =>
  page.evaluate((name) => {
    const probe = document.createElement("span");
    probe.style.color = `var(${name})`;
    document.body.append(probe);
    const colour = getComputedStyle(probe).color;
    probe.remove();
    return colour;
  }, token);

async function cuesCase(page, url, engine) {
  await boot(page, url);
  await openChat(page);
  await makeWait(page, 1);
  await page.waitForFunction(() => document.querySelector("#phone-top button.phone-ai")?.dataset.waiting === "1");
  const look = () =>
    phoneButton(page).evaluate((n) => {
      const s = getComputedStyle(n);
      const count = n.querySelector(".ai-count");
      return {
        text: n.textContent, glyph: n.textContent.trim().charAt(0), label: n.classList.contains("label"), transform: s.textTransform,
        spacing: s.letterSpacing, color: s.color, bg: getComputedStyle(document.getElementById("phone-top")).backgroundColor,
        count: count?.textContent ?? null, countColour: count ? getComputedStyle(count).color : null,
        border: [s.borderTopStyle, s.borderTopWidth, s.borderTopColor], outline: [s.outlineStyle, s.outlineWidth], transition: s.transitionDuration,
      };
    });
  const plain = await look();
  assert.equal(plain.glyph, "✦", "the glyph is the shape");
  assert.equal(plain.count, " · 1", "the number is text");
  assert.equal(plain.countColour, await tokenColour(page, "--hw-warn"), "the number has the warn tone");
  assert.ok(plain.label && plain.transform === "uppercase" && plain.spacing !== "normal", `the label style: ${JSON.stringify(plain)}`);
  assert.notEqual(plain.color, plain.bg, "the words are not the colour of the strip");

  // contrast-high: the words, the number and its tone stay, and the text is apart from the strip.
  await page.evaluate(() => document.documentElement.classList.add("contrast-high"));
  const high = await look();
  assert.equal(high.text, "✦ AI · 1");
  assert.equal(high.countColour, await tokenColour(page, "--hw-warn"));
  assert.notEqual(high.color, high.bg);
  await page.evaluate(() => document.documentElement.classList.remove("contrast-high"));

  // motion-reduce: nothing of the button moves.
  await page.evaluate(() => document.documentElement.classList.add("motion-reduce"));
  assert.ok((await look()).transition.split(",").every((d) => parseFloat(d) === 0), "no transition under motion-reduce");
  await page.evaluate(() => document.documentElement.classList.remove("motion-reduce"));

  // forced colours (Chromium can emulate them; WebKit cannot): the words stay and the button keeps a visible border or outline.
  if (engine === "chromium") {
    await page.emulateMedia({ forcedColors: "active" });
    const forced = await look();
    assert.equal(forced.text, "✦ AI · 1");
    const bordered = forced.border[0] !== "none" && parseFloat(forced.border[1]) >= 1 && forced.border[2] !== "rgba(0, 0, 0, 0)";
    const outlined = forced.outline[0] !== "none" && parseFloat(forced.outline[1]) >= 1;
    assert.ok(bordered || outlined, `the button keeps a frame in forced colours: ${JSON.stringify(forced)}`);
    await page.emulateMedia({ forcedColors: "none" });
  }

  // The keyboard reaches the button in the strip's visual order and shows the 2 px ring.
  const option = engine === "webkit" && process.platform === "darwin"; // WebKit on macOS visits buttons with Option+Tab only
  await page.locator("#phone-top .phone-title").focus();
  const seen = [];
  for (let i = 0; i < 8; i++) {
    const at = await page.evaluate(() => {
      const n = document.activeElement;
      return { cls: n?.className ?? "", x: n?.getBoundingClientRect().left ?? -1, inStrip: !!n?.closest("#phone-top") };
    });
    seen.push(at);
    if (at.cls.includes("phone-ai")) break;
    assert.ok(at.inStrip, `the focus left the strip before the button (${seen.map((s) => s.cls).join(" | ")})`);
    await page.keyboard.press(option ? "Alt+Tab" : "Tab");
  }
  assert.ok(seen.at(-1).cls.includes("phone-ai"), `Tab reaches the button (${seen.map((s) => s.cls).join(" | ")})`);
  for (let i = 1; i < seen.length; i++) assert.ok(seen[i].x >= seen[i - 1].x, `Tab goes left to right (${seen.map((s) => `${s.cls}@${s.x}`).join(" | ")})`);
  const ring = await phoneButton(page).evaluate((n) => ({ width: getComputedStyle(n).outlineWidth, style: getComputedStyle(n).outlineStyle }));
  assert.deepEqual(ring, { width: "2px", style: "solid" }, "the button has the 2 px focus ring");
}

async function fitCase(page, url) {
  // The narrowest phone of the other tests, a long tab name, the lock chip, a remote pane's writer label with Take over, two waiting calls.
  await boot(page, url, { width: 360, height: 760, tabs: [{ title: "a-long-tab-name-for-the-strip", panes: [1], focus: 1 }], vault: LOCKED });
  await openChat(page);
  await makeWait(page, 2);
  // Back to the terminal through the sheet's pane row; it shows a session of a server that another device writes to.
  await page.locator("#phone-top .phone-title").click();
  await page.waitForSelector("#phone-sheet:not([hidden])");
  await page.locator('#phone-sheet .phone-pane[data-pane="1"]').click();
  await page.waitForFunction(() => window.__MOCK__.view().focus === 1);
  await page.evaluate(() => window.__MOCK__.remoteSessions.setPane(1, { remote: { writer: "watching" } }));
  await page.waitForFunction(() => document.querySelector("#phone-chip .ph-writer")?.textContent === "WATCHING");
  await page.waitForFunction(() => document.querySelector("#phone-top button.phone-ai")?.dataset.waiting === "2");
  const fit = await page.evaluate(() => {
    const top = document.getElementById("phone-top").getBoundingClientRect();
    const box = (s) => {
      const n = document.querySelector(s);
      const r = n.getBoundingClientRect();
      return { s, left: r.left, right: r.right, top: r.top, bottom: r.bottom, w: r.width, h: r.height, whole: n.scrollWidth <= n.clientWidth + 1 };
    };
    const order = ["#phone-top .phone-title", "#vault-chip", "#phone-chip .ph-writer", "#phone-chip .ph-take", "#phone-top button.phone-ai"].map(box);
    const name = document.querySelector("#phone-top .phone-tab");
    const ws = document.querySelector("#phone-top .phone-ws");
    return {
      order, top: [top.left, top.right, top.top, top.bottom],
      overflow: document.scrollingElement.scrollWidth > innerWidth || document.getElementById("phone-top").scrollWidth > document.getElementById("phone-top").clientWidth + 1,
      nameCut: name.scrollWidth > name.clientWidth, wsWhole: ws.scrollWidth <= ws.clientWidth + 1,
    };
  });
  const detail = JSON.stringify(fit);
  assert.equal(fit.overflow, false, `nothing of the strip overflows: ${detail}`);
  for (const b of fit.order) {
    assert.ok(b.left >= fit.top[0] - 0.5 && b.right <= fit.top[1] + 0.5, `${b.s} is inside the strip: ${detail}`);
    assert.ok(b.w > 0, `${b.s} is drawn: ${detail}`);
  }
  for (let i = 1; i < fit.order.length; i++) {
    assert.ok(fit.order[i].left >= fit.order[i - 1].right - 0.5, `${fit.order[i].s} does not overlap ${fit.order[i - 1].s}: ${detail}`);
  }
  for (const b of fit.order.slice(1)) assert.ok(b.whole, `${b.s} is not cut: ${detail}`);
  for (const s of ["#phone-top .phone-title", "#vault-chip", "#phone-chip .ph-take", "#phone-top button.phone-ai"]) {
    const b = fit.order.find((o) => o.s === s);
    assert.ok(b.h >= 44 && b.w >= 44, `${s} keeps its 44 px target (${b.w} by ${b.h})`);
  }
  assert.equal(fit.nameCut, true, `the name of the tab or pane is what is cut: ${detail}`);
  assert.equal(fit.wsWhole, true, `the workspace name is not cut: ${detail}`);
}

async function elsewhereCase(page, url) {
  // A tablet, a desktop and a browser window (even one as narrow as a phone) have no phone strip, so no button in one.
  for (const opts of [{ width: 1000, height: 700, screen: [1000, 700] }, { os: "macos" }, { os: "web" }]) {
    await boot(page, url, opts);
    const name = opts.os ?? "tablet";
    assert.equal(await page.locator("#phone-top").count(), 0, `${name}: no phone strip`);
    assert.equal(await page.locator("button.phone-ai").count(), 0, `${name}: no phone button`);
    assert.equal(await page.locator(".ai-btn").count(), 1, `${name}: one AI button, the title bar's`);
    assert.equal((await page.locator(".ai-btn").textContent()).trim(), "✦ AI", `${name}: the title-bar button is unchanged`);
  }
  // The desktop's title-bar button keeps its count as before.
  await boot(page, url, { os: "macos", width: 1000, height: 700 });
  await page.locator(".ai-btn").click();
  await page.waitForSelector(".block-chat textarea.chat-input");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profiles").length >= 1);
  await makeWait(page, 1);
  await page.waitForFunction(() => document.querySelector(".ai-btn")?.dataset.waiting === "1");
  assert.equal(await page.locator(".ai-btn").textContent(), "✦ AI · 1");
  assert.equal(await page.locator(".ai-btn").getAttribute("aria-label"), "AI chat, 1 call waits for your decision");
  assert.equal(await page.locator("button.phone-ai").count(), 0, "macos: still no phone button");
}

async function wordsCase(page, url) {
  // The record says it: 6.15 gives the words and the name, and says that on a phone the count is the strip's button (6.6).
  const waiting = spec.slice(spec.indexOf("### 6.15 Approving a tool call"), spec.indexOf("### 6.16"));
  const para = waiting.slice(waiting.indexOf("**Waiting.**"), waiting.indexOf("\n\n", waiting.indexOf("**Waiting.**"))).replace(/\s+/g, " ");
  assert.ok(para.includes("`✦ AI · 2`"), `6.15 writes the count: ${para}`);
  assert.ok(para.includes("`AI chat, 2 calls wait for your decision`"), `6.15 names the button: ${para}`);
  assert.match(para, /on a phone the same count is the button of the top strip \(6\.6\)/i, `6.15 sends the phone to 6.6: ${para}`);
  const mobile = spec.slice(spec.indexOf("### 6.6 Mobile"), spec.indexOf("### 6.7")).replace(/\s+/g, " ");
  const layout = mobile.slice(mobile.indexOf("**Phone layout.**"), mobile.indexOf("**Groups of the sheet.**"));
  for (const words of ["`✦ AI · N`", "44 px", "warn", "6.15", "6.12", "cut first"]) assert.ok(layout.includes(words), `6.6 says ${words}: ${layout}`);
  assert.ok(!/then the button `✦`;/.test(layout), "6.6 no longer draws a count-less button at the end of the strip");

  // The page's words for two calls are the record's.
  await boot(page, url);
  await openChat(page);
  await makeWait(page, 2);
  await page.waitForFunction(() => document.querySelector("#phone-top button.phone-ai")?.dataset.waiting === "2");
  assert.equal(`\`${await phoneButton(page).textContent()}\``, "`✦ AI · 2`");
  assert.equal(`\`${await phoneButton(page).getAttribute("aria-label")}\``, "`AI chat, 2 calls wait for your decision`");
}
