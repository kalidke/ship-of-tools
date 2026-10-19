#!/usr/bin/env bash
# installer-julia-step.sh -- install.sh's Julia steps (julia_pick_chan, julia_run): the channel is chosen once, by a probe;
# each step runs once, and its stderr (an installer warning, a failing step's own error) reaches the caller. Sourced by
# installer-wrapper.sh (its check helpers and sandboxed PATH), never run.

# A stand-in julia: logs each call's argv, prints a line on stderr for a real step, fails a step whose argv holds FAILSTEP,
# and refuses a +channel unless CHAN_OK=1 (a box whose juliaup lacks the channel).
mk_julia_step() {  # <dir>
    mkdir -p "$1"
    cat > "$1/julia" <<'JL'
#!/bin/sh
printf 'julia %s\n' "$*" >> "$STUB_LOG"
case "$1" in
    +*) [ "${CHAN_OK:-0}" = 1 ] || { echo "julia: channel not installed" >&2; exit 1; } ;;
esac
case "$*" in
    *-e\ exit\(0\)*) exit 0 ;;
esac
echo "stderr-of-step $*" >&2
case "$*" in *FAILSTEP*) exit 3 ;; esac
exit 0
JL
    chmod +x "$1/julia"
}

d="$WORK/julia-step"; mkdir -p "$d"
mk_julia_step "$d/bin"
export STUB_LOG="$d/log"

for ok in 1 0; do
    : > "$STUB_LOG"
    export CHAN_OK=$ok
    case_start "channel present=$ok: a step runs once, with its stderr shown"
    err="$(PATH="$d/bin:$TOOLS" bash -c 'SOT_INSTALL_SOURCE_ONLY=1; . "$1"; chan=1.12; julia_pick_chan; julia_run --project=x -e "step"' _ "$(dirname "$0")/../install.sh" 2>&1 >/dev/null)" || true
    check "the step's stderr line is shown once" 1 "$(printf '%s\n' "$err" | grep -c 'stderr-of-step')"
    check "the step ran once" 1 "$(grep -c -- '-e step' "$STUB_LOG")"
    if [ "$ok" = 1 ]; then want="julia +1.12 --project=x -e step"; else want="julia --project=x -e step"; fi
    check "the step ran on the chosen julia" "$want" "$(grep -- '-e step' "$STUB_LOG")"
done

case_start "one probe serves every step"
: > "$STUB_LOG"; export CHAN_OK=1
PATH="$d/bin:$TOOLS" bash -c 'SOT_INSTALL_SOURCE_ONLY=1; . "$1"; chan=1.12; julia_pick_chan; julia_run --project=x -e "step"; julia_run --project=y -e "step2"' _ "$(dirname "$0")/../install.sh" >/dev/null 2>&1
check "one probe for two steps" 1 "$(grep -c 'exit(0)' "$STUB_LOG")"
check "both steps ran on the channel" 2 "$(grep -c '^julia +1.12 --project=[xy] -e step' "$STUB_LOG")"

case_start "a failing step shows its own error and runs once"
: > "$STUB_LOG"; export CHAN_OK=1
rc=0
err="$(PATH="$d/bin:$TOOLS" bash -c 'SOT_INSTALL_SOURCE_ONLY=1; . "$1"; chan=1.12; julia_pick_chan; julia_run -e FAILSTEP' _ "$(dirname "$0")/../install.sh" 2>&1 >/dev/null)" || rc=$?
check "exit status is the step's" 3 "$rc"
check "its error is shown once" 1 "$(printf '%s\n' "$err" | grep -c 'stderr-of-step')"
check "it ran once" 1 "$(grep -c FAILSTEP "$STUB_LOG")"
