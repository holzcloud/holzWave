// Renders the PNGs of docs/design from docs/design/haarlinie-voll.html, and checks that the page still draws.
//
//   node docs/design/render.cjs --view <data-v> [--os mac|win|lin] --out <png>   one view, the element #stage
//   node docs/design/render.cjs --phones <element id> --out <png>                 one row of phones, that element
//   node docs/design/render.cjs --check                                           every view and every phones row
//   node docs/design/render.cjs --dump <data-v|phones id|all>                     the SHA-256 of the markup of one view or row, or a
//                                                                                 JSON object with one for every view and phones row
//   --html <file>                                                                 with any mode: render this copy of the design page
//                                                                                 instead of the committed one (the default is unchanged)
//
// The dump makes a change that is meant to touch one view provable: a copy of the page before the change (`git show
// <commit>:docs/design/haarlinie-voll.html`) and the page after it are dumped and compared, and every key whose value differs
// is a view or row that changed.
//
// The page is opened from its file address in headless Chromium at 1400 by 1000 with a device scale factor of 2, as
// CLAUDE.md says; the stage is made exactly 1120 by 680 (the page fits it to the width of the window otherwise). Every request that is not a file address is refused, so the web font host is never reached; the
// bundled faces of app/src/fonts (JetBrains Mono and Manrope) are put into the page instead, so the images do not
// depend on what the machine has installed. The view buttons (`#views button[data-v=...]`) and the platform buttons
// (`#os button[data-os=...]`) are the page's own.
//
// It needs `playwright-core` (the version app/package.json pins) in app/node_modules, or anywhere `require` finds it,
// and a Chromium that Playwright can launch (PLAYWRIGHT_BROWSERS_PATH points at one that is not in the default place).
// A later change that draws something new in the page reuses this script for its images.
const crypto = require('crypto');
const fs = require('fs');
const path = require('path');
const { pathToFileURL } = require('url');

const PAGE = path.join(__dirname, 'haarlinie-voll.html');
const FONTS = path.join(__dirname, '..', '..', 'app', 'src', 'fonts');
const VIEWPORT = { width: 1400, height: 1000 };
const SCALE = 2;

/** playwright-core from app/node_modules, else wherever `require` finds it. */
function loadPlaywright() {
  try {
    return require(path.join(__dirname, '..', '..', 'app', 'node_modules', 'playwright-core'));
  } catch {
    return require('playwright-core');
  }
}

/** The @font-face rules of the bundled faces, as data URLs: the page is a file and a font from another file may be refused. */
function fontCss() {
  const face = (family, file, weight, style) => {
    const data = fs.readFileSync(path.join(FONTS, file)).toString('base64');
    return `@font-face{font-family:'${family}';font-style:${style};font-weight:${weight};font-display:block;src:url(data:font/woff2;base64,${data}) format('woff2')}`;
  };
  return [
    face('JetBrains Mono', 'JetBrainsMono-Regular.woff2', '400 500', 'normal'),
    face('JetBrains Mono', 'JetBrainsMono-Bold.woff2', '600 800', 'normal'),
    face('JetBrains Mono', 'JetBrainsMono-Italic.woff2', '400', 'italic'),
    face('Manrope', 'Manrope-Variable-latin.woff2', '200 800', 'normal'),
  ].join('\n');
}

const usage = () => {
  console.error('usage: node docs/design/render.cjs --view <data-v> [--os mac|win|lin] --out <png>');
  console.error('       node docs/design/render.cjs --phones <element id> --out <png>');
  console.error('       node docs/design/render.cjs --check');
  console.error('       node docs/design/render.cjs --dump <data-v|phones id|all>');
  console.error('       any mode: --html <file> renders that copy of the design page');
  process.exit(2);
};

function parse(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--check') out.check = true;
    else if (['--view', '--os', '--phones', '--out', '--dump', '--html'].includes(a)) {
      const v = argv[++i];
      if (v === undefined || v.startsWith('--')) usage();
      out[a.slice(2)] = v;
    } else usage();
  }
  const modes = [out.check, out.view, out.phones, out.dump].filter((m) => m !== undefined).length;
  if (modes !== 1) usage();
  if ((out.view || out.phones) && !out.out) usage();
  if (out.os && !['mac', 'win', 'lin'].includes(out.os)) usage();
  if (out.os && !out.view) usage();
  return out;
}

