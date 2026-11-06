// Stages the files of the Microsoft Store package of holzWave and writes its manifest. It runs on any host (Node only, no
// dependency); packing the folder with makeappx is a separate Windows step (scripts/build-msix.ps1). The Store signs the
// package, so nothing here makes or needs a certificate.
//
//   node packaging/msix/build.mjs --arch x64 --release-dir target/release [--linux-wsh app/src-tauri/binaries]
//        [--identity packaging/msix/identity.json] [--out target/msix] [--json]
//
// The three identity values (package name, publisher, publisher display name) come only from the identity file, which holds
// what the owner's Partner Center shows. The example file in this folder has placeholders and is refused. Everything is
// written under --out only. The version of the package is msixVersion() of the version in app/src-tauri/tauri.conf.json.
//
// Never run through makeappx, the certification kit or the Store: the manifest shape is checked by this script and its
// tests, not by Microsoft's tools.
import { copyFileSync, existsSync, lstatSync, mkdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { msixVersion } from "./version.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "..", "..");

export const ARCHS = ["x64", "arm64"];
export const TEMPLATE_PATH = join(HERE, "AppxManifest.template.xml");
export const ICONS_DIR = join(REPO, "app", "src-tauri", "icons");
export const TAURI_CONF = join(REPO, "app", "src-tauri", "tauri.conf.json");

/** The logos of the manifest: copied from the app's own icons, nothing is drawn for the package. */
export const LOGOS = ["StoreLogo.png", "Square44x44Logo.png", "Square150x150Logo.png"];

/** The files that must be in the release folder, and where they land in the package (the folder of the executable). */
export const REQUIRED_FILES = ["holzwave-app.exe", "wsh.exe"];

/** The Linux wsh builds (resources that the app installs on a remote Linux host): file in --linux-wsh, place in the package. */
export const LINUX_WSH = [
  ["wsh-linux-x86_64", "wsh/linux-x86_64"],
  ["wsh-linux-aarch64", "wsh/linux-aarch64"],
];

const PLACEHOLDER = /PASTE|PLACEHOLDER|\{\{|\}\}|^<.*>$|^\s*$/i;

/**
 * Reads and checks the identity file. Throws when a value is missing, empty or still a placeholder, or has the wrong shape.
 * @param {string} file
 * @returns {{identityName: string, publisher: string, publisherDisplayName: string}}
 */
export function readIdentity(file) {
  let raw;
  try {
    raw = readFileSync(file, "utf8");
  } catch {
    throw new Error(
      `no identity file at ${file}: copy packaging/msix/identity.example.json to packaging/msix/identity.json and paste the three values from Partner Center`,
    );
  }
  let data;
  try {
    data = JSON.parse(raw);
  } catch (e) {
    throw new Error(`the identity file ${file} is not JSON: ${e.message}`);
  }
  const out = {};
  for (const [key, field] of [["IdentityName", "identityName"], ["Publisher", "publisher"], ["PublisherDisplayName", "publisherDisplayName"]]) {
    const value = data?.[key];
    if (typeof value !== "string" || PLACEHOLDER.test(value)) {
      throw new Error(`${key} in ${file} is missing or still a placeholder: it must be the value shown in the owner's Partner Center`);
    }
    if (value.length > 256 || /[\u0000-\u001f]/.test(value)) throw new Error(`${key} in ${file} is too long or holds a control character`);
    out[field] = value;
  }
  if (!/^[A-Za-z0-9][A-Za-z0-9.-]{2,49}$/.test(out.identityName)) {
    throw new Error(`IdentityName ${JSON.stringify(out.identityName)} is not a package name (3 to 50 letters, digits, dots and dashes)`);
  }
  if (!/^CN=.+/.test(out.publisher)) {
    throw new Error(`Publisher ${JSON.stringify(out.publisher)} does not start with CN=: paste it as Partner Center shows it`);
  }
  return out;
}

const XML_ESCAPES = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&apos;" };
export function xmlEscape(text) {
  return String(text).replace(/[&<>"']/g, (c) => XML_ESCAPES[c]);
}

/**
 * Fills the template. Strict: every placeholder must be given, a value is XML-escaped, and a `{{...}}` left over is an error.
 * @param {string} template
 * @param {Record<string, string>} values
 */
export function renderManifest(template, values) {
  const used = new Set();
  const filled = template.replace(/\{\{([A-Z_]+)\}\}/g, (_, name) => {
    if (!Object.hasOwn(values, name)) throw new Error(`the template needs a value for {{${name}}}`);
    used.add(name);
    return xmlEscape(values[name]);
  });
  for (const name of Object.keys(values)) if (!used.has(name)) throw new Error(`the template has no place for ${name}`);
  if (/\{\{|\}\}/.test(filled)) throw new Error("a placeholder is left in the manifest");
  return filled;
}

/**
 * A check of the written manifest. It is not an XML parser (Node has none) and not Microsoft's schema: it checks that the
 * tags balance, reads the attributes it needs and compares them with what was asked. makeappx does the schema check.
 * @returns {{identity: Record<string,string>, capabilities: string[], application: Record<string,string>, visual: Record<string,string>, properties: Record<string,string>, assets: string[]}}
 */
export function checkManifest(text, expected) {
  const body = text.replace(/<!--[\s\S]*?-->/g, "").replace(/^<\?xml[^>]*\?>/, "");
  if (/<!--|-->/.test(body)) throw new Error("a comment is not closed");
  const stack = [];
  const tagRe = /<(\/?)([A-Za-z][A-Za-z0-9:.]*)((?:\s+[A-Za-z:][A-Za-z0-9:.-]*\s*=\s*(?:"[^"<]*"|'[^'<]*'))*)\s*(\/?)>/g;
  let last = 0;
  const found = { identity: null, capabilities: [], application: null, visual: null };
  const texts = {};
  let m;
  while ((m = tagRe.exec(body)) !== null) {
    const between = body.slice(last, m.index);
    if (/[<>]/.test(between)) throw new Error("malformed markup in the manifest");
    if (stack.length > 0 && between.trim() !== "") texts[stack[stack.length - 1]] = (texts[stack[stack.length - 1]] ?? "") + between.trim();
    last = tagRe.lastIndex;
    const [, closing, name, attrText, selfClosing] = m;
    const attrs = {};
    for (const a of attrText.matchAll(/([A-Za-z:][A-Za-z0-9:.-]*)\s*=\s*(?:"([^"]*)"|'([^']*)')/g)) attrs[a[1]] = a[2] ?? a[3];
    if (closing) {
      if (stack.pop() !== name) throw new Error(`the tag </${name}> does not match`);
      continue;
    }
    if (name === "Identity") found.identity = attrs;
    if (name === "Application") found.application = attrs;
    if (name === "uap:VisualElements") found.visual = attrs;
    if (name === "rescap:Capability") found.capabilities.push(`rescap:${attrs.Name}`);
    if (name === "Capability") found.capabilities.push(attrs.Name);
    if (!selfClosing) stack.push(name);
  }
  if (stack.length > 0) throw new Error(`the tag <${stack[stack.length - 1]}> is not closed`);
  if (body.slice(last).trim() !== "") throw new Error("text after the last tag");
  const root = /<Package\b([^>]*)>/.exec(body)?.[1] ?? "";
  for (const ns of [
    'xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10"',
    'xmlns:uap="http://schemas.microsoft.com/appx/manifest/uap/windows10"',
    'xmlns:rescap="http://schemas.microsoft.com/appx/manifest/foundation/windows10/restrictedcapabilities"',
  ]) {
    if (!root.includes(ns)) throw new Error(`the namespace ${ns} is missing`);
  }
  if (!found.identity || !found.application || !found.visual) throw new Error("Identity, Application or VisualElements is missing");
  if (/\{\{|\}\}/.test(text)) throw new Error("a placeholder is left in the manifest");
  const assets = [found.visual.Square150x150Logo, found.visual.Square44x44Logo, /<Logo>([^<]*)<\/Logo>/.exec(body)?.[1]].filter(Boolean);
  const result = {
    identity: found.identity,
    capabilities: found.capabilities,
    application: found.application,
    visual: found.visual,
    properties: { DisplayName: texts.DisplayName, PublisherDisplayName: texts.PublisherDisplayName },
    assets,
  };
  if (expected) {
    for (const key of ["Name", "Publisher", "Version", "ProcessorArchitecture"]) {
      if (result.identity[key] !== expected[key]) throw new Error(`Identity ${key} is ${result.identity[key]}, expected ${expected[key]}`);
    }
    if (result.properties.PublisherDisplayName !== expected.PublisherDisplayName) throw new Error("PublisherDisplayName is not the identity file's value");
  }
  return result;
}

/** The XML-unescaped form of what checkManifest reads, so a test can compare it with the identity file. */
export function xmlUnescape(text) {
  return String(text).replace(/&(amp|lt|gt|quot|apos);/g, (_, n) => ({ amp: "&", lt: "<", gt: ">", quot: '"', apos: "'" })[n]);
}

/** True when `file` is a regular file (not a symbolic link) that really lies inside `dir`. */
function isPlainFileInside(dir, file) {
  const st = lstatSync(file, { throwIfNoEntry: false });
  if (!st || !st.isFile() || st.isSymbolicLink()) return false;
  const real = realpathSync(file);
  const base = realpathSync(dir);
  return real.startsWith(base + sep);
}

function takeFile(srcDir, name, what) {
  const file = join(srcDir, name);
  if (!existsSync(file)) throw new Error(`${what} is missing: no ${name} in ${srcDir}`);
  if (!isPlainFileInside(srcDir, file)) throw new Error(`${name} in ${srcDir} is a symbolic link or not a regular file inside the folder: refused`);
  return file;
}

/**
 * Stages the package folder and writes the manifest. Writes only under `outDir`.
 * @param {{arch: string, releaseDir: string, identityFile: string, outDir: string, linuxWsh?: string, tauriConf?: string, templatePath?: string, iconsDir?: string}} o
 */
export function stage(o) {
  if (!ARCHS.includes(o.arch)) throw new Error(`--arch must be x64 or arm64, not ${JSON.stringify(o.arch)}`);
  const identity = readIdentity(o.identityFile);
  const conf = JSON.parse(readFileSync(o.tauriConf ?? TAURI_CONF, "utf8"));
  const version = msixVersion(conf.version);
  const releaseDir = resolve(o.releaseDir);
  if (!existsSync(releaseDir)) throw new Error(`the release folder ${releaseDir} does not exist`);

  // Everything that can fail is checked before anything is written.
  const sources = REQUIRED_FILES.map((name) => [takeFile(releaseDir, name, name === "holzwave-app.exe" ? "the app" : "the wsh sidecar"), name]);
  if (o.linuxWsh) {
    const linuxDir = resolve(o.linuxWsh);
    for (const [name, place] of LINUX_WSH) sources.push([takeFile(linuxDir, name, "a Linux wsh build"), place]);
  }
  const iconsDir = o.iconsDir ?? ICONS_DIR;
  const logos = LOGOS.map((name) => {
    const file = join(iconsDir, name);
    if (!existsSync(file)) throw new Error(`the app icon ${name} is missing from ${iconsDir}`);
    return [file, `Assets/${name}`];
  });
  const manifest = renderManifest(readFileSync(o.templatePath ?? TEMPLATE_PATH, "utf8"), {
    IDENTITY_NAME: identity.identityName,
    PUBLISHER: identity.publisher,
    PUBLISHER_DISPLAY_NAME: identity.publisherDisplayName,
    VERSION: version,
    ARCH: o.arch,
  });

  const outDir = resolve(o.outDir);
  const stageDir = join(outDir, `stage-${o.arch}`);
  rmSync(stageDir, { recursive: true, force: true });
  for (const [src, place] of [...sources, ...logos]) {
    const dest = join(stageDir, ...place.split("/"));
    mkdirSync(dirname(dest), { recursive: true });
    copyFileSync(src, dest);
  }
  const manifestPath = join(stageDir, "AppxManifest.xml");
  writeFileSync(manifestPath, manifest, "utf8");

  // Read the manifest back and check it against what was asked, and that every asset it names is in the folder.
  const checked = checkManifest(readFileSync(manifestPath, "utf8"), {
    Name: xmlEscape(identity.identityName),
    Publisher: xmlEscape(identity.publisher),
    Version: version,
    ProcessorArchitecture: o.arch,
    PublisherDisplayName: xmlEscape(identity.publisherDisplayName),
  });
  for (const asset of checked.assets) {
    if (!existsSync(join(stageDir, ...asset.split("\\")))) throw new Error(`the manifest names ${asset}, which is not in the staged folder`);
  }
  if (!existsSync(join(stageDir, checked.application.Executable))) throw new Error("the executable named by the manifest is not in the staged folder");

  const msixPath = join(outDir, `holzWave_${version}_${o.arch}.msix`);
  return {
    arch: o.arch,
    version,
    stageDir,
    manifestPath,
    msixPath,
    command: ["makeappx", "pack", "/d", stageDir, "/p", msixPath, "/o"],
    linuxWshStaged: Boolean(o.linuxWsh),
  };
}

function parseArgs(argv) {
  const opts = { arch: "x64", identityFile: join(HERE, "identity.json"), outDir: join(REPO, "target", "msix"), json: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const next = () => {
      if (i + 1 >= argv.length) throw new Error(`${a} needs a value`);
      return argv[++i];
    };
    if (a === "--arch") opts.arch = next();
    else if (a === "--release-dir") opts.releaseDir = next();
    else if (a === "--linux-wsh") opts.linuxWsh = next();
    else if (a === "--identity") opts.identityFile = next();
    else if (a === "--out") opts.outDir = next();
    else if (a === "--json") opts.json = true;
    else throw new Error(`unknown argument ${a}`);
  }
  if (!opts.releaseDir) throw new Error("--release-dir <folder with holzwave-app.exe and wsh.exe> is needed");
  return opts;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const opts = parseArgs(process.argv.slice(2));
    const r = stage(opts);
    if (opts.json) {
      console.log(JSON.stringify(r));
    } else {
      console.log(`msix: staged ${r.stageDir} (version ${r.version}, ${r.arch})`);
      if (!r.linuxWshStaged) console.log("msix: no --linux-wsh given: the package has no Linux wsh resources (the app cannot install wsh on a remote host from it)");
      console.log(`msix: pack with  ${r.command.map((c) => (/\s/.test(c) ? `"${c}"` : c)).join(" ")}`);
    }
  } catch (e) {
    console.error(`msix: ${e.message}`);
    process.exit(1);
  }
}
