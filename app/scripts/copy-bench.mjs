// The renderer benchmark is built apart from the app (`npm run bench:build`) and never lands in dist/.
import { copyFileSync, mkdirSync } from "node:fs";
mkdirSync("dist-bench", { recursive: true });
copyFileSync("bench/index.html", "dist-bench/index.html");
