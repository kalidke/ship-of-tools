#!/usr/bin/env bash
# update-probe.sh START ENTRY MODE OUT HOMEDIR -- one case of the update runbook, in a scratch home (Linux and macOS).
#
#   START    the tag the computer runs first, installed by that tag's own installer
#   ENTRY    main: the runbook's command (main's install.sh fetched from GitHub, GITHUB_TOKEN and GH_TOKEN cleared)
#            installed: the installed checkout's own entry, bash $PREFIX/repo/current/scripts/install.sh
#   MODE     plain (the start unpinned, as a computer that followed its channel), pinned (the start named by --version),
#            token (as plain, and a dummy GITHUB_TOKEN reaches the entry: ENTRY=installed keeps it, main clears it)
#   OUT      a folder for the logs and summary.txt
#   HOMEDIR  a fresh, empty folder that becomes $HOME
# Nothing of the caller's reaches the run: SOT_*, XDG_*, CLAUDE_CONFIG_DIR, CODEX_HOME and the tokens are unset.
# Bash 3.2.
set -u
START="$1" ENTRY="$2" MODE="$3" OUT="$4" H="$5"
REPO=kalidke/ship-of-tools
mkdir -p "$OUT" "$H" || exit 2
[ -z "$(ls -A "$H")" ] || { echo "probe: $H is not empty" >&2; exit 2; }
OUT="$(cd "$OUT" && pwd)"; H="$(cd "$H" && pwd)"
for v in $(compgen -v SOT_); do unset "$v"; done
unset XDG_CONFIG_HOME XDG_STATE_HOME XDG_DATA_HOME XDG_CACHE_HOME JULIA_LOAD_PATH JULIA_PROJECT CLAUDE_CONFIG_DIR CODEX_HOME \
    GITHUB_TOKEN GH_TOKEN
export HOME="$H"
PREFIX="$HOME/.local/share/sot"
S="$OUT/summary.txt"
: > "$S"
say() { printf '%s\n' "$*" | tee -a "$S"; }
case "$(uname -s)" in
    Linux) ROLE="--local --no-service" ;;
    Darwin) ROLE="--local" ;;
    *) say "no case for $(uname -s)"; exit 2 ;;
esac

mkdir -p "$HOME/.config/sot" "$HOME/.local/bin" "$HOME/work"
printf '# synthetic probe policy\nprobe-denied-publication\n' > "$HOME/.config/sot/publish-list.txt"
printf '#!/bin/sh\nexit 0\n' > "$HOME/.local/bin/claude"
chmod +x "$HOME/.local/bin/claude"

field() { [ -f "$PREFIX/install.json" ] && sed -n -E "s/^ *\"$1\": *\"?([^\",]*)\"?,?$/\1/p" "$PREFIX/install.json" | head -1; }
state() {  # <label>
    local cur=""
    [ -e "$PREFIX/repo/current" ] && cur="$(cd "$PREFIX/repo/current" 2>/dev/null && pwd -P)"
    say "$1 install.json: version=$(field version) tag=$(field tag) pinned=$(field pinned) commit=$(field commit | cut -c1-9)"
    say "$1 sotd --version: $("$PREFIX/bin/sotd" --version 2>&1 | head -1)"
    say "$1 repo/current: ${cur#$PREFIX/} ($(git -C "$PREFIX/repo/current" describe --tags 2>/dev/null))"
    say "$1 comm VERSION: $(head -1 "$HOME/.sot-comm/VERSION" 2>/dev/null)"
}
fingerprint() {  # a digest of what an update changes: the record, the binaries, the checkout pointer
    {
        cat "$PREFIX/install.json" 2>/dev/null
        for f in "$PREFIX"/bin/*; do [ -f "$f" ] && cksum < "$f"; done
        cd "$PREFIX/repo/current" 2>/dev/null && pwd -P
        ls "$PREFIX/repo/versions" 2>/dev/null
    } | cksum
}
# run <label> <command...>: output to $OUT/<label>.out; RC and LAST (the last non-empty line).
run() {
    local label="$1"; shift
    say "--- $label: $*"
    local t0=$(date +%s)
    RC=0
    ( cd "$HOME/work" && "$@" ) > "$OUT/$label.out" 2>&1 < /dev/null || RC=$?
    LAST="$(grep -v '^[[:space:]]*$' "$OUT/$label.out" | tail -1)"
    say "$label: exit $RC after $(( $(date +%s) - t0 )) s; last line: $LAST"
}

# ---- the start ---------------------------------------------------------------------------------------------------
curl -fsSL -o "$OUT/start-install.sh" "https://raw.githubusercontent.com/$REPO/$START/scripts/install.sh" \
    || { say "could not fetch $START's install.sh"; exit 2; }
if [ "$MODE" = pinned ]; then
    # shellcheck disable=SC2086
    run start bash "$OUT/start-install.sh" --version "$START" $ROLE
else
    # shellcheck disable=SC2086
    run start env SOT_INSTALL_TAG="$START" SOT_INSTALL_IMPLICIT=1 bash "$OUT/start-install.sh" $ROLE
fi
state start
[ -f "$PREFIX/install.json" ] || { say "RESULT start-failed"; exit 1; }

# ---- the update, as the runbook says ------------------------------------------------------------------------------
entry_main() {
    curl -fsSL https://raw.githubusercontent.com/kalidke/ship-of-tools/main/scripts/install.sh | env -u GITHUB_TOKEN -u GH_TOKEN bash
}
entry_installed() { bash "$PREFIX/repo/current/scripts/install.sh"; }
[ "$MODE" != token ] || export GITHUB_TOKEN=probe-dummy-not-a-token
run update "entry_$ENTRY"
UPDATE_LAST="$LAST"
state update
# ---- the same command again: the computer is current, so a plain PASS that changes nothing ------------------------
before="$(fingerprint)"
run again "entry_$ENTRY"
AGAIN_LAST="$LAST"
after="$(fingerprint)"
state again
if [ "$before" = "$after" ]; then say "again: install.json, bin/*, repo/current and repo/versions unchanged"
else say "again: CHANGED install.json, bin/*, repo/current or repo/versions"; fi
case "$UPDATE_LAST" in "PASS: Ship of Tools 0.7.0 at "*) u=PASS ;; *) u="NOT-PASS" ;; esac
case "$AGAIN_LAST" in "PASS: Ship of Tools 0.7.0 at "*) a=PASS ;; *) a="NOT-PASS" ;; esac
say "RESULT update=$u again=$a"
