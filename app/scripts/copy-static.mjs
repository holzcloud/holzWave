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
