#!/usr/bin/env node
// The deployment checker of the web build: it reads the JSON form of the Compose file or the rendered Helm chart and prints
// one line per violation of the runtime contract of the image.
//
//   docker compose -f packaging/compose/docker-compose.yml config --format json | node scripts/web-deploy-check.mjs compose
//   helm template r packaging/helm/holzwave -f <values> | KUBECONFIG=/dev/null kubectl annotate --local -f - -o json \
//       holzwave.example/checked=yes | node scripts/web-deploy-check.mjs chart
//   node scripts/web-deploy-check.mjs chart-values packaging/helm/holzwave/ci/files-values.yaml
//   node scripts/web-deploy-check.mjs self-test
//
// `chart` reads one JSON object after the other (what kubectl prints for a file with several documents) or a `List`;
// `chart-values` renders the chart with helm and converts the YAML with kubectl itself. `self-test` feeds a clean sample and one
// mutated copy per rule to the same rules and requires exactly the expected violation for each, so the guard is shown to be
// able to fail.
//
// The contract: the server runs as 10001 with a read-only root and exactly the capabilities CHOWN, KILL, SETGID and SETUID; the
// option that stops a process from gaining privileges is absent (it would stop the file capabilities of the spawn helper);
// nothing is privileged, nothing shares a host namespace, nothing mounts a container-runtime socket or a host path; one TLS
// port is the only port; one replica with the Recreate strategy and a ReadWriteOnce claim, because the store is SQLite on one
// local volume.
//
// A violation line is `violation: <rule>: <where>`. It holds the name of the rule and the name of the object, never a value
// from the input. Exit status: 0 clean, 1 violations, 2 unusable input or a missing tool. Node built-ins only.

import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const CAPABILITIES = ["CHOWN", "KILL", "SETGID", "SETUID"];
const TLS_PORT = 8443;
const USER = 10001;
const RUN_DIR = "/run/holzwave";
const DATA_DIR = "/data";
const SOCKET_PATTERN = /(docker|containerd|crio|cri-o|podman)(\.sock)?$|\.sock$|\/var\/lib\/docker|\/run\/containerd|\/var\/run\/docker/i;

// ---- reading -------------------------------------------------------------------------------------------------------

class Unusable extends Error {}

// Splits a text that holds one JSON object after the other. It tracks depth and the state of a string, so that a brace inside
// a string does not count. Nothing but white space may stand between two objects.
export function splitObjects(text) {
  const objects = [];
  let depth = 0;
  let inString = false;
  let escaped = false;
  let start = -1;
  for (let i = 0; i < text.length; i += 1) {
    const c = text[i];
    if (inString) {
      if (escaped) escaped = false;
      else if (c === "\\") escaped = true;
      else if (c === '"') inString = false;
      continue;
    }
    if (c === '"') {
      if (depth === 0) throw new Unusable("a string outside an object");
      inString = true;
    } else if (c === "{") {
      if (depth === 0) start = i;
      depth += 1;
    } else if (c === "}") {
      depth -= 1;
      if (depth < 0) throw new Unusable("a closing brace without an opening one");
      if (depth === 0) {
        objects.push(JSON.parse(text.slice(start, i + 1)));
        start = -1;
      }
    } else if (depth === 0 && !/\s/.test(c)) {
      throw new Unusable("text outside an object");
    }
  }
  if (depth !== 0 || inString) throw new Unusable("the input ends inside an object");
  return objects;
}

function readAll(stream) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    let size = 0;
    stream.on("data", (chunk) => {
      size += chunk.length;
      if (size > 16 * 1024 * 1024) reject(new Unusable("the input is larger than 16 MiB"));
      else chunks.push(chunk);
    });
    stream.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
    stream.on("error", reject);
  });
}

// ---- the Compose rules ---------------------------------------------------------------------------------------------

// One line per rule and place, however many times the rule fired there.
const unique = (found) => found.filter(([rule, where], i) => found.findIndex(([r, w]) => r === rule && w === where) === i);

const list = (value) => (Array.isArray(value) ? value : []);
const sameSet = (a, b) => a.length === b.length && b.every((item) => a.includes(item));
const upper = (items) => list(items).map((item) => String(item).toUpperCase().replace(/^CAP_/, ""));

