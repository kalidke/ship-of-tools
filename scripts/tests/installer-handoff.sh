#!/usr/bin/env bash
# installer-handoff.sh -- the apply that hands off to a release's own sot-apply, and what follows an apply on Unix: the
# staged copy that runs for real and applies onto the install, the packer's copy that is executable, and comm that
# follows the applied commit. Sourced by installer-apply.sh (its fixtures, check helpers and sandboxed PATH), never run.

# The staged release's own sot-apply, as the packer puts it there: `cp` carries the source's mode into the archive.
stage_own_apply() {  # <dir> [<mode>]: the fixture's staged release holds this tree's sot-apply.sh
    local s="$1/prefix/updates/v9.9.9-$TGT/sot-v9.9.9-$TGT/sot-apply"
    cp "$SRC/scripts/sot-apply.sh" "$s"
    [ -z "${2:-}" ] || chmod "$2" "$s"
}
# Julia stand-in: one line per `update_comm`, so a test counts the comm refreshes an apply made.
mk_julia() {  # <dir>
    printf '#!/bin/sh\necho "julia $*" >> "%s/julia.log"\n' "$1" > "$1/stubs/julia"; chmod +x "$1/stubs/julia"
    : > "$1/julia.log"
}
run_handoff() {  # <dir> [apply]: run the installed apply (default: the fixture's); sets AP_RC; comm is the fixture's own
    local d="$1" apply="${2:-$1/prefix/bin/sot-apply}"
    rm -rf "${d:?}/prefix/updates/.lock"
    AP_RC=0
    ( HOME="$d/home" SOT_COMM_HOME="$d/ch" PATH="$d/stubs:$TOOLS" STUB_LOG="$d/log" STUB_SOCKET="$d/sot.sock" \
        "$apply" ) > "$d/out" 2>&1 || AP_RC=$?
}
sotd_text() { cat "$1/prefix/bin/sotd"; }
wrapper_kind() { grep -q '^# sot-launch: all-in-one' "$1/home/.local/bin/sot-launch" && echo rendered || echo old; }

case_start "the_staged_apply_applies_onto_the_install_that_re_executed_it"
# A REAL staged copy, never a stand-in: it takes the install prefix from the apply that handed it the pointer, since its
# own path lies under the install's updates folder, not under a prefix of its own.
d="$WORK/ho-real"; mk_apply_fixture "$d" 1 1; stage_own_apply "$d" 0755; mk_julia "$d"
run_handoff "$d"
check "the installed apply re-executes the staged copy once" "1" "$(grep -c 're-executing the staged sot-apply' "$d/out" || true)"
check "the staged copy applies: tag, sotd, pointer" "v9.9.9 new-sotd gone" "$(installed_tag "$d") $(sotd_text "$d") $(armed "$d")"
check "the unit-owning wrapper is re-rendered" "rendered" "$(wrapper_kind "$d")"
check "it exits 0 and says APPLIED once" "0 1" "$AP_RC $(grep -c 'sot-apply: APPLIED v9.9.9' "$d/out" || true)"
case_start "a_packed_copy_made_by_cp_hands_off_the_same_way"
# The packer copies scripts/sot-apply.sh with `cp`, so the staged copy's mode is the source file's.
d="$WORK/ho-packed"; mk_apply_fixture "$d" 1 1; stage_own_apply "$d"
run_handoff "$d"
check "the staged copy ran and applied" "1 v9.9.9 gone" "$(grep -c 're-executing the staged sot-apply' "$d/out" || true) $(installed_tag "$d") $(armed "$d")"
case_start "old_installed_apply_hands_off_to_the_release_apply"
# v0.6.5's sot-apply.sh is installed: it re-executes the staged copy with SOT_APPLY_REEXECED=1 and no prefix, so the
# staged copy reads the prefix from where the stage sits (<prefix>/updates/<tag>-<target>/<top>/).
if old="$(cd "$SRC" && git show v0.6.5:scripts/sot-apply.sh 2>/dev/null)" && [ -n "$old" ]; then
    d="$WORK/ho-old"; mk_apply_fixture "$d" 1 1; stage_own_apply "$d" 0755; mk_julia "$d"
    printf '%s\n' "$old" > "$d/prefix/bin/sot-apply"; chmod 0755 "$d/prefix/bin/sot-apply"
    run_handoff "$d"
    check "the old apply re-executed the staged copy" "1" "$(grep -c 're-executing the staged sot-apply' "$d/out" || true)"
    check "the staged copy applied onto the install" "v9.9.9 new-sotd gone" "$(installed_tag "$d") $(sotd_text "$d") $(armed "$d")"
    check "the v0.6.5-shaped wrapper is re-rendered" "rendered" "$(wrapper_kind "$d")"
