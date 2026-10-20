// Renders assets/brand/banner.png from banner.svg with Chromium (Playwright).
// The background, logo and text are drawn flat; the screen group (#screen-tilt)
// is turned slightly towards the text, like the other holzcloud README banners.
//
//   node assets/brand/render-banner.cjs
//
// The PNG uses Plus Jakarta Sans (SIL Open Font License 1.1). Pass a CSS file with
// @font-face rules (fonts embedded as data URLs) as the first argument, or install
// the font locally.
const fs = require('fs');
const path = require('path');
// `playwright`, else the playwright-core of app/node_modules (the one the app's tests pin).
let chromium;
try {
  ({ chromium } = require('playwright'));
} catch {
  ({ chromium } = require(path.join(__dirname, '..', '..', 'app', 'node_modules', 'playwright-core')));
}

const dir = __dirname;
const svg = fs.readFileSync(path.join(dir, 'banner.svg'), 'utf8');
const fontCSS = process.argv[2] ? fs.readFileSync(process.argv[2], 'utf8') : '';

const html = `<!doctype html><html><head><style>${fontCSS}
html, body { margin: 0; width: 3200px; height: 800px; overflow: hidden; background: #14091A; }
.layer { position: absolute; inset: 0; width: 3200px; height: 800px; }
.flat #screen-tilt { display: none; }
.screen svg > *:not(defs):not(#screen-tilt) { display: none; }
.screen { transform-origin: 2486px 405px; transform: perspective(2200px) rotateY(-12deg); }
</style></head><body>
<div class="layer flat">${svg}</div>
<div class="layer screen">${svg}</div>
</body></html>`;

(async () => {
  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: 3200, height: 800 } });
  await page.setContent(html, { waitUntil: 'networkidle' });
  await page.evaluate(() => document.fonts.ready);
  await page.screenshot({ path: path.join(dir, 'banner.png') });
  await browser.close();
})();
