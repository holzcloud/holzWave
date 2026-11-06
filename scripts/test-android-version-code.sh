#!/bin/sh
# Prove the version code scheme of scripts/android-version-code.sh: the fixed table, the strict order over many versions,
# and the refusals. It needs nothing but sh, awk and sort.
#
#   sh scripts/test-android-version-code.sh
#
# Exit status: 0 everything held, 1 a check failed.

here=$(cd "$(dirname "$0")" && pwd -P)
script="$here/android-version-code.sh"

checks=0
failures=0
ok() {
    checks=$((checks + 1))
    echo "  ok   $1"
}
bad() {
    checks=$((checks + 1))
    failures=$((failures + 1))
    echo "  FAIL $1" >&2
}

tmp=$(mktemp -d "${TMPDIR:-/tmp}/holzwave-version-code.XXXXXX") || exit 1
trap 'rm -rf "$tmp"' EXIT HUP INT TERM

# Test 1: the table of the scheme.
table="0.0.1-alpha1 1101
0.0.1-alpha2 1102
0.0.1-beta1 1201
0.0.1-beta2 1202
0.0.1-rc1 1301
0.0.1 1900
0.0.2-alpha1 2101
0.1.0 100900
1.0.0 10000900
0.0.1-alpha99 1199
0.0.1-rc99 1399
209.99.99 2099999900
209.99.99-rc99 2099999399"
echo "$table" | while read -r version want; do
    got=$(sh "$script" "$version" 2>"$tmp/err")
    status=$?
    if [ "$status" -eq 0 ] && [ "$got" = "$want" ] && [ ! -s "$tmp/err" ]; then
        echo "  ok   $version -> $want"
    else
        echo "  FAIL $version printed '$got' (status $status), wanted $want" >&2
        echo x >> "$tmp/failed"
    fi
done
rows=$(echo "$table" | wc -l | tr -d ' ')
checks=$((checks + rows))
if [ -f "$tmp/failed" ]; then
    failures=$((failures + $(wc -l < "$tmp/failed" | tr -d ' ')))
fi

# The largest code stays below Android's limit of 2100000000.
top=$(sh "$script" 209.99.99)
if [ "$top" -lt 2100000000 ]; then ok "the largest final version, $top, is below 2100000000"; else bad "the largest final version is $top"; fi

# Test 2: the order is strict over a list of real and invented versions. The list is shuffled on purpose and sorted by an
# independent key (numbers, then the stage rank, then the pre-release number), so the order of the codes is not the order
# of the list as it is written.
cat > "$tmp/versions" <<'EOF'
1.0.0
0.0.1-rc1
0.0.1
0.1.0-beta2
0.0.2-beta1
0.0.1-beta2
0.0.1-alpha1
0.0.1-alpha2
0.0.1-beta1
0.0.1-rc2
0.0.2-alpha1
0.0.2-alpha2
0.0.2-rc1
0.0.2
0.0.3-beta1
0.0.3
0.0.9
0.0.10-alpha1
0.0.10
0.0.99
0.1.0-alpha1
0.1.0-beta1
0.1.0-rc1
0.1.0
0.1.1
0.1.9
0.2.0-beta3
0.2.0
0.9.0
0.10.0-alpha1
0.10.0
0.99.0
0.99.99
1.0.0-alpha1
1.0.0-beta1
1.0.0-rc1
1.0.1-alpha1
1.0.1
1.1.0-beta2
1.1.0
1.9.9
2.0.0-rc1
2.0.0
10.0.0-alpha1
10.0.0
99.99.99
209.0.0-alpha1
209.99.99
EOF
count=$(wc -l < "$tmp/versions" | tr -d ' ')
if [ "$count" -ge 40 ]; then ok "the order list has $count versions (at least 40)"; else bad "the order list has only $count versions"; fi