else
    printf '    SKIP the v0.6.5 hand-off (no tag v0.6.5 in this checkout)\n'
fi

case_start "the_packed_sot_apply_is_executable"
if mode="$(cd "$SRC" && git ls-files -s scripts/sot-apply.sh 2>/dev/null | cut -d' ' -f1)" && [ -n "$mode" ]; then
    check "git holds scripts/sot-apply.sh at mode 100755" "100755" "$mode"
else
    printf '    NOT CHECKED git holds the mode (this tree is not a git checkout); the archive check below still runs\n'
fi
# pack-build.sh itself, with cargo, git and getconf stood in: it packs sot-apply.sh with `cp`, and the archive lists the mode.
if [ "$(uname -sm)" != "Linux x86_64" ]; then
    printf '    SKIP the packed archive (pack-build.sh runs on Linux x86_64 only)\n'
else
    d="$WORK/ho-pack"; mk_pack_fixture "$d" 2.31
    mkdir -p "$d/tree/deploy"; cp "$SRC/deploy/sotd.service" "$d/tree/deploy/"
    cp -p "$SRC/scripts/sot-apply.sh" "$d/tree/scripts/"
    mkdir -p "$d/tree/rust"
    for t in cat mkdir cp tar gzip mktemp rm sha256sum chmod; do ln -sf "$(command -v "$t")" "$d/bin/$t"; done
    printf '#!/bin/sh\nif [ "$1" = rev-parse ]; then echo 0123456789abcdef0123456789abcdef01234567; elif [ "$1 $2" = "branch -r" ]; then echo origin/main; fi\n' > "$d/bin/git"
    cat > "$d/bin/cargo" <<'CG'
#!/bin/sh
for t in release x86_64-unknown-linux-gnu/release; do
    mkdir -p "$CARGO_TARGET_DIR/$t"
    for b in sot sotd sot-capsule; do
        printf '#!/bin/sh\necho "%s 1.2.3 (fixture)"\n' "$b" > "$CARGO_TARGET_DIR/$t/$b"; chmod +x "$CARGO_TARGET_DIR/$t/$b"
    done
done
CG
    chmod +x "$d/bin/git" "$d/bin/cargo"
    PK_RC=0
    PK_OUT="$(cd "$d/tree" && env -i PATH="$d/bin" CARGO_TARGET_DIR="$d/target" /bin/bash scripts/pack-build.sh "$d/out" 2>&1)" || PK_RC=$?
    check "the pack succeeds" "0" "$PK_RC"; [ "$PK_RC" = 0 ] || printf '%s\n' "$PK_OUT"
    check "the archive lists sot-apply executable" "-rwx" \
        "$(tar -tvzf "$d/out"/sot-*-linux-x86_64.tar.gz 2>/dev/null | awk '/\/sot-apply$/ { print substr($1, 1, 4) }')"
fi

case_start "an_apply_brings_comm_to_the_applied_commit"
# The apply is the one place every Unix box passes through, so it refreshes comm like install.sh and the rendered
# wrapper do: update_comm runs when comm's stamp names another commit than the applied checkout's. Every role: the apply
# reads none.
for role in both be-only; do
    d="$WORK/ho-comm-$role"; mk_apply_fixture "$d" 1 1; mk_julia "$d"; mkdir -p "$d/ch/bin"; printf 'abcdef123\n' > "$d/ch/VERSION"
    [ "$role" = both ] || printf '{\n  "tag": "v9.9.8",\n  "version": "9.9.8",\n  "commit": "aaa",\n  "daemon": true,\n  "frontend": false,\n  "service": "systemd"\n}\n' > "$d/prefix/install.json"
    run_handoff "$d"
    check "$role: the apply applied" "v9.9.9" "$(installed_tag "$d")"
    check "$role: one update_comm, on the applied checkout" "1 1" \
        "$(grep -c 'update_comm' "$d/julia.log" || true) $(grep -c -e "--project=$d/co " "$d/julia.log" || true)"
