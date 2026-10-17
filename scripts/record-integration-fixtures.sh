#!/bin/sh
# Record what the command-line tools holzWave wraps really print, as sanitised test fixtures.
#
# Usage: sh scripts/record-integration-fixtures.sh <kubectl|az|gam|talosctl|all>
#
# The fixtures land in core/tests/fixtures/integrations/<tool>/ and every file is listed in MANIFEST.md there, with its
# source (recorded from a real run, or written by hand). Exit status: 0 on success, 2 for an unknown tool or a tool that
# is not installed.
#
# Safety rules of this script:
#   * Every run points the tool at scratch locations (KUBECONFIG, AZURE_CONFIG_DIR, GAMCFGDIR, TALOSCONFIG, HOME) and
#     starts with a closed environment (env -i), so no setting, token or credential of the user reaches a tool.
#   * Read verbs only. It never runs a raw kubeconfig dump, a verbosity flag, a token command, a credential export,
#     login, set, "select ... save" or "config ... save".
#   * Every output that can carry an identity passes scripts/sanitise-integration-fixture.py before it is written below
#     the fixture directory.
#   * The scratch directory is removed on exit.

set -u

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
OUT="$ROOT/core/tests/fixtures/integrations"
SANITISE="$HERE/sanitise-integration-fixture.py"