awk '{
    v = $0; pre = ""; stage = 9; num = 0
    i = index(v, "-")
    if (i > 0) { pre = substr(v, i + 1); v = substr(v, 1, i - 1) }
    split(v, p, ".")
    if (pre != "") {
        word = pre; sub(/[0-9]+$/, "", word); num = substr(pre, length(word) + 1) + 0
        stage = (word == "alpha") ? 1 : (word == "beta") ? 2 : 3
    }
    printf "%d %d %d %d %d %s\n", p[1], p[2], p[3], stage, num, $0
}' "$tmp/versions" | sort -n -k1,1 -k2,2 -k3,3 -k4,4 -k5,5 | awk '{ print $6 }' > "$tmp/sorted"

: > "$tmp/codes"
while read -r version; do
    printf '%s %s\n' "$(sh "$script" "$version")" "$version" >> "$tmp/codes"
done < "$tmp/sorted"

if awk '{ if (NR > 1 && $1 <= prev) { printf "%s (%s) is not above %s (%s)\n", $2, $1, prev_version, prev; bad = 1 } prev = $1; prev_version = $2 } END { exit bad }' "$tmp/codes" 2> "$tmp/order-err"; then
    ok "the codes of $count versions are strictly increasing in version order"
else
    bad "the order broke"
    awk '{ if (NR > 1 && $1 <= prev) { printf "%s (%s) is not above %s (%s)\n", $2, $1, prev_version, prev } prev = $1; prev_version = $2 }' "$tmp/codes" >&2
fi

# alpha < beta < rc < final inside one patch, and every build of 0.0.N is below every build of 0.0.(N+1).
a=$(sh "$script" 0.0.1-alpha99)
b=$(sh "$script" 0.0.1-beta1)
bb=$(sh "$script" 0.0.1-beta99)
r=$(sh "$script" 0.0.1-rc1)
rr=$(sh "$script" 0.0.1-rc99)
f=$(sh "$script" 0.0.1)
next=$(sh "$script" 0.0.2-alpha1)
if [ "$a" -lt "$b" ] && [ "$bb" -lt "$r" ] && [ "$rr" -lt "$f" ] && [ "$f" -lt "$next" ]; then
    ok "the highest alpha is below the first beta, the highest beta below the first rc, the highest rc below the final, the final below the next patch"
else
    bad "a stage boundary is not strict: $a $b $bb $r $rr $f $next"
fi

# Test 3: refusals. Exit status 2, one line on stderr, nothing on stdout.
refused() { # <what> <version>
    what=$1
    shift
    out=$(sh "$script" "$@" 2>"$tmp/err")
    status=$?
    lines=$(wc -l < "$tmp/err" | tr -d ' ')
    if [ "$status" -eq 2 ] && [ -z "$out" ] && [ "$lines" -eq 1 ]; then
        ok "refused: $what"
    else
        bad "$what: status $status, stdout '$out', $lines line(s) on stderr"
    fi
}
refused "a patch over 99" 0.0.100
refused "a minor over 99" 0.100.0
refused "a major over 209" 210.0.0
refused "a pre-release number over 99" 0.0.1-beta100
refused "a pre-release number of 0" 0.0.1-beta0
refused "a pre-release without a number" 0.0.1-beta
refused "an unknown stage word" 0.0.1-gamma1
refused "a dotted pre-release" 0.0.1-beta.2
refused "build metadata" 0.0.1+build5
refused "build metadata after a pre-release" 0.0.1-beta2+x
refused "an empty string" ""
refused "a word" gamma
refused "two numbers" 1.0
refused "four numbers" 1.0.0.0
refused "a leading zero" 0.01.0
refused "a negative number" 0.-1.0
refused "a very long number" 0.0.99999999999999999999
refused "no argument"
refused "two arguments" 0.0.1 0.0.2

echo "android-version-code-test: $checks checks, $failures failed"
[ "$failures" -eq 0 ] || exit 1
echo "android-version-code-test: ok"
