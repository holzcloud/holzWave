// A PDF in a real browser against the real server over TLS: a preview pane opens an uploaded PDF, PDF.js (the module, the worker, a packed CMap
// and a standard font) is served with 200 and the right type by the server's closed extension list, the first page is drawn on the canvas and the
// counter says "1 / 1". The PDF is made here: one page with a text in a standard font (Helvetica, not embedded) and one in ZapfDingbats (no system font has it), one in a font with a
// predefined CMap (UniJIS-UCS2-H), so that both data directories are read.
import { runTest, signIn, poll, check, equal } from "./lib.mjs";

/** A PDF of one page with the two texts, with a correct cross-reference table. */
function makePdf() {
  const content = "BT /F1 36 Tf 40 140 Td (Hello holzWave) Tj ET\nBT /F3 36 Tf 300 140 Td (abc) Tj ET\nBT /F2 36 Tf 40 80 Td <00480065006c006c006f> Tj ET";
  const objects = [
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R /F2 6 0 R /F3 8 0 R >> >> >>",
    `<< /Length ${content.length} >>\nstream\n${content}\nendstream`,
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    "<< /Type /Font /Subtype /Type0 /BaseFont /MS-Mincho /Encoding /UniJIS-UCS2-H /DescendantFonts [7 0 R] >>",
    "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /MS-Mincho /CIDSystemInfo << /Registry (Adobe) /Ordering (Japan1) /Supplement 2 >> /DW 1000 >>",
    "<< /Type /Font /Subtype /Type1 /BaseFont /ZapfDingbats >>",
  ];
  let out = "%PDF-1.4\n";
  const offsets = [];
  objects.forEach((body, i) => {
    offsets.push(out.length);
    out += `${i + 1} 0 obj\n${body}\nendobj\n`;
  });
  const xref = out.length;
  out += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n${offsets.map((o) => `${String(o).padStart(10, "0")} 00000 n \n`).join("")}`;
  out += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
  return Buffer.from(out, "latin1");
}

await runTest("pdf", {}, async ({ harness, open }) => {
  const owner = await harness.account({ role: "admin", name: "pd-owner" });
  const ownerContext = await open();
  const page = await ownerContext.context.newPage();
  const seen = [];
  page.on("response", (r) => {
    if (r.url().includes("/vendor/pdfjs/")) seen.push({ path: new URL(r.url()).pathname, status: r.status(), type: r.headers()["content-type"] ?? "" });
  });
  await signIn(page, harness, owner);

  // Upload the PDF into the person's upload folder, then open it in a preview pane.
  const bytes = makePdf();
  const stored = await page.evaluate(async (data) => {
    const { http } = await import(new URL("bridge.js", location.href).href);
    const bin = atob(data);
    const array = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) array[i] = bin.charCodeAt(i);
    return http("PUT", `/api/upload?name=${encodeURIComponent("sample.pdf")}`, new Blob([array]));
  }, bytes.toString("base64"));
  check(typeof stored?.path === "string", "the PDF was uploaded");
  const refused = await page.evaluate(
    async ({ path }) => {
      const { openBlock } = await import(new URL("blocks.js", location.href).href);
      return openBlock("preview", "local", path, "tab");
    },
    { path: stored.path },
  );
  equal(refused, null, "the preview pane opened");

  await page.locator(".pdf-count").waitFor({ timeout: 30_000 });
  equal((await page.locator(".pdf-count").innerText()).trim(), "1 / 1", "the page counter");
  // The first page is drawn: the canvas has a size and dark pixels (the text).
  const ink = await poll(
    async () =>
      page.evaluate(() => {
        const c = document.querySelector("canvas.pdf-canvas");
        if (!c || c.width < 10) return undefined;
        const d = c.getContext("2d").getImageData(0, 0, c.width, c.height).data;
        let dark = 0;
        for (let i = 0; i < d.length; i += 4) if (d[i + 3] > 0 && d[i] < 100 && d[i + 1] < 100 && d[i + 2] < 100) dark++;
        return dark > 50 ? dark : undefined;
      }),
    "the first page is drawn with text on the canvas",
    30_000,
  );
  check((await page.locator(".pdf-area .bk-message").count()) === 0, "the viewer shows no failure sentence");

  const wanted = [/pdf\.min\.mjs$/, /pdf\.worker\.min\.mjs$/, /cmaps\/UniJIS-UCS2-H\.bcmap$/, /standard_fonts\/[^/]+\.(ttf|pfb)$/];
  for (const want of wanted) {
    const hit = seen.find((s) => want.test(s.path));
    check(hit !== undefined, `${want} was requested (saw ${seen.map((s) => s.path.split("/").slice(-1)[0]).join(", ")})`);
    equal(hit.status, 200, `${hit.path} status`);
  }
  check(seen.every((s) => s.status === 200), `every PDF.js file answered 200 (${seen.filter((s) => s.status !== 200).map((s) => `${s.path} ${s.status}`).join(", ")})`);
  const modules = seen.filter((s) => s.path.endsWith(".mjs"));
  check(modules.every((s) => /javascript/.test(s.type)), `the modules are served as JavaScript (${modules.map((s) => s.type).join(", ")})`);

  ownerContext.expectClean("the owner's context");
  console.log(`pdf: preview of an uploaded PDF: counter 1 / 1, ${ink} dark pixels on the canvas; ${seen.length} PDF.js files all 200 (${seen.map((s) => s.path.split("/").slice(-1)[0]).join(", ")})`);
});
