#!/usr/bin/env bash
# test-crlf-gh-auth.sh — sot-gh-auth.sh reads eleven values out of JSON with
# `jq -r`, and a native (non-MSYS) jq on Windows opens stdout in text mode and
# rewrites every \n it writes to \r\n. Command substitution strips the final \n
# and leaves the carriage return glued to the value, so on a Windows box each of
# those reads was wrong in its own way:
#
#   * `device_code` — written to a 0600 file and handed to curl as
#     `--data-urlencode device_code@<file>`, so the CR is SENT and GitHub
#     rejects the exchange;
#   * `interval` / `expires_in` — `[ "$interval" -ge 1 ]` fails, so a silent
#     default replaces the server's value;
#   * `error` — every `case` arm misses, so an authorization that is merely
#     PENDING falls through to `*)` and the poll aborts with "token error";
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
# Confirmed against the UNFIXED script (2026-09-29): seven of the eight
# assertions fail. The `error` read trips FIRST — the pending arm misses, so the
# script exits 7 after a single poll and the token never reaches the gh stub at
# all, which is why the two token facts come back empty rather than HAS_CR=1.
# The device-code file curl was handed does carry a CR, and the interval falls
# back to 5. So this test can see the defect it is about, at every site.
#
# HERMETIC: a temp $SOT_COMM_HOME and $GH_CONFIG_DIR, stub curl/gh/jq/sleep
# confined to a PATH prefix, and no network.
#
# Usage: comm/core/tests/test-crlf-gh-auth.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GH_AUTH="$SCRIPT_DIR/../scripts/sot-gh-auth.sh"
REAL_JQ="$(command -v jq)"
FAKE_TOKEN='FAKE-TOKEN-not-a-real-credential'
PASS=0; FAIL=0

ok()   { PASS=$((PASS+1)); printf '  ok   %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  FAIL %s\n' "$1"; }
check() { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (expected '$3', got '$2')"; fi; }

# One hermetic world per case: stubs on PATH, a pending device-code state file.
setup() {
    TMP="$(mktemp -d "${TMPDIR:-/tmp}/sot-gh-crlf.XXXXXX")"
    STUB="$TMP/stub"; mkdir -p "$STUB" "$TMP/comm" "$TMP/gh"

    # jq: the real one, with every \n rewritten to \r\n. Streamed, and the real
    # exit status preserved so a failing filter still fails.
    cat > "$STUB/jq" <<JQ
#!/usr/bin/env bash
"$REAL_JQ" "\$@" | sed \$'s/\$/\r/'
exit "\${PIPESTATUS[0]}"
JQ

    # curl: no network. Call 1 answers "pending", call 2 hands over the token.
    # It also records whether the device_code FILE it was given holds a CR,
    # which is what proves the write-time strip.
    cat > "$STUB/curl" <<'CURL'
#!/usr/bin/env bash
n=$(( $(cat "$SOT_TEST_TMP/curl.count" 2>/dev/null || echo 0) + 1 ))
printf '%s' "$n" > "$SOT_TEST_TMP/curl.count"
for a in "$@"; do
    case "$a" in
        device_code@*)
            f="${a#device_code@}"
            if LC_ALL=C grep -q $'\r' "$f"; then echo "1" > "$SOT_TEST_TMP/dc.hascr"
            else echo "0" > "$SOT_TEST_TMP/dc.hascr"; fi ;;
    esac
done
if [ "$n" = 1 ]; then printf '{"error":"authorization_pending"}'
else printf '{"access_token":"%s","token_type":"bearer","scope":"repo"}' "$SOT_TEST_FAKE_TOKEN"; fi
CURL

    # gh: records only two facts about the bytes on stdin — never the value.
    cat > "$STUB/gh" <<'GH'
#!/usr/bin/env bash
case "$*" in
    *"--with-token"*)
        bytes=$(cat)
        if printf '%s' "$bytes" | LC_ALL=C grep -q $'\r'; then cr=1; else cr=0; fi
        printf 'HAS_CR=%s\nLEN=%s\n' "$cr" "$(printf '%s' "$bytes" | wc -c | tr -d ' ')" \
            > "$SOT_TEST_TMP/gh.token-facts"
        exit 0 ;;
    *"auth status"*)  echo "stub: logged in"; exit 0 ;;
    *"setup-git"*)    exit 0 ;;
    *"api user"*)     echo "tester"; exit 0 ;;
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
    printf '%s' '{"device_code":"DC-FAKE-0000","user_code":"ABCD-1234","verification_uri":"https://example.invalid/device","interval":1,"expires_in":900}' \
        > "$TMP/comm/gh-device-auth.json"
}

run_poll() {
    ( unset GH_TOKEN GITHUB_TOKEN
      export PATH="$STUB:$PATH" SOT_COMM_HOME="$TMP/comm" GH_CONFIG_DIR="$TMP/gh" \
             SOT_TEST_TMP="$TMP" SOT_TEST_FAKE_TOKEN="$FAKE_TOKEN"
      bash "$GH_AUTH" poll >"$TMP/out" 2>"$TMP/err" )
    echo $?
}

echo "test-crlf-gh-auth.sh"

# ---------------------------------------------------------------- case 1 ------
# The token reaches gh with no carriage return, and at its full length. Length
# is asserted too: a strip that ate a real byte would pass a CR check alone.
setup
rc="$(run_poll)"
check "poll exits 0 under a CRLF jq" "$rc" "0"
facts="$(cat "$TMP/gh.token-facts" 2>/dev/null || echo MISSING)"
check "the token reached gh with no CR" "$(printf '%s' "$facts" | sed -n 's/^HAS_CR=//p')" "0"
check "and with every byte intact" "$(printf '%s' "$facts" | sed -n 's/^LEN=//p')" \
      "$(printf '%s' "$FAKE_TOKEN" | wc -c | tr -d ' ')"
check "the device code curl was given had no CR" "$(cat "$TMP/dc.hascr" 2>/dev/null)" "0"
if LC_ALL=C grep -qF "$FAKE_TOKEN" "$TMP/out" "$TMP/err"; then
    bad "the script printed the token"
else
    ok "the script never printed the token"
fi
rm -rf "$TMP"

# ---------------------------------------------------------------- case 2 ------
# A pending authorization must keep waiting, not abort. Under a CRLF jq the
# `error` value carried a CR, every case arm missed, and the poll died on the
# first response with "token error". Two curl calls is the proof it waited.
setup
rc="$(run_poll)"
check "a pending authorization does not abort the poll" "$rc" "0"
check "it polled again after 'authorization_pending'" "$(cat "$TMP/curl.count" 2>/dev/null)" "2"
# ---------------------------------------------------------------- case 3 ------
# The interval is the server's 1, not the 5 the numeric test falls back to when
# the value it reads ends in a carriage return.
check "the server's interval survived the read" "$(cat "$TMP/sleep.first" 2>/dev/null)" "1"
rm -rf "$TMP"

printf 'PASS=%s FAIL=%s\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
