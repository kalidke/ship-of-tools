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
# comm/ whose non-comment lines name a comm script (`comm-*.sh` or
# `comm-lib`) sources lib-home-guard.sh before any command but `set`. That
# check first proves it flags a copy of test-hub-files.sh without its source
# line.
#
# Usage: comm/core/tests/test-rm-guard.sh
# Exit: 0 if no unguarded site or suite, 1 naming each one.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)" || { echo "FATAL: not in a git checkout" >&2; exit 1; }
T="$(mktemp -d "${TMPDIR:-/tmp}/sot-rm-guard-XXXXXX")" && [ -d "$T" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
trap 'rm -rf "${T:?}"' EXIT

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

files=(); suites=()
while IFS= read -r f; do
    case "$f" in comm/*) case "${f##*/}" in test-*.sh) suites+=("$REPO/$f") ;; esac ;; esac
    case "${f##*/}" in
        *.sh) files+=("$REPO/$f") ;;
        *.*) ;;
        *) [ -f "$REPO/$f" ] && head -n 1 "$REPO/$f" 2>/dev/null | grep -q -E '^#!.*[/ ](ba|da|k|z)?sh([[:space:]]|$)' \
               && files+=("$REPO/$f") ;;
    esac
done < <(git -C "$REPO" ls-files)
[ "${#files[@]}" -gt 0 ] || { echo "FATAL: no shell files found" >&2; exit 1; }

sites="$(perl -e "$RM_SCAN" "${files[@]}")"

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
sed '/^\. .*lib-home-guard\.sh/d' "$SCRIPT_DIR/test-hub-files.sh" > "$T/test-copy.sh"
[ "$(unguarded "$T/test-copy.sh")" = "$T/test-copy.sh" ] && [ -z "$(unguarded "$SCRIPT_DIR/test-hub-files.sh")" ] \
    || { echo "FAIL: the home-guard check does not tell test-hub-files.sh from a copy without its source line"; exit 1; }
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
exit "$rc"
