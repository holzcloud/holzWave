// DOM helpers. Styles are assigned one property at a time, never through `cssText` or `setAttribute("style")`:
// that keeps a strict `style-src` (no `unsafe-inline`) possible.
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

/** Place an absolutely positioned element at `r` (CSS pixels). */
export function setRect(node: HTMLElement, r: PxRect): void {
  node.style.left = `${r.x}px`;
  node.style.top = `${r.y}px`;
  node.style.width = `${r.w}px`;
  node.style.height = `${r.h}px`;
}
