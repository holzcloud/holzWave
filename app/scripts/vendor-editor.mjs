// Bundles the code editor (vendor-src/editor.mjs, CodeMirror 6) into dist/vendor/editor. The page itself has no bundler;
// this is the one exception, for a library that is many small packages. esbuild splits each language into a chunk of its
// own, so a file of one language loads that language only. The licences of every package that went into the bundle are
// collected into one LICENSE file next to it.
import { build } from "esbuild";
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

if (!existsSync("vendor-src/editor.mjs") || !existsSync("node_modules/@codemirror/view")) {
  console.warn("vendor-editor: sources or packages are missing, the editor is not built");
  process.exit(0);
}

const out = "dist/vendor/editor";
mkdirSync(out, { recursive: true });

const result = await build({
  entryPoints: ["vendor-src/editor.mjs"],
  outdir: out,
  bundle: true,
  splitting: true,
  format: "esm",
  target: ["es2022", "safari16"],
  minify: true,
  legalComments: "none",
  sourcemap: false,
  metafile: true,
  logLevel: "warning",
});

// The licence of every package that is in the bundle.
const packages = new Set();
for (const input of Object.keys(result.metafile.inputs)) {
  const m = input.match(/^node_modules\/((?:@[^/]+\/)?[^/]+)\//);
  if (m) packages.add(m[1]);
}
let text = "The code editor is a bundle of the packages below, each under the licence that follows its name.\n";
for (const name of [...packages].sort()) {
  const dir = join("node_modules", name);
  const file = readdirSync(dir).find((f) => /^licen[sc]e(\.md|\.txt)?$/i.test(f));
  const pkg = JSON.parse(readFileSync(join(dir, "package.json"), "utf8"));
  text += `\n${"=".repeat(78)}\n${name} ${pkg.version} (${pkg.license ?? "see below"})\n${"=".repeat(78)}\n`;
  text += file ? readFileSync(join(dir, file), "utf8") : "(no licence file in the package)\n";
}
writeFileSync(join(out, "LICENSE"), text);
