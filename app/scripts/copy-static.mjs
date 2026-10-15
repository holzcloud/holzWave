// The frontend has no bundler: tsc emits ES modules, this copies the static files next to them.
// Only three kinds of file are copied: index.html, every src/*.css and the src/fonts directory. Anything else in
// src is either compiled by tsc or not part of the app, so the bundle holds nothing unreferenced.
import { copyFileSync, cpSync, existsSync, mkdirSync, readdirSync } from "node:fs";

mkdirSync("dist", { recursive: true });
copyFileSync("src/index.html", "dist/index.html");

for (const entry of readdirSync("src", { withFileTypes: true })) {
  if (entry.isFile() && entry.name.endsWith(".css")) copyFileSync(`src/${entry.name}`, `dist/${entry.name}`);
}

if (existsSync("src/fonts")) cpSync("src/fonts", "dist/fonts", { recursive: true });

// PDF.js (Apache-2.0), the PDF preview. Vendored from node_modules and loaded only when a PDF is opened: the page has no
// bundler, and these are the library's own ES modules. The worker is a module worker from the same origin (worker-src
// follows default-src 'self'), the CMaps and the standard fonts are fetched from the same origin (connect-src 'self').
if (existsSync("node_modules/pdfjs-dist/legacy/build/pdf.min.mjs")) {
  const to = "dist/vendor/pdfjs";
  mkdirSync(to, { recursive: true });
  // The legacy build: the current one needs `Map.prototype.getOrInsertComputed`, which the webviews that ship today (WebKit,
  // WebView2) do not have yet; the legacy build is the same code with the missing features supplied.
  for (const file of ["legacy/build/pdf.min.mjs", "legacy/build/pdf.worker.min.mjs", "LICENSE"]) {
    copyFileSync(`node_modules/pdfjs-dist/${file}`, `${to}/${file.split("/").pop()}`);
  }
  for (const dir of ["cmaps", "standard_fonts"]) cpSync(`node_modules/pdfjs-dist/${dir}`, `${to}/${dir}`, { recursive: true });
}
