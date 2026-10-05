#!/usr/bin/env bash
# test-rm-guard.sh — a delete rooted in a variable aborts when the variable is
# empty. Every `rm` in a tracked shell file (`*.sh`, and any extensionless
# file whose first line is a sh shebang) whose path starts with a variable
# writes it `${VAR:?}`: an unset or empty VAR then stops the shell instead of
# turning `"$VAR"/*` into `/*`. A variable that is legitimately empty at that
# point is skipped before the delete (`[ -z "${VAR:-}" ] || rm -f -- "${VAR:?}"`).
#
# The check reads each rm's arguments up to the end of its command (a `;`,
# `&`, `|`, `)`, a redirect or the end of the line), steps over the inside of
# `$(...)`, and names every argument that starts — after any quotes, escaped
# or not — with `$VAR`, `${VAR}`, `${VAR<any other modifier>}`, or a `$(...)`
# followed by anything in the same argument (its output may be empty too, and
# `"$(f)"/*` is then `/*`, `"$(f)"*` every file here). A `$(...)` that is the
# whole argument passes, and so does `$((...))`, which is never empty.
# Commands inside strings (`bash -c '…'`, ssh command lines, `trap '…'`,
# heredocs written to a stub) are read the same way. Comment lines are
# skipped. A file with a dot in its NAME is read only as `*.sh`; a dot in a
# directory never hides an extensionless script. Before the walk, the pattern
# runs over its own table of must-flag and must-pass lines, one per shape.
#
# The same walk holds the home guard to its word: every `test-*.sh` under
# comm/ and agents/ whose non-comment lines name a comm script (`comm-*.sh` or
# `comm-lib`) sources lib-home-guard.sh before any command but `set`. That
# check first proves it flags a copy of test-hub-files.sh without its source
# line.
#
# The same walk pins the suites' clock reads (comm/tests/CLAUDE.md's timing rule):
# every tracked `*.sh` under comm/tests and agents/tests but this file is counted
# for `EPOCHREALTIME`, `date ... +%s...` and `SECONDS` on its non-comment lines, and
# the count must equal its row in the clock table; a file with no row reads no
# clock. A row may only fall: a new or raised row is a review question against the
# rule. The count first proves itself on its own table of lines.
#
# Usage: comm/tests/test-rm-guard.sh
# Exit: 0 if no unguarded site or suite, 1 naming each one.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)" || { echo "FATAL: not in a git checkout" >&2; exit 1; }
T="$(mktemp -d "${TMPDIR:-/tmp}/sot-rm-guard-XXXXXX")" && [ -d "$T" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
trap 'rm -rf "${T:?}"' EXIT
guard_fresh_home "$T"

RM_SCAN='
my $V = q{(?:[A-Za-z_]\w*+|\d++)};
my $TOK = qr/(?:^|\s)(?:\\*["\x27])*\\*\$(?:\(X*+(?:\\*["\x27])*+\S|\{$V(?:\[[@*]\])?+\}|$V|\{$V(?:\[[@*]\])?+(?:[^}:]|:[^?])[^}]*\})/;
for my $f (@ARGV) {
    open my $fh, "<", $f or next;
    while (my $line = <$fh>) {
        next if $line =~ /^\s*#/;
        while ($line =~ /(?<![\w.\/-])rm(?:\s+-[\w-]*)*(?=\s)/g) {
            my ($i, $depth, $mask) = (pos($line), 0, "");
            while ($i < length $line) {
                my $c = substr($line, $i, 1);
                if (substr($line, $i, 2) eq q{$(}) { $mask .= ($depth || substr($line, $i, 3) eq q{$((}) ? "XX" : q{$(}; $depth++; $i += 2; next; }
                if ($depth) { $depth-- if $c eq ")"; $depth++ if $c eq "("; $mask .= "X"; $i++; next; }
                last if $c =~ /[;&|>)\n]/;
                $mask .= $c; $i++;
            }
            while ($mask =~ /$TOK/g) { (my $t = $&) =~ s/^\s+//; print "$f:$.: $t\n"; }
        }
    }
}'

# The pattern's own table: each row is a verdict and one line, with RM for rm
# so this file's own walk does not read the rows as deletes.
n=0; want=()
while IFS= read -r row; do
    n=$((n + 1)); want[n]="${row%% *}"
    printf '%s\n' "${row#* }" | sed 's/RM/rm/' >> "$T/table"
done <<'EOF'
flag RM -rf "$(f)"*
flag RM -rf "$(f)"/x
flag RM -rf "$D"/*
flag RM -f $D/x
flag RM -f "${D}/x"
flag RM -f "${D%/}/x"
flag RM -f "${D:-/tmp}/x"
flag RM -f "${A[@]}"
flag RM -f "$1"
flag RM -f \"$D\"
flag RM -f -- x "$D"
flag bash -c 'RM -f "$D"'
flag trap 'RM -rf "$D"' EXIT
pass RM -rf "$((n))/x"
pass RM -f "${D:?}/x"
pass RM -f "${A[@]:?}"
pass RM -f "$(f)"
pass RM -f "$(dirname "$D")"
pass RM -f x; echo "$D"
pass RM -f x > "$D"
pass RM -f x | tee "$D"
pass # RM -f "$D"
pass firm "$D"
EOF
flagged=" $(perl -e "$RM_SCAN" "$T/table" | cut -d: -f2 | sort -un | tr '\n' ' ')"
table_bad=0
for i in $(seq 1 "$n"); do
    case "$flagged" in *" $i "*) got=flag ;; *) got=pass ;; esac
    [ "$got" = "${want[i]}" ] || { echo "FAIL: the delete pattern's table row $i must ${want[i]}: $(sed -n "${i}p" "$T/table")"; table_bad=1; }
done
[ "$table_bad" -eq 0 ] || exit 1
echo "PASS: the delete pattern flags and passes each of its $n table rows"

# The clock reads: COUNT FILE for each file with at least one, over the non-comment lines.
CLOCK_SCAN='
for my $f (@ARGV) {
    open my $fh, "<", $f or next;
    my $n = 0;
    while (my $line = <$fh>) {
        next if $line =~ /^\s*#/;
        $n++ while $line =~ /EPOCHREALTIME|\bdate\b[^;&|)\n]*\+%s|\bSECONDS\b/g;
    }
    print "$n $f\n" if $n;
}'
# Its own table: each row is the count the pattern must give and one line.
n=0
while IFS= read -r row; do
    n=$((n + 1)); printf '%s\n' "${row#* }" > "$T/clock-row"
    got="$(perl -e "$CLOCK_SCAN" "$T/clock-row" | cut -d' ' -f1)"
    [ "${got:-0}" = "${row%% *}" ] || { echo "FAIL: the clock pattern's table row $n must count ${row%% *}: ${row#* }"; exit 1; }
done <<'EOF'
1 t0=$(date +%s%N)
1 now=$(date -u +%s)
2 a=$EPOCHREALTIME; b=$EPOCHREALTIME
1 [ $((SECONDS - t0)) -lt 3 ] || exit 1
0 # t0=$(date +%s%N)
0 sleep 0.05
0 stamp=$(date +%Y-%m-%dT%H:%M:%SZ)
EOF
echo "PASS: the clock pattern counts each of its $n table rows"

files=(); suites=(); clockfiles=()
while IFS= read -r f; do
    case "$f" in comm/*|agents/*) case "${f##*/}" in test-*.sh) suites+=("$REPO/$f") ;; esac ;; esac
    case "$f" in comm/tests/test-rm-guard.sh) ;; comm/tests/*.sh|agents/tests/*.sh) clockfiles+=("$REPO/$f") ;; esac
    case "${f##*/}" in
        *.sh) files+=("$REPO/$f") ;;
        *.*) ;;
        *) [ -f "$REPO/$f" ] && head -n 1 "$REPO/$f" 2>/dev/null | grep -q -E '^#!.*[/ ](ba|da|k|z)?sh([[:space:]]|$)' \
               && files+=("$REPO/$f") ;;
    esac