/** The page in a fresh browser: the fonts in, every request that is not a file address refused. `file` is another copy of the page. */
async function open(file = PAGE) {
  const { chromium } = loadPlaywright();
  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: VIEWPORT, deviceScaleFactor: SCALE });
  const problems = [];
  const refused = [];
  page.on('pageerror', (e) => problems.push(`page error: ${e.message}`));
  await page.route('**/*', (route) => {
    const url = route.request().url();
    if (url.startsWith('file:') || url.startsWith('data:')) return route.continue();
    refused.push(url);
    return route.abort();
  });
  await page.goto(pathToFileURL(path.resolve(file)).href);
  await page.addStyleTag({ content: fontCss() });
  // `#fit` has a 1 px border on each side, so 1122 px leave the stage 1120 px: its own size, and a scale of 1.
  await page.addStyleTag({ content: '#fit{width:1122px}' });
  await page.evaluate(() => {
    fit();
    return document.fonts.ready;
  });
  return { browser, page, problems, refused };
}

async function show(page, view, os) {
  if (os) await page.click(`#os button[data-os="${os}"]`);
  await page.click(`#views button[data-v="${view}"]`);
  await page.evaluate(() => document.fonts.ready);
}

async function check(file) {
  const { browser, page, problems, refused } = await open(file);
  const failures = [...problems];
  const views = await page.$$eval('#views button', (bs) => bs.map((b) => b.dataset.v));
  if (views.length === 0) failures.push('the view picker has no buttons');
  for (const view of ['kipane', 'kistates', 'kiask', 'kioffer', 'kiapprove', 'kiapprovestates', 'kiwrite', 'kiwritestates', 'kicommand', 'kicommandstates', 'kiset', 'kichats', 'kisidecards', 'kiactions', 'kiexplain', 'kidescribe', 'kiwsh', 'kiwshstates', 'kidevice', 'kidevicestates', 'kimon', 'kimonstates']) if (!views.includes(view)) failures.push(`the view ${view} is missing`);
  for (const view of views) {
    await show(page, view);
    const stage = await page.$eval('#stage .m', (m) => m.children.length).catch(() => 0);
    const note = await page.$eval('#note', (n) => n.textContent.trim().length);
    if (stage === 0) failures.push(`the view ${view} drew nothing`);
    if (note === 0) failures.push(`the view ${view} has no note`);
  }
  // The card of a `wsh ai` request: the two buttons and the words that say the destination leaves this computer, and four cards in the states view.
  await show(page, 'kiwsh');
  const wshText = await page.$eval('#stage', (n) => n.textContent);
  for (const words of ['Send once', "Don't send", 'leaves this computer']) if (!wshText.includes(words)) failures.push(`the view kiwsh does not show ${words}`);
  await show(page, 'kiwshstates');
  const wshCards = await page.$$eval('#stage .aw-card', (n) => n.length);
  if (wshCards !== 4) failures.push(`the view kiwshstates holds ${wshCards} cards, not 4`);
  // The on-device model: the chat with its row and the place word, and the four frames of the states view with the button and the reason.
  await show(page, 'kidevice');
  const deviceText = await page.$eval('#stage', (n) => n.textContent);
  for (const words of ['Apple on-device', 'on device']) if (!deviceText.includes(words)) failures.push(`the view kidevice does not show ${words}`);
  await show(page, 'kidevicestates');
  const deviceFrames = await page.$$eval('#stage .ad-frame', (n) => n.length);
  if (deviceFrames !== 4) failures.push(`the view kidevicestates holds ${deviceFrames} frames, not 4`);
  const deviceStates = await page.$eval('#stage', (n) => n.textContent);
  for (const words of ['Check again', 'Apple Intelligence is switched off.']) if (!deviceStates.includes(words)) failures.push(`the view kidevicestates does not show ${words}`);
  // The monitor of another computer: the pane with the host's name in its header, and the four frames of the states view with the words of each state.
  await show(page, 'kimon');
  const monText = await page.$eval('#stage', (n) => n.textContent);
  for (const words of ['talos-1 · 1 s', 'system', 'node server']) if (!monText.includes(words)) failures.push(`the view kimon does not show ${words}`);
  await show(page, 'kimonstates');
  const monFrames = await page.$$eval('#stage .mo-frame', (n) => n.length);
  if (monFrames !== 4) failures.push(`the view kimonstates holds ${monFrames} frames, not 4`);
  const monStates = await page.$eval('#stage', (n) => n.textContent);
  for (const words of ['Connecting to talos-1', 'No answer from talos-1', 'Not supported: build-mac does not run Linux']) if (!monStates.includes(words)) failures.push(`the view kimonstates does not show ${words}`);
  // The chords of the AI actions follow the platform switch: the symbols on macOS, the words on Windows and Linux.
  const keysOf = { mac: ['⇧⌘Y', '⇧⌘G'], win: ['Ctrl+Shift+Y', 'Ctrl+Shift+G'], lin: ['Ctrl+Shift+Y', 'Ctrl+Shift+G'] };
  for (const [os, wanted] of Object.entries(keysOf)) {
    await show(page, 'kiactions', os);
    const text = await page.$eval('#stage', (n) => n.textContent);
    for (const chord of wanted) if (!text.includes(chord)) failures.push(`the view kiactions does not show ${chord} under ${os}`);
  }
  await page.click('#os button[data-os="mac"]');
  const phones = await page.$$eval('#phones .ph1', (n) => n.length);
  if (phones !== 3) failures.push(`#phones holds ${phones} phones, not 3`);
  const chatPhones = await page.$$eval('#phones-chat .ph1', (n) => n.length);
  if (chatPhones !== 2) failures.push(`#phones-chat holds ${chatPhones} phones, not 2`);
  const providerPhones = await page.$$eval('#phones-aiset .ph1', (n) => n.length);
  if (providerPhones !== 2) failures.push(`#phones-aiset holds ${providerPhones} phones, not 2`);
  const consentPhones = await page.$$eval('#phones-consent .ph1', (n) => n.length);
  if (consentPhones !== 2) failures.push(`#phones-consent holds ${consentPhones} phones, not 2`);
  const approvalPhones = await page.$$eval('#phones-approval .ph1', (n) => n.length);
  if (approvalPhones !== 2) failures.push(`#phones-approval holds ${approvalPhones} phones, not 2`);
  const writePhones = await page.$$eval('#phones-write .ph1', (n) => n.length);
  if (writePhones !== 2) failures.push(`#phones-write holds ${writePhones} phones, not 2`);
  const commandPhones = await page.$$eval('#phones-command .ph1', (n) => n.length);
  if (commandPhones !== 2) failures.push(`#phones-command holds ${commandPhones} phones, not 2`);
  const sidePhones = await page.$$eval('#phones-side .ph1', (n) => n.length);
  if (sidePhones !== 2) failures.push(`#phones-side holds ${sidePhones} phones, not 2`);
  const actionPhones = await page.$$eval('#phones-actions .ph1', (n) => n.length);
  if (actionPhones !== 2) failures.push(`#phones-actions holds ${actionPhones} phones, not 2`);
  const wshPhones = await page.$$eval('#phones-wsh .ph1', (n) => n.length);
  if (wshPhones !== 2) failures.push(`#phones-wsh holds ${wshPhones} phones, not 2`);
  failures.push(...problems.filter((p) => !failures.includes(p)));
  await browser.close();
  console.log(`${views.length} views, ${phones} + ${chatPhones} + ${providerPhones} + ${consentPhones} + ${approvalPhones} + ${writePhones} + ${commandPhones} + ${sidePhones} + ${actionPhones} + ${wshPhones} phones, ${refused.length} requests refused (the web font host)`);
  if (failures.length > 0) {
    for (const f of failures) console.error(`FAIL ${f}`);
    process.exit(1);
  }
  console.log('ok');
}