function mountsOf(service) {
  const mounts = [];
  for (const entry of list(service.volumes)) {
    if (typeof entry === "string") {
      const [source, target] = entry.split(":");
      mounts.push({ type: "bind", source, target });
    } else if (entry && typeof entry === "object") {
      mounts.push({ type: entry.type, source: entry.source ?? "", target: entry.target ?? "" });
    }
  }
  return mounts;
}

function tmpfsTargets(service) {
  const targets = [];
  for (const entry of list(service.tmpfs)) targets.push(String(entry).split(":")[0]);
  for (const mount of mountsOf(service)) if (mount.type === "tmpfs") targets.push(mount.target);
  return targets;
}

function userOk(user) {
  return user === `${USER}:${USER}` || user === String(USER) || user === `${USER}`;
}

function checkComposeService(name, service, { server }) {
  const found = [];
  const at = `service ${name}`;
  if (service.privileged === true) found.push(["privileged", at]);
  if (!userOk(service.user)) found.push(["user", at]);
  if (service.read_only !== true) found.push(["writable-root", at]);
  if (!sameSet(upper(service.cap_drop), ["ALL"])) found.push(["cap-drop", at]);
  const added = upper(service.cap_add);
  if (!sameSet(added, server ? CAPABILITIES : [])) found.push(["cap-add", at]);
  if (list(service.security_opt).some((option) => /no-new-privileges/i.test(String(option)))) {
    found.push(["no-new-privileges", at]);
  }
  for (const key of ["network_mode", "pid", "ipc", "uts", "userns_mode", "cgroup"]) {
    if (/^host$/i.test(String(service[key] ?? ""))) found.push(["host-namespace", at]);
  }
  if (list(service.devices).length > 0) found.push(["device", at]);
  if (mountsOf(service).some((mount) => SOCKET_PATTERN.test(mount.source) || SOCKET_PATTERN.test(mount.target))) {
    found.push(["socket-mount", at]);
  }
  const ports = list(service.ports);
  if (server) {
    for (const port of ports) {
      const target = typeof port === "object" && port !== null ? Number(port.target) : Number(String(port).split(":").pop().split("/")[0]);
      if (target !== TLS_PORT) found.push(["port", at]);
    }
    if (!tmpfsTargets(service).includes(RUN_DIR)) found.push(["run-tmpfs", at]);
  } else if (ports.length > 0) {
    found.push(["port", at]);
  }
  return found;
}

export function checkCompose(config) {
  const found = [];
  const services = config && typeof config === "object" ? config.services : undefined;
  if (!services || typeof services !== "object") throw new Unusable("the Compose JSON has no services");
  for (const [name, server] of [["holzwave", true], ["keygen", false]]) {
    if (!services[name]) found.push(["service-missing", `service ${name}`]);
    else found.push(...checkComposeService(name, services[name], { server }));
  }
  return unique(found);
}

// ---- the chart rules -----------------------------------------------------------------------------------------------

function flatten(objects) {
  const all = [];
  for (const object of objects) {
    if (object && object.kind === "List") all.push(...list(object.items));
    else all.push(object);
  }
  return all;
}

function secretKeyNames(value, path = "") {
  const bad = [];
  if (Array.isArray(value)) value.forEach((item) => bad.push(...secretKeyNames(item, path)));
  else if (value && typeof value === "object") {
    for (const [key, inner] of Object.entries(value)) {
      if (/(secret|password)$/i.test(key)) bad.push(key);
      bad.push(...secretKeyNames(inner, `${path}.${key}`));
    }
  }
  return bad;
}

function checkPod(deployment, found) {
  const at = `Deployment ${deployment.metadata?.name ?? "?"}`;
  const pod = deployment.spec?.template?.spec ?? {};
  const podSecurity = pod.securityContext ?? {};
  if (pod.automountServiceAccountToken !== false) found.push(["service-account-token", at]);
  for (const key of ["hostNetwork", "hostPID", "hostIPC"]) if (pod[key] === true) found.push(["host-namespace", at]);
  for (const volume of list(pod.volumes)) {
    if (volume.hostPath) found.push(["host-path", at]);
  }
  const containers = list(pod.containers);
  if (containers.length !== 1) found.push(["container-count", at]);
  for (const container of list(pod.initContainers).concat(containers)) {
    for (const mount of list(container.volumeMounts)) {
      if (SOCKET_PATTERN.test(String(mount.mountPath ?? ""))) found.push(["socket-mount", at]);
    }
  }
  for (const container of list(pod.initContainers)) checkContainer(container, podSecurity, at, found, true);
  for (const container of containers) checkContainer(container, podSecurity, at, found, false);
}

