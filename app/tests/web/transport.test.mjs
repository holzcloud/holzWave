// The transport the server chose is the one the browser reports, and a lot of output crosses it intact: the extension string
// of a socket is empty or `permessage-deflate` exactly as the measurement verdict and the server's default say (and the two
// have to agree); 300 numbered lines arrive whole and in order; one line of 200,000 characters arrives intact; and the page's
// own socket never shows an offline or reconnecting mark meanwhile.
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";
import { runTest, signIn, oracle, poll, check, equal } from "./lib.mjs";

const ROOT = fileURLToPath(new URL("../../../", import.meta.url));

// What the measurement decided (first line of its verdict file) and what the server does without a setting (the default
// variant of its compression enum).
const verdict = readFileSync(`${ROOT}spike/web-deflate/VERDICT`, "utf8").split("\n")[0].trim();
const source = readFileSync(`${ROOT}server/src/config.rs`, "utf8");
const enumBody = /pub enum Compression \{([\s\S]*?)\n\}/.exec(source)?.[1] ?? "";
const defaultVariant = /#\[default\]\s*(\w+)/.exec(enumBody)?.[1] ?? "";
const expectDeflate = verdict === "pass" && defaultVariant === "Deflate";
// The verdict and the default must not disagree with each other: a pass with a default of off, or a fail with a default of deflate.
const agree = (verdict === "pass" && defaultVariant === "Deflate") || (verdict === "fail" && defaultVariant === "Off");

const sha = (text) => createHash("sha256").update(text).digest("hex");