const sha256 = (text) => crypto.createHash('sha256').update(text).digest('hex');

/**
 * The SHA-256 of the rendered markup of every view (the stage and the note under the picker) and of every row of phones (the
 * markup inside the row), by id. `which` is one id or `all`; no image is made.
 */
async function dump(which, file) {
  const { browser, page, problems } = await open(file);
  try {
    const views = await page.$$eval('#views button', (bs) => bs.map((b) => b.dataset.v));
    const rows = await page.$$eval('.phones', (ns) => ns.map((n) => n.id));
    const hashes = {};
    for (const view of views) {
      if (which !== 'all' && which !== view) continue;
      await show(page, view);
      const stage = await page.$eval('#stage', (n) => n.innerHTML);
      const note = await page.$eval('#note', (n) => n.textContent);
      hashes[view] = sha256(`${stage}\n${note}`);
    }
    for (const row of rows) {
      if (which !== 'all' && which !== row) continue;
      hashes[row] = sha256(await page.$eval(`#${row}`, (n) => n.innerHTML));
    }
    if (which !== 'all' && hashes[which] === undefined) throw new Error(`no view or phones row ${which}`);
    console.log(which === 'all' ? JSON.stringify(hashes, null, 2) : hashes[which]);
  } finally {
    await browser.close();
  }
  if (problems.length > 0) {
    for (const p of problems) console.error(`FAIL ${p}`);
    process.exit(1);
  }
}

async function main() {
  const args = parse(process.argv.slice(2));
  if (args.check) return check(args.html);
  if (args.dump !== undefined) return dump(args.dump, args.html);
  const { browser, page, problems } = await open(args.html);
  try {
    if (args.view) {
      await show(page, args.view, args.os);
      await page.locator('#stage').screenshot({ path: args.out });
    } else {
      const target = page.locator(`#${args.phones}`);
      if ((await target.count()) !== 1) throw new Error(`no element #${args.phones}`);
      await target.screenshot({ path: args.out });
    }
  } finally {
    await browser.close();
  }
  if (problems.length > 0) {
    for (const p of problems) console.error(`FAIL ${p}`);
    process.exit(1);
  }
  console.log(`wrote ${args.out}`);
}

main().catch((e) => {
  console.error(e.message);
  process.exit(1);
});
