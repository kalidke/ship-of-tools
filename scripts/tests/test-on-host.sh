#!/usr/bin/env bash
# test-on-host.sh -- scripts/tests/on-host.sh against a stub `ssh` that runs
# `bash -s` locally: words arrive intact, the remote run has no SOT_ variables,
# the caller's environment is not echoed, and the refusals exit 2.
#
# Run: scripts/tests/test-on-host.sh

set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ON_HOST="$HERE/on-host.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK:?}"' EXIT
fails=0
passes=0

ok() {  # <description> <condition status>
    if [ "$2" -eq 0 ]; then
        printf '    ok   %s\n' "$1"
        passes=$((passes + 1))
    else
        printf '    FAIL %s\n' "$1"
        fails=$((fails + 1))
    fi
}

mkdir -p "$WORK/bin" "$WORK/dir"
cat > "$WORK/bin/ssh" <<'STUB'
#!/usr/bin/env bash
# stub: ssh HOST bash -s  ->  bash -s here, stdin passed through
shift
exec "$@"
STUB
chmod +x "$WORK/bin/ssh"
export PATH="$WORK/bin:$PATH"

export SOT_CANARY=value-xyz XDG_STATE_HOME=/xdg-canary JULIA_LOAD_PATH=jlp-canary JULIA_PROJECT=jp-canary

# 1. words arrive exactly
nasty=$'a b' q="it's \"q\"" d='$HOME $(echo x)' nl=$'line1\nline2;' semi='x; echo y'
got="$("$ON_HOST" host "$WORK/dir" -- printf '[%s]' "$nasty" "$q" "$d" "$nl" "$semi" 2>"$WORK/err")"
want="$(printf '[%s]' "$nasty" "$q" "$d" "$nl" "$semi")"
[ "$got" = "$want" ]; ok "words with spaces, quotes, \$, newlines and ; arrive intact" $?

# 2. remote run is clean, caller's variables survive
out="$("$ON_HOST" host "$WORK/dir" -- bash -c 'echo "sot=[$(compgen -v SOT_)] x=[${XDG_STATE_HOME-}] l=[${JULIA_LOAD_PATH-}] p=[${JULIA_PROJECT-}]"; pwd' 2>"$WORK/err")"
[ "$out" = "sot=[] x=[] l=[] p=[]
$WORK/dir" ]; ok "SOT_ and the three named variables are unset, cwd is DIR" $?
[ "$SOT_CANARY" = value-xyz ] && [ "$XDG_STATE_HOME" = /xdg-canary ]; ok "caller's variables untouched" $?

# 3. only the command's output
"$ON_HOST" host "$WORK/dir" -- echo hello >"$WORK/o" 2>"$WORK/e"
[ "$(cat "$WORK/o")" = hello ] && [ ! -s "$WORK/e" ]; ok "output is only the command's own" $?
grep -q value-xyz "$WORK/o" "$WORK/e" "$WORK/err"; [ $? -ne 0 ]; ok "canary never appears" $?

# exit status passes through; missing DIR fails loud
"$ON_HOST" host "$WORK/dir" -- bash -c 'exit 7' >/dev/null 2>&1; [ $? -eq 7 ]; ok "command's exit status passes through" $?
"$ON_HOST" host "$WORK/nope" -- echo hi >"$WORK/o" 2>"$WORK/e"; rc=$?
[ "$rc" -ne 0 ] && [ ! -s "$WORK/o" ] && [ -s "$WORK/e" ]; ok "missing DIR fails loud, runs nothing" $?

# 4. refusals
refuse() {  # <description> args...
    local d="$1"; shift
    "$ON_HOST" "$@" >"$WORK/o" 2>"$WORK/e"; local rc=$?
    [ "$rc" -eq 2 ] && [ ! -s "$WORK/o" ] && [ "$(wc -l <"$WORK/e")" -eq 1 ]
    ok "refuses: $d" $?
}
refuse "empty command" host "$WORK/dir" --
refuse "missing --" host "$WORK/dir" echo hi
refuse "empty host" "" "$WORK/dir" -- echo hi
refuse "empty dir" host "" -- echo hi
refuse "no arguments"

printf '%d passed, %d failed\n' "$passes" "$fails"
[ "$fails" -eq 0 ]
