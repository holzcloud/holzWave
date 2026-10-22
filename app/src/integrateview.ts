// The drawing of the three shapes of a view model (docs/design.md section 6.25): a table, a key-value list and a text.
// Every string in them is the core's cleaned text, and it is set with `textContent` and nothing else: a name that looks
// like an element is shown as those characters. This file has no markup in it, makes no URL and builds no argument.
import { el } from "./dom.js";
import { cutCell } from "./integratestate.js";
import type { KvModel, RowAction, TableModel, TextModel, Tone } from "./integratestate.js";

/** What the block gives the renderers: which row is selected, the actions it may draw and what a press does. */
export interface DrawOptions {
  /** The index of the selected row of a table, or -1. */
  selected: number;
  /** Whether an action is drawn: the block lets only the ones it knows how to run through. */
  drawn(action: RowAction): boolean;
  /** An action was pressed. */
  press(action: RowAction): void;
  /** A row of a table was pressed. */
  pick(index: number): void;
}

const MAX_BUTTON = 40;

function tonal(node: HTMLElement, tone: Tone): void {
  if (tone !== "normal") node.dataset.tone = tone;
}

function actionButton(action: RowAction, o: DrawOptions, parent: HTMLElement, index: number): HTMLButtonElement {
  const b = el("button", "ig-act", parent);
  b.type = "button";
  b.dataset.field = "";
  b.dataset.act = String(index);
  b.textContent = cutCell(action.label, MAX_BUTTON);
  b.addEventListener("click", (ev) => {
    ev.stopPropagation();
    o.press(action);
  });
  return b;
}

/** A header row, one row per entry, the actions of a row at its end. */
export function renderTable(host: HTMLElement, view: TableModel, o: DrawOptions): void {
  host.replaceChildren();
  const table = el("table", "ig-table", host);
  table.setAttribute("role", "table");
  table.setAttribute("aria-label", cutCell(view.title === "" ? "Table" : view.title, 60));
  const withActions = view.rows.some((r) => r.actions.some((a) => o.drawn(a)));
  const thead = el("thead", "", table);
  const head = el("tr", "ig-head", thead);
  head.setAttribute("role", "row");
  for (const column of view.columns) {
    const th = el("th", "ig-th label", head);
    th.scope = "col";
    th.textContent = cutCell(column.label, 60);
  }
  if (withActions) {
    const th = el("th", "ig-th ig-th-act", head);
    th.scope = "col";
    th.setAttribute("aria-label", "Actions");
  }
  const body = el("tbody", "", table);
  view.rows.forEach((row, i) => {
    const tr = el("tr", "ig-row", body);
    tr.setAttribute("role", "row");
    tr.dataset.i = String(i);
    tr.setAttribute("aria-selected", String(i === o.selected));
    tonal(tr, row.tone);
    row.cells.forEach((cell, c) => {
      const td = el("td", "ig-cell", tr);
      const shown = cutCell(cell);
      td.textContent = shown;
      if (shown !== cell) td.title = cell;
      if (c === 0 && (row.tone === "warn" || row.tone === "danger")) td.dataset.mark = "!";
    });
    if (withActions) {
      const td = el("td", "ig-cell ig-acts", tr);
      row.actions.forEach((action, a) => {
        if (o.drawn(action)) actionButton(action, o, td, a);
      });
    }
    tr.addEventListener("click", () => o.pick(i));
  });
}

/** Labelled values. */
export function renderKv(host: HTMLElement, view: KvModel, o: DrawOptions): void {
  host.replaceChildren();
  const list = el("div", "ig-kv", host);
  list.setAttribute("role", "table");
  list.setAttribute("aria-label", cutCell(view.title === "" ? "Details" : view.title, 60));
  for (const row of view.rows) {
    const line = el("div", "ig-kvrow", list);
    line.setAttribute("role", "row");
    tonal(line, row.tone);
    const k = el("span", "ig-k", line);
    k.setAttribute("role", "rowheader");
    k.textContent = cutCell(row.label, 60);
    const v = el("span", "ig-v", line);
    v.setAttribute("role", "cell");
    const shown = cutCell(row.value);
    v.textContent = shown;
    if (shown !== row.value) v.title = row.value;
    if (row.tone === "warn" || row.tone === "danger") line.dataset.mark = "!";
  }
  const actions = view.actions.filter((a) => o.drawn(a));
  if (actions.length > 0) {
    const bar = el("div", "ig-acts ig-kvacts", host);
    actions.forEach((action, a) => actionButton(action, o, bar, a));
  }
}

/** A preformatted text. */
export function renderText(host: HTMLElement, view: TextModel): void {
  host.replaceChildren();
  const pre = el("pre", "ig-text", host);
  pre.tabIndex = 0;
  pre.setAttribute("aria-label", cutCell(view.title === "" ? "Text" : view.title, 60));
  pre.textContent = view.text;
}
