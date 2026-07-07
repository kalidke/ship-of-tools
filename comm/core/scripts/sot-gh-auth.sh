#!/usr/bin/env bash
# sot-gh-auth.sh — headless-friendly GitHub CLI (gh) auth via the OAuth device
# flow, WITHOUT gh's browser auto-launch.
#
# Why this exists: on a headless box `gh auth login` runs `xdg-open` which spawns
# a text `www-browser`. That browser wedges on a cookie prompt, hides the
# one-time code, and blocks the terminal — so the login never completes and the
# expired token lingers in hosts.yml. The device flow needs NO local browser at
# all: you enter a short code in ANY browser (e.g. your Windows one) and gh on
# this box completes by polling. This script drives that flow directly (the same
# flow gh uses internally), so there is nothing to wedge.
#
#   1. request a device code from GitHub for gh's public OAuth client,
#   2. print the short user code + https://github.com/login/device,
#   3. poll GitHub until you authorize in any browser,
#   4. hand the resulting token to gh (`gh auth login --with-token`) + setup-git.
#
# Storage: `--insecure-storage` FORCES the token into ~/.config/gh/hosts.yml
# (plaintext, 0600) instead of an OS keyring. That is deliberate — on the
# shared-home multi-host setup a hosts.yml token
# covers every Linux node; a keyring token would not. (The Windows FE has its own
# separate gh auth — different HOME.) Revoke with `gh auth logout` + revoking
# "GitHub CLI" at https://github.com/settings/connections.
#
# Secrets hygiene: the access token is piped to gh via STDIN (never argv); the
# device_code is passed to curl via a 0600 temp file (never argv → never in
# /proc/PID/cmdline). No token/code is ever echoed. State + temp file are removed
# by an EXIT/INT/TERM trap in the polling path.
#
# Usage:
#   sot-gh-auth.sh            full flow: request a code, print it, poll to done
#   sot-gh-auth.sh request    request+print a code, save state, exit immediately
#   sot-gh-auth.sh poll       poll the saved request through to completion
#   sot-gh-auth.sh status     show gh auth status (exit 0 iff a valid live token)
#
# The request/poll split is for a controller (e.g. a Claude session) that must
# surface the code to a human BEFORE blocking on the poll. Humans running it in a
# terminal can just use the no-arg full flow.
#
# Env overrides:
#   GH_OAUTH_CLIENT_ID  OAuth client_id (default: gh's public github.com id)
#   SOT_GH_SCOPES       requested scopes (default: repo read:org gist workflow)
#   GH_HOST             host (default github.com)
set -euo pipefail

