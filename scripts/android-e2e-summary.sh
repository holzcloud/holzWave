#!/bin/sh
# The table of the emulator proofs: what ran, what did not run, what failed. Pure: it reads lines, it touches no device.
#
#   sh scripts/android-e2e-summary.sh < results.txt
#
# Input: lines of the form
#
#   result <TAB> <section> <TAB> <check> <TAB> <ok|not-run|FAIL> <TAB> <reason>
#
# (scripts/android-e2e.sh prints them, five fields separated by tabs, so that a check or a reason may hold spaces, pipes and
# quotes). Every other line is ignored. A status other than the three is a malformed line and counts as a FAIL, because a line
# nobody can read is not a pass.
#
# Output: one row per section with the counts of ok, not run and FAIL, the total, every `not run` with its reason, every FAIL with
# its reason, and a last line:
#
#   e2e summary: ok, not run: N
#   e2e summary: FAIL: M, not run: N
#
# Exit status: 0 when no FAIL (a `not run` is not a pass and is not a failure: it is counted on its own and named, and the last line
# says how many there were), 1 when any FAIL, 2 when no result line was read at all (a run that printed nothing did not pass).

awk -F'\t' '
    function cut(s, n) { return length(s) > n ? substr(s, 1, n - 3) "..." : s }
    $1 == "result" && NF >= 4 {
        total++
        sec = $2
        if (!(sec in seen)) { seen[sec] = 1; order[++sections] = sec }
        status = $4
        reason = (NF >= 5) ? $5 : ""
        if (status == "ok") { ok[sec]++; oks++ }
        else if (status == "not-run") { nr[sec]++; nrs++; nrlist[nrs] = sec "\t" $3 "\t" reason }
        else {
            if (status != "FAIL") reason = "unreadable status \"" status "\"" (reason == "" ? "" : ": " reason)
            fl[sec]++; fls++; fllist[fls] = sec "\t" $3 "\t" reason
        }
        next
    }
    END {
        if (total == 0) {
            print "e2e summary: no result line was read, so nothing ran and nothing passed" > "/dev/stderr"
            exit 2
        }
        printf "%-12s %6s %8s %6s\n", "section", "ok", "not run", "FAIL"
        for (i = 1; i <= sections; i++) {
            sec = order[i]
            printf "%-12s %6d %8d %6d\n", sec, ok[sec] + 0, nr[sec] + 0, fl[sec] + 0
        }
        printf "%-12s %6d %8d %6d\n", "total", oks + 0, nrs + 0, fls + 0
        if (nrs > 0) {
            print ""
            print "not run (each one is a proof that was NOT made):"
            for (i = 1; i <= nrs; i++) {
                split(nrlist[i], f, "\t")
                printf "  %s: %s -- %s\n", f[1], cut(f[2], 100), (f[3] == "" ? "no reason given" : f[3])
            }
        }
        if (fls > 0) {
            print ""
            print "FAIL:"
            for (i = 1; i <= fls; i++) {
                split(fllist[i], f, "\t")
                printf "  %s: %s -- %s\n", f[1], cut(f[2], 100), (f[3] == "" ? "no reason given" : f[3])
            }
        }
        print ""
        if (fls > 0) printf "e2e summary: FAIL: %d, not run: %d\n", fls, nrs + 0
        else printf "e2e summary: ok, not run: %d\n", nrs + 0
        exit (fls > 0) ? 1 : 0
    }
'
