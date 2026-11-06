// The Microsoft Store package stager: manifest from the template and an identity file, the staged folder, and what it refuses.
// Runs on any host with temporary folders only; makeappx is never involved. The identity used here is obviously fake and is
// never written into the repository.
import test from "node:test";
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import {
  LINUX_WSH,
  checkManifest,
  readIdentity,
  renderManifest,
  stage,
  xmlUnescape,
  TEMPLATE_PATH,
} from "../../../packaging/msix/build.mjs";
import { msixVersion } from "../../../packaging/msix/version.mjs";

const REPO = fileURLToPath(new URL("../../../", import.meta.url));
const BUILD = join(REPO, "packaging", "msix", "build.mjs");
const EXAMPLE = join(REPO, "packaging", "msix", "identity.example.json");
const CONF = JSON.parse(readFileSync(join(REPO, "app", "src-tauri", "tauri.conf.json"), "utf8"));

const FAKE = {
  IdentityName: "FakeVendor.FakeApp",
  Publisher: "CN=00000000-0000-0000-0000-000000000000",
  PublisherDisplayName: "Fake Vendor",
};

function scratch() {
  const dir = mkdtempSync(join(tmpdir(), "hw-msix-"));
  const release = join(dir, "release");
  mkdirSync(release);
  writeFileSync(join(release, "holzwave-app.exe"), "app");
  writeFileSync(join(release, "wsh.exe"), "wsh");
  const identity = join(dir, "identity.json");
  writeFileSync(identity, JSON.stringify(FAKE));
  return { dir, release, identity, out: join(dir, "out") };
}

function run(t, extra = {}) {
  const s = scratch();
  t.after(() => rmSync(s.dir, { recursive: true, force: true }));
  return { s, result: stage({ arch: "x64", releaseDir: s.release, identityFile: s.identity, outDir: s.out, ...extra }) };
}

test("the generated manifest is well formed, with the three namespaces, the identity and the architecture", (t) => {
  for (const arch of ["x64", "arm64"]) {
    const s = scratch();
    t.after(() => rmSync(s.dir, { recursive: true, force: true }));
    const r = stage({ arch, releaseDir: s.release, identityFile: s.identity, outDir: s.out });
    const text = readFileSync(r.manifestPath, "utf8");
    const m = checkManifest(text);
    assert.equal(m.identity.Name, FAKE.IdentityName);
    assert.equal(m.identity.Publisher, FAKE.Publisher);
    assert.equal(m.identity.Version, msixVersion(CONF.version));
    assert.equal(m.identity.ProcessorArchitecture, arch);
    assert.ok(text.includes("http://schemas.microsoft.com/appx/manifest/uap/windows10"));
    assert.ok(text.includes("http://schemas.microsoft.com/appx/manifest/foundation/windows10/restrictedcapabilities"));
    assert.match(r.msixPath, new RegExp(`holzWave_${msixVersion(CONF.version).replaceAll(".", "\\.")}_${arch}\\.msix$`));
  }
  // The template alone: no comment holds a double dash (XML forbids it), and the tags balance once it is filled.
  const template = readFileSync(TEMPLATE_PATH, "utf8");
  for (const c of template.matchAll(/<!--([\s\S]*?)-->/g)) assert.ok(!c[1].includes("--"), "a comment holds --");
  assert.throws(() => checkManifest("<Package><Identity></Package>"), /does not match|not closed/);
});

test("the only capabilities are runFullTrust and internetClient, and the application is the full trust executable", (t) => {
  const { result } = run(t);
  const m = checkManifest(readFileSync(result.manifestPath, "utf8"));
  assert.deepEqual([...m.capabilities].sort(), ["internetClient", "rescap:runFullTrust"]);
  assert.equal(m.application.EntryPoint, "Windows.FullTrustApplication");
  assert.equal(m.application.Executable, "holzwave-app.exe");
  const template = readFileSync(TEMPLATE_PATH, "utf8");
  for (const forbidden of ["Extension", "Protocol", "FileTypeAssociation", "StartupTask", "uap:Capability", "DeviceCapability"]) {
    assert.ok(!template.includes(forbidden), `the template declares ${forbidden}`);
  }
});

