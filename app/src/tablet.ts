// The tablet layout (docs/design.md section 6.6): the desktop's tiled panes, tabs and bar with the changes a finger needs. The numbers
// that differ by layout are decided here, as pure functions that node can test, so the desktop and the phone cannot change by
// accident: the divider's touch area, the pane header's height, and whether a pane can be given a window of its own.
//
// `initTablet` follows the layout class (the `hw:layout` event on `document`; the class is also on the root element, so a module that
// starts late reads it there). On the tablet it adds `tablet.css`; on any other class it removes it again and loads nothing.
import { DIVIDER_HIT, HEADER_REM, TABLET_DIVIDER_HIT, TABLET_HEADER_REM } from "./geometry.js";
import type { Layout } from "./layoutclass.js";

/** CSS px of a divider's touch area across its axis: 24 on the tablet, the desktop's 8 everywhere else. */
export function dividerHit(layout: Layout): number {
  return layout === "tablet" ? TABLET_DIVIDER_HIT : DIVIDER_HIT;
}

/** The pane header's height in root font sizes: 44 px at the base size on the tablet, 24 px everywhere else. */
export function headerRem(layout: Layout): number {
  return layout === "tablet" ? TABLET_HEADER_REM : HEADER_REM;
}

/**
 * Whether a pane or a tab may be given a window of its own on this platform (`app_info`'s name). A browser has no second window to
 * show, and Android's minimum version has none either; every desktop has.
 */
export function tearOffAllowed(os: string): boolean {
  return os !== "web" && os !== "android";
}

// ---- the page --------------------------------------------------------------------------------------

/** The class the tablet module knows; `desktop` until `initTablet` has read the root. */
let current: Layout = "desktop";
const watchers = new Set<() => void>();
let link: HTMLLinkElement | null = null;
let started = false;

/** The layout class as this module saw it last: what the overlay reads for the divider's touch area. */
export function tabletLayout(): Layout {
  return current;
}

/** Call `fn` whenever the class changed (the dividers move to their new width). */
export function onTabletChange(fn: () => void): void {
  watchers.add(fn);
}

/** Ask the page to measure again: the stylesheet changed the bar and so the stage. The scene follows `resize`. */
function remeasure(): void {
  window.dispatchEvent(new Event("resize"));
}

function addSheet(): Promise<void> {
  if (link) return Promise.resolve();
  const node = document.createElement("link");
  node.rel = "stylesheet";
  node.href = new URL("./tablet.css", document.baseURI).href;
  link = node;
  return new Promise((resolve) => {
    node.addEventListener("load", () => resolve(), { once: true });
    node.addEventListener(
      "error",
      () => {
        node.remove();
        if (link === node) link = null;
        resolve(); // the layout still works with the desktop's sizes
      },
      { once: true },
    );
    document.head.append(node);
  });
}

function removeSheet(): void {
  link?.remove();
  link = null;
}

async function follow(next: Layout): Promise<void> {
  if (next === current) return;
  current = next;
  for (const fn of watchers) fn();
  if (next === "tablet") {
    await addSheet();
    if (current !== "tablet") return; // the window changed again while the sheet loaded; that call has the last word
  } else {
    removeSheet();
  }
  remeasure();
}

/** Follow the layout class. Call once; the overlay does at start-up, before the class is first announced. */
export function initTablet(): void {
  if (started) return;
  started = true;
  const root = document.documentElement;
  void follow(root.classList.contains("layout-tablet") ? "tablet" : "desktop");
  document.addEventListener("hw:layout", (e) => {
    const layout = (e as CustomEvent<{ layout?: string }>).detail?.layout;
    void follow(layout === "tablet" ? "tablet" : layout === "phone" ? "phone" : "desktop");
  });
}
