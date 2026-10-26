#!/bin/sh
# Runs the Autobahn WebSocket test suite against the candidate and exits 0 only when no case FAILED.
#
#   sh autobahn/run.sh
#
# The candidate (an echo server with permessage-deflate and the 8 MiB message cap) listens on 127.0.0.1 only. The
# container reaches it through Docker's host alias. The only volume is the report folder, which also holds the
# configuration (the port is put into a copy of fuzzingclient.json there). The cases are the base protocol (1 to 7 and
# 10), the limit cases of 9 (without the four that send 8 and 16 MiB messages, which are above the cap by design) and the
# permessage-deflate cases 12 and 13. The result is also written into results.json.
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
PROJECT=$(cd "$HERE/.." && pwd)
# Pinned by tag and by digest. Only an amd64 image exists; on Apple silicon it runs under emulation.
IMAGE='crossbario/autobahn-testsuite:25.10.1@sha256:519915fb568b04c9383f70a1c405ae3ff44ab9e35835b085239c258b6fac3074'
REPORTS="$HERE/reports"

record() { # record <json>: write the Autobahn section of results.json
  node -e '
    const fs = require("fs"), p = process.argv[1];
    let all = {}; try { all = JSON.parse(fs.readFileSync(p, "utf8")); } catch {}
    all.autobahn = JSON.parse(process.argv[2]);
    fs.writeFileSync(p + ".tmp", JSON.stringify(all, null, 2)); fs.renameSync(p + ".tmp", p);
  ' "$PROJECT/results.json" "$1"
}
not_run() {
  echo "Autobahn NOT RUN: $1" >&2
  record "{\"ran\": false, \"reason\": \"$1\"}"
  exit 1
}

command -v docker >/dev/null 2>&1 || not_run "Docker is missing"
docker info >/dev/null 2>&1 || not_run "the Docker daemon is not running"
command -v node >/dev/null 2>&1 || not_run "node is missing"

cargo build --locked --offline -q --manifest-path "$PROJECT/Cargo.toml" --bin candidate
LOG=$(mktemp)
"$PROJECT/target/debug/candidate" >"$LOG" 2>&1 &
PID=$!
trap 'kill "$PID" 2>/dev/null || true; rm -f "$LOG"' EXIT INT TERM
PORT=
for _ in $(seq 1 50); do
  PORT=$(sed -n 's/^listening 127\.0\.0\.1:\([0-9][0-9]*\)$/\1/p' "$LOG" | head -n 1)
  [ -n "$PORT" ] && break
  sleep 0.1
done
[ -n "$PORT" ] || not_run "the candidate did not start"

rm -rf "$REPORTS"
mkdir -p "$REPORTS"
sed "s/@PORT@/$PORT/" "$HERE/fuzzingclient.json" >"$REPORTS/fuzzingclient.json"
echo "candidate on 127.0.0.1:$PORT, running $IMAGE"
docker run --rm --platform linux/amd64 -v "$REPORTS:/reports" "$IMAGE" \
  wstest -m fuzzingclient -s /reports/fuzzingclient.json || not_run "the suite did not run to the end"

node -e '
  const fs = require("fs");
  const [index, results, image] = process.argv.slice(1);
  const data = JSON.parse(fs.readFileSync(index, "utf8")).yawc;
  // The rule: a case counts as failed unless its behaviour is OK, NON-STRICT or INFORMATIONAL. How the connection was
  // closed (behaviorClose) is listed apart, as findings, and does not decide.
  const allowed = new Set(["OK", "NON-STRICT", "INFORMATIONAL"]);
  const counts = {}; const closeCounts = {}; const failed = []; const closeFindings = [];
  for (const [id, c] of Object.entries(data)) {
    counts[c.behavior] = (counts[c.behavior] || 0) + 1;
    closeCounts[c.behaviorClose] = (closeCounts[c.behaviorClose] || 0) + 1;
    if (!allowed.has(c.behavior)) failed.push(`${id}: ${c.behavior}`);
    if (c.behaviorClose !== "OK" && c.behaviorClose !== "INFORMATIONAL") closeFindings.push(`${id}: ${c.behaviorClose}`);
  }
  const cases = Object.keys(data).length;
  const summary = { ran: true, image, cases, counts, close_counts: closeCounts, failed: failed.length, failed_cases: failed,
    close_findings: closeFindings,
    deflate_cases: Object.keys(data).filter((id) => /^1[23]\./.test(id)).length,
    excluded: ["9.1.5", "9.1.6", "9.2.5", "9.2.6"] };
  let all = {}; try { all = JSON.parse(fs.readFileSync(results, "utf8")); } catch {}
  all.autobahn = summary;
  fs.writeFileSync(results + ".tmp", JSON.stringify(all, null, 2)); fs.renameSync(results + ".tmp", results);
  console.log(JSON.stringify(summary, null, 2));
  process.exit(failed.length === 0 && cases > 0 ? 0 : 1);
' "$REPORTS/out/index.json" "$PROJECT/results.json" "$IMAGE"
