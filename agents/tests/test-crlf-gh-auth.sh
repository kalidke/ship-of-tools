#!/usr/bin/env bash
# test-crlf-gh-auth.sh — sot-gh-auth.sh reads eight values out of JSON, across
# thirteen `jq -r` sites, and a native (non-MSYS) jq on Windows opens stdout in
# text mode and rewrites every \n it writes to \r\n. Command substitution strips
# the final \n and leaves the carriage return glued to the value, so on a Windows
# box each of those reads was wrong in its own way:
#
#   * `device_code` — written to a 0600 file and handed to curl as
#     `--data-urlencode device_code@<file>`, so the CR is SENT and GitHub
#     rejects the exchange;
#   * `interval` / `expires_in` — `[ "$interval" -ge 1 ]` fails, so a silent
#     default replaces the server's value;
#   * `error` — every `case` arm misses, so an authorization that is merely
#     PENDING falls through to `*)` and the poll aborts with "token error";
#   * `user_code` / `verification_uri` / `expires_in` — printed as the
#     `SOT_GH_USER_CODE=` / `SOT_GH_VERIFY_URL=` / `SOT_GH_EXPIRES_IN=` lines a
#     controller greps, so a CR corrupts that parse;
#   * `access_token` — stored in gh's hosts.yml and then sent on every request.
#
# This test makes ANY jq behave like a native Windows one, with a wrapper first
# on PATH that re-emits the real jq's output with \r before every \n. The
# wrapper STREAMS: an earlier test in this directory captured the stub's output
# in `$( )`, which strips the trailing newline and so modelled a second bug at
# the same time, capable of hiding the one under test.
#
# THE TOKEN IS NEVER READ OR PRINTED BY ANYTHING HERE. The value used is a fake
# literal, and the gh stub records only two facts about the bytes it receives on
# stdin: whether they contain a carriage return, and how many there are. No
# assertion, log line or failure message prints the value itself.
#
# Confirmed against the UNFIXED script (2026-09-29): the `error` read trips
# first, so the script exits 7 after a single poll and the token never reaches
# the gh stub at all; the device-code file carries a CR; the interval falls back
# to 5; and the three machine-parseable lines each end in one. Eleven of its
# twelve assertions fail there, so this test can see the defect at every site.
#
# HERMETIC: a temp $SOT_COMM_HOME and $GH_CONFIG_DIR, stub curl/gh/jq/sleep
# confined to a PATH prefix, and no network. One `request` and one `poll` per
# run, with every assertion made against those two runs' artifacts.
#
# Usage: comm/core/tests/test-crlf-gh-auth.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.

set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-home-guard.sh" || exit 2   # never the live comm home

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REAL_JQ="$(command -v jq)"
FAKE_TOKEN='FAKE-TOKEN-not-a-real-credential'
PASS=0; FAIL=0

ok()  { PASS=$((PASS+1)); printf '  ok   %s\n' "$1"; }
bad() { FAIL=$((FAIL+1)); printf '  FAIL %s\n' "$1"; }
# A mismatch that is ONLY a carriage return prints as "expected '900', got
# '900'" unless the message says otherwise — which in this suite of all places
# would send a reader looking in the wrong direction.
check() {
    if [ "$2" = "$3" ]; then ok "$1"; return; fi
    local got="$2" note=""
    case "$got" in *$'\r') note=" [the value ends in a carriage return]"; got="${got%$'\r'}" ;; esac
    bad "$1 (expected '$3', got '$got'$note)"
}
has_cr() { LC_ALL=C grep -q $'\r' "$@"; }

setup() {
    TMP="$(mktemp -d "${TMPDIR:-/tmp}/sot-gh-crlf.XXXXXX")"
    guard_fresh_home "$TMP"
    GH_AUTH="$(guard_stage_bin "$TMP")/sot-gh-auth.sh" || exit 2
    STUB="$TMP/stub"; mkdir -p "$STUB" "$TMP/comm" "$TMP/gh"

    # jq: the real one, with every \n rewritten to \r\n. Streamed, and the real
    # exit status preserved so a failing filter still fails.
    cat > "$STUB/jq" <<JQ
#!/usr/bin/env bash
"$REAL_JQ" "\$@" | sed \$'s/\$/\r/'
exit "\${PIPESTATUS[0]}"
JQ

    # curl: no network. Routed by endpoint. The token endpoint answers "pending"
    # once, then hands over the fake token. It also records whether the
    # device_code FILE it was given holds a CR, which is what proves the
    # write-time strip.
    cat > "$STUB/curl" <<'CURL'
#!/usr/bin/env bash
url=""; for a in "$@"; do case "$a" in https://*) url="$a" ;; esac; done
for a in "$@"; do
    case "$a" in
        device_code@*)
            f="${a#device_code@}"
            if LC_ALL=C grep -q $'\r' "$f"; then echo 1 > "$SOT_TEST_TMP/dc.hascr"
            else echo 0 > "$SOT_TEST_TMP/dc.hascr"; fi ;;
    esac
