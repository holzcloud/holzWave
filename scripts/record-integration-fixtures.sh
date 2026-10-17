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
trap 'rm -rf "$SCRATCH"' EXIT INT TERM

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
    unscratch <"$SCRATCH/view.json" | python3 -I "$SANITISE" --keep-names --redact-exec-env --indent 4 >"$SCRATCH/view.clean" || return 2
    cp "$SCRATCH/view.clean" "$OUT/kubectl/config_view.json" || return 2
    say "recorded kubectl/config_view.json"
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
