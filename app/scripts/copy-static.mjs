// The frontend has no bundler: tsc emits ES modules, this copies the pages next to them.
import { copyFileSync, mkdirSync } from "node:fs";
mkdirSync("dist", { recursive: true });
copyFileSync("src/index.html", "dist/index.html");