test("the example identity file is refused, placeholders and empty values never get through, a complete one does", (t) => {
  const s = scratch();
  t.after(() => rmSync(s.dir, { recursive: true, force: true }));
  assert.throws(() => readIdentity(EXAMPLE), /placeholder/);
  assert.throws(() => stage({ arch: "x64", releaseDir: s.release, identityFile: EXAMPLE, outDir: s.out }), /placeholder/);
  assert.ok(!existsSync(s.out), "nothing is written when the identity is refused");
  for (const bad of [
    { ...FAKE, PublisherDisplayName: "" },
    { ...FAKE, PublisherDisplayName: "   " },
    { ...FAKE, IdentityName: "PASTE-FROM-PARTNER-CENTER" },
    { ...FAKE, Publisher: "CN=PASTE-FROM-PARTNER-CENTER" },
    { ...FAKE, Publisher: "Fake Vendor" },
    { ...FAKE, IdentityName: "{{IDENTITY_NAME}}" },
    { ...FAKE, IdentityName: "<name>" },
    { ...FAKE, IdentityName: "a b" },
    { IdentityName: FAKE.IdentityName, Publisher: FAKE.Publisher },
  ]) {
    writeFileSync(s.identity, JSON.stringify(bad));
    assert.throws(() => readIdentity(s.identity), /placeholder|Publisher|IdentityName/, JSON.stringify(bad));
  }
  assert.throws(() => readIdentity(join(s.dir, "absent.json")), /no identity file/);
  writeFileSync(s.identity, "not json");
  assert.throws(() => readIdentity(s.identity), /not JSON/);
  writeFileSync(s.identity, JSON.stringify(FAKE));
  assert.deepEqual(readIdentity(s.identity), {
    identityName: FAKE.IdentityName,
    publisher: FAKE.Publisher,
    publisherDisplayName: FAKE.PublisherDisplayName,
  });
  // The real identity file is never part of the repository.
  const ignored = spawnSync("git", ["-C", REPO, "check-ignore", "-q", "packaging/msix/identity.json"]);
  assert.equal(ignored.status, 0, "packaging/msix/identity.json is not ignored by git");
});

test("a value with markup characters is escaped, a template placeholder left over is an error", (t) => {
  const s = scratch();
  t.after(() => rmSync(s.dir, { recursive: true, force: true }));
  const tricky = { ...FAKE, PublisherDisplayName: `A & B <"x"> 'y'` };
  writeFileSync(s.identity, JSON.stringify(tricky));
  const r = stage({ arch: "x64", releaseDir: s.release, identityFile: s.identity, outDir: s.out });
  const text = readFileSync(r.manifestPath, "utf8");
  assert.ok(text.includes("A &amp; B &lt;&quot;x&quot;&gt; &apos;y&apos;"));
  assert.equal(xmlUnescape(checkManifest(text).properties.PublisherDisplayName), tricky.PublisherDisplayName);
  assert.throws(() => renderManifest("<a>{{ONE}} {{TWO}}</a>", { ONE: "1" }), /\{\{TWO\}\}/);
  assert.throws(() => renderManifest("<a>{{ONE}}</a>", { ONE: "1", EXTRA: "2" }), /EXTRA/);
  assert.equal(renderManifest("<a>{{ONE}}</a>", { ONE: "x" }), "<a>x</a>");
  assert.throws(() => renderManifest("<a>{{ONE}}</a>", { ONE: "{{TWO}}" }), /left/);
});

