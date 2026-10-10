// Static file server for the browser tests: 127.0.0.1 only, ephemeral port, files under `root` only.
import { createServer } from "node:http";
import { readFile, realpath } from "node:fs/promises";
import { extname, resolve, sep } from "node:path";

const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".woff2": "font/woff2",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".map": "application/json; charset=utf-8",
};

/**
 * Serve `root` with `csp` as the Content-Security-Policy response header.
 * Resolves to `{ url, close }`; `url` ends with a slash.
 */
export async function startServer({ root, csp }) {
  if (typeof csp !== "string" || csp.length === 0) throw new Error("startServer needs a non-empty csp");
  const base = await realpath(resolve(root));

  const server = createServer(async (req, res) => {
    const send = (status, body = "", headers = {}) => {
      res.writeHead(status, {
        "Cache-Control": "no-store",
        "X-Content-Type-Options": "nosniff",
        "Content-Type": "text/plain; charset=utf-8",
        ...headers,
      });
      res.end(req.method === "HEAD" ? undefined : body);
    };
    if (req.method !== "GET" && req.method !== "HEAD") return send(405, "method not allowed");

    let pathname;
    try {
      pathname = decodeURIComponent(new URL(req.url ?? "/", "http://127.0.0.1").pathname);
    } catch {
      return send(400, "bad request");
    }
    if (pathname.includes("\0")) return send(400, "bad request");
    if (pathname.endsWith("/")) pathname += "index.html";

    let file;
    try {
      // realpath also refuses a symlink that points out of `root`.
      file = await realpath(resolve(base, "." + pathname));
    } catch {
      return send(404, "not found");
    }
    if (file !== base && !file.startsWith(base + sep)) return send(403, "forbidden");

    let body;
    try {
      body = await readFile(file);
    } catch {
      return send(404, "not found");
    }
    send(200, body, {
      "Content-Type": TYPES[extname(file)] ?? "application/octet-stream",
      "Content-Security-Policy": csp,
    });
  });

  await new Promise((ok, fail) => {
    server.once("error", fail);
    server.listen(0, "127.0.0.1", ok);
  });
  const { port } = server.address();

  return {
    url: `http://127.0.0.1:${port}/`,
    close: () =>
      new Promise((ok) => {
        server.close(() => ok());
        server.closeAllConnections();
      }),
  };
}
