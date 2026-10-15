"use strict";
/**
 * `check tdd-review-checkpoint` as a gate module (#5139, epic #5056, ADR-5057 §4 first bullet): it
 * returns a `GateResult`; the command router formats it. Imports no io module and performs no
 * direct console/stdout/stderr write (ESLint-enforced).
 *
 * End-of-phase advisory check: scans `type: tdd` plans for RED/GREEN/REFACTOR gate-sequence
 * compliance (`test(<plan>):` / `feat(<plan>):` / `refactor(<plan>):` commits) and builds a review
 * table. `passed` is always true (advisory — never truly blocks); `block` is `violations > 0` so
 * the host loop can read one uniform field.
 *
 * `type: tdd` is detected by the Frontmatter Module's `frontmatterKeyHasValue` (the old
 * `^type:\s*tdd\s*$` multiline test over the fence owner's block, key and value escaped).
 *
 * A plan's commits come from the evaluation-scope resolver (#5164, ADR-5057 §4): the commits
 * reachable from HEAD whose SUBJECT is `<type>(<phase>-<plan>):`, anchored and zero-padding
 * tolerant. A plan id that is not `<phase>-<plan>` (a plan named `.*-PLAN.md`) matches nothing,
 * and a git failure is "no commits" exactly as before.
 *
 * Argv after the verb: `<phase>` (a number; an unresolvable phase reports zero plans).
 */
var __importDefault = (this && this.__importDefault) || function (mod) {
    return (mod && mod.__esModule) ? mod : { "default": mod };
};
Object.defineProperty(exports, "__esModule", { value: true });
exports.evaluateTddReviewCheckpoint = evaluateTddReviewCheckpoint;
const node_path_1 = __importDefault(require("node:path"));
const gate_verdict_cjs_1 = require("./gate-verdict.cjs");
const gate_phase_context_cjs_1 = require("./gate-phase-context.cjs");
const gate_evidence_cjs_1 = require("./gate-evidence.cjs");
const gate_evaluation_scope_cjs_1 = require("./gate-evaluation-scope.cjs");
// eslint-disable-next-line @typescript-eslint/no-require-imports
const frontmatterMod = require("./frontmatter.cjs");
const { frontmatterKeyHasValue } = frontmatterMod;
/**
 * True when the plan's frontmatter declares `type: tdd` (CRLF included, #2449). The block is the
 * one fence owner's, read as RAW text — not through the YAML parser — so a block the parser refuses
 * (a `--- x` line before `type:`) still classifies, exactly as under the old `^type:\s*tdd\s*$`
 * multiline regex, whose edge behaviour (a duplicate `type:` key, a value on the next line,
 * `type : tdd` NOT matching, `type: "tdd"` / `type: tdd # note` NOT matching) is preserved.
 */
function isTddPlan(content) {
    return frontmatterKeyHasValue(content, 'type', 'tdd');
}
/**
 * The commit types (`test`, `feat`, `refactor`, …) among the plan's own commits — those that
 * touched at least one path (`pathspecs: ['.']`, as the old `git log -- .` lookup did). An
 * unresolvable scope (git unavailable, an id that is not `<phase>-<plan>`) is "no commits".
 */