test("the logos are the app's own icons, DisplayName is holzWave, every asset the manifest names is in the folder", (t) => {
  const { result } = run(t);
  const m = checkManifest(readFileSync(result.manifestPath, "utf8"));
  assert.equal(m.properties.DisplayName, "holzWave");
  assert.equal(m.properties.PublisherDisplayName, FAKE.PublisherDisplayName);
  assert.equal(m.visual.DisplayName, "holzWave");
  assert.deepEqual(
    [...m.assets].sort(),
    ["Assets\\Square150x150Logo.png", "Assets\\Square44x44Logo.png", "Assets\\StoreLogo.png"].sort(),
  );
  for (const logo of ["StoreLogo.png", "Square44x44Logo.png", "Square150x150Logo.png"]) {
    const staged = readFileSync(join(result.stageDir, "Assets", logo));
    const icon = readFileSync(join(REPO, "app", "src-tauri", "icons", logo));
    assert.ok(staged.equals(icon), `${logo} is not the app icon`);
  }
  assert.match(m.visual.BackgroundColor, /^#[0-9a-f]{6}$/);
  // The background is the ground colour of the design tokens.
  const css = readFileSync(join(REPO, "app", "src", "app.css"), "utf8");
  assert.equal(m.visual.BackgroundColor, /--hw-ground:\s*(#[0-9a-f]{6})/.exec(css)[1]);
});

test("the stager takes the release files from the given folder only and stages nothing else", (t) => {
  const s = scratch();
  t.after(() => rmSync(s.dir, { recursive: true, force: true }));
  writeFileSync(join(s.release, "unrelated.dll"), "x");
  writeFileSync(join(s.release, "notes.txt"), "x");
  const linux = join(s.dir, "linux");
  mkdirSync(linux);
  for (const [name] of LINUX_WSH) writeFileSync(join(linux, name), "linux");
  const r = stage({ arch: "x64", releaseDir: s.release, identityFile: s.identity, outDir: s.out, linuxWsh: linux });
  const listing = (dir, prefix = "") =>
    readdirSync(dir, { withFileTypes: true }).flatMap((e) => (e.isDirectory() ? listing(join(dir, e.name), `${prefix}${e.name}/`) : [`${prefix}${e.name}`]));
  assert.deepEqual(listing(r.stageDir).sort(), [
    "Assets/Square150x150Logo.png",
    "Assets/Square44x44Logo.png",
    "Assets/StoreLogo.png",
    "AppxManifest.xml",
    "holzwave-app.exe",
    "wsh.exe",
    "wsh/linux-aarch64",
    "wsh/linux-x86_64",
  ].sort());
  assert.deepEqual(readdirSync(s.out).sort(), ["stage-x64"]);

  // The exe missing, a sidecar missing, a Linux build missing: refused, and nothing is written.
  const t2 = scratch();
  t.after(() => rmSync(t2.dir, { recursive: true, force: true }));
  rmSync(join(t2.release, "holzwave-app.exe"));
  assert.throws(() => stage({ arch: "x64", releaseDir: t2.release, identityFile: t2.identity, outDir: t2.out }), /holzwave-app\.exe/);
  assert.ok(!existsSync(t2.out));
  writeFileSync(join(t2.release, "holzwave-app.exe"), "app");
  rmSync(join(t2.release, "wsh.exe"));
  assert.throws(() => stage({ arch: "x64", releaseDir: t2.release, identityFile: t2.identity, outDir: t2.out }), /wsh\.exe/);
  writeFileSync(join(t2.release, "wsh.exe"), "wsh");
  assert.throws(() => stage({ arch: "x64", releaseDir: t2.release, identityFile: t2.identity, outDir: t2.out, linuxWsh: linux + "-absent" }), /wsh-linux/);
  assert.throws(() => stage({ arch: "x86", releaseDir: t2.release, identityFile: t2.identity, outDir: t2.out }), /--arch/);
});

test("a symbolic link in the release folder is not followed out of it", (t) => {
  const s = scratch();
  t.after(() => rmSync(s.dir, { recursive: true, force: true }));
  const outside = join(s.dir, "outside.exe");
  writeFileSync(outside, "not the release build");
  rmSync(join(s.release, "wsh.exe"));
  symlinkSync(outside, join(s.release, "wsh.exe"));
  assert.throws(() => stage({ arch: "x64", releaseDir: s.release, identityFile: s.identity, outDir: s.out }), /symbolic link/);
  assert.ok(!existsSync(s.out), "nothing is written when a file is refused");
});

test("the command line prints one makeappx pack command, and the wrapper uses the same name and folder", (t) => {
  const s = scratch();
  t.after(() => rmSync(s.dir, { recursive: true, force: true }));
  const args = [BUILD, "--arch", "arm64", "--release-dir", s.release, "--identity", s.identity, "--out", s.out, "--json"];
  const json = JSON.parse(execFileSync(process.execPath, args, { encoding: "utf8" }));
  const version = msixVersion(CONF.version);
  assert.deepEqual(json.command, ["makeappx", "pack", "/d", join(s.out, "stage-arm64"), "/p", join(s.out, `holzWave_${version}_arm64.msix`), "/o"]);
  const text = execFileSync(process.execPath, args.slice(0, -1), { encoding: "utf8" });
  assert.equal(text.split("\n").filter((l) => l.includes("makeappx pack")).length, 1);
  assert.match(text, /makeappx pack \/d .* \/p .*\.msix \/o/);
  // The refusal exits with a message and a failing status.
  const refused = spawnSync(process.execPath, [BUILD, "--release-dir", s.release, "--identity", EXAMPLE, "--out", s.out], { encoding: "utf8" });
  assert.equal(refused.status, 1);
  assert.match(refused.stderr, /placeholder/);
  // The PowerShell wrapper (read, never run here) runs the stager and packs what it printed.
  const wrapper = readFileSync(join(REPO, "scripts", "build-msix.ps1"), "utf8");
  for (const needle of ["packaging/msix/build.mjs", "--json", "makeappx", "$plan.command", "Win32NT"]) {
    assert.ok(wrapper.includes(needle), `build-msix.ps1 does not mention ${needle}`);
  }
  assert.ok(!/signtool|New-SelfSignedCertificate|Export-PfxCertificate|\.pfx/i.test(wrapper), "the wrapper must not make or use a certificate");
  assert.ok(!/Invoke-WebRequest|Invoke-RestMethod|gh release|\bgh\b upload/.test(wrapper), "the wrapper must not upload anything");
});