done
d="$WORK/ho-comm-current"; mk_apply_fixture "$d" 1 1; mk_julia "$d"; mkdir -p "$d/ch/bin"
printf '%s\n' "$(cd "$d/co" && git rev-parse --short=9 HEAD)" > "$d/ch/VERSION"
run_handoff "$d"
check "comm already at the applied commit: no update_comm" "v9.9.9 0" "$(installed_tag "$d") $(grep -c 'update_comm' "$d/julia.log" || true)"
d="$WORK/ho-comm-refused"; mk_apply_fixture "$d" 1 1; mk_julia "$d"; mk_pointer_sotd "$d" 3 "refuse: x"
run_handoff "$d"
check "an apply that applied nothing refreshes nothing" "v9.9.8 0" "$(installed_tag "$d") $(grep -c 'update_comm' "$d/julia.log" || true)"
d="$WORK/ho-comm-nojulia"; mk_apply_fixture "$d" 1 1
run_handoff "$d"
check "no julia: the apply still succeeds" "0 v9.9.9" "$AP_RC $(installed_tag "$d")"

# ---- review-U3's own cases (scratch only, appended to installer-handoff.sh in scratch copies) ----
run_args() {  # <dir> [args]: run the installed apply with args; sets AP_RC
    local d="$1"; shift
    rm -rf "${d:?}/prefix/updates/.lock"
    AP_RC=0
    ( HOME="$d/home" SOT_COMM_HOME="$d/ch" PATH="$d/stubs:$TOOLS" STUB_LOG="$d/log" STUB_SOCKET="$d/sot.sock" \
        "$d/prefix/bin/sot-apply" "$@" ) > "$d/out" 2>&1 || AP_RC=$?
}
arm_next() {  # <dir>: arm v9.9.10 whose stage holds this tree's sot-apply, executable
    local d="$1" top="sot-v9.9.10-$TGT"
    mkdir -p "$d/prefix/updates/v9.9.10-$TGT/$top"
    cp "$SRC/scripts/sot-apply.sh" "$d/prefix/updates/v9.9.10-$TGT/$top/sot-apply"; chmod 0755 "$d/prefix/updates/v9.9.10-$TGT/$top/sot-apply"
    printf '{\n  "tag": "v9.9.10",\n  "target": "%s",\n  "checkout": "%s/co",\n  "commit": "x",\n  "asset": "%s.tar.gz",\n  "asset_sha256": "y"\n}\n' \
        "$TGT" "$d" "$top" > "$d/prefix/updates/pending-$TGT.json"
}
case_start "a_rollback_through_a_hand_off_rolls_back"
d="$WORK/rv-rb"; mk_apply_fixture "$d" 1 1; mk_julia "$d"
run_args "$d"
check "first apply: v9.9.9" "v9.9.9 new-sotd" "$(installed_tag "$d") $(sotd_text "$d")"
arm_next "$d"
run_args "$d" --rollback
check "rollback via the staged v9.9.10 copy" "1 v9.9.8 old-sotd gone 1" \
    "$(grep -c 're-executing the staged sot-apply' "$d/out" || true) $(installed_tag "$d") $(sotd_text "$d") $(armed "$d") $(grep -c 'rollback complete' "$d/out" || true)"
if old="$(cd "$SRC" && git show v0.6.5:scripts/sot-apply.sh 2>/dev/null)" && [ -n "$old" ]; then
    case_start "an_old_apply_rolls_back_through_the_hand_off"
    d="$WORK/rv-rb-old"; mk_apply_fixture "$d" 1 1; mk_julia "$d"
    run_args "$d"
    printf '%s\n' "$old" > "$d/prefix/bin/sot-apply"; chmod 0755 "$d/prefix/bin/sot-apply"
    arm_next "$d"
    run_args "$d" --rollback
        check "v0.6.5 rollback via the staged copy" "1 v9.9.8 old-sotd" \
        "$(grep -c 're-executing the staged sot-apply' "$d/out" || true) $(installed_tag "$d") $(sotd_text "$d")"
fi
case_start "drop_only_never_applies"
d="$WORK/rv-do1"; mk_apply_fixture "$d" 1 1; stage_own_apply "$d" 0755; mk_julia "$d"
run_args "$d" --drop-only
check "a fitting pointer: nothing applied, kept, no hand-off" "v9.9.8 old-sotd armed 0 0" \
    "$(installed_tag "$d") $(sotd_text "$d") $(armed "$d") $(grep -c 're-executing' "$d/out" || true) $(grep -c update_comm "$d/julia.log" || true)"