done < <(git -C "$REPO" ls-files)
[ "${#files[@]}" -gt 0 ] || { echo "FATAL: no shell files found" >&2; exit 1; }
[ "${#suites[@]}" -ge 29 ] || { echo "FATAL: found ${#suites[@]} comm suites, expected at least 29" >&2; exit 1; }

sites="$(perl -e "$RM_SCAN" "${files[@]}")"
clock_got="$(perl -e "$CLOCK_SCAN" "${clockfiles[@]}" | sed "s#^\([0-9]*\) $REPO/#\1 #" | sort -k2)"

# unguarded SUITE... — each suite whose first command other than `set` is not
# the guard's source line.
unguarded() {
    local f
    for f in "$@"; do
        awk '/^[[:space:]]*#/ { next }
             !seen && !/^[[:space:]]*$/ && !/^[[:space:]]*set[[:space:]]/ {
                 seen = 1; first = ($0 ~ /^[[:space:]]*(\.|source)[[:space:]].*lib-home-guard\.sh/) }
             END { exit !(seen && !first) }' "$f" && printf '%s\n' "$f"
    done
}
printf 'set -u\necho x\n' > "$T/test-synthetic.sh"
[ "$(unguarded "$T/test-synthetic.sh")" = "$T/test-synthetic.sh" ] && [ -z "$(unguarded "$SCRIPT_DIR/test-hub-files.sh")" ] \
    || { echo "FAIL: the home-guard check does not tell test-hub-files.sh from a synthetic suite with no guard"; exit 1; }
