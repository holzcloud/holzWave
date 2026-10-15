**Step 2a: Resume mode (only when `$RESUME_BATCH_ID` is set)**

Skip this step entirely if `$RESUME_BATCH_ID` is empty.

Resume re-derives eligibility via the batch's own `resumeBatch` propagation —
it is the single source of truth for which items are still runnable. Never
re-parse a task list or re-run `quick-batch create` on resume (row 9/16 of
the design's behavior table).

```bash
_GSD_SHIM_NAME="gsd-tools.cjs"; _GSD_RUNTIME_ROOT="${RUNTIME_DIR:-$(git rev-parse --show-toplevel 2>/dev/null || pwd)}"; GSD_TOOLS="${_GSD_RUNTIME_ROOT}/gsd-core/bin/${_GSD_SHIM_NAME}"; _gsd_at() { for _p; do if [ -f "$_p" ]; then GSD_TOOLS="$_p"; return 0; fi; done; return 1; }; _gsd_id_ok() { case "$("$1" runtime-identity --raw 2>/dev/null || true)" in '{"packageName":"@opengsd/gsd-core"'*'}') return 0;; *) return 1;; esac; }; _gsd_homes() { set -- "${CLAUDE_CONFIG_DIR:-$HOME/.claude}" "${ANTIGRAVITY_CONFIG_DIR:-$HOME/.gemini/antigravity}" "$HOME/.gemini/antigravity-ide" "$HOME/.gemini/antigravity-cli" "${AUGMENT_CONFIG_DIR:-$HOME/.augment}" "${CLINE_CONFIG_DIR:-$HOME/.cline}" "${CODEBUDDY_CONFIG_DIR:-$HOME/.codebuddy}" "${CODEX_HOME:-$HOME/.codex}" "${COPILOT_CONFIG_DIR:-${COPILOT_HOME:-$HOME/.copilot}}" "${CURSOR_CONFIG_DIR:-$HOME/.cursor}" "${HERMES_HOME:-$HOME/.hermes}" "${KILO_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/kilo}" "${KIMI_CONFIG_DIR:-$HOME/.config/agents}" "$HOME/.agents" "${KIMI_CODE_HOME:-$HOME/.kimi-code}" "${OPENCODE_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/opencode}" "${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}" "${QWEN_CONFIG_DIR:-$HOME/.qwen}" "${TRAE_CONFIG_DIR:-$HOME/.trae}" "${WINDSURF_CONFIG_DIR:-$HOME/.codeium/windsurf}" "${ZCODE_CONFIG_DIR:-$HOME/.zcode}" "${GROK_AGENTS_HOME:-$HOME/.agents}"; for _h; do _gsd_at "$_h/gsd-core/bin/${_GSD_SHIM_NAME}" && return 0; done; return 1; }; if _gsd_at "${_GSD_RUNTIME_ROOT}/gsd-core/bin/${_GSD_SHIM_NAME}" "${_GSD_RUNTIME_ROOT}/.claude/gsd-core/bin/${_GSD_SHIM_NAME}" "${_GSD_RUNTIME_ROOT}/.codex/gsd-core/bin/${_GSD_SHIM_NAME}"; then gsd_run() { node "$GSD_TOOLS" "$@"; }; elif _gsd_homes; then gsd_run() { node "$GSD_TOOLS" "$@"; }; elif unset -f gsd_run; _G="$(command -v gsd_run)"; [ -n "$_G" ] && _gsd_id_ok "$_G"; then GSD_TOOLS="$_G"; gsd_run() { "$GSD_TOOLS" "$@"; }; else echo "ERROR: gsd-tools.cjs not found at $GSD_TOOLS and no identity-proving gsd_run is on PATH. Run: npx -y @opengsd/gsd-core@latest --claude --local" >&2; exit 1; fi; GSD_IDENTITY_STATUS=unverified; _gsd_id_ok gsd_run && GSD_IDENTITY_STATUS=ok; export GSD_IDENTITY_STATUS; [ "$GSD_IDENTITY_STATUS" = ok ] || echo "WARNING: \"$GSD_TOOLS\" did not prove it is @opengsd/gsd-core - it is either a different package or an @opengsd/gsd-core older than the runtime-identity verb. See docs/how-to/diagnose-a-foreign-gsd-tools.md" >&2; if [ -n "${CLAUDE_ENV_FILE:-}" ] && [ -n "${GSD_TOOLS:-}" ]; then printf "export PATH='%s':\"\$PATH\"\n" "${GSD_TOOLS%/*}" >> "$CLAUDE_ENV_FILE" 2>/dev/null || true; fi
CURRENT_BASE=$(git rev-parse HEAD)
QB_RESUME_JSON=$(gsd_run quick-batch resume --batch "$RESUME_BATCH_ID" --current-base-revision "$CURRENT_BASE" --raw)
QB_RESUME_RC=$?
if [[ "$QB_RESUME_JSON" == @file:* ]]; then QB_RESUME_JSON=$(cat "${QB_RESUME_JSON#@file:}"); fi
```

**If `$QB_RESUME_RC` is non-zero:** the resume was refused closed — an unknown
batch id (row 18) or a diverged base revision (row 17, ADR-1239 "Base
divergence"). Print the CLI's error message verbatim and STOP. Do not dispatch
anything, do not create a new batch on the user's behalf.

**Otherwise:** parse `$QB_RESUME_JSON` for `eligible` (array of quick ids),
`transitions` (status changes just applied — e.g. a `blocked` item reverting
to `pending`, or a crash-window STATE-row detection completing an item
without re-appending, row 45), and `manifest` (the full, current batch
document).

```bash
BATCH_ID="$RESUME_BATCH_ID"
BATCH_MANIFEST_JSON=$(printf '%s' "$QB_RESUME_JSON" | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{try{const j=JSON.parse(s);process.stdout.write(JSON.stringify(j.manifest))}catch{process.stdout.write("")}})')
```

Report to user:
```
Resuming batch ${BATCH_ID}: ${eligible.length} item(s) eligible now.
```

If `transitions` is non-empty, display it as a diagnostic (which items moved
to `blocked`/`complete` since the batch was last touched) — this is expected,
successful crash-window recovery, not an error (per the design's negative-space
note: a `resumeBatch` call producing zero transitions is also success, not a
no-op failure).

Continue to Step 3 in `quick-batch.md` — the DAG-layer loop in `planner-wave.md`
reads `$BATCH_MANIFEST_JSON`/`$BATCH_ID` exactly the same way whether this
batch was just created or just resumed; it re-derives per-item progress from
which artifacts already exist on disk (PLAN.md/SUMMARY.md/VERIFICATION.md),
never from a separate "resume" code path.
