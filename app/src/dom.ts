// DOM helpers. Styles are assigned one property at a time through the style object, never as one piece of text
// or a style attribute: that keeps a strict `style-src` (no `unsafe-inline`) possible.
import type { PxRect } from "./frame.js";

/** Create an element with a class name, optionally appended to `parent`. */
export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  cls = "",
  parent?: Element,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (parent) parent.append(node);
  return node;
}

const sheets = new Map<string, Promise<void>>();

/**
 * Load a stylesheet once. A later feature adds its own file with this instead of editing index.html or app.css,
 * so the page keeps no inline style. Resolves when the sheet applied; rejects when it cannot be loaded. The same
 * href (resolved against the page) gives the same promise and never a second `<link>`; a sheet the page already
 * links counts as loaded. A failed load is forgotten, so a later call can try again.
 */
export function loadStyles(href: string): Promise<void> {
  const url = new URL(href, document.baseURI).href;
  const known = sheets.get(url);
  if (known) return known;

  const done = new Promise<void>((resolve, reject) => {
    const present = Array.from(document.querySelectorAll<HTMLLinkElement>('link[rel="stylesheet"]')).find(
      (l) => l.href === url,
    );
    const link = present ?? document.createElement("link");
    if (present?.sheet) return resolve();
    link.addEventListener("load", () => resolve(), { once: true });
    link.addEventListener(
      "error",
      () => {
        sheets.delete(url); // a later call may try again with a fresh link
        if (!present) link.remove();
        reject(new Error(`stylesheet ${href} could not be loaded`));
      },
      { once: true },
    );
    if (!present) {
      link.rel = "stylesheet";
      link.href = url;
      document.head.append(link);
    }
  });
  sheets.set(url, done);
  return done;
}

/** Set the text of `node` (never markup) when it differs: a render that changes nothing touches nothing. */
export function setText(node: Node, text: string): void {
  if (node.textContent !== text) node.textContent = text;
}

/** Set an attribute, or remove it for `null`, only when that changes it. */
export function setAttr(node: Element, name: string, value: string | null): void {
  if (value === null) {
    if (node.hasAttribute(name)) node.removeAttribute(name);
  } else if (node.getAttribute(name) !== value) {
    node.setAttribute(name, value);
  }
}

/**
 * Keep `nodes` at `count` elements of `parent`: new ones come from `make` and go before `anchor` (the end of
 * `parent` without one), surplus ones are removed from the end. The elements that stay are the same ones.
 */
export function fitNodes<T extends Element>(
  parent: Element,
  nodes: T[],
  count: number,
  make: () => T,
  anchor: Node | null = null,
): void {
  while (nodes.length < count) {
    const node = make();
    parent.insertBefore(node, anchor);
    nodes.push(node);
  }
  while (nodes.length > count) nodes.pop()?.remove();
}

/** Place an absolutely positioned element at `r` (CSS pixels). */
export function setRect(node: HTMLElement, r: PxRect): void {
  node.style.left = `${r.x}px`;
  node.style.top = `${r.y}px`;
  node.style.width = `${r.w}px`;
  node.style.height = `${r.h}px`;
}
