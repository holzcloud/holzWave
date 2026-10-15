// Connections on the page: the saved list, the actions that open them (palette: `Connect: <name>`), the dialog that
// manages them, and the once-a-second look at the state of the panes that run one. The core owns everything; this
// module asks and shows. A connection opens in a new tab, which the core makes and answers with the new view.
import { registerAction, unregisterAction } from "./actions.js";
import { openBlock } from "./blocks.js";
import { call } from "./bridge.js";
import { focusInput } from "./keyboard.js";
import { connSummary, paneConns, readConnections, readPaneConns, setPaneConns } from "./connstate.js";
import type { Connection } from "./connstate.js";
import type { View } from "./types.js";

/** How often the state of the panes is asked for. A reconnect counter that moves is the reason for once a second. */
export const POLL_MS = 1000;

let saved: Connection[] = [];
let applyView: (view: View) => void = () => {};
let onChange: () => void = () => {};
const dynamicIds = new Set<string>();

/** The saved connections as last listed. */
export function connections(): readonly Connection[] {
  return saved;
}

/** The words of the palette entry that opens a connection. */
export function connectTitle(c: Connection): string {
  return `Connect: ${c.name} (${c.kind})`;
}

/** Ask the core for the list and make one palette action per connection. Returns the list. */
export async function refreshConnections(): Promise<readonly Connection[]> {
  try {
    saved = readConnections(await call<unknown>("connections_list"));
  } catch {
    saved = [];
  }
  for (const id of dynamicIds) unregisterAction(id);
  dynamicIds.clear();
  for (const c of saved) {
    const id = `connect.${c.id}`;
    if (dynamicIds.has(id)) continue; // the core's ids are unique; a repeat must not break the others
    registerAction({ id, title: connectTitle(c), run: () => void connect(c.id) });
    dynamicIds.add(id);
    if (c.kind === "ssh") {
      // The files of a host, over SFTP on a connection of their own.
      const filesId = `files.${c.id}`;
      registerAction({ id: filesId, title: `Files: ${c.name} (sftp)`, run: () => void openBlock("files", c.id, "", "tab") });
      dynamicIds.add(filesId);
    }
  }
  onChange();
  return saved;
}

/** Open connection `id` in a new tab. Resolves to null on success and to the core's reason on failure. */
export async function connect(id: string): Promise<string | null> {
  try {
    applyView(await call<View>("connect", { id }));
    focusInput();
    return null;
  } catch (e) {
    return String(e);
  }
}

/** Ask for the state of the panes' connections once. True when it changed. */
export async function pollConnections(): Promise<boolean> {
  let raw: unknown = null;
  try {
    raw = await call<unknown>("pane_connections");
  } catch {
    // no answer is no connections
  }
  const changed = setPaneConns(readPaneConns(raw));
  if (changed) onChange();
  return changed;
}

/** The text of the status line's connection segment. */
export function connectionSummary(): string {
  return connSummary(paneConns.values());
}

/**
 * Register `connections.open`, load the list and start the poll. `apply` shows the view a connect answers with;
 * `changed` is called when the list or a pane's state changed, so the headers and the status line draw again;
 * `open` shows the dialog (injected: this module does not import it).
 */
export async function initConnections(
  apply: (view: View) => void,
  changed: () => void,
  open: () => void,
): Promise<void> {
  applyView = apply;
  onChange = changed;
  registerAction({ id: "connections.open", title: "Connections…", run: open });
  await refreshConnections();
  await pollConnections();
  setInterval(() => void pollConnections(), POLL_MS);
}
