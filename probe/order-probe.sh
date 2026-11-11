#!/usr/bin/env bash
# order-probe.sh OUT HOMEDIR -- the daemons-before-windows commands of docs/UPDATE-AGENT.md step 2 and step 5 on macOS: the
# blocks are probe/daemons-1.sh, probe/daemons-2.sh and probe/step5-1.sh, extracted byte for byte. A computer that runs
# 0.6.6 (installed by that tag's own installer, in a scratch home) is a hub that declares itself and one far computer;
# its 0.6.6 daemon runs, started by hand. Then: the release read, the plan and the status as the runbook runs them, and
# the same status by the release's own sotd (0.7.0, unpacked from its archive and never installed), which must read the
# 0.6.6 daemon as a protocol mismatch naming 0.6.6. Bash 3.2.
set -u
OUT="$1" H="$2"
P="$(cd "$(dirname "$0")" && pwd)"
REPO=kalidke/ship-of-tools
mkdir -p "$OUT" "$H" || exit 2
[ -z "$(ls -A "$H")" ] || { echo "probe: $H is not empty" >&2; exit 2; }
OUT="$(cd "$OUT" && pwd)"; H="$(cd "$H" && pwd)"
for v in $(compgen -v SOT_); do unset "$v"; done
unset XDG_CONFIG_HOME XDG_STATE_HOME XDG_DATA_HOME XDG_CACHE_HOME JULIA_LOAD_PATH JULIA_PROJECT CLAUDE_CONFIG_DIR CODEX_HOME GITHUB_TOKEN GH_TOKEN
export HOME="$H"
PREFIX="$HOME/.local/share/sot"
S="$OUT/summary.txt"; : > "$S"
ok=1
say() { printf '%s\n' "$*" | tee -a "$S"; }
bad() { say "FAILED: $*"; ok=0; }
mkdir -p "$HOME/.config/sot" "$HOME/.local/bin" "$HOME/neutral"
printf '# synthetic probe policy\nprobe-denied-publication\n' > "$HOME/.config/sot/publish-list.txt"
printf '#!/bin/sh\nexit 0\n' > "$HOME/.local/bin/claude"; chmod +x "$HOME/.local/bin/claude"
for f in daemons-1.sh daemons-2.sh step5-1.sh; do say "$f sha256: $(shasum -a 256 "$P/$f" | cut -c1-16)"; done
curl -fsSL -o "$OUT/start-install.sh" "https://raw.githubusercontent.com/$REPO/v0.6.6/scripts/install.sh" || exit 2
case "$(uname -s)" in Darwin) ROLE="--local"; ASSET="sot-0.7.0-macos-aarch64.tar.gz" ;; *) ROLE="--local --no-service"; ASSET="sot-0.7.0-linux-x86_64.tar.gz" ;; esac
# shellcheck disable=SC2086
( cd "$HOME/neutral" && SOT_INSTALL_TAG=v0.6.6 SOT_INSTALL_IMPLICIT=1 bash "$OUT/start-install.sh" $ROLE ) > "$OUT/start.out" 2>&1 < /dev/null
say "start: $(grep -v '^[[:space:]]*$' "$OUT/start.out" | tail -1)"
say "start sotd --version: $("$PREFIX/bin/sotd" --version 2>&1 | head -1)"
# the group: this computer is its hub and declares itself and one far computer
export SOT_SELF_HOST=probehub SOT_HOSTS="$HOME/hosts.toml"
printf 'hub = "probehub"\n[host.probehub]\ndaemon = true\n[host.probefar]\ndaemon = true\n' > "$SOT_HOSTS"
( cd "$HOME"; nohup "$PREFIX/bin/sotd" --project-root "$HOME" --label sot > "$OUT/daemon.log" 2>&1 < /dev/null & echo $! > "$OUT/daemon.pid" )
sock="$("$PREFIX/bin/sotd" session-socket-path sot)"
for _ in $(seq 1 60); do [ -S "$sock" ] && break; sleep 0.5; done
say "daemon socket: $([ -S "$sock" ] && echo present || echo MISSING)"
# the runbook's blocks
tagline="$(cd "$HOME" && bash "$P/daemons-1.sh" 2>&1 | tail -1)"
say "release read: $tagline"
case "$tagline" in */releases/tag/v[0-9]*) say "release read: ok" ;; *) bad "release read" ;; esac
( cd "$HOME" && bash "$P/daemons-2.sh" ) > "$OUT/step2.out" 2>&1; say "--- step 2 plan and status (0.6.6's sotd)"; cat "$OUT/step2.out" | tee -a "$S"
grep -q '^self probehub$' "$OUT/step2.out" && grep -q '^hub probehub$' "$OUT/step2.out" || bad "plan has no self/hub line"
grep -Eq '^probehub hub,daemon 0\.6\.6 up \(host: probehub\)' "$OUT/step2.out" || bad "the self row does not read 0.6.6 up"
grep -Eq '^probefar daemon unreachable: ' "$OUT/step2.out" || bad "the far row does not read unreachable"
# the release's own sotd reading the same daemon
asset="$ASSET"
mkdir -p "$OUT/b070"
curl -fsSL -o "$OUT/b070/$asset" "https://github.com/$REPO/releases/download/v0.7.0/$asset" && tar -xzf "$OUT/b070/$asset" -C "$OUT/b070"
b="$(find "$OUT/b070" -name sotd -type f | head -1)"
say "0.7.0's sotd: $("$b" --version 2>&1 | head -1)"
"$b" status > "$OUT/status070.out" 2>&1; say "--- 0.7.0's sotd status against the 0.6.6 daemon"; cat "$OUT/status070.out" | tee -a "$S"
grep -Eq '^probehub hub,daemon unreachable: .*protocol mismatch: backend 0\.6\.6 \(protocol 3\) vs frontend 0\.7\.0 \(protocol 4\)' "$OUT/status070.out" || bad "0.7.0 does not read the 0.6.6 daemon as a mismatch naming 0.6.6"
# step 5's block (the same status)
( cd "$HOME" && bash "$P/step5-1.sh" ) > "$OUT/step5.out" 2>&1; say "--- step 5's status"; cat "$OUT/step5.out" | tee -a "$S"
kill "$(cat "$OUT/daemon.pid")" 2>/dev/null
if [ "$ok" = 1 ]; then say "RESULT PASS"; exit 0; else say "RESULT FAIL"; exit 1; fi
