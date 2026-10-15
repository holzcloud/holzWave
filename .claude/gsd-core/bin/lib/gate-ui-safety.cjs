"use strict";
/**
 * `check ui-safety-gate` as a gate module (#5139, epic #5056, ADR-5057 §4 first bullet): it returns
 * a `GateResult`; the command router formats it. Imports no io module and performs no direct
 * console/stdout/stderr write (ESLint-enforced).
 *
 * Post-wave check that verifies UI-changed files conform to the active UI-SPEC for the phase.
 * Uses `checkUiPresence` from `ui-safety-gate.cjs` (frontend detection is not reimplemented) and
 * looks for frontend files in the phase's evaluation scope (#5164, ADR-5057 §4): the union of the
 * phase's own commits' file sets from `gate-evaluation-scope`, not the last commit. A scope the
 * resolver could not read, or had to widen, is reported (`scopeStatus` / `scopeReason`), never
 * silently treated as "no UI files".
 *
 * Argv after the verb: `<phase>`.
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.computeUiSafetyGate = computeUiSafetyGate;
exports.evaluateUiSafetyGate = evaluateUiSafetyGate;
const gate_verdict_cjs_1 = require("./gate-verdict.cjs");
const gate_phase_context_cjs_1 = require("./gate-phase-context.cjs");
const gate_evaluation_scope_cjs_1 = require("./gate-evaluation-scope.cjs");
const runtime_slash_cjs_1 = require("./runtime-slash.cjs");
const ui_safety_gate_cjs_1 = require("./ui-safety-gate.cjs");
const UI_FILE_EXTENSIONS_RE = /\.(tsx|jsx|css|scss|sass|less|vue|svelte|html)$/i;
const UI_PATH_PATTERNS_RE = /\/(components|pages|views|screens|layouts|ui|frontend)\//i;
/**
 * Pure logic for ui-safety-gate — exposed for direct behavioral testing.
 *
 *   (a) ROADMAP phase section via the shared lookup (same as ui-plan-gate) → is this a frontend phase.
 *   (b) checkUiPresence (frontend detection).
 *   (c) UI files among the phase's evaluation scope (`resolveEvaluationScope`, phase unit; every
 *       git call is bounded by the resolver's seam; an unreadable scope is reported, not "clean").
 *   (d) Phase directory → `*-UI-SPEC.md`.
 *
 * `block = frontend && hasUiFiles && !hasUiSpec`.
 */
function computeUiSafetyGate(projectDir, phase) {
    // (a) phase section text (same two-pass lookup as computeUiPlanGate)
    const { phaseSection, phaseLookupFailed, readError: roadmapReadError } = (0, gate_phase_context_cjs_1.lookupRoadmapPhase)(projectDir, phase);
    // (b) frontend detection — reuse the existing helper; no reimplementation
    const presenceResult = (0, ui_safety_gate_cjs_1.checkUiPresence)(phaseSection);
    const frontend = presenceResult.hasUI;
    // (c) any UI files in the phase's own commits? (deleted paths count: a removed component is a UI change)
    const scope = (0, gate_evaluation_scope_cjs_1.resolveEvaluationScope)(projectDir, { kind: 'phase', phase });
    const hasUiFiles = scope.changedFiles.some((f) => f.trim() && (UI_FILE_EXTENSIONS_RE.test(f) || UI_PATH_PATTERNS_RE.test(f)));
    // (d) phase directory and *-UI-SPEC.md
    // `none` is "no spec"; `unreadable` is "could not look" (#5170) and is carried to the verdict.
    const uiSpec = (0, gate_phase_context_cjs_1.locateUiSpec)(projectDir, phase);
    const hasUiSpec = uiSpec.kind === 'found';
    const readError = roadmapReadError ?? (uiSpec.kind === 'unreadable' ? uiSpec.reason : undefined);
    // block only when: this is a frontend phase AND UI files were changed AND no UI-SPEC exists
    const block = frontend && hasUiFiles && !hasUiSpec;
    const result = { frontend, hasUiFiles, hasUiSpec, block };
    if (block) {
        result.message = `UI files changed in this wave but no UI-SPEC.md exists for Phase ${phase}. ` +
            `Run ${(0, runtime_slash_cjs_1.runtimeCommand)(projectDir, `ui-phase ${phase}`)} to generate the design contract before continuing.`;
    }
    if (phaseLookupFailed)
        result.phaseLookupFailed = true;
    if (scope.status !== 'resolved') {
        result.scopeStatus = scope.status;
        result.scopeReason = scope.reason ?? '';
    }
    if (readError !== undefined)
        result.readError = readError;
    return result;
}
function evaluateUiSafetyGate(input) {
    const phase = input.args[0] || '';
    if (!phase) {
        return (0, gate_verdict_cjs_1.gateUsageFailure)(gate_verdict_cjs_1.GATE_FAILURE_CODE.SDK_MISSING_ARG, 'ui-safety-gate requires a phase argument: check ui-safety-gate <phase>');
    }
    const result = computeUiSafetyGate(input.projectDir, phase);
    // A scope the resolver could not read, or a ROADMAP / phase directory that could not be read, is
    // "could not look", never a pass (ADR-5057 §4): the outcome is `unreadable` and the exit status
    // follows it. `block` is the gate's own policy and is unchanged.
    if (result.scopeStatus === 'unresolvable' || result.readError !== undefined) {
        return (0, gate_verdict_cjs_1.gateUnreadable)(result.block, { ...result });
    }
    return (0, gate_verdict_cjs_1.gateVerdict)(result.block ? 'block' : 'pass', result.block, { ...result });
}
