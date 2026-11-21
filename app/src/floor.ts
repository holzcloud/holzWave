// The one script of the application document. On Android it checks, before anything of the application runs, that the system
// WebView has every feature the page uses, and only then loads the application module `/main.js`; on an older WebView it shows
// one notice instead (docs/design.md section 6.6, `Web view too old`), so nothing of the application runs and nothing throws.
// On the desktop and in a desktop browser it loads the application at once: the floor is the Android WebView's.
//
// A classic script in the syntax of ES2017, so that an old WebView can parse it (no optional chaining, no nullish operators, no
// class fields, no import or export), wrapped in a function so that nothing leaks into the global scope. The floor and the checks
// follow the feature table of tests/unit/webview-floor.test.mjs, which reads them from the built file: a feature the page starts
// to use, or a floor that is not the highest version among the used features, fails that test.
(function () {
  /** The oldest Android System WebView the page runs on: the highest first version among the features the built page uses. */
  var FLOOR = 125;

  var LABEL = "Web view too old";
  var NEEDS = "holzWave needs Android System WebView ";
  var OR_LATER = " or later.";
  var HAS = " This phone has version ";
  var UPDATE = "Update Android System WebView in the Play Store, then open holzWave again.";

  /** Built-ins newer than Chrome 80 that the page calls. */
  var BUILTINS = [
    function () { return typeof "".replaceAll === "function"; },
    function () { return typeof Element.prototype.replaceChildren === "function"; },
    function () { return typeof [].at === "function" && typeof "".at === "function"; },
    function () { return typeof Object.hasOwn === "function"; },
    function () { return "inert" in HTMLElement.prototype; },
  ];

  /** Properties, values and selectors newer than Chrome 80 that the stylesheets use, as `CSS.supports` reads them. */
  var SUPPORTS = [
    "color-scheme: dark",
    "appearance: none",
    "selector(:focus-visible)",
    "inset: 0",
    "text-underline-offset: 1px",
    "selector(:is(a))",
    "text-decoration-thickness: 1px",
    "forced-color-adjust: none",
    "accent-color: auto",
    "selector(:has(a))",
    "height: 1dvh",
    "color: color-mix(in srgb, currentcolor, transparent)",
  ];

  /** Media features the page queries. Each list names every value, so it matches when the feature is known and never when it is not. */
  var MEDIA = [
    "(forced-colors: none), (forced-colors: active)",
    "(prefers-contrast: no-preference), (prefers-contrast: more), (prefers-contrast: less), (prefers-contrast: custom)",
  ];

  /** The major version the WebView names in its user agent (`Chrome/125.0…`), or 0 when it names none. */
  function webviewVersion(agent: string): number {
    var m = /\bChrome\/(\d+)\./.exec(agent);
    return m && m[1] ? parseInt(m[1], 10) : 0;
  }

  /**
   * Whether the WebView has everything the page uses. A version below the floor fails on its own: syntax, the layout contexts
   * and the vendored libraries have no feature test, and the version stands for them. A version that cannot be read leaves the
   * decision to the feature tests.
   */
  function holds(version: number): boolean {
    if (version !== 0 && version < FLOOR) return false;
    try {
      for (var i = 0; i < BUILTINS.length; i++) if (!BUILTINS[i]!()) return false;
      if (typeof CSS === "undefined" || typeof CSS.supports !== "function") return false;
      for (var j = 0; j < SUPPORTS.length; j++) if (!CSS.supports(SUPPORTS[j]!)) return false;
      if (typeof window.matchMedia !== "function") return false;
      for (var k = 0; k < MEDIA.length; k++) if (!window.matchMedia(MEDIA[k]!).matches) return false;
    } catch (e) {
      return false;
    }
    return true;
  }

  function node(tag: string, className: string, text?: string): HTMLElement {
    var el = document.createElement(tag);
    el.className = className;
    if (text !== undefined) el.textContent = text;
    return el;
  }

  /** The notice of the design record in place of the application: its own stylesheet, the label and the two lines. */
  function notice(version: number): void {
    var sheet = document.createElement("link");
    sheet.rel = "stylesheet";
    sheet.href = "/floor.css";
    document.head.appendChild(sheet);
    var app = document.getElementById("app");
    if (app && app.parentNode) app.parentNode.removeChild(app);

    var page = node("div", "floor-page");
    var box = node("div", "floor");
    box.setAttribute("role", "alert");
    var label = node("p", "floor-label label");
    var mark = node("span", "floor-mark", "!");
    mark.setAttribute("aria-hidden", "true");
    label.appendChild(mark);
    label.appendChild(document.createTextNode(LABEL));
    box.appendChild(label);
    box.appendChild(node("p", "floor-line", NEEDS + FLOOR + OR_LATER + (version !== 0 ? HAS + version + "." : "")));
    box.appendChild(node("p", "floor-line", UPDATE));
    page.appendChild(box);
    document.body.appendChild(page);
  }

  /** The application, as the module it is, from the root (the server answers this document at more than one path). */
  function start(): void {
    var script = document.createElement("script");
    script.type = "module";
    script.src = "/main.js";
    document.body.appendChild(script);
  }

  var agent = navigator.userAgent;
  var version = webviewVersion(agent);
  if (/\bAndroid\b/.test(agent) && !holds(version)) notice(version);
  else start();
})();
