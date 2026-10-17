# Integration fixtures

What the command-line tools holzWave wraps (`kubectl`, `az`, GAM, `talosctl`) print, as sanitised files that the parser
tests read. Every file below this directory, except this one, is listed in the table exactly once with its source:

- `recorded`: a real run of the tool produced it, through `scripts/record-integration-fixtures.sh`. The run used scratch
  configuration only, a closed environment and read verbs only, and the output passed
  `scripts/sanitise-integration-fixture.py` (identifiers, names and addresses are synthetic).
- `written`: the tool could not be run here (it is absent, or no account is signed in), so the file was written by hand
  from the tool's documents. Replace it with a real capture on a machine that has the tool; no parser has to change.
- drift: a variant of a fixture with a changed shape (an unknown member, a missing member, a wrong type, a cut in the
  middle of a value). It is always `written`, its note starts with `drift`, and its base fixture sits next to it.

Re-record with `sh scripts/record-integration-fixtures.sh <tool|all>`, review `git diff` for anything real, then run
`cargo test -p holzwave-core --test integrate_real`. The tests refuse a fixture that is missing from this table, is listed
twice or is listed and absent, a `recorded` row without a tool version, and a fixture that holds a secret or an identity.

The real-tool tests in the same file run with `HW_INTEGRATION_REAL=1`.

| File | Source | Tool and version | Date | Notes |
|---|---|---|---|---|
| `kubectl/config_view_canary.kubeconfig` | written | kubectl 1.36.1 reads it | 2026-10-18 | scratch kubeconfig with canary secrets: a bearer token and an exec plugin whose environment holds a value; the one file that may hold them |
| `kubectl/config_view.json` | recorded | kubectl 1.36.1 | 2026-10-18 | `config view -o json` of the canary kubeconfig; the tool redacts the token itself; it does not redact exec environment values, so the recorder replaces them with `REDACTED` |
