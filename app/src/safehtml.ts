// Markdown arrives from the core as HTML that `ammonia` already cleaned (core/src/preview.rs). The page does not trust
// that either, and does not use `innerHTML` (docs/architecture.md): it parses the HTML into an inert document, which
// runs nothing and loads nothing, and copies it into the page node by node through an allow-list of its own: a known
// element, a few known attributes, a link only with an `http`, `https` or `mailto` address. Anything else is dropped;
// an unknown element is unwrapped (its text stays), a script or style is dropped with its content.

/** The elements a document may have; the same list as the core's. */
export const ELEMENTS: ReadonlySet<string> = new Set([
  "a", "p", "br", "hr", "h1", "h2", "h3", "h4", "h5", "h6", "ul", "ol", "li", "blockquote", "pre", "code", "em", "strong",
  "del", "table", "thead", "tbody", "tr", "th", "td", "sup", "div", "span", "input",
]);

/** Elements that are dropped with everything inside them, not unwrapped. */
const DROPPED: ReadonlySet<string> = new Set([
  "script", "style", "iframe", "object", "embed", "svg", "math", "template", "noscript", "textarea", "select", "form", "link", "meta", "title", "base",
]);

/** The most nesting and the most nodes copied: a hostile document cannot make the copy deep or huge. */
export const MAX_DEPTH = 64;
export const MAX_NODES = 200_000;

/** Whether `href` is a link the page may show: `http`, `https` or `mailto`, written out. */
export function safeHref(href: string): boolean {
  const trimmed = href.trim();
  return /^(https?:\/\/|mailto:)[^\s]*$/i.test(trimmed);
}

/** Copy `html` into `into` as nodes the allow-list accepts. Returns how many nodes it copied. */
export function appendSafe(html: string, into: Element): number {
  const doc = new DOMParser().parseFromString(`<!doctype html><body>${html}`, "text/html");
  let count = 0;

  const copy = (from: Node, to: Element, depth: number): void => {
    for (const child of Array.from(from.childNodes)) {
      if (count >= MAX_NODES) return;
      if (child.nodeType === Node.TEXT_NODE) {
        to.append(document.createTextNode(child.textContent ?? ""));
        count += 1;
        continue;
      }
      if (child.nodeType !== Node.ELEMENT_NODE) continue;
      const src = child as Element;
      const tag = src.localName;
      if (DROPPED.has(tag)) continue;
      if (!ELEMENTS.has(tag) || depth >= MAX_DEPTH) {
        // Unwrapped: its text and its allowed children stay, the element itself does not.
        if (depth < MAX_DEPTH) copy(src, to, depth + 1);
        else to.append(document.createTextNode(src.textContent ?? ""));
        continue;
      }
      const out = document.createElement(tag);
      count += 1;
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
      if (tag !== "input" && tag !== "br" && tag !== "hr") copy(src, out, depth + 1);
      to.append(out);
    }
  };

  copy(doc.body, into, 0);
  return count;
}
