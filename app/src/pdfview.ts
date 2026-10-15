// The PDF viewer of a preview pane: PDF.js, vendored and loaded only when a PDF is opened (dist/vendor/pdfjs). One page
// at a time is drawn to a canvas; the toolbar has previous, next, zoom and fit. The document is read through the core in
// ranges (`file_range`, the same grant as every other preview), so a large PDF is never loaded whole, and PDF.js is
// told not to run scripts, XFA forms or WebAssembly (the policy has no `wasm-unsafe-eval`). Names and numbers are put on
// the screen with `textContent`.
import { call } from "./bridge.js";
import { el, setText } from "./dom.js";

/** The parts of PDF.js this module uses; the library is loaded at run time, so its types are not imported. */
interface PdfPage {
  getViewport(o: { scale: number }): { width: number; height: number };
  render(o: { canvas: HTMLCanvasElement; viewport: unknown; transform?: number[] }): { promise: Promise<void>; cancel(): void };
  cleanup(): void;
}
interface PdfDocument {
  numPages: number;
  getPage(n: number): Promise<PdfPage>;
}
interface PdfJs {
  GlobalWorkerOptions: { workerSrc: string };
  PDFDataRangeTransport: new (length: number, initial: Uint8Array | null) => {
    requestDataRange: (begin: number, end: number) => void;
    onDataRange(begin: number, chunk: Uint8Array | null): void;
  };
  getDocument(o: Record<string, unknown>): { promise: Promise<PdfDocument>; destroy(): Promise<void> };
}

let library: Promise<PdfJs> | null = null;

/** Load PDF.js once. The path is a string at run time on purpose: the module is a file of the bundle, not of the build. */
function pdfjs(): Promise<PdfJs> {
  library ??= (async () => {
    const base = new URL("./vendor/pdfjs/", location.href).href;
    const lib = (await import(`${base}pdf.min.mjs`)) as PdfJs;
    lib.GlobalWorkerOptions.workerSrc = `${base}pdf.worker.min.mjs`;
    return lib;
  })();
  library.catch(() => {
    library = null; // a failed load is tried again on the next PDF
  });
  return library;
}

/** The first piece a document is started with; PDF.js asks for the rest as it needs it. */
const FIRST = 256 * 1024;
const ZOOM_MIN = 0.25;
const ZOOM_MAX = 5;

/** What the viewer gives back to the preview pane. */
export interface PdfView {
  /** A key while the pane has the focus; true when it was used. */
  key(e: KeyboardEvent): boolean;
  dispose(): void;
}

/** The scale that makes a page as wide as the room, but never beyond the zoom limits. */
export function fitScale(pageWidth: number, room: number): number {
  if (!(pageWidth > 0) || !(room > 0)) return 1;
  return Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, room / pageWidth));
}