done
case "$url" in
  */login/device/code)
    printf '{"device_code":"DC-FAKE-0000","user_code":"ABCD-1234","verification_uri":"https://example.invalid/device","interval":1,"expires_in":900}' ;;
  */login/oauth/access_token)
    n=$(( $(cat "$SOT_TEST_TMP/tok.count" 2>/dev/null || echo 0) + 1 ))
    printf '%s' "$n" > "$SOT_TEST_TMP/tok.count"
    if [ "$n" = 1 ]; then printf '{"error":"authorization_pending"}'
    else printf '{"access_token":"%s","token_type":"bearer","scope":"repo"}' "$SOT_TEST_FAKE_TOKEN"; fi ;;
  *) printf '{}' ;;
esac
CURL

    # gh: records only two facts about the bytes on stdin — never the value.
    # `auth status` always fails, which is what makes `request` proceed instead
    # of reporting "already authenticated"; finish()'s own status call tolerates
    # a failure by design.
    cat > "$STUB/gh" <<'GH'
#!/usr/bin/env bash
case "$*" in
    *"--with-token"*)
        bytes=$(cat)
        if printf '%s' "$bytes" | LC_ALL=C grep -q $'\r'; then cr=1; else cr=0; fi
        printf 'HAS_CR=%s\nLEN=%s\n' "$cr" "$(printf '%s' "$bytes" | wc -c | tr -d ' ')" \
            > "$SOT_TEST_TMP/gh.token-facts"
        exit 0 ;;
    *"auth status"*) echo "stub: not logged in" >&2; exit 1 ;;
    *"setup-git"*)   exit 0 ;;
    *"api user"*)    echo "tester"; exit 0 ;;
    *) exit 0 ;;
esac
GH

    # sleep: never actually wait; record what interval the script asked for.
    cat > "$STUB/sleep" <<'SLP'
#!/usr/bin/env bash
[ -f "$SOT_TEST_TMP/sleep.first" ] || printf '%s' "$1" > "$SOT_TEST_TMP/sleep.first"
exit 0
SLP
    chmod +x "$STUB"/*
}

run() {  # run() <subcommand> <stdout-file> <stderr-file>; echoes the exit status
    ( unset GH_TOKEN GITHUB_TOKEN
      export PATH="$STUB:$PATH" SOT_COMM_HOME="$TMP/comm" GH_CONFIG_DIR="$TMP/gh" \
             SOT_TEST_TMP="$TMP" SOT_TEST_FAKE_TOKEN="$FAKE_TOKEN"
      bash "$GH_AUTH" "$1" >"$2" 2>"$3" )
    echo $?
}

echo "test-crlf-gh-auth.sh"
setup

# --- request: the lines a controller greps, and the state file it leaves -------
rc="$(run request "$TMP/req.out" "$TMP/req.err")"
check "request exits 0 under a CRLF jq" "$rc" "0"
for k in SOT_GH_USER_CODE SOT_GH_VERIFY_URL SOT_GH_EXPIRES_IN; do
    line="$(grep "^${k}=" "$TMP/req.out" 2>/dev/null | head -1)"
    if [ -z "$line" ]; then bad "$k is printed"
    elif printf '%s' "$line" | has_cr -; then bad "$k has no trailing CR"
    else ok "$k has no trailing CR"; fi
done
check "the expiry a controller reads is the server's" \
      "$(sed -n 's/^SOT_GH_EXPIRES_IN=//p' "$TMP/req.out" | head -1)" "900"

# --- poll: the token, the device code, the pending arm, the interval ----------
rc="$(run poll "$TMP/poll.out" "$TMP/poll.err")"
check "poll exits 0 under a CRLF jq" "$rc" "0"
facts="$(cat "$TMP/gh.token-facts" 2>/dev/null || echo MISSING)"
check "the token reached gh with no CR" "$(printf '%s' "$facts" | sed -n 's/^HAS_CR=//p')" "0"
check "and with every byte intact" "$(printf '%s' "$facts" | sed -n 's/^LEN=//p')" \
      "$(printf '%s' "$FAKE_TOKEN" | wc -c | tr -d ' ')"
check "the device code curl was given had no CR" "$(cat "$TMP/dc.hascr" 2>/dev/null)" "0"
# Under a CRLF jq the `error` value carried a CR, every case arm missed, and the
# poll died on the first response. Two calls to the token endpoint is the proof
# it kept waiting instead.
check "a pending authorization did not abort the poll" "$(cat "$TMP/tok.count" 2>/dev/null)" "2"
# The interval is the server's 1, not the 5 the numeric test falls back to when
# the value it read ends in a carriage return.
check "the server's interval survived the read" "$(cat "$TMP/sleep.first" 2>/dev/null)" "1"

# Not vacuous: it requires that the script actually produced output first, so a
# run that died early cannot pass this by printing nothing.
if [ ! -s "$TMP/poll.out" ]; then
    bad "the script never printed the token (no output at all — nothing was proved)"
elif LC_ALL=C grep -qF "$FAKE_TOKEN" "$TMP/poll.out" "$TMP/poll.err" "$TMP/req.out" "$TMP/req.err"; then
    bad "the script printed the token"
else
    ok "the script never printed the token"
fi

rm -rf "${TMP:?}"
printf 'PASS=%s FAIL=%s\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