function planCommitKinds(projectDir, planId) {
    const scope = (0, gate_evaluation_scope_cjs_1.resolveEvaluationScope)(projectDir, { kind: 'plan', planId }, { pathspecs: ['.'], commitsOnly: true });
    const kinds = new Set();
    for (const commit of scope.commits) {
        const kind = /^([a-z]+)\(/.exec(commit.subject)?.[1];
        if (kind)
            kinds.add(kind);
    }
    return kinds;
}
/**
 * The verdict when the review could not read a plan or the phase directory. Advisory policy is
 * unchanged (`block: false`); the outcome is `unreadable` (exit UNAVAILABLE) and `passed` is false
 * because nothing was certified.
 */
function unreadableReview(phase, tddPlans, unreadable) {
    const names = unreadable.map((u) => `${u.source} (${u.reason})`).join(', ');
    return (0, gate_verdict_cjs_1.gateUnreadable)(false, {
        block: false,
        passed: false,
        tddPlans,
        violations: 0,
        table: '',
        rows: [],
        unreadable,
        message: `TDD review could not read: ${names}. Phase ${phase} was not reviewed.`,
    });
}
function evaluateTddReviewCheckpoint(input) {
    const { projectDir } = input;
    const phase = input.args[0] || '';
    if (!phase) {
        return (0, gate_verdict_cjs_1.gateUsageFailure)(gate_verdict_cjs_1.GATE_FAILURE_CODE.SDK_MISSING_ARG, 'tdd.review-checkpoint requires a phase argument: check tdd.review-checkpoint <phase>');
    }
    // #5170 (ADR-5057 §4): an ABSENT phase directory is `none` (no plans, the skip below); anything
    // that exists but cannot be read is `unreadable` and the verdict says so — an unreadable plan is
    // never "not a TDD plan".
    const located = (0, gate_phase_context_cjs_1.resolvePhaseDir)(projectDir, phase);
    if (located.kind === 'unreadable') {
        return unreadableReview(phase, 0, [{ source: `phase ${phase}`, reason: located.reason }]);
    }
    const phaseDir = located.kind === 'found' ? located.value : '';
    // Find all PLAN.md files with type: tdd in frontmatter
    const tddPlanFiles = [];
    const unreadable = [];
    if (phaseDir) {
        const entries = (0, gate_evidence_cjs_1.readDirEvidence)(phaseDir);
        if (entries.kind === 'unreadable') {
            unreadable.push({ source: phaseDir, reason: entries.reason });
        }
        else if (entries.kind === 'found') {
            // #3183: canonical plan set (root+nested, superseded-excluded) from the single owner. A scan
            // that did not see every plan (an unreadable nested plans/) is `unreadable`, never a short list.
            const planSet = (0, gate_evidence_cjs_1.readPlanSetEvidence)(phaseDir);
            if (planSet.kind === 'unreadable') {
                unreadable.push({ source: planSet.span ?? phaseDir, reason: planSet.reason });
            }
            else {
                for (const file of planSet.value) {
                    const planPath = node_path_1.default.join(phaseDir, file);
                    const plan = (0, gate_evidence_cjs_1.readTextEvidence)(planPath);
                    if (plan.kind === 'unreadable')
                        unreadable.push({ source: planPath, reason: plan.reason });
                    else if (plan.kind === 'found' && isTddPlan(plan.value))
                        tddPlanFiles.push(planPath);
                }
            }
        }
    }
    if (unreadable.length > 0)
        return unreadableReview(phase, tddPlanFiles.length, unreadable);
    if (tddPlanFiles.length === 0) {
        return (0, gate_verdict_cjs_1.gateVerdict)('skip', false, {
            // Uniform gate contract: block = violations > 0 (advisory; never truly blocks).
            block: false,
            passed: true,
            tddPlans: 0,
            violations: 0,
            table: '',
            rows: [],
            message: `No type:tdd plans found in phase ${phase}. TDD review skipped.`,
        });
    }
    // For each TDD plan, extract the plan ID (e.g. "01-02-PLAN.md" → "01-02") and check git log
    const rows = [];
    for (const planPath of tddPlanFiles) {
        const planId = node_path_1.default.basename(planPath, '-PLAN.md');
        const kinds = planCommitKinds(projectDir, planId);
        const red = kinds.has('test');
        const green = kinds.has('feat');
        const refactor = kinds.has('refactor');
        const missing = [];
        if (!red)
            missing.push('RED');
        if (!green)
            missing.push('GREEN');
        const status = missing.length === 0 ? 'Pass' : 'FAIL';
        rows.push({ planId, red, green, refactor, status, missing });
    }
    const violations = rows.filter(r => r.status === 'FAIL').length;
    // Build review table
    const tableHeader = '| Plan | RED | GREEN | REFACTOR | Status |';
    const tableDivider = '|------|-----|-------|----------|--------|';
    const tableRows = rows.map(r => `| ${r.planId.padEnd(4)} | ${r.red ? ' ✓ ' : ' ✗ '} | ${r.green ? '  ✓  ' : '  ✗  '} | ${r.refactor ? '   ✓    ' : '   —    '} | ${r.status.padEnd(6)} |`);
    let table = [
        `### TDD REVIEW — Phase ${phase}`,
        '',
        `TDD Plans: ${tddPlanFiles.length} | Gate violations: ${violations}`,
        '',
        tableHeader,
        tableDivider,
        ...tableRows,
    ].join('\n');
    if (violations > 0) {
        table += '\n\n⚠ Gate violations are advisory — review before advancing.';
        for (const r of rows.filter(row => row.status === 'FAIL')) {
            table += `\n  Plan ${r.planId} missing: ${r.missing.join(', ')} gate commit(s).`;
            table += `\n  Expected commit pattern: test(${r.planId}): ... → feat(${r.planId}): ...`;
        }
    }
    // Uniform gate contract: block = violations > 0. The gate is advisory (blocking:false in
    // capability.json), so block:true only surfaces as a warning, never halts. The human-readable
    // report is carried in `message` (and `table`) for the dispatch's advisory branch.
    return (0, gate_verdict_cjs_1.gateVerdict)(violations > 0 ? 'advisory' : 'pass', violations > 0, {
        block: violations > 0,
        passed: true,
        tddPlans: tddPlanFiles.length,
        violations,
        table,
        rows,
        message: table,
    });
}