// The rules of one container. An init container runs without any capability and has no probes.
function checkContainer(container, podSecurity, at, found, init) {
  const security = container.securityContext ?? {};
  const effective = (key) => security[key] ?? podSecurity[key];
  if (effective("runAsNonRoot") !== true) found.push(["run-as-non-root", at]);
  if (Number(effective("runAsUser")) !== USER) found.push(["user", at]);
  if (Number(effective("runAsGroup") ?? USER) !== USER) found.push(["user", at]);
  if (security.readOnlyRootFilesystem !== true) found.push(["writable-root", at]);
  if (security.privileged === true) found.push(["privileged", at]);
  if (security.allowPrivilegeEscalation === false) found.push(["allow-privilege-escalation", at]);
  if (!sameSet(upper(security.capabilities?.drop), ["ALL"])) found.push(["cap-drop", at]);
  if (!sameSet(upper(security.capabilities?.add), init ? [] : CAPABILITIES)) found.push(["cap-add", at]);
  const seccomp = (security.seccompProfile ?? podSecurity.seccompProfile)?.type;
  if (seccomp !== "RuntimeDefault") found.push(["seccomp", at]);
  if (!init) {
    for (const probe of ["startupProbe", "readinessProbe", "livenessProbe"]) {
      const command = list(container[probe]?.exec?.command).map(String);
      if (!command.includes("healthcheck")) found.push(["probe", `${at} ${probe}`]);
    }
  }
  const image = String(container.image ?? "");
  const tagged = /@sha256:[0-9a-f]{64}$/.test(image) || /^[^@]*:[^:/@]+$/.test(image.replace(/^[^/]*:\d+\//, ""));
  if (image === "" || !tagged || /:latest(@|$)/.test(image)) found.push(["image-tag", at]);
}

export function checkChart(objects) {
  const found = [];
  const all = flatten(objects);
  if (all.length === 0) throw new Unusable("the chart rendered no object");
  const deployments = all.filter((object) => object?.kind === "Deployment");
  if (deployments.length !== 1) found.push(["deployment-count", "chart"]);
  for (const object of all) {
    const kind = object?.kind;
    const name = object?.metadata?.name ?? "?";
    if (kind === "Secret") found.push(["secret-object", `Secret ${name}`]);
    if (["StatefulSet", "DaemonSet", "ReplicaSet", "ReplicationController", "Job", "CronJob"].includes(kind)) {
      found.push(["workload-kind", `${kind} ${name}`]);
    }
    if (kind === "PersistentVolumeClaim" && !sameSet(list(object.spec?.accessModes), ["ReadWriteOnce"])) {
      found.push(["access-mode", `PersistentVolumeClaim ${name}`]);
    }
    if (kind === "ConfigMap") checkConfigMap(object, found);
  }
  for (const deployment of deployments) {
    const at = `Deployment ${deployment.metadata?.name ?? "?"}`;
    if (deployment.spec?.replicas !== 1) found.push(["replicas", at]);
    if (deployment.spec?.strategy?.type !== "Recreate") found.push(["strategy", at]);
    checkPod(deployment, found);
  }
  return unique(found);
}

function checkConfigMap(map, found) {
  const text = map.data?.["server.json"];
  if (text === undefined) return;
  const at = `ConfigMap ${map.metadata?.name ?? "?"}`;
  let config;
  try {
    config = JSON.parse(text);
  } catch {
    found.push(["config-json", at]);
    return;
  }
  if (config.data_dir !== DATA_DIR) found.push(["config-data-dir", at]);
  if (config.runtime_dir !== RUN_DIR) found.push(["config-runtime-dir", at]);
  if (config.tls?.mode === "proxy" && list(config.tls.trusted_proxies).length === 0) found.push(["trusted-proxies", at]);
  if (secretKeyNames(config).length > 0) found.push(["config-secret", at]);
}

// ---- the self-test -------------------------------------------------------------------------------------------------

function cleanCompose() {
  const hardened = () => ({
    user: "10001:10001",
    read_only: true,
    cap_drop: ["ALL"],
    volumes: [{ type: "volume", source: "key", target: "/etc/holzwave/key", read_only: true }],
  });
  return {
    name: "holzwave",
    services: {
      holzwave: {
        ...hardened(),
        cap_add: [...CAPABILITIES],
        ports: [{ mode: "ingress", target: TLS_PORT, published: "8443", protocol: "tcp" }],
        tmpfs: ["/run/holzwave:rw,nosuid,nodev,noexec,uid=10001,gid=10001,mode=0700", "/tmp:rw,nosuid,nodev,noexec,mode=1777"],
      },
      keygen: hardened(),
    },
  };
}

function cleanChart() {
  const container = {
    name: "holzwave",
    image: `ghcr.io/holzcloud/holzwave:0.0.1-beta1@sha256:${"a".repeat(64)}`,
    securityContext: {
      runAsNonRoot: true,
      runAsUser: USER,
      runAsGroup: USER,
      readOnlyRootFilesystem: true,
      capabilities: { drop: ["ALL"], add: [...CAPABILITIES] },
    },
    startupProbe: { exec: { command: ["holzwave-server", "healthcheck"] } },
    readinessProbe: { exec: { command: ["holzwave-server", "healthcheck"] } },
    livenessProbe: { exec: { command: ["holzwave-server", "healthcheck"] } },
    volumeMounts: [{ name: "data", mountPath: DATA_DIR }],
  };
  const initContainer = {
    name: "prepare",
    image: container.image,
    securityContext: {
      runAsNonRoot: true,
      runAsUser: USER,
      runAsGroup: USER,
      readOnlyRootFilesystem: true,
      capabilities: { drop: ["ALL"] },
    },
  };
  return [
    { apiVersion: "v1", kind: "ServiceAccount", metadata: { name: "holzwave" }, automountServiceAccountToken: false },
    {
      apiVersion: "v1",
      kind: "ConfigMap",
      metadata: { name: "holzwave" },
      data: {
        "server.json": JSON.stringify({
          public_url: "https://holzwave.example.net",
          data_dir: DATA_DIR,
          runtime_dir: RUN_DIR,
          tls: { mode: "proxy", trusted_proxies: ["10.0.0.0/8"] },
          auth: { oidc: [{ id: "corp", client_secret_file: "/etc/holzwave/oidc/corp/secret" }] },
        }),
      },
    },
    { apiVersion: "v1", kind: "PersistentVolumeClaim", metadata: { name: "holzwave" }, spec: { accessModes: ["ReadWriteOnce"] } },
    {
      apiVersion: "apps/v1",
      kind: "Deployment",
      metadata: { name: "holzwave" },
      spec: {
        replicas: 1,
        strategy: { type: "Recreate" },
        template: {
          spec: {
            automountServiceAccountToken: false,
            securityContext: { fsGroup: USER, seccompProfile: { type: "RuntimeDefault" } },
            initContainers: [initContainer],
            containers: [container],
            volumes: [{ name: "data", persistentVolumeClaim: { claimName: "holzwave" } }],
          },
        },
      },
    },
  ];
}

const clone = (value) => structuredClone(value);
const deploymentOf = (objects) => objects.find((object) => object.kind === "Deployment");
const containerOf = (objects) => deploymentOf(objects).spec.template.spec.containers[0];
const initOf = (objects) => deploymentOf(objects).spec.template.spec.initContainers[0];
const configOf = (objects) => {
  const map = objects.find((object) => object.kind === "ConfigMap");
  const config = JSON.parse(map.data["server.json"]);
  return { map, config, save: () => { map.data["server.json"] = JSON.stringify(config); } };
};

// One entry per rule: [name, sample, how to break it, the rule that must be the only one reported].
const COMPOSE_CASES = [
  ["privileged", (c) => { c.services.holzwave.privileged = true; }],
  ["user", (c) => { c.services.holzwave.user = "0:0"; }],
  ["writable-root", (c) => { c.services.holzwave.read_only = false; }],
  ["cap-drop", (c) => { c.services.holzwave.cap_drop = []; }],
  ["cap-add", (c) => { c.services.holzwave.cap_add.push("DAC_OVERRIDE"); }],
  ["no-new-privileges", (c) => { c.services.holzwave.security_opt = ["no-new-privileges:true"]; }],
  ["host-namespace", (c) => { c.services.holzwave.pid = "host"; }],
  ["device", (c) => { c.services.holzwave.devices = ["/dev/kvm:/dev/kvm"]; }],
  ["socket-mount", (c) => { c.services.holzwave.volumes.push({ type: "bind", source: "/var/run/docker.sock", target: "/var/run/docker.sock" }); }],
  ["port", (c) => { c.services.holzwave.ports.push({ mode: "ingress", target: 8080, published: "8080", protocol: "tcp" }); }],
  ["run-tmpfs", (c) => { c.services.holzwave.tmpfs = ["/tmp"]; }],
  ["service-missing", (c) => { delete c.services.keygen; }],
];

const CHART_CASES = [
  ["deployment-count", (o) => { o.push(clone(deploymentOf(o))); }, ["deployment-count"]],
  ["replicas", (o) => { deploymentOf(o).spec.replicas = 2; }],
  ["strategy", (o) => { deploymentOf(o).spec.strategy.type = "RollingUpdate"; }],
  ["service-account-token", (o) => { delete deploymentOf(o).spec.template.spec.automountServiceAccountToken; }],
  ["host-namespace", (o) => { deploymentOf(o).spec.template.spec.hostNetwork = true; }],
  ["host-path", (o) => { deploymentOf(o).spec.template.spec.volumes.push({ name: "h", hostPath: { path: "/srv" } }); }],
  ["socket-mount", (o) => { containerOf(o).volumeMounts.push({ name: "s", mountPath: "/var/run/docker.sock" }); }],
  ["run-as-non-root", (o) => { containerOf(o).securityContext.runAsNonRoot = false; }],
  ["user", (o) => { containerOf(o).securityContext.runAsUser = 0; }],
  ["writable-root", (o) => { containerOf(o).securityContext.readOnlyRootFilesystem = false; }],
  ["privileged", (o) => { containerOf(o).securityContext.privileged = true; }],
  ["allow-privilege-escalation", (o) => { containerOf(o).securityContext.allowPrivilegeEscalation = false; }],
  ["cap-drop", (o) => { containerOf(o).securityContext.capabilities.drop = []; }],
  ["cap-add", (o) => { containerOf(o).securityContext.capabilities.add.push("SYS_ADMIN"); }],
  ["seccomp", (o) => { delete deploymentOf(o).spec.template.spec.securityContext.seccompProfile; }],
  ["privileged (init container)", (o) => { initOf(o).securityContext.privileged = true; }, ["privileged"]],
  ["cap-add (init container)", (o) => { initOf(o).securityContext.capabilities.add = ["CHOWN"]; }, ["cap-add"]],
  ["image-tag (init container)", (o) => { initOf(o).image = "registry.example.net/holzwave:latest"; }, ["image-tag"]],
  ["probe", (o) => { delete containerOf(o).readinessProbe; }],
  ["image-tag", (o) => { containerOf(o).image = "ghcr.io/holzcloud/holzwave:latest"; }],
  ["access-mode", (o) => { o.find((x) => x.kind === "PersistentVolumeClaim").spec.accessModes = ["ReadWriteMany"]; }],
  ["secret-object", (o) => { o.push({ apiVersion: "v1", kind: "Secret", metadata: { name: "made-by-the-chart" } }); }],
  ["workload-kind", (o) => { o.push({ apiVersion: "apps/v1", kind: "StatefulSet", metadata: { name: "second" } }); }],
  ["config-json", (o) => { o.find((x) => x.kind === "ConfigMap").data["server.json"] = "{"; }],
  ["config-data-dir", (o) => { const c = configOf(o); c.config.data_dir = "/var/lib/other"; c.save(); }],
  ["config-runtime-dir", (o) => { const c = configOf(o); c.config.runtime_dir = "/tmp"; c.save(); }],
  ["trusted-proxies", (o) => { const c = configOf(o); c.config.tls.trusted_proxies = []; c.save(); }],
  ["config-secret", (o) => { const c = configOf(o); c.config.auth.oidc[0].client_secret = "x"; c.save(); }],
];

function names(found) {
  return found.map(([rule]) => rule);
}

export function selfTest(log = console.log) {
  let failures = 0;
  const expect = (label, got, want) => {
    const same = got.length === want.length && want.every((rule, i) => got[i] === rule);
    if (same) log(`ok: ${label}`);
    else {
      failures += 1;
      log(`FAILED: ${label}: expected [${want.join(", ")}], got [${got.join(", ")}]`);
    }
  };
  expect("compose: the clean sample has no violation", names(checkCompose(cleanCompose())), []);
  for (const [rule, mutate] of COMPOSE_CASES) {
    const sample = cleanCompose();
    mutate(sample);
    expect(`compose: ${rule}`, names(checkCompose(sample)), [rule]);
  }
  expect("chart: the clean sample has no violation", names(checkChart(cleanChart())), []);
  for (const [rule, mutate, expected] of CHART_CASES) {
    const sample = cleanChart();
    mutate(sample);
    expect(`chart: ${rule}`, names(checkChart(sample)), expected ?? [rule]);
  }
  // The edges of the reader: several objects one after the other, a brace inside a string, a List, and garbage.
  const joined = cleanChart().map((object) => JSON.stringify(object)).join("\n");
  expect("chart: objects one after the other", names(checkChart(splitObjects(joined))), []);
  expect("chart: a List of objects", names(checkChart(splitObjects(JSON.stringify({ kind: "List", items: cleanChart() })))), []);
  expect("reader: a brace inside a string", [String(splitObjects('{"a":"}{"}')[0].a)], ["}{"]);
  let refused = false;
  try {
    splitObjects('{"a":1} nonsense');
  } catch (error) {
    refused = error instanceof Unusable;
  }
  expect("reader: text between objects is unusable", [String(refused)], ["true"]);
  log(failures === 0 ? "self-test: ok" : `self-test: ${failures} failed`);
  return failures === 0;
}

// ---- the command line ----------------------------------------------------------------------------------------------

function report(found) {
  for (const [rule, where] of found) console.log(`violation: ${rule}: ${where}`);
  return found.length === 0 ? 0 : 1;
}

function render(valuesFile) {
  const env = { ...process.env, KUBECONFIG: "/dev/null" };
  const chart = fileURLToPath(new URL("../packaging/helm/holzwave", import.meta.url));
  let yaml;
  try {
    yaml = execFileSync("helm", ["template", "holzwave", chart, "-f", valuesFile], { env, encoding: "utf8", maxBuffer: 16 * 1024 * 1024 });
  } catch (error) {
    if (error.code === "ENOENT") throw new Unusable("helm is missing");
    throw new Unusable("helm template failed");
  }
  try {
    return execFileSync("kubectl", ["annotate", "--local", "-f", "-", "-o", "json", "holzwave.example/checked=yes"], {
      env,
      input: yaml,
      encoding: "utf8",
      maxBuffer: 16 * 1024 * 1024,
    });
  } catch (error) {
    if (error.code === "ENOENT") throw new Unusable("kubectl is missing");
    throw new Unusable("kubectl could not convert the rendered chart");
  }
}

async function main(argv) {
  const [command, argument] = argv;
  try {
    if (command === "self-test" && argv.length === 1) return selfTest() ? 0 : 1;
    if (command === "compose" && argv.length === 1) return report(checkCompose(JSON.parse(await readAll(process.stdin))));
    if (command === "chart" && argv.length === 1) return report(checkChart(splitObjects(await readAll(process.stdin))));
    if (command === "chart-values" && argv.length === 2) return report(checkChart(splitObjects(render(argument))));
  } catch (error) {
    console.error(`web-deploy-check: ${error instanceof Unusable || error instanceof SyntaxError ? error.message : "unusable input"}`);
    return 2;
  }
  console.error("usage: node scripts/web-deploy-check.mjs compose|chart|self-test (JSON on standard input), or chart-values FILE");
  return 2;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  process.exitCode = await main(process.argv.slice(2));
}
