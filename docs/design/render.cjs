// Renders the PNGs of docs/design from docs/design/haarlinie-voll.html, and checks that the page still draws.
//
//   node docs/design/render.cjs --view <data-v> [--os mac|win|lin] --out <png>   one view, the element #stage
//   node docs/design/render.cjs --phones <element id> --out <png>                 one row of phones, that element
//   node docs/design/render.cjs --check                                           every view and every phones row
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
  process.exit(2);
};

function parse(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--check') out.check = true;
    else if (['--view', '--os', '--phones', '--out'].includes(a)) {
      const v = argv[++i];
      if (v === undefined || v.startsWith('--')) usage();
      out[a.slice(2)] = v;
    } else usage();
  }
  const modes = [out.check, out.view, out.phones].filter((m) => m !== undefined).length;
  if (modes !== 1) usage();
  if ((out.view || out.phones) && !out.out) usage();
  if (out.os && !['mac', 'win', 'lin'].includes(out.os)) usage();
  if (out.os && !out.view) usage();
  return out;
}

/** The page in a fresh browser: the fonts in, every request that is not a file address refused. */
async function open() {
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
  await page.goto(pathToFileURL(PAGE).href);
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

async function check() {
  const { browser, page, problems, refused } = await open();
  const failures = [...problems];
  const views = await page.$$eval('#views button', (bs) => bs.map((b) => b.dataset.v));
  if (views.length === 0) failures.push('the view picker has no buttons');
  for (const view of ['kipane', 'kistates', 'kiset']) if (!views.includes(view)) failures.push(`the view ${view} is missing`);
  for (const view of views) {
    await show(page, view);
    const stage = await page.$eval('#stage .m', (m) => m.children.length).catch(() => 0);
    const note = await page.$eval('#note', (n) => n.textContent.trim().length);
    if (stage === 0) failures.push(`the view ${view} drew nothing`);
    if (note === 0) failures.push(`the view ${view} has no note`);
  }
  const phones = await page.$$eval('#phones .ph1', (n) => n.length);
  if (phones !== 3) failures.push(`#phones holds ${phones} phones, not 3`);
  const chatPhones = await page.$$eval('#phones-chat .ph1', (n) => n.length);
  if (chatPhones !== 2) failures.push(`#phones-chat holds ${chatPhones} phones, not 2`);
  const providerPhones = await page.$$eval('#phones-aiset .ph1', (n) => n.length);
  if (providerPhones !== 2) failures.push(`#phones-aiset holds ${providerPhones} phones, not 2`);
  failures.push(...problems.filter((p) => !failures.includes(p)));
  await browser.close();
  console.log(`${views.length} views, ${phones} + ${chatPhones} + ${providerPhones} phones, ${refused.length} requests refused (the web font host)`);
  if (failures.length > 0) {
    for (const f of failures) console.error(`FAIL ${f}`);
    process.exit(1);
  }
  console.log('ok');
}

async function main() {
  const args = parse(process.argv.slice(2));
  if (args.check) return check();
  const { browser, page, problems } = await open();
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