bad="$(unguarded "${suites[@]}")"

rc=0
if [ -z "$sites" ]; then
    echo "PASS: every rm rooted in a variable is guarded (${#files[@]} shell files)"
else
    printf '%s\n' "$sites" | sed "s#^$REPO/#  #"
    echo "FAIL: $(printf '%s\n' "$sites" | wc -l) rm site(s) rooted in an unguarded variable — write it \${VAR:?}"
    rc=1
fi
if [ -z "$bad" ]; then
    echo "PASS: every comm suite sources the home guard first (${#suites[@]} suites)"
else
    printf '%s\n' "$bad" | sed "s#^$REPO/##" | while IFS= read -r f; do
        echo "FAIL: $f does not source lib-home-guard.sh before any command but set"
    done
    rc=1
fi

# The clock table: COUNT PATH REASON. A row may only fall; an `owed` row is a wait the timing rule still has to replace.
clock_table="$(cat <<'EOF'
2 agents/tests/test-despawn-resolve.sh owed: a 5 s wall-clock wait for the stub socket
2 agents/tests/test-sot-fe-reauth.sh owed: a 5 s wall-clock wait for the stub socket
2 agents/tests/test-sot-fe-version.sh owed: a 5 s wall-clock wait for the stub socket
2 agents/tests/test-spawn-capsule-workspace.sh owed: a 5 s wall-clock wait for the stub socket
2 agents/tests/test-spawn-remote-no-local-row.sh owed: a 5 s wall-clock wait for the stub socket
4 comm/tests/comm-matrix.sh live matrix over real boxes: times delivery on purpose
2 comm/tests/hub_files/lock_shell.sh a lower bound: the send behind a frozen holder waited its 1 s
4 comm/tests/join_disambiguation/slot_guard.sh owed: two 10 s wall-clock waits
4 comm/tests/join_disambiguation/spawn_and_lock.sh owed: two 10 s wall-clock waits
2 comm/tests/test-agent-join.sh owed: a 5 s wall-clock wait for the stub socket
8 comm/tests/test-comm-e2e-readers.sh needs peer hosts: times delivery on purpose
2 comm/tests/test-endpoint-gate.sh owed: a 3 s upper bound its rc 124 check already covers
2 comm/tests/test-inbox-lock-onehost.sh needs a peer host: prints the elapsed time
14 comm/tests/test-inbox-lock-twohost.sh needs peer hosts: times holders across boxes
2 comm/tests/test-join-disambiguation.sh owed: a 5 s wall-clock wait for the stub socket
15 comm/tests/test-registry-lock.sh lower bounds (t8, t10, comm-status's 10 s) and t13's test of the lock's own clock
3 comm/tests/test-registry-twohost.sh needs peer hosts: a timed run window
3 comm/tests/test-relay-file-first.sh a lower bound: the full 5 s receipt window
EOF
)"
clock_bad=0; clock_rows=0
while read -r got file; do
    [ -n "$file" ] || continue
    row="$(printf '%s\n' "$clock_table" | awk -v f="$file" '$2 == f { print $1 }')"
    if [ -z "$row" ]; then
        echo "FAIL: $file reads the clock $got times and has no row"; clock_bad=1
    elif [ "$got" != "$row" ]; then
        echo "FAIL: $file reads the clock $got times, its row says $row (a new read: count the code's waits with sleep_log or await a signal; a lower count: lower the row)"; clock_bad=1
    fi
    clock_rows=$((clock_rows + 1))
done <<< "$clock_got"
while read -r _ file _; do
    printf '%s\n' "$clock_got" | awk -v f="$file" '$2 == f { found = 1 } END { exit !found }' \
        || { echo "FAIL: $file has a row but reads no clock (delete the row)"; clock_bad=1; }
done <<< "$clock_table"
if [ "$clock_bad" -eq 0 ]; then
    echo "PASS: every clock read in comm/tests and agents/tests matches its row ($clock_rows files)"
else
    rc=1
fi
exit "$rc"