await runTest("transport", {}, async ({ harness, open }) => {
  check(agree, `the verdict file says "${verdict}" and the server's default is ${defaultVariant}: they disagree`);

  const person = await harness.account({ role: "member", name: "pt-person" });
  const context = await open();
  const page = await context.context.newPage();
  await signIn(page, harness, person);
  // Every mark the status line shows from now on is noted.
  await page.evaluate(() => {
    window.__marks = new Set();
    const note = () => document.querySelectorAll(".web-mark").forEach((m) => window.__marks.add(m.dataset.mark));
    new MutationObserver(note).observe(document.body, { childList: true, subtree: true });
    note();
  });

  const socket = await oracle(page);
  const extensions = await socket.extensions();
  equal(extensions, expectDeflate ? "permessage-deflate" : "", "the extensions the browser reports for the socket");
  const layout = await poll(async () => {
    const view = await socket.layout();
    return view.panes?.length > 0 ? view : undefined;
  }, "the application shows a pane", 20_000);
  const pane = layout.focus;
  const rows = (await socket.screen(pane)).split("\n").length;

  const typeLine = async (text) => {
    await page.locator("canvas#term").click();
    await page.keyboard.type(text);
    await page.keyboard.press("Enter");
  };
  /**
   * Join pages (each an array of rows, the top one first). The last scroll up is cut short at the top of the history, so the top
   * page may repeat rows of the page below it: the longest run of rows that ends the upper page and starts the lower one is
   * taken once.
   */
  const stitch = (pages) => {
    const out = [...pages[0]];
    for (const next of pages.slice(1)) {
      let k = Math.min(out.length, next.length);
      for (; k > 0; k--) {
        const tail = out.slice(out.length - k);
        if (tail.every((row, i) => row === next[i]) && tail.some((row) => row.trim() !== "")) break;
      }
      out.push(...next.slice(k));
    }
    return out;
  };
  /** All the rows of the pane from the top of its history to its bottom, in order, read page by page with the viewport. */
  const wholeHistory = async (wanted) => {
    await socket.ok("scroll_to_bottom", { pane });
    const pages = [(await socket.screen(pane)).split("\n")];
    for (let i = 0; i < 400; i++) {
      await socket.ok("scroll", { pane, rows: -rows });
      const next = (await socket.screen(pane)).split("\n");
      if (next.join("\n") === pages[0].join("\n")) break;
      pages.unshift(next);
      if (wanted(stitch(pages).join("\n"))) break;
    }
    await socket.ok("scroll_to_bottom", { pane });
    return stitch(pages).join("\n");
  };

  // ---- 300 numbered lines ---------------------------------------------------------------------------------------------------
  await typeLine("i=1; while [ $i -le 300 ]; do printf 'burst-%03d\\n' $i; i=$((i+1)); done; echo burst-done");
  await poll(async () => ((await socket.screen(pane)).split("\n").some((l) => l.trim() === "burst-done") ? true : undefined), "the burst is finished", 30_000);
  const history = await wholeHistory((text) => text.includes("burst-001\n"));
  const numbers = history.split("\n").map((l) => /^burst-(\d{3})$/.exec(l.trim())).filter(Boolean).map((m) => Number(m[1]));
  equal(numbers.length, 300, "the number of burst lines in the history");
  check(numbers.every((n, i) => n === i + 1), "the burst lines are 1 to 300, whole and in order");

  // ---- long lines ------------------------------------------------------------------------------------------------------------------------
  // A line of N characters made of 10-character groups (a counter in hexadecimal and in decimal), so that any lost, repeated or
  // reordered piece shows. The shell prints it, then a marker; the rows of the history between the command and the marker are
  // read back page by page.
  const lineOf = (groups) => Array.from({ length: groups }, (_, i) => i.toString(16).padStart(5, "0") + String(i % 100_000).padStart(5, "0")).join("");
  const printLong = async (groups, marker) => {
    await typeLine(`clear; awk 'BEGIN { for (i = 0; i < ${groups}; i++) printf "%05x%05d", i, i; printf "\\n"; print "${marker}" }'`);
    await poll(async () => ((await socket.screen(pane)).split("\n").some((l) => l.trim() === marker) ? true : undefined), `${marker} is on the screen`, 30_000);
    const history = (await wholeHistory((text) => /^0{10}0000100001/m.test(text))).split("\n");
    const end = history.lastIndexOf(marker);
    check(end > 0, `${marker} is in the history`);
    // The rows of the line: the run of rows made only of the line's characters that ends just before the marker.
    let from = end;
    while (from > 0 && /^[0-9a-f]+$/.test(history[from - 1])) from--;
    return history.slice(from, end).join("");
  };

  // 40,000 characters fit in what the pane keeps: the whole line is compared.
  const small = lineOf(4_000);
  const gotSmall = await printLong(4_000, "long-small-done");
  equal(gotSmall.length, small.length, "the length of the 40,000-character line as the screen holds it");
  equal(sha(gotSmall), sha(small), "the hash of the 40,000-character line");
  equal(gotSmall.slice(0, 64), small.slice(0, 64), "the first 64 characters of the 40,000-character line");

  // 200,000 characters: the end of the line is compared with the end of the line that was printed, and the line was complete up to
  // its last character because the marker follows it. Now the pane keeps the 10,000 lines it is set to (before, a
  // hidden byte limit of the engine cut it to about 1,000 rows and only a tail of 57,608 characters arrived), so more of the line arrives; the
  // comparison stays "as far as the pane keeps it" and needs 40,000 characters at least.
  const big = lineOf(20_000);
  equal(big.length, 200_000, "the long line is 200,000 characters");
  const gotBig = await printLong(20_000, "long-done");
  check(gotBig.length >= 40_000, `the pane kept a tail of the long line that is worth comparing (${gotBig.length} characters)`);
  equal(gotBig.slice(-64), big.slice(-64), "the last 64 characters of the 200,000-character line");
  equal(sha(gotBig), sha(big.slice(big.length - gotBig.length)), "the hash of the kept tail of the 200,000-character line");
  const kept = gotBig.length;

  // ---- the page's own socket stayed up ---------------------------------------------------------------------------------------------
  const marks = await page.evaluate(() => [...window.__marks]);
  check(!marks.includes("offline") && !marks.includes("reconnecting"), `the status line showed an offline or reconnecting mark (${marks.join(", ") || "none"})`);
  equal(await socket.closed(), null, "the oracle socket is still open");
  await socket.close();
  context.expectClean("the context of the transport test");
  console.log(
    `transport: verdict "${verdict}", default ${defaultVariant}, extensions "${extensions}" (${expectDeflate ? "deflate expected" : "none expected"}); ` +
      `burst of 300 lines: ${numbers.length} read back in order; line of 40,000 characters: whole, hash equal; line of 200,000 characters: the last ${kept} characters the pane kept equal the end of the line, last 64 characters equal; marks seen: ${marks.join(", ") || "none"}`,
  );
});