SCRATCH=$(mktemp -d "${TMPDIR:-/tmp}/holzwave-record.XXXXXX") || {
    echo "cannot create a scratch directory" >&2
    exit 2
}
SERVER_PID=""
cleanup() {
    if [ -n "$SERVER_PID" ]; then
        kill "$SERVER_PID" 2>/dev/null
        wait "$SERVER_PID" 2>/dev/null
    fi
    rm -rf "$SCRATCH"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

usage() {
    echo "usage: sh scripts/record-integration-fixtures.sh <kubectl|az|gam|talosctl|all>" >&2
    exit 2
}

say() {
    printf '%s\n' "$*"
}

# The absolute path of a tool found on the PATH, or nothing.
tool_path() {
    found=$(command -v "$1" 2>/dev/null) || return 1
    case "$found" in
        /*) printf '%s\n' "$found" ;;
        *) return 1 ;;
    esac
}

# Run a command with a hard time limit in seconds: limited SECONDS COMMAND [ARGS...]. Standard input is closed. Output
# goes where the caller redirected it.
limited() {
    secs=$1
    shift
    "$@" </dev/null &
    pid=$!
    (
        sleep "$secs"
        kill -TERM "$pid" 2>/dev/null
        sleep 1
        kill -KILL "$pid" 2>/dev/null
    ) &
    dog=$!
    wait "$pid"
    status=$?
    kill "$dog" 2>/dev/null
    wait "$dog" 2>/dev/null
    return "$status"
}

# Milliseconds since the epoch.
now_ms() {
    python3 -I -c 'import time; print(int(time.time() * 1000))'
}

# Replace the scratch directory in a stream by a neutral path.
unscratch() {
    sed "s#$SCRATCH#/path/to#g"
}

# ---------------------------------------------------------------------------------------------------------------------
# kubectl
# ---------------------------------------------------------------------------------------------------------------------

# The scratch kubeconfig with canary secrets: a bearer token and an exec plugin whose environment holds a value. The
# same text is kept as a fixture so that a test can run the real tool on it.
write_canary_kubeconfig() {
    cat >"$1" <<'EOF'
apiVersion: v1
kind: Config
current-context: dev
clusters:
- name: dev-cluster
  cluster:
    server: https://dev.example.com:6443
    certificate-authority-data: Zm9v
- name: arn:aws:eks:eu-central-1:000000000000:cluster/demo
  cluster:
    server: https://eks.example.com
- name: kind-test
  cluster:
    server: https://127.0.0.1:6443
contexts:
- name: dev
  context:
    cluster: dev-cluster
    user: token-user
- name: arn:aws:eks:eu-central-1:000000000000:cluster/demo
  context:
    cluster: arn:aws:eks:eu-central-1:000000000000:cluster/demo
    user: exec-user
- name: kind-test
  context:
    cluster: kind-test
    user: token-user
    namespace: test
users:
- name: token-user
  user:
    token: HW-CANARY-TOKEN-0001
- name: exec-user
  user:
    exec:
      apiVersion: client.authentication.k8s.io/v1beta1
      command: aws
      args: ["eks", "get-token", "--cluster-name", "demo"]
      env:
      - name: AWS_PROFILE
        value: HW-CANARY-EXEC-ENV
      interactiveMode: IfAvailable
EOF
}

# Run kubectl with a closed environment: HOME and KUBECONFIG in the scratch directory. Never --raw, never -v.
kube() {
    env -i PATH=/usr/bin:/bin HOME="$SCRATCH" KUBECONFIG="$SCRATCH/kubeconfig" "$KUBECTL" --request-timeout=5s "$@"
}

# Clean one kubectl output: the scratch path becomes neutral, names stay (they are scratch names), JSON is re-indented.
clean_kube_json() {
    unscratch | python3 -I "$SANITISE" --keep-names --redact-exec-env --indent "$1"
}

# A small Kubernetes API server on the loopback interface that answers the discovery documents and two lists. It binds to
# 127.0.0.1 and an ephemeral port, writes the port to the file named by its first argument and serves until killed. It
# does not use http.server's own server class: that one resolves the host name of the bound address on start and can
# wait for a DNS answer for half a minute.
fake_api_source() {
    cat <<'EOF'
import http.server, json, socketserver, sys

def namespace(index, name):
    return {"apiVersion": "v1", "kind": "Namespace",
            "metadata": {"name": name, "uid": "00000000-0000-4000-8000-%012d" % index,
                         "creationTimestamp": "2026-01-0%dT09:00:00Z" % index,
                         "labels": {"kubernetes.io/metadata.name": name}},
            "spec": {"finalizers": ["kubernetes"]}, "status": {"phase": "Active"}}

def container(name, ready, restarts, state):
    return {"name": name, "ready": ready, "restartCount": restarts, "state": state, "started": ready,
            "image": "registry.example.com/%s:1" % name, "imageID": ""}

def pod(index, name, owner, phase, node, ip, containers, conditions):
    return {"apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": name, "namespace": "default", "uid": "00000000-0000-4000-8000-%012d" % (100 + index),
                         "creationTimestamp": "2026-01-05T10:00:00Z", "labels": {"app": owner},
                         "ownerReferences": [{"apiVersion": "apps/v1", "kind": "ReplicaSet", "name": owner,
                                              "uid": "00000000-0000-4000-8000-%012d" % (200 + index),
                                              "controller": True, "blockOwnerDeletion": True}]},
            "spec": {"nodeName": node, "restartPolicy": "Always", "serviceAccountName": "default",
                     "containers": [{"name": c["name"], "image": c["image"], "imagePullPolicy": "IfNotPresent",
                                     "resources": {"requests": {"cpu": "100m", "memory": "128Mi"}}} for c in containers]},
            "status": {"phase": phase, "hostIP": "192.0.2.1%d" % index, "podIP": ip, "qosClass": "Burstable",
                       "startTime": "2026-01-05T10:00:02Z", "conditions": conditions, "containerStatuses": containers}}

READY = [{"type": "Ready", "status": "True", "lastTransitionTime": "2026-01-05T10:00:08Z"}]
NOT_READY = [{"type": "Ready", "status": "False", "lastTransitionTime": "2026-01-05T10:00:08Z",
              "reason": "ContainersNotReady", "message": "containers with unready status"}]
RUNNING = lambda: {"running": {"startedAt": "2026-01-05T10:00:05Z"}}
NAMESPACES = {"apiVersion": "v1", "kind": "NamespaceList", "metadata": {"resourceVersion": "1"},
              "items": [namespace(1, "default"), namespace(2, "kube-system"), namespace(3, "team-a")]}
PODS = {"apiVersion": "v1", "kind": "PodList", "metadata": {"resourceVersion": "1"}, "items": [
    pod(1, "web-7d9f", "web", "Running", "node-1", "192.0.2.21",
        [container("app", True, 0, RUNNING()), container("sidecar", True, 0, RUNNING())], READY),
    pod(2, "batch-1", "batch", "Pending", "", "",
        [container("job", False, 0, {"waiting": {"reason": "ContainerCreating"}})], NOT_READY),
    pod(3, "api-5c6d", "api", "Running", "node-2", "192.0.2.22",
        [container("api", False, 7, {"waiting": {"reason": "CrashLoopBackOff",
                                                 "message": "back-off 5m0s restarting failed container"}})], NOT_READY)]}
RESOURCES = {"kind": "APIResourceList", "apiVersion": "v1", "groupVersion": "v1", "resources": [
    {"name": "namespaces", "singularName": "", "namespaced": False, "kind": "Namespace",
     "verbs": ["get", "list"], "shortNames": ["ns"]},
    {"name": "pods", "singularName": "", "namespaced": True, "kind": "Pod",
     "verbs": ["get", "list"], "shortNames": ["po"]}]}
DOCUMENTS = {
    "/api": {"kind": "APIVersions", "versions": ["v1"],
             "serverAddressByClientCIDRs": [{"clientCIDR": "0.0.0.0/0", "serverAddress": "127.0.0.1:1"}]},
    "/apis": {"kind": "APIGroupList", "apiVersion": "v1", "groups": []},
    "/api/v1": RESOURCES,
    "/api/v1/namespaces": NAMESPACES,
    "/api/v1/namespaces/default/pods": PODS,
}

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        pass
    def do_GET(self):
        document = DOCUMENTS.get(self.path.split("?")[0])
        status = {"kind": "Status", "apiVersion": "v1", "status": "Failure", "reason": "NotFound", "code": 404}
        body = json.dumps(document if document is not None else status).encode()
        self.send_response(200 if document is not None else 404)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

class Server(socketserver.ThreadingMixIn, socketserver.TCPServer):
    daemon_threads = True
    allow_reuse_address = True

if len(sys.argv) > 1 and sys.argv[1] == "--print-lists":
    json.dump({"namespaces": NAMESPACES, "pods": PODS}, sys.stdout)
    sys.exit(0)
server = Server(("127.0.0.1", 0), Handler)
with open(sys.argv[1], "w") as port_file:
    port_file.write(str(server.server_address[1]))
server.serve_forever()
EOF
}

# The namespace and pod lists. First choice: the real kubectl against the fake API server above (its answer is the
# envelope kubectl really prints around the items). When that fails or hangs, the lists are written from the same data and
# the manifest rows must say written. Sets LISTS_SOURCE to recorded or written.
LISTS_SOURCE=written
record_kubectl_lists() {
    fake_api_source >"$SCRATCH/fake_api.py"
    python3 -I "$SCRATCH/fake_api.py" "$SCRATCH/port" &
    SERVER_PID=$!
    tries=0
    while [ ! -s "$SCRATCH/port" ] && [ "$tries" -lt 50 ]; do
        sleep 0.1
        tries=$((tries + 1))
    done
    if [ ! -s "$SCRATCH/port" ]; then
        say "fake API: the server did not start"
    else
        port=$(cat "$SCRATCH/port")
        if limited 15 kube --server="http://127.0.0.1:$port" get namespaces -o json >"$SCRATCH/ns.json" 2>"$SCRATCH/ns.err" &&
            limited 15 kube --server="http://127.0.0.1:$port" get pods -n default -o json >"$SCRATCH/pods.json" 2>"$SCRATCH/pods.err"; then
            clean_kube_json 4 <"$SCRATCH/ns.json" >"$OUT/kubectl/namespaces.json" || return 2
            clean_kube_json 4 <"$SCRATCH/pods.json" >"$OUT/kubectl/pods.json" || return 2
            LISTS_SOURCE=recorded
            say "recorded kubectl/namespaces.json and kubectl/pods.json against a local fake API server"
        else
            say "fake API: kubectl failed or hung: $(head -c 300 "$SCRATCH/ns.err" "$SCRATCH/pods.err" 2>/dev/null)"
        fi
    fi
    kill "$SERVER_PID" 2>/dev/null
    wait "$SERVER_PID" 2>/dev/null
    SERVER_PID=""
    if [ "$LISTS_SOURCE" != recorded ]; then
        python3 -I "$SCRATCH/fake_api.py" --print-lists >"$SCRATCH/lists.json" || return 2
        python3 -I -c '
import json, sys
lists = json.load(open(sys.argv[1]))
for name, key in (("namespaces", "namespaces"), ("pods", "pods")):
    doc = {"apiVersion": "v1", "items": lists[key]["items"], "kind": "List", "metadata": {"resourceVersion": ""}}
    json.dump(doc, open(sys.argv[2] + "/" + name + ".json", "w"), indent=4, sort_keys=True)
    open(sys.argv[2] + "/" + name + ".json", "a").write("\n")
' "$SCRATCH/lists.json" "$OUT/kubectl" || return 2
        say "WROTE kubectl/namespaces.json and kubectl/pods.json from the API list shapes: the manifest rows say written"
    fi
}

record_kubectl() {
    KUBECTL=$(tool_path kubectl) || {
        say "kubectl is not installed: nothing recorded" >&2
        return 2
    }
    mkdir -p "$OUT/kubectl" || return 2
    say "kubectl: $KUBECTL"
    say "kubectl version: $(env -i PATH=/usr/bin:/bin HOME="$SCRATCH" "$KUBECTL" version --client 2>/dev/null | head -n 1)"

    write_canary_kubeconfig "$SCRATCH/kubeconfig" || return 2
    write_canary_kubeconfig "$OUT/kubectl/config_view_canary.kubeconfig" || return 2

    # The tool redacts a bearer token and does not redact the values of an exec plugin's environment: the sanitiser
    # replaces those. Context names are not identities in this scratch file, so they stay.
    if ! limited 15 kube config view -o json >"$SCRATCH/view.json" 2>"$SCRATCH/view.err"; then
        say "kubectl config view failed: $(cat "$SCRATCH/view.err")" >&2
        return 2
    fi
    clean_kube_json 4 <"$SCRATCH/view.json" >"$SCRATCH/view.clean" || return 2
    cp "$SCRATCH/view.clean" "$OUT/kubectl/config_view.json" || return 2
    say "recorded kubectl/config_view.json"

    if ! limited 15 kube version --client -o json >"$SCRATCH/version.json" 2>"$SCRATCH/version.err"; then
        say "kubectl version failed: $(cat "$SCRATCH/version.err")" >&2
        return 2
    fi
    clean_kube_json 2 <"$SCRATCH/version.json" >"$OUT/kubectl/version_client.json" || return 2
    say "recorded kubectl/version_client.json"

    record_kubectl_lists || return 2
}

# ---------------------------------------------------------------------------------------------------------------------

[ "$#" -eq 1 ] || usage

status=0
case "$1" in
    kubectl)
        record_kubectl || status=$?
        ;;
    all)
        record_kubectl || status=$?
        ;;
    *)
        usage
        ;;
esac
exit "$status"