/** Show the PDF behind `token` (size `size`) in `host`. `status` is where the page counter is written. */
export async function showPdf(host: HTMLElement, token: string, size: number, status: (text: string) => void): Promise<PdfView> {
  const lib = await pdfjs();

  const bar = el("div", "pdf-bar", host);
  const prev = el("button", "", bar);
  prev.type = "button";
  prev.textContent = "◀ Prev";
  const counter = el("span", "pdf-count", bar);
  const next = el("button", "", bar);
  next.type = "button";
  next.textContent = "Next ▶";
  const out = el("button", "", bar);
  out.type = "button";
  out.textContent = "−";
  out.title = "Zoom out (-)";
  const zoomText = el("span", "pdf-zoom", bar);
  const into = el("button", "", bar);
  into.type = "button";
  into.textContent = "+";
  into.title = "Zoom in (+)";
  const fit = el("button", "", bar);
  fit.type = "button";
  fit.textContent = "Fit";
  fit.title = "Fit the width (0)";
  const area = el("div", "pdf-area", host);
  const canvas = el("canvas", "pdf-canvas", area);

  // The document is read in ranges through the core.
  const read = async (begin: number, end: number): Promise<Uint8Array> => {
    const raw = await call<unknown>("file_range", { token, offset: begin, len: end - begin });
    if (raw instanceof ArrayBuffer) return new Uint8Array(raw);
    if (ArrayBuffer.isView(raw)) return new Uint8Array(raw.buffer, raw.byteOffset, raw.byteLength);
    if (Array.isArray(raw)) return Uint8Array.from(raw as number[]);
    throw new Error("the PDF could not be read");
  };
  const first = await read(0, Math.min(size, FIRST));
  const transport = new lib.PDFDataRangeTransport(size, first);
  transport.requestDataRange = (begin, end) => {
    void read(begin, Math.min(end, size)).then((chunk) => transport.onDataRange(begin, chunk));
  };
  const task = lib.getDocument({
    range: transport,
    length: size,
    // No scripts, no XFA forms, no WebAssembly (the policy forbids it; the decoders fall back), fonts and CMaps from the bundle.
    enableXfa: false,
    useWasm: false,
    cMapUrl: new URL("./vendor/pdfjs/cmaps/", location.href).href,
    cMapPacked: true,
    standardFontDataUrl: new URL("./vendor/pdfjs/standard_fonts/", location.href).href,
  });
  const doc = await task.promise;

  let page = 1;
  let zoom = 0; // 0 is "fit the width"
  let rendering: { cancel(): void } | null = null;
  let gone = false;

  const draw = async (): Promise<void> => {
    rendering?.cancel();
    rendering = null;
    const mine = ++drawn;
    const p = await doc.getPage(page);
    if (gone || mine !== drawn) return;
    const base = p.getViewport({ scale: 1 });
    const scale = zoom === 0 ? fitScale(base.width, area.clientWidth - 24) : zoom;
    const viewport = p.getViewport({ scale });
    const ratio = Math.max(1, window.devicePixelRatio || 1);
    canvas.width = Math.floor(viewport.width * ratio);
    canvas.height = Math.floor(viewport.height * ratio);
    canvas.style.width = `${Math.floor(viewport.width)}px`;
    canvas.style.height = `${Math.floor(viewport.height)}px`;
    const task = p.render({ canvas, viewport, transform: ratio === 1 ? undefined : [ratio, 0, 0, ratio, 0, 0] });
    rendering = task;
    try {
      await task.promise;
    } catch {
      // cancelled by a newer draw
      return;
    }
    setText(zoomText, `${Math.round(scale * 100)} %`);
    p.cleanup();
  };
  let drawn = 0;

  const show = (): void => {
    setText(counter, `${page} / ${doc.numPages}`);
    status(`${page} / ${doc.numPages}`);
    prev.disabled = page <= 1;
    next.disabled = page >= doc.numPages;
    void draw();
  };
  const go = (n: number): void => {
    page = Math.max(1, Math.min(doc.numPages, n));
    area.scrollTo({ top: 0 });
    show();
  };
  const zoomBy = (factor: number): void => {
    const current = zoom === 0 ? Number.parseFloat(zoomText.textContent ?? "100") / 100 || 1 : zoom;
    zoom = Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, current * factor));
    void draw();
  };

  prev.addEventListener("click", () => go(page - 1));
  next.addEventListener("click", () => go(page + 1));
  out.addEventListener("click", () => zoomBy(1 / 1.25));
  into.addEventListener("click", () => zoomBy(1.25));
  fit.addEventListener("click", () => {
    zoom = 0;
    void draw();
  });
  const observer = new ResizeObserver(() => {
    if (zoom === 0) void draw();
  });
  observer.observe(area);

  show();

  return {
    key(e) {
      switch (e.key) {
        case "PageDown":
        case "ArrowRight":
          go(page + 1);
          return true;
        case "PageUp":
        case "ArrowLeft":
          go(page - 1);
          return true;
        case "Home":
          go(1);
          return true;
        case "End":
          go(doc.numPages);
          return true;
        case "+":
        case "=":
          zoomBy(1.25);
          return true;
        case "-":
          zoomBy(1 / 1.25);
          return true;
        case "0":
          zoom = 0;
          void draw();
          return true;
        case "ArrowDown":
          area.scrollBy({ top: 48 });
          return true;
        case "ArrowUp":
          area.scrollBy({ top: -48 });
          return true;
        case " ":
          area.scrollBy({ top: area.clientHeight * 0.9 });
          return true;
        default:
          return false;
      }
    },
    dispose() {
      gone = true;
      rendering?.cancel();
      observer.disconnect();
      // Destroying the loading task ends the worker's work on the document and frees it.
      void task.destroy().catch(() => {});
      bar.remove();
      area.remove();
    },
  };
}
