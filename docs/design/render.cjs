// Renders the PNGs of docs/design from docs/design/haarlinie-voll.html, and checks that the page still draws.
//
//   node docs/design/render.cjs --view <data-v> [--os mac|win|lin|web] --out <png>   one view, the element #stage
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
  console.error('usage: node docs/design/render.cjs --view <data-v> [--os mac|win|lin|web] --out <png>');
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
  if (out.os && !['mac', 'win', 'lin', 'web'].includes(out.os)) usage();
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
  for (const view of ['kipane', 'kistates', 'kiask', 'kioffer', 'kiapprove', 'kiapprovestates', 'kiwrite', 'kiwritestates', 'kicommand', 'kicommandstates', 'kiset', 'kichats', 'kisidecards', 'kiactions', 'kiexplain', 'kidescribe', 'kiwsh', 'kiwshstates', 'kidevice', 'kidevicestates', 'kimon', 'kimonstates', 'attn']) if (!views.includes(view)) failures.push(`the view ${view} is missing`);
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
  // The attention marks: the four kinds with their words in the legend, and one tab that shows all four side by side.
  await show(page, 'attn');
  const attnText = await page.$eval('#stage', (n) => n.textContent);
  for (const words of ['waits for you', 'rang the bell', 'command failed', 'command finished']) if (!attnText.includes(words)) failures.push(`the view attn does not show ${words}`);
  const attnAll = await page.$$eval('#stage .tab', (tabs) => tabs.map((t) => [...t.querySelectorAll('.attn .mk')].map((m) => m.textContent).join('')));
  if (attnAll[1] !== '?▲!✓') failures.push(`the second tab of the view attn shows ${attnAll[1]}, not ?▲!✓`);
  // The browser build: seven views, each with the words its section of docs/design.md fixes, and a platform without window buttons.
  for (const view of ['web-signin', 'web-first-admin', 'web-invite', 'web-unlock', 'web-account', 'web-people', 'web-window']) if (!views.includes(view)) failures.push(`the view ${view} is missing`);
  const webWords = {
    'web-signin': ['Sign in with a passkey', 'Sign-in failed. Check your details or ask an admin.', 'Continue with Keycloak'],
    'web-first-admin': ['Set up this server', 'Setup token', 'Password and authenticator app', 'Use at least 12 characters. Not your username, not one repeated character.'],
    'web-invite': ['You were invited', 'Copy link', 'I have saved this code', 'Tick the box to continue.'],
    'web-unlock': ['Your saved passwords are locked', 'That did not unlock your secrets.', 'Use recovery code', 'an admin can reset your secrets'],
    'web-account': ['Confirm it is you', 'Sign out everywhere', 'this session', 'Recovery code'],
    'web-people': ['locked out', 'Unlock sign-in', 'Reset secrets', 'Invitation link'],
    'web-window': ['Drop to upload', 'Download', 'secrets locked', 'Unlock', 'session ends in 4 min', 'Not available in the browser.', 'blocked by the browser'],
  };
  for (const [view, wanted] of Object.entries(webWords)) {
    if (!views.includes(view)) continue;
    await show(page, view);
    const text = await page.$eval('#stage', (n) => n.textContent);
    for (const words of wanted) if (!text.includes(words)) failures.push(`the view ${view} does not show ${words}`);
    const buttons = await page.$$eval('#stage .wc', (ns) => ns.filter((n) => getComputedStyle(n).display !== 'none').length);
    if (buttons !== 0) failures.push(`the view ${view} shows ${buttons} window button groups`);
  }
  await show(page, 'web-invite');
  if ((await page.$$eval('#stage [aria-disabled="true"]', (n) => n.length)) !== 1) failures.push('the view web-invite does not show the disabled Continue of the recovery step');
  for (const [os, groups] of [['mac', 1], ['web', 0]]) {
    await show(page, 'alltag', os);
    const shown = await page.$$eval('#stage .wc', (ns) => ns.filter((n) => getComputedStyle(n).display !== 'none').length);
    if (shown !== groups) failures.push(`the platform ${os} shows ${shown} window button groups, not ${groups}`);
  }
  await page.click('#os button[data-os="mac"]');
  // The chords of the AI actions follow the platform switch: the symbols on macOS, the words on Windows and Linux.
  const keysOf = { mac: ['⇧⌘Y', '⇧⌘G'], win: ['Ctrl+Shift+Y', 'Ctrl+Shift+G'], lin: ['Ctrl+Shift+Y', 'Ctrl+Shift+G'], web: ['Ctrl+Shift+Y', 'Ctrl+Shift+G'] };
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
  const attnPhones = await page.$$eval('#phones-attn .ph1', (n) => n.length);
  if (attnPhones !== 2) failures.push(`#phones-attn holds ${attnPhones} phones, not 2`);
  // Touch (docs/design.md 6.6): the key row states, the selection, pinch and pane list rows, and the tablet view.
  const keyPhones = await page.$$eval('#phones-keys .ph1', (n) => n.length);
  if (keyPhones !== 4) failures.push(`#phones-keys holds ${keyPhones} phones, not 4`);
  const selectPhones = await page.$$eval('#phones-select .ph1', (n) => n.length);
  if (selectPhones !== 3) failures.push(`#phones-select holds ${selectPhones} phones, not 3`);
  // The three states of ctrl differ by a shape (an underline, a mark before the label), not by colour alone; alt stays idle in the locked phone.
  const sticky = await page.$$eval('#phones-keys .ph1', (phones) =>
    phones.map((ph) => {
      const shape = (k) => {
        if (!k) return null;
        const underline = getComputedStyle(k).textDecorationLine.includes('underline');
        const content = getComputedStyle(k, '::before').content;
        return { underline, mark: content !== 'none' && content !== 'normal' };
      };
      return { row: ph.querySelectorAll('.acc').length, ctrl: shape(ph.querySelector('[data-key="ctrl"]')), alt: shape(ph.querySelector('[data-key="alt"]')) };
    }),
  );
  if (sticky.length === 4) {
    const [idle, armed, locked, hardware] = sticky;
    if (!idle.ctrl || idle.ctrl.underline || idle.ctrl.mark) failures.push('#phones-keys: the idle ctrl key shows an underline or a mark');
    if (!armed.ctrl || !armed.ctrl.underline || armed.ctrl.mark) failures.push('#phones-keys: the armed ctrl key is not an underline without a mark');
    if (!locked.ctrl || !locked.ctrl.underline || !locked.ctrl.mark) failures.push('#phones-keys: the locked ctrl key does not show an underline and a mark');
    if (!locked.alt || locked.alt.underline || locked.alt.mark) failures.push('#phones-keys: alt in the locked phone is not idle');
    if (hardware.row !== 0) failures.push('#phones-keys: the phone with a hardware keyboard still draws the key row');
    for (const [i, ph] of [idle, armed, locked].entries()) if (ph.row !== 1) failures.push(`#phones-keys: phone ${i + 1} draws ${ph.row} key rows, not 1`);
  }
  // Every touch target drawn in these rows is 22 px at the half scale of the mock, which stands for 44 px.
  for (const row of ['phones-keys', 'phones-select']) {
    const small = await page.$$eval(`#${row} .acc span, #${row} .bn span, #${row} .it, #${row} .ws, #${row} .sm-h, #${row} .sm-menu span`, (ns) =>
      ns.map((n) => n.getBoundingClientRect()).filter((r) => r.width > 0 && (r.width < 21.5 || r.height < 21.5)).map((r) => `${Math.round(r.width)} by ${Math.round(r.height)}`),
    );
    if (small.length > 0) failures.push(`#${row} draws ${small.length} touch targets under 22 px (${small.join(', ')})`);
  }
  const selectText = await page.$eval('#phones-select', (n) => n.textContent);
  for (const words of ['UI scale 125 %', 'Copy', 'Paste']) if (!selectText.includes(words)) failures.push(`#phones-select does not show ${words}`);
  const handles = await page.$$eval('#phones-select .sm-h', (n) => n.length);
  if (handles !== 2) failures.push(`#phones-select draws ${handles} selection handles, not 2`);
  const paneRows = await page.$$eval('#phones-select .it .sm-pn', (n) => n.length);
  if (paneRows !== 3) failures.push(`#phones-select lists ${paneRows} panes, not 3`);
  if (!views.includes('tablet')) failures.push('the view tablet is missing');
  else {
    await show(page, 'tablet');
    const tablet = await page.$$eval('#stage [data-target]', (ns) => ns.map((n) => n.getBoundingClientRect()).filter((r) => r.width > 0 || r.height > 0));
    const tabletSmall = tablet.filter((r) => r.width < 43.5 || r.height < 43.5);
    if (tablet.length < 20) failures.push(`the view tablet draws ${tablet.length} touch targets, fewer than 20`);
    if (tabletSmall.length > 0) failures.push(`the view tablet draws ${tabletSmall.length} touch targets under 44 px`);
    const tabletParts = await page.$eval('#stage', (n) => ({
      keyRow: n.querySelectorAll('.acc').length,
      panes: n.querySelectorAll('.tb-area .pane').length,
      band: n.querySelectorAll('.tb-hit').length,
      windowButtons: [...n.querySelectorAll('.wc')].filter((w) => getComputedStyle(w).display !== 'none').length,
    }));
    if (tabletParts.keyRow !== 1) failures.push(`the view tablet draws ${tabletParts.keyRow} key rows, not 1`);
    if (tabletParts.panes !== 2) failures.push(`the view tablet draws ${tabletParts.panes} panes, not 2`);
    if (tabletParts.band !== 1) failures.push(`the view tablet draws ${tabletParts.band} divider touch areas, not 1`);
    if (tabletParts.windowButtons !== 0) failures.push('the view tablet draws window buttons');
    const band = await page.$eval('#stage .tb-hit', (n) => n.getBoundingClientRect().width).catch(() => 0);
    if (Math.round(band) !== 24) failures.push(`the divider touch area of the view tablet is ${Math.round(band)} px wide, not 24`);
  }
  // Lock, notice and hand-over (docs/design.md 6.6): two rows of phones, the three chip states told apart by a glyph and a word, and the exact words of the spec.
  const lockPhones = await page.$$eval('#phones-lock .ph1', (n) => n.length);
  if (lockPhones !== 5) failures.push(`#phones-lock holds ${lockPhones} phones, not 5`);
  const handoverPhones = await page.$$eval('#phones-handover .ph1', (n) => n.length);
  if (handoverPhones !== 4) failures.push(`#phones-handover holds ${handoverPhones} phones, not 4`);
  const chips = await page.$$eval('#phones-lock > div:first-child .lk-chip', (ns) =>
    ns.map((n) => ({ state: n.dataset.state, label: n.textContent.trim(), glyph: n.querySelector('svg[data-glyph]')?.dataset.glyph, shape: n.querySelector('svg')?.innerHTML, border: getComputedStyle(n).borderTopStyle })),
  );
  if (chips.length !== 3) failures.push(`the first phone of #phones-lock draws ${chips.length} chip states, not 3`);
  else {
    const labels = chips.map((c) => c.label);
    if (labels.join('|') !== 'LOCKED|OPEN|NO LOCK') failures.push(`the chip labels are ${labels.join('|')}, not LOCKED|OPEN|NO LOCK`);
    if (new Set(chips.map((c) => c.glyph)).size !== 3 || new Set(chips.map((c) => c.shape)).size !== 3) failures.push('the three chip states do not each have a glyph of their own');
    if (chips[2].border !== 'dashed') failures.push('the chip Not protected has no dashed border');
    if (chips[0].border === 'dashed' || chips[1].border === 'dashed') failures.push('a chip other than Not protected has a dashed border');
  }
  // Every touch target of both rows is 22 px at the half scale of the mock; the unlock sheet's Unlock button, the chip, the switch and both hand-over rows are among them.
  for (const [row, wanted] of [['phones-lock', ['.lk-chip', '.lk-sw', '.lk-btn']], ['phones-handover', ['.ho-row', '.ho-ent']]]) {
    const small = await page.$$eval(`#${row} [data-target]`, (ns) => ns.map((n) => n.getBoundingClientRect()).filter((r) => r.width < 21.5 || r.height < 21.5).map((r) => `${Math.round(r.width)} by ${Math.round(r.height)}`));
    if (small.length > 0) failures.push(`#${row} draws ${small.length} touch targets under 22 px (${small.join(', ')})`);
    for (const sel of wanted) if ((await page.$$eval(`#${row} ${sel}[data-target]`, (n) => n.length)) === 0) failures.push(`#${row} draws no ${sel} target`);
  }
  if ((await page.$$eval('#phones-handover .ho-row[data-target]', (n) => n.length)) !== 2) failures.push('#phones-handover does not draw exactly two sheet rows');
  if ((await page.$$eval('#phones-handover .ho-ent[data-target]', (n) => n.length)) !== 2) failures.push('#phones-handover does not draw exactly two palette entries');
  // The words of the spec, each in the spec and in the page, so the two stay in step. No password field anywhere (the app only starts the system prompt).
  const spec = fs.readFileSync(path.join(__dirname, '..', 'design.md'), 'utf8');
  const lockText = (await page.$eval('#phones-lock', (n) => n.textContent)).replace(/\s+/g, ' ');
  const handoverText = (await page.$eval('#phones-handover', (n) => n.textContent)).replace(/\s+/g, ' ');
  const words = {
    lock: [
      'Secrets are locked', 'Unlock with your fingerprint, face or screen lock to use saved passwords, keys and the AI key.', 'Not now', 'Not unlocked',
      'Locked: unlock to connect',
      'Paused: secrets are locked',
      'This device has no screen lock. holzWave still keeps your secrets encrypted with a key of this device, but anyone who can open the phone can use them. Set a screen lock in the system settings to protect them.', 'Use protected mode', 'Dismiss',
      'Protect secrets with the screen lock', 'Asked once when you open holzWave and again after 5 minutes in the background. Live connections keep running while locked.', 'Anyone who can open this phone can then use your saved passwords and keys.', 'This applies to this device only', 'Turn off', 'Keep on',
      'Saved secrets can no longer be read', 'The key that protected them is gone from this device. Your passwords, keys and the AI key have to be entered again.', 'Start fresh', 'Nothing else is lost: layouts, history and settings stay.',
    ],
    handover: [
      'Copy a file into the shell folder', 'Copy a file out of the shell folder', 'Copied report.pdf (1.2 MB) to ~/handover/report.pdf', 'Copied report.pdf (1.2 MB) to ~/handover/report-2.pdf', 'Copied report.pdf (1.2 MB) out',
      'report.pdf is 612 MB; the limit is 256 MB', 'File to copy out, from the shell folder',
      'holzWave is running', '3 shells, 1 connection', 'Stop all',
    ],
  };
  const specText = spec.replace(/\s+/g, ' ');
  for (const [name, list, text] of [['lock', words.lock, lockText], ['handover', words.handover, handoverText]]) {
    for (const w of list) {
      if (!specText.includes(w)) failures.push(`docs/design.md does not hold the words: ${w}`);
      if (!text.includes(w)) failures.push(`#phones-${name} does not show: ${w}`);
    }
  }
  if ((await page.$$eval('#phones-lock input, #phones-lock [type="password"]', (n) => n.length)) !== 0) failures.push('#phones-lock draws an input or a password field');
  // Nothing is drawn for what is not built: no file browser, no folder mirror, no all-files permission, no account screen.
  for (const [name, text] of [['lock', lockText], ['handover', handoverText]]) {
    for (const bad of ['All files', 'Mirror', 'Browse', 'Sign in', 'Enter password', 'PIN']) if (text.includes(bad)) failures.push(`#phones-${name} names a feature that is not built: ${bad}`);
  }
  failures.push(...problems.filter((p) => !failures.includes(p)));
  await browser.close();
  console.log(`${views.length} views, ${phones} + ${chatPhones} + ${providerPhones} + ${consentPhones} + ${approvalPhones} + ${writePhones} + ${commandPhones} + ${sidePhones} + ${actionPhones} + ${wshPhones} + ${attnPhones} + ${keyPhones} + ${selectPhones} + ${lockPhones} + ${handoverPhones} phones, ${refused.length} requests refused (the web font host)`);
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
