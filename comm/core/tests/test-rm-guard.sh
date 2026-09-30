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
# followed by `/` (its output may be empty too, and `"$(f)"/*` is then `/*`).
# Commands inside strings (`bash -c '…'`, ssh command lines, `trap '…'`,
# heredocs written to a stub) are read the same way. Comment lines are
# skipped. A file with a dot in its NAME is read only as `*.sh`; a dot in a
# directory never hides an extensionless script.
#
# Usage: comm/core/tests/test-rm-guard.sh
# Exit: 0 if no unguarded site, 1 naming each one.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)" || { echo "FATAL: not in a git checkout" >&2; exit 1; }

files=()
while IFS= read -r f; do
    case "${f##*/}" in
        *.sh) files+=("$REPO/$f") ;;
        *.*) ;;
        *) [ -f "$REPO/$f" ] && head -n 1 "$REPO/$f" 2>/dev/null | grep -q -E '^#!.*[/ ](ba|da|k|z)?sh([[:space:]]|$)' \
               && files+=("$REPO/$f") ;;
    esac
done < <(git -C "$REPO" ls-files)
[ "${#files[@]}" -gt 0 ] || { echo "FATAL: no shell files found" >&2; exit 1; }

sites="$(perl -e '
my $V = q{(?:[A-Za-z_]\w*+|\d++)};
my $TOK = qr/(?:^|\s)(?:\\*["\x27])*\\*\$(?:\(X*(?:\\*["\x27])*\/|\{$V(?:\[[@*]\])?+\}|$V|\{$V(?:\[[@*]\])?+(?:[^}:]|:[^?])[^}]*\})/;
for my $f (@ARGV) {
    open my $fh, "<", $f or next;
    while (my $line = <$fh>) {
        next if $line =~ /^\s*#/;
        while ($line =~ /(?<![\w.\/-])rm(?:\s+-[\w-]*)*(?=\s)/g) {
            my ($i, $depth, $mask) = (pos($line), 0, "");
            while ($i < length $line) {
                my $c = substr($line, $i, 1);
                if (substr($line, $i, 2) eq q{$(}) { $mask .= $depth ? "XX" : q{$(}; $depth++; $i += 2; next; }
                if ($depth) { $depth-- if $c eq ")"; $depth++ if $c eq "("; $mask .= "X"; $i++; next; }
                last if $c =~ /[;&|>)\n]/;
                $mask .= $c; $i++;
            }
            while ($mask =~ /$TOK/g) { (my $t = $&) =~ s/^\s+//; print "$f:$.: $t\n"; }
        }
    }
}' "${files[@]}")"

if [ -z "$sites" ]; then
    echo "PASS: every rm rooted in a variable is guarded (${#files[@]} shell files)"
    exit 0
fi
printf '%s\n' "$sites" | sed "s#^$REPO/#  #"
echo "FAIL: $(printf '%s\n' "$sites" | wc -l) rm site(s) rooted in an unguarded variable — write it \${VAR:?}"
exit 1