d="$WORK/rv-do2"; mk_apply_fixture "$d" 1 1; stage_own_apply "$d" 0755; mk_pointer_sotd "$d" 3 "refuse: x"
run_args "$d" --drop-only
check "a refused pointer: dropped, nothing applied" "v9.9.8 gone 0" \
    "$(installed_tag "$d") $(armed "$d") $(grep -c 're-executing' "$d/out" || true)"

case_start "the_staged_copy_asks_the_installed_sotd_under_the_lock"
d="$WORK/rv-il"; mk_apply_fixture "$d" 1 1; stage_own_apply "$d" 0755; mk_julia "$d"
# The installed sotd lets the pointer through on its first answer (above the re-exec) and refuses it on its second (under
# the lock, in the staged copy): the pointer armed in between that no longer fits.
printf '#!/bin/sh\n[ "$1 $2" = "update pointer" ] || exit 1\nn=$(cat "%s/asks" 2>/dev/null || echo 0); n=$((n + 1)); echo $n > "%s/asks"\n[ $n -ge 2 ] || exit 0\necho "refuse: armed in between"\nexit 3\n' "$d" "$d" > "$d/prefix/bin/sotd"
chmod 0755 "$d/prefix/bin/sotd"
run_handoff "$d"
check "re-executed, asked twice, dropped, nothing applied" "1 2 gone v9.9.8" \
    "$(grep -c 're-executing the staged sot-apply' "$d/out" || true) $(cat "$d/asks") $(armed "$d") $(installed_tag "$d")"

case_start "the_comm_refresh_is_bounded_and_fails_open"
# A julia that stalls (a launcher that hangs in a non-interactive shell) costs the refresh its bound, not the unit's start
# timeout: it warns and returns 0, and update_comm never ran, so no stamp was written.
d="$WORK/ho-stall"; mkdir -p "$d/bin" "$d/co" "$d/ch/bin" "$d/home"
printf '#!/bin/sh\nsleep 20\n' > "$d/bin/julia"; chmod +x "$d/bin/julia"
( cd "$d/co" && git init -q . && : > f && git add f && git -c user.name=t -c user.email=t@t commit -q -m c )
printf 'abcdef123\n' > "$d/ch/VERSION"
if [ -e "$TOOLS/timeout" ]; then
    start="$(date +%s)"
    out="$( HOME="$d/home" SOT_COMM_HOME="$d/ch" SOT_COMM_REFRESH_TIMEOUT_S=1 PATH="$d/bin:$TOOLS" bash -c '. "$1"; sot_comm_refresh "$2"; echo "rc=$?"' x "$LIB" "$d/co" 2>&1 )"
    took=$(( $(date +%s) - start ))
    check "a stalled julia is cut off within the bound" "yes" "$([ "$took" -lt 10 ] && echo yes || echo "no (${took}s)")"
    check "it says the refresh timed out and returns 0" "1 1" "$(printf '%s\n' "$out" | grep -c 'timed out' || true) $(printf '%s\n' "$out" | grep -c '^rc=0$' || true)"
    check "the stamp is untouched" "abcdef123" "$(cat "$d/ch/VERSION")"
else
    printf '    SKIP the stall bound (no timeout on this OS)\n'
fi

case_start "the_unit_applies_in_the_daemons_environment"
# ExecStartPre runs sot-apply through the same bash -c form as ExecStart, so the comm refresh sees what ~/.bashrc exports
# (the per-host depot, julia, CODEX_HOME, SOT_COMM_HOME).
d="$WORK/ho-unit"; mkdir -p "$d/home" "$d/prefix/bin"
render_sotd_unit "$d/prefix" "$SRC/deploy/sotd.service" "$d/unit"
pre="$(sed -n "s/^ExecStartPre=-//p" "$d/unit")"
check "the unit has one ExecStartPre, sourcing .bashrc and exec'ing the prefix's sot-apply" "1 1" \
    "$(printf '%s\n' "$pre" | grep -c '\.bashrc' || true) $(printf '%s\n' "$pre" | grep -c "exec \"$d/prefix/bin/sot-apply\"" || true)"
printf '#!/bin/sh\necho "marker=$SOT_UNIT_MARKER" > "%s/seen"\n' "$d" > "$d/prefix/bin/sot-apply"; chmod +x "$d/prefix/bin/sot-apply"
printf 'export SOT_UNIT_MARKER=from-bashrc\n' > "$d/home/.bashrc"
( eval "HOME='$d/home' $pre" ) >/dev/null 2>&1 || true
check "the apply sees what .bashrc exports" "marker=from-bashrc" "$(cat "$d/seen" 2>/dev/null)"
