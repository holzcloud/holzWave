// What ships: app/dist is what Tauri's `frontendDist` serves, so it may hold only index.html, compiled modules,
// stylesheets and fonts. These tests run after `npm run build` (see `npm test`) and look at the real output; the
// copy step is exercised by running the real scripts/copy-static.mjs in a scratch directory, never a mock.
import test from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";

const APP = fileURLToPath(new URL("../../", import.meta.url));
const DIST = join(APP, "dist");
const SRC = join(APP, "src");
const COPY_STATIC = join(APP, "scripts", "copy-static.mjs");

/** Every file under `dir` as a forward-slash path relative to it, sorted. */
function listFiles(dir) {
  const out = [];
  const walk = (d) => {
    for (const e of readdirSync(d, { withFileTypes: true })) {
      const p = join(d, e.name);
      if (e.isDirectory()) walk(p);
      else out.push(relative(dir, p).split(sep).join("/"));
    }
  };
  walk(dir);
  return out.sort();
}

/** Run the real copy-static.mjs with `root` as the working directory, as `npm run build` does inside app/. */
function runCopyStatic(root) {
  execFileSync(process.execPath, [COPY_STATIC], { cwd: root, stdio: "pipe" });
}

/** A scratch project: `files` maps a path under the root to its content. Removed when `body` settles. */
function withScratch(files, body) {
  const root = mkdtempSync(join(tmpdir(), "holzwave-dist-"));
  try {
    for (const [path, content] of Object.entries(files)) {
      const full = join(root, path);
      mkdirSync(join(full, ".."), { recursive: true });
      writeFileSync(full, content);
    }
    return body(root);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

const ALLOWED = (f) => f === "index.html" || /^[^/]+\.js$/.test(f) || /^[^/]+\.css$/.test(f) || f.startsWith("fonts/");

test("dist holds only index.html, js, css and fonts, and no benchmark, test or map file", () => {
  assert.ok(existsSync(DIST), "app/dist is missing: run `npm run build` first");
  const files = listFiles(DIST);
  const stray = files.filter((f) => !ALLOWED(f));
  assert.deepEqual(stray, [], `files that must not ship: ${stray.join(", ")}`);
  assert.deepEqual(files.filter((f) => /(^|\/)bench/.test(f)), [], "the benchmark must not ship");
  assert.deepEqual(files.filter((f) => /\.test\.js$/.test(f)), [], "test files must not ship");
  assert.deepEqual(files.filter((f) => /\.map$/.test(f)), [], "source maps must not ship");
});

test("dist has index.html and a compiled module for every source module", () => {
  const files = new Set(listFiles(DIST));
  assert.ok(files.has("index.html"), "index.html is missing from dist");
  const modules = readdirSync(SRC).filter((f) => f.endsWith(".ts"));
  assert.ok(modules.length > 0, "src has no modules");
  for (const ts of modules) {
    const js = ts.replace(/\.ts$/, ".js");
    assert.ok(files.has(js), `${js} is missing from dist`);
  }
});

test("every stylesheet and font file in src is present in dist", () => {
  const files = new Set(listFiles(DIST));
  for (const css of readdirSync(SRC).filter((f) => f.endsWith(".css"))) {
    assert.ok(files.has(css), `${css} is missing from dist`);
  }
  const fonts = join(SRC, "fonts");
  if (existsSync(fonts)) {
    for (const f of listFiles(fonts)) assert.ok(files.has(`fonts/${f}`), `fonts/${f} is missing from dist`);
  }
});

test("copy-static copies index.html, every stylesheet and the fonts directory, and nothing else", () => {
  withScratch(
    {
      "src/index.html": "<!doctype html>",
      "src/app.css": "body{}",
      "src/theme.css": ":root{}",
      "src/fonts/Mono-Regular.woff2": "r",
      "src/fonts/Mono-Bold.woff2": "b",
      "src/fonts/extra/Mono-Italic.woff2": "i",
      "src/index-bench.html": "<!doctype html>",
      "src/notes.txt": "not shipped",
      "src/main.ts": "export {};",
    },
    (root) => {
      runCopyStatic(root);
      assert.deepEqual(listFiles(join(root, "dist")), [
        "app.css",
        "fonts/Mono-Bold.woff2",
        "fonts/Mono-Regular.woff2",
        "fonts/extra/Mono-Italic.woff2",
        "index.html",
        "theme.css",
      ]);
    },
  );
});

test("copy-static still succeeds when src has no stylesheet and no fonts directory", () => {
  withScratch({ "src/index.html": "<!doctype html>" }, (root) => {
    runCopyStatic(root);
    assert.deepEqual(listFiles(join(root, "dist")), ["index.html"]);
    assert.ok(!existsSync(join(root, "dist", "fonts")), "an empty fonts directory must not be created");
  });
});
