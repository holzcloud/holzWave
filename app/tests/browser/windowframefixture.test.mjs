// Every case of the shell's shared fixture (app/tests/fixtures/window-frame.json) drawn through the real page: the Rust decision
// of plan 14-06 and the page of this plan speak one contract, so a changed word on either side fails here with the case's name.
// The cases and the checks live in windowframe.test.mjs next to the Windows cases they extend; this file runs them on their own
// because the whole fixture takes longer than the half of the time one test file may take. The Tauri global is a stub and the
// window is a variable of the test: nothing here was seen on a real Windows or Linux window.
import { currentBackend } from "./lib.mjs";
import { fixtureCase } from "./windowframe.test.mjs";

export const backends = ["tauri"];
export const backendNote = "the window buttons exist only in the desktop shell; the web backend draws none and windowframe.test.mjs checks that";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  if (currentBackend() !== "tauri") return;
  await fixtureCase(page, url);
  if (errors.length > 0) throw new Error(`the page reported errors: ${errors.join("; ")}`);
}
