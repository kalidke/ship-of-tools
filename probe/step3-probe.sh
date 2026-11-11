#!/usr/bin/env bash
# step3-probe.sh START OUT HOMEDIR -- the runbook's step 3 command for Linux and macOS, verbatim from
# docs/UPDATE-AGENT.md (probe/step3.sh, extracted by extract.py), on a computer that runs START, in a scratch home.
# The start is installed by that tag's own installer, unpinned, from a folder outside any checkout. On Linux, when an
# unprivileged network namespace can be made, the command runs once with no network first and must print
# `STOP: cannot reach GitHub` and change nothing. Then it runs twice from inside a clone of the repository with a dummy
# GITHUB_TOKEN exported (the command clears no token), as an agent runs it. Exit 0 only when both runs end PASS at the
# newest release, the second changes nothing, neither names a caller clone, ~/sot-update.log ends with the same line,
# and the caller's folder and environment are as they were. Bash 3.2.
set -u
START="$1" OUT="$2" H="$3"
STEP3="$(cd "$(dirname "$0")" && pwd)/step3.sh"
CLONE="$(cd "$(dirname "$0")/.." && pwd)"
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
case "$(uname -s)" in Linux) ROLE="--local --no-service" ;; *) ROLE="--local" ;; esac
mkdir -p "$HOME/.config/sot" "$HOME/.local/bin" "$HOME/neutral"
printf '# synthetic probe policy\nprobe-denied-publication\n' > "$HOME/.config/sot/publish-list.txt"
printf '#!/bin/sh\nexit 0\n' > "$HOME/.local/bin/claude"; chmod +x "$HOME/.local/bin/claude"
say "step3.sh sha256: $( (shasum -a 256 "$STEP3" 2>/dev/null || sha256sum "$STEP3") | cut -c1-16)"
field() { sed -n -E "s/^ *\"$1\": *\"?([^\",]*)\"?,?$/\1/p" "$PREFIX/install.json" 2>/dev/null | head -1; }
state() {
    say "$1 install.json: version=$(field version) tag=$(field tag) pinned=$(field pinned) commit=$(field commit | cut -c1-9)"
    say "$1 sotd --version: $("$PREFIX/bin/sotd" --version 2>&1 | head -1)"
    say "$1 repo/current: $(cd "$PREFIX/repo/current" 2>/dev/null && pwd -P | sed "s|^$PREFIX/||") ($(git -C "$PREFIX/repo/current" describe --tags 2>/dev/null))"
}
fingerprint() { { cat "$PREFIX/install.json"; for f in "$PREFIX"/bin/*; do [ -f "$f" ] && cksum < "$f"; done; cd "$PREFIX/repo/current" && pwd -P; ls "$PREFIX/repo/versions"; } | cksum; }
clone_state() { { git -C "$CLONE" status --porcelain; git -C "$CLONE" config --get core.hooksPath; ls -a "$(git -C "$CLONE" rev-parse --git-common-dir)/hooks"; } | cksum; }
run() {  # <label> <folder> <command...>
    local label="$1" dir="$2" t0; shift 2
    t0=$(date +%s)
    RC=0
    ( cd "$dir" && "$@" ) > "$OUT/$label.out" 2>&1 < /dev/null || RC=$?
    cat "$OUT/$label.out"
    LAST="$(grep -v '^[[:space:]]*$' "$OUT/$label.out" | tail -1)"
    say "$label: exit $RC after $(( $(date +%s) - t0 )) s, run from $dir; last line: $LAST"
}
curl -fsSL -o "$OUT/start-install.sh" "https://raw.githubusercontent.com/$REPO/$START/scripts/install.sh" || exit 2
# shellcheck disable=SC2086
run start "$HOME/neutral" env SOT_INSTALL_TAG="$START" SOT_INSTALL_IMPLICIT=1 bash "$OUT/start-install.sh" $ROLE
state start
ok=1
clone_before="$(clone_state)"
if [ "$(uname -s)" = Linux ] && unshare -rn true 2>/dev/null; then
    before="$(fingerprint)"
    run offline "$CLONE" unshare -rn env HOME="$HOME" bash "$STEP3"
    case "$LAST" in "STOP: cannot reach GitHub") say "offline: STOP as written" ;; *) say "offline: NOT the STOP line"; ok=0 ;; esac
    [ "$before" = "$(fingerprint)" ] && say "offline: nothing changed" || { say "offline: CHANGED"; ok=0; }
else
    say "offline: not run here (no unprivileged network namespace)"
fi
export GITHUB_TOKEN=probe-dummy-not-a-token
# The first run is sourced into a shell, as an agent's shell tool runs pasted lines; that shell's folder afterwards is
# the caller's.
run update "$CLONE" bash -c '. "$1"; pwd > "$2"; echo "SOT_INSTALL_TAG=${SOT_INSTALL_TAG:-}" >> "$2"' _ "$STEP3" "$OUT/caller-after"
U="$LAST"
U_LOG="$(grep -v '^[[:space:]]*$' "$HOME/sot-update.log" 2>/dev/null | tail -1)"
state update
before="$(fingerprint)"
run again "$CLONE" bash "$STEP3"
A="$LAST"
A_LOG="$(grep -v '^[[:space:]]*$' "$HOME/sot-update.log" 2>/dev/null | tail -1)"
after="$(fingerprint)"
state again
for p in "update|$U|$U_LOG" "again|$A|$A_LOG"; do
    l="${p%%|*}" rest="${p#*|}"; v="${rest%%|*}" lg="${rest#*|}"
    case "$v" in "PASS: Ship of Tools 0.7.0 at "*) ;; *) say "$l: NOT a PASS at 0.7.0"; ok=0 ;; esac
    case "$v" in *"checked caller"*) say "$l: the PASS line names a caller clone"; ok=0 ;; *) say "$l: no caller clone named" ;; esac
    say "$l: $(grep -m1 'caller clone enrolled' "$OUT/$l.out" | sed 's/^[= ]*//')"
    [ "$lg" = "$v" ] && say "$l: ~/sot-update.log ends with the same line" || { say "$l: ~/sot-update.log ends '$lg'"; ok=0; }
done
if [ "$before" = "$after" ]; then say "again: install.json, bin/*, repo/current and repo/versions unchanged"; else say "again: CHANGED"; ok=0; fi
# Step 2's daemon check, as the runbook writes it (no daemon runs here, so it must print why it could not tell).
sv="$(~/.local/share/sot/bin/sotd same-version --endpoint "unix:$(~/.local/share/sot/bin/sotd session-socket-path sot)" 2>&1)"; svrc=$?
say "step 2's same-version, no daemon running: exit $svrc: $sv"
[ -n "$sv" ] || { say "same-version printed nothing with no daemon"; ok=0; }
[ "$clone_before" = "$(clone_state)" ] && say "the clone it ran from: status, hooksPath and hooks unchanged" || { say "the clone it ran from: CHANGED"; ok=0; }
if [ "$(sed -n 1p "$OUT/caller-after")" = "$CLONE" ] && [ "$(sed -n 2p "$OUT/caller-after")" = "SOT_INSTALL_TAG=" ]; then
    say "the shell that ran the lines: still in $CLONE, no SOT_INSTALL_TAG"
else
    say "the shell that ran the lines: $(tr '\n' ' ' < "$OUT/caller-after")"; ok=0
fi
if [ "$ok" = 1 ]; then say "RESULT PASS"; exit 0; else say "RESULT FAIL"; exit 1; fi
