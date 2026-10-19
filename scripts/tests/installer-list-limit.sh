# installer-list-limit.sh -- the prelude's release listing under GitHub's anonymous rate limit. Sourced by
# installer-entry.sh inside entry_takes_the_installed_channel (its $d fixture and prelude helper), never run. The curl
# stand-in answers the status in $CTL/api-code (default 200), and 403 when $CTL/api-needs-token is set and the request
# carried no Authorization header.

# The listing under GitHub's anonymous rate limit: the token goes with the request only when one is set, and a 403 or 429
# says what it is and what to do.
entry_record "$d" 0.6.6-rc9.13 1 0 none
prelude --be-only
check "no token: the listing request carries no Authorization header" "0" "$(count_in "$d/ctl/net.log" 'Authorization')"
: > "$d/ctl/api-needs-token"
prelude --be-only
check "no token against a listing that wants one: the FAIL names the refusal, the retry and --version" \
    "FAIL: target: could not list the releases of kalidke/ship-of-tools: GitHub refused the anonymous request (HTTP 403, its rate limit for this network address); retry in an hour, set GITHUB_TOKEN to send a token, or name the release with --version <tag>, which needs no listing" "$ENTRY_LAST"
check "that FAIL exits 1" "1" "$ENTRY_RC"
prelude GITHUB_TOKEN=tok-test-1 --be-only
check "GITHUB_TOKEN is sent as a bearer header" "tag=v9.9.10-rc1 implicit=1 args=--be-only" "$(cat "$d/ctl/prelude.log")"
check "the header is on the one listing request" "1" "$(count_in "$d/ctl/net.log" 'Authorization: Bearer tok-test-1')"
prelude GH_TOKEN=tok-test-2 --be-only
check "GH_TOKEN is sent when GITHUB_TOKEN is not set" "1" "$(count_in "$d/ctl/net.log" 'Authorization: Bearer tok-test-2')"
rm -f "$d/ctl/api-needs-token"; echo 429 > "$d/ctl/api-code"
prelude GITHUB_TOKEN=tok-test-1 --be-only
check "a 429 with a token says the token did not help" "FAIL: target: could not list the releases of kalidke/ship-of-tools: GitHub refused the request (HTTP 429, its rate limit) even with the token in GITHUB_TOKEN or GH_TOKEN; retry in an hour, or name the release with --version <tag>, which needs no listing" "$ENTRY_LAST"
echo 500 > "$d/ctl/api-code"
prelude --be-only
check "any other failure keeps the plain line" "FAIL: target: could not list the releases of kalidke/ship-of-tools" "$ENTRY_LAST"
rm -f "$d/ctl/api-code"
