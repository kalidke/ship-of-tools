#!/usr/bin/env bash
# test-sigpipe-scan.sh -- sigpipe-scan.awk flags each early-stopping reader in a pipeline and passes the whole-input forms.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/sigpipe-scan-XXXXXX")"; trap 'rm -rf "${tmp:?}"' EXIT
fail=0
scan() { printf '%s\n' "$1" > "$tmp/s.sh"; awk -f "$here/sigpipe-scan.awk" "$tmp/s.sh" | grep -c . || true; }
want() {  # <name> <hits> <script line>
    local got; got="$(scan "$3")"
    if [ "$got" = "$2" ]; then echo "PASS $1"; else echo "FAIL $1: $got hit(s), want $2"; fail=$((fail + 1)); fi
}
want "grep -q after a pipe" 1 'printf "%s" "$v" | grep -q x'
want "grep -m1 after a pipe" 1 'x="$(cmd | grep -m1 x)"'
want "head after a pipe" 1 'cmd | head -n 1'
want "head at the start of a pipeline's second line" 1 $'cmd \\\n    | head -1'
want "a trailing pipe joins the reader" 1 $'cmd |\n    head -1'
want "awk with exit in a quoted program on later lines" 1 $'printf "%s" "$1" | awk \'\n  $1 == a { exit }\n\''
want "sed with a q command" 1 'cmd | sed -n "1{p;q}"'
want "read as a stage" 1 'cmd | read -r x'
want "sed -n 1p reads everything" 0 'cmd | sed -n 1p'
want "grep to /dev/null reads everything" 0 'cmd | grep x >/dev/null'
want "a here-string has no writer process" 0 'grep -q x <<<"$v"'
want "head of a file is no pipeline reader" 0 'head -n 1 "$f"'
want "a comment line" 0 '# cmd | head -1'
want "an alternation in single quotes" 0 "grep -E 'a|head' file"
want "awk without exit" 0 $'cmd | awk \'\n  { n++ }\n  END { print n }\n\''
exit "$fail"
