// Markdown arrives from the core as HTML that `ammonia` already cleaned (core/src/preview.rs). The page does not trust
// that either, and does not use `innerHTML` (docs/architecture.md): it parses the HTML into an inert document, which
// runs nothing and loads nothing, and copies it into the page node by node through an allow-list of its own: a known
// element, a few known attributes, a link only with an `http`, `https` or `mailto` address. Anything else is dropped;
// an unknown element is unwrapped (its text stays), a script or style is dropped with its content. The copy is also
// bounded in depth, in nodes and in the nodes of one block, so that neither the copy nor the layout of a hostile
// document can hold the page.

/** The elements a document may have; the same list as the core's. */
export const ELEMENTS: ReadonlySet<string> = new Set([
  "a", "p", "br", "hr", "h1", "h2", "h3", "h4", "h5", "h6", "ul", "ol", "li", "blockquote", "pre", "code", "em", "strong",
  "del", "table", "thead", "tbody", "tr", "th", "td", "sup", "div", "span", "input",
]);

/** Elements that are dropped with everything inside them, not unwrapped. */
const DROPPED: ReadonlySet<string> = new Set([
  "script", "style", "iframe", "object", "embed", "svg", "math", "template", "noscript", "textarea", "select", "form", "link", "meta", "title", "base",
]);

/** The elements that lay out as a block: what is inside one is counted apart from what is around it. */
const BLOCKS: ReadonlySet<string> = new Set([
  "p", "hr", "h1", "h2", "h3", "h4", "h5", "h6", "ul", "ol", "li", "blockquote", "pre", "table", "thead", "tbody", "tr", "th",
  "td", "div",
]);

/** The most nesting and the most nodes copied: a hostile document cannot make the copy deep or huge. */
export const MAX_DEPTH = 64;
export const MAX_NODES = 200_000;
/**
 * The most text and inline nodes one block takes (a paragraph, a list item, a cell, the document itself); the rest of
 * that block is left out, the blocks after it are not. Without it the copy is cheap and the layout is not: with
 * `overflow-wrap: anywhere`, which the chat and the Markdown preview set, WebKit lays out a run of inline boxes in time
 * that grows with the square of their number (20 000 boxes took 0.35 s, 100 000 took 8.7 s), so one reply made of
 * 300 000 `<span>`s held the page for seconds. A real block has a few hundred nodes; at this size a whole document of
 * such blocks lays out in a fraction of a second.
 */
export const MAX_BLOCK_NODES = 5_000;

/** Whether `href` is a link the page may show: `http`, `https` or `mailto`, written out. */
export function safeHref(href: string): boolean {
  const trimmed = href.trim();
  return /^(https?:\/\/|mailto:)[^\s]*$/i.test(trimmed);
}

/** Copy `html` into `into` as nodes the allow-list accepts. Returns how many nodes it copied. */
export function appendSafe(html: string, into: Element): number {
  const doc = new DOMParser().parseFromString(`<!doctype html><body>${html}`, "text/html");
  let count = 0;

  const copy = (from: Node, to: Element, depth: number, block: { n: number }): void => {
    for (const child of Array.from(from.childNodes)) {
      if (count >= MAX_NODES) return;
      if (child.nodeType === Node.TEXT_NODE) {
        if (block.n >= MAX_BLOCK_NODES) continue;
        to.append(document.createTextNode(child.textContent ?? ""));
        count += 1;
        block.n += 1;
        continue;
      }
      if (child.nodeType !== Node.ELEMENT_NODE) continue;
      const src = child as Element;
      const tag = src.localName;
      if (DROPPED.has(tag)) continue;
      if (!ELEMENTS.has(tag) || depth >= MAX_DEPTH) {
        // Unwrapped: its text and its allowed children stay, the element itself does not; they count for the block around.
        if (depth < MAX_DEPTH) copy(src, to, depth + 1, block);
        else to.append(document.createTextNode(src.textContent ?? ""));
        continue;
      }
      // A block is never refused for the nodes of the block around it: it has its own count.
      const starts = BLOCKS.has(tag);
      if (!starts && block.n >= MAX_BLOCK_NODES) continue;
      const out = document.createElement(tag);
      count += 1;
      block.n += 1;
      if (tag === "a") {
        const href = src.getAttribute("href") ?? "";
        if (safeHref(href)) {
          out.setAttribute("href", href.trim());
          out.setAttribute("rel", "noopener noreferrer");
          out.setAttribute("target", "_blank");
        }
      } else if (tag === "code") {
        const cls = src.getAttribute("class") ?? "";
        if (/^language-[A-Za-z0-9_+#.-]{1,32}$/.test(cls)) out.setAttribute("class", cls);
      } else if (tag === "input") {
        // A task list's box: a disabled checkbox, nothing else about an input.
        out.setAttribute("type", "checkbox");
        out.setAttribute("disabled", "");
        if (src.hasAttribute("checked")) out.setAttribute("checked", "");
      }
      if (tag !== "input" && tag !== "br" && tag !== "hr") copy(src, out, depth + 1, starts ? { n: 0 } : block);
      to.append(out);
    }
  };

  copy(doc.body, into, 0, { n: 0 });
  return count;
}