usage() { sed -n '2,45p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

# gh CLI's public github.com OAuth app client_id (device flow enabled). Public,
# not a secret — it identifies the app ("GitHub CLI" on the consent screen), not
# the user. Overridable for GHE.
CLIENT_ID="${GH_OAUTH_CLIENT_ID:-178c6fc778ccc68e1d6a}"
SCOPES="${SOT_GH_SCOPES:-repo read:org gist workflow}"
HOST="${GH_HOST:-github.com}"
STATE="${SOT_COMM_HOME:-$HOME/.sot-comm}/gh-device-auth.json"
GH_CFG_DIR="${GH_CONFIG_DIR:-$HOME/.config/gh}"

need() { command -v "$1" >/dev/null 2>&1 || { echo "sot-gh-auth: missing required tool: $1" >&2; exit 3; }; }

# exit 0 iff gh has a valid LIVE token for HOST (gh auth status hits the API).
already_authed() { gh auth status --hostname "$HOST" >/dev/null 2>&1; }

# gh honors GH_TOKEN/GITHUB_TOKEN over hosts.yml — if one is set, re-auth is a
# no-op (that env token is what's in effect, and gh auth status would falsely
# pass). Refuse loudly rather than silently skip repairing hosts.yml.
guard_env() {
    if [ -n "${GH_TOKEN:-}${GITHUB_TOKEN:-}" ]; then
        echo "sot-gh-auth: GH_TOKEN/GITHUB_TOKEN is set — gh uses THAT and ignores hosts.yml." >&2
        echo "  unset it first:  unset GH_TOKEN GITHUB_TOKEN" >&2
        exit 4
    fi
}

request() {
    need gh; need curl; need jq
    local resp err device_code user_code verify interval expires
    resp=$(curl -fsS -X POST "https://${HOST}/login/device/code" \
        -H "Accept: application/json" \
        --data-urlencode "client_id=${CLIENT_ID}" \
        --data-urlencode "scope=${SCOPES}") \
        || { echo "sot-gh-auth: device-code request failed (network? host '${HOST}'?)" >&2; exit 5; }
    err=$(printf '%s' "$resp" | jq -r '.error // empty')
    if [ -n "$err" ]; then
        echo "sot-gh-auth: GitHub rejected the device-code request: $err — $(printf '%s' "$resp" | jq -r '.error_description // ""')" >&2
        exit 5
    fi
    device_code=$(printf '%s' "$resp" | jq -r '.device_code')
    user_code=$(printf '%s' "$resp" | jq -r '.user_code')
    verify=$(printf '%s' "$resp" | jq -r '.verification_uri')
    interval=$(printf '%s' "$resp" | jq -r '.interval')
    expires=$(printf '%s' "$resp" | jq -r '.expires_in')
    # Persist for a later `poll`. The device_code is sensitive during its ~15min
    # window (it exchanges for the token once you authorize) — write it 0600.
    # NOTE: no cleanup trap here on purpose — the split flow needs this to survive
    # until `poll` consumes it (poll owns the trap).
    mkdir -p "$(dirname "$STATE")"
    ( umask 077; printf '%s\n' "$resp" > "$STATE" )
    # Machine-parseable lines FIRST (a controller greps these), then a human block.
    echo "SOT_GH_USER_CODE=${user_code}"
    echo "SOT_GH_VERIFY_URL=${verify}"
    echo "SOT_GH_EXPIRES_IN=${expires}"
    echo
    echo "════════════════════════════════════════════════"
    echo "  gh auth — enter this code in ANY browser"
    echo "  1. open:   ${verify}"
    echo "  2. code:   ${user_code}"
    echo "  (e.g. your Windows browser — expires in ~$((expires / 60)) min)"
    echo "════════════════════════════════════════════════"
}

finish() {
    local access="$1" who
    # STDIN, never argv. --insecure-storage forces hosts.yml (shared-HOME cluster
    # coverage) instead of an OS keyring.
    printf '%s' "$access" | gh auth login --hostname "$HOST" --git-protocol https \
        --with-token --insecure-storage \
        || { echo "sot-gh-auth: gh rejected the token" >&2; exit 8; }
    gh auth setup-git --hostname "$HOST" 2>/dev/null || true
    chmod 600 "${GH_CFG_DIR}/hosts.yml" 2>/dev/null || true
    who=$(gh api user -q .login 2>/dev/null || echo '?')
    echo "sot-gh-auth: ✓ authenticated as ${who} on ${HOST}."
    gh auth status --hostname "$HOST" 2>&1 || true   # confirm account + scopes
    echo "  token stored 0600 in ${GH_CFG_DIR}/hosts.yml — shared Linux HOME covers the NFS"
    echo "  shared-home multi-host setup; the Windows FE auths separately."
    echo "  revoke: gh auth logout -h ${HOST}  +  revoke 'GitHub CLI' at https://github.com/settings/connections"
}

poll() {
    need gh; need curl; need jq
    [ -f "$STATE" ] || { echo "sot-gh-auth: no pending request — run 'sot-gh-auth.sh request' first" >&2; exit 6; }
    local device_code interval expires waited=0 tok access err dcfile
    device_code=$(jq -r '.device_code' "$STATE")
    interval=$(jq -r '.interval' "$STATE"); [ "$interval" -ge 1 ] 2>/dev/null || interval=5
    expires=$(jq -r '.expires_in' "$STATE"); [ "$expires" -ge 1 ] 2>/dev/null || expires=899
    # device_code -> 0600 temp file so it is passed to curl via --data-urlencode
    # name@file (off argv, never in /proc/PID/cmdline). Trap removes both the temp
    # and the state file on ANY exit (success, error, or abort) — poll owns them.
    dcfile=$(mktemp "${TMPDIR:-/tmp}/sot-gh-dc.XXXXXX")
    chmod 600 "$dcfile"
    printf '%s' "$device_code" > "$dcfile"
    trap 'rm -f "$STATE" "$dcfile"' EXIT INT TERM
    echo "sot-gh-auth: waiting for you to authorize… (polling every ${interval}s, code TTL ~$((expires / 60))min)"
    while :; do
        sleep "$interval"
        waited=$((waited + interval))
        tok=$(curl -fsS -X POST "https://${HOST}/login/oauth/access_token" \
            -H "Accept: application/json" \
            --data-urlencode "device_code@${dcfile}" \
            --data-urlencode "client_id=${CLIENT_ID}" \
            --data-urlencode "grant_type=urn:ietf:params:oauth:grant-type:device_code") \
            || { echo "sot-gh-auth: token-poll network error; retrying" >&2; continue; }
        access=$(printf '%s' "$tok" | jq -r '.access_token // empty')
        err=$(printf '%s' "$tok" | jq -r '.error // empty')
        if [ -n "$access" ]; then finish "$access"; return 0; fi
        case "$err" in
            authorization_pending) : ;;                       # not yet — keep waiting
            slow_down)             interval=$((interval + 5)) ;;  # GitHub asked us to back off
            expired_token) echo "sot-gh-auth: the code expired before you authorized — re-run for a fresh one." >&2; exit 7 ;;
            access_denied) echo "sot-gh-auth: authorization was denied." >&2; exit 7 ;;
            "")            echo "sot-gh-auth: unexpected empty token response; retrying" >&2 ;;
            *)             echo "sot-gh-auth: token error: $err — $(printf '%s' "$tok" | jq -r '.error_description // ""')" >&2; exit 7 ;;
        esac
        if [ "$waited" -ge "$expires" ]; then
            echo "sot-gh-auth: timed out after ${waited}s (code TTL ~${expires}s) — re-run for a fresh code." >&2
            exit 7
        fi
    done
}

case "${1:-full}" in
    -h|--help) usage; exit 0 ;;
    status)    need gh; gh auth status --hostname "$HOST"; exit $? ;;
    request)
        guard_env
        if already_authed; then echo "sot-gh-auth: already authenticated on ${HOST} — nothing to do (run 'gh auth logout' to force re-auth)."; exit 0; fi
        request ;;
    poll)      guard_env; poll ;;
    full)
        guard_env
        if already_authed; then echo "sot-gh-auth: already authenticated on ${HOST}."; gh auth status --hostname "$HOST"; exit 0; fi
        request; echo; poll ;;
    *) echo "sot-gh-auth: unknown command '$1' (see --help)" >&2; exit 2 ;;
esac
