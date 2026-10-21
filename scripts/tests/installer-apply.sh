#!/usr/bin/env bash
# installer-apply.sh -- the sot-apply transaction, the one-copy helper, the network refusal and the dev launcher's git bound.
# Run: scripts/tests/installer-apply.sh

set -euo pipefail

# shellcheck source=installer-support.sh
. "$(dirname "$0")/installer-support.sh"

# ---------------------------------------------------------------------------
# sot-apply fixtures: an install prefix with a real sot-apply, a git checkout
# holding this tree's library and unit template, and a staged v9.9.9.
SRC="$(cd "$(dirname "$0")/../.." && pwd)"
mk_apply_fixture() {  # <dir> <owned-unit 0|1> <owned-wrapper 0|1>
    local d="$1" ou="$2" ow="$3" h="$1/home" P="$1/prefix" co="$1/co" other=/other/prefix
    local stage="$1/prefix/updates/v9.9.9-$TGT" top="sot-v9.9.9-$TGT" sha unit_owner wrap_owner
    mkdir -p "$h/.config/systemd/user" "$h/.local/bin" "$P/bin" "$P/updates" "$P/repo" "$d/prev-co" \
        "$co/scripts/lib" "$co/deploy" "$stage/$top" "$d/stubs"
    mk_stubs "$d/stubs"
    : > "$d/log"
    cp "$SRC/scripts/sot-apply.sh" "$P/bin/sot-apply"; chmod 0755 "$P/bin/sot-apply"
    printf 'old-sot\n' > "$P/bin/sot"; printf 'old-sotd\n' > "$P/bin/sotd"
    chmod 0755 "$P/bin/sot" "$P/bin/sotd"
    printf '{\n  "tag": "v9.9.8",\n  "version": "9.9.8",\n  "commit": "aaa",\n  "service": "systemd"\n}\n' > "$P/install.json"
    ln -sfn "$d/prev-co" "$P/repo/current"
    cp "$SRC/scripts/lib/sot-daemon.sh" "$co/scripts/lib/"; cp "$SRC/deploy/sotd.service" "$co/deploy/"
    ( cd "$co" && git init -q . && git add . && git -c user.name=t -c user.email=t@t commit -q -m c )
    printf 'new-sot\n' > "$stage/$top/sot"; printf 'new-sotd\n' > "$stage/$top/sotd"
    printf 'asset\n' > "$stage/$top.tar.gz"
    sha="$(sha256_of "$stage/$top.tar.gz" | cut -d' ' -f1)"
    printf '{}\n' > "$stage/manifest.json"
    printf '{\n  "tag": "v9.9.9",\n  "target": "'"$TGT"'",\n  "checkout": "%s",\n  "commit": "%s",\n  "asset": "%s.tar.gz",\n  "asset_sha256": "%s"\n}\n' \
        "$co" "$(cd "$co" && git rev-parse HEAD)" "$top" "$sha" > "$P/updates/pending-$TGT.json"
    unit_owner="$other"; [ "$ou" = 1 ] && unit_owner="$P"
    wrap_owner="$other"; [ "$ow" = 1 ] && wrap_owner="$P"
    printf '[Service]\nExecStart=%s/bin/sotd --x\nRestart=always\n' "$unit_owner" > "$h/.config/systemd/user/sotd.service"
    printf '#!/usr/bin/env bash\nPENDING="%s/updates/pending-'"$TGT"'.json"\nstart_daemon_if_needed() {\n:\n}\n' "$wrap_owner" > "$h/.local/bin/sot-launch"
    chmod 0755 "$h/.local/bin/sot-launch"
    cp "$h/.config/systemd/user/sotd.service" "$d/unit.orig"; cp "$h/.local/bin/sot-launch" "$d/wrap.orig"
}
seed_link() {  # <dir>: the unit as an earlier install enabled it, with the old link
    mkdir -p "$1/home/.config/systemd/user/default.target.wants"
    ln -s ../sotd.service "$1/home/.config/systemd/user/default.target.wants/sotd.service"
}
wiring() { local u="$1/home/.config/systemd/user"; printf '%s %s' "$([ -f "$u/default.target.d/sotd.conf" ] && echo dropin || echo none)" "$([ -L "$u/default.target.wants/sotd.service" ] && echo link || echo none)"; }
run_apply() {  # <dir> [args]: sets AP_RC; rmdir is not in $TOOLS, so the staging lock is cleared here
    local d="$1"; shift
    rm -rf "${d:?}/prefix/updates/.lock"
    AP_RC=0
    ( HOME="$d/home" PATH="$d/stubs:$TOOLS" STUB_LOG="$d/log" STUB_SOCKET="$d/sot.sock" \
        "$d/prefix/bin/sot-apply" "$@" ) > "$d/out" 2>&1 || AP_RC=$?
}
reloads() { grep -c '^--user daemon-reload$' "$1/log" || true; }

# ---------------------------------------------------------------------------
case_start "apply_rerenders_owned_unit"
d="$WORK/ap1"; mk_apply_fixture "$d" 1 1; run_apply "$d"
U="$d/home/.config/systemd/user/sotd.service"
check "the applied tag is recorded" "1" "$(grep -c '"tag": "v9.9.9"' "$d/prefix/install.json" || true)"
check "the owned unit now restarts on failure" "1" "$(grep -c '^Restart=on-failure$' "$U" || true)"
linux_check "exactly one daemon-reload" "1" "$(reloads "$d")"
check "the unit backup equals the original" "same" "$(cmp -s "$d/unit.orig" "$d/prefix/updates/sotd.service.prev-$TGT" && echo same || echo differ)"

# shellcheck source=installer-apply-boot.sh
. "$(dirname "${BASH_SOURCE[0]}")/installer-apply-boot.sh"

# ---------------------------------------------------------------------------
case_start "apply_skips_foreign_unit"
d="$WORK/ap2"; mk_apply_fixture "$d" 0 0; run_apply "$d"
check "the applied tag is recorded" "1" "$(grep -c '"tag": "v9.9.9"' "$d/prefix/install.json" || true)"
check "a foreign unit is byte-identical" "same" "$(cmp -s "$d/unit.orig" "$d/home/.config/systemd/user/sotd.service" && echo same || echo differ)"
check "no daemon-reload" "0" "$(reloads "$d")"
check "no unit backup" "no" "$([ -e "$d/prefix/updates/sotd.service.prev-$TGT" ] && echo yes || echo no)"

# ---------------------------------------------------------------------------
case_start "apply_rerenders_owned_wrapper"
d="$WORK/ap3"; mk_apply_fixture "$d" 1 1
W="$d/home/.local/bin/sot-launch"; ino0="$(ls -i "$W" | awk '{print $1}')"
run_apply "$d"
check "the wrapper carries the marker" "1" "$(grep -c '^# sot-launch: all-in-one$' "$W" || true)"
check "the wrapper is executable" "yes" "$([ -x "$W" ] && echo yes || echo no)"
check "the wrapper is a new inode" "changed" "$([ "$(ls -i "$W" | awk '{print $1}')" != "$ino0" ] && echo changed || echo same)"
check "the wrapper backup equals the original" "same" "$(cmp -s "$d/wrap.orig" "$d/prefix/updates/sot-launch.prev-$TGT" && echo same || echo differ)"

# ---------------------------------------------------------------------------
case_start "apply_skips_foreign_wrapper"
d="$WORK/ap4"; mk_apply_fixture "$d" 1 0; run_apply "$d"
check "the applied tag is recorded" "1" "$(grep -c '"tag": "v9.9.9"' "$d/prefix/install.json" || true)"
check "a foreign wrapper is byte-identical" "same" "$(cmp -s "$d/wrap.orig" "$d/home/.local/bin/sot-launch" && echo same || echo differ)"

# ---------------------------------------------------------------------------
case_start "apply_failure_restores_unit_and_wrapper"
d="$WORK/ap5"; mk_apply_fixture "$d" 1 1; seed_link "$d"
chmod 0555 "$d/home/.local/bin"
run_apply "$d"
chmod 0755 "$d/home/.local/bin"
check "the unit is byte-equal to the original" "same" "$(cmp -s "$d/unit.orig" "$d/home/.config/systemd/user/sotd.service" && echo same || echo differ)"
linux_check "two daemon-reloads (re-render, restore)" "2" "$(reloads "$d")"
check "sotd is back to its pre-apply content" "old-sotd" "$(cat "$d/prefix/bin/sotd")"
check "repo/current points at the previous checkout" "$d/prev-co" "$(readlink "$d/prefix/repo/current")"
check "the pending pointer is still armed" "yes" "$([ -f "$d/prefix/updates/pending-$TGT.json" ] && echo yes || echo no)"
check "the wrapper is byte-equal to the original" "same" "$(cmp -s "$d/wrap.orig" "$d/home/.local/bin/sot-launch" && echo same || echo differ)"
check "the boot drop-in stays, written before the wrapper failed" "dropin none" "$(wiring "$d")"

# ---------------------------------------------------------------------------
case_start "rollback_restores_unit_and_wrapper"
d="$WORK/ap6"; mk_apply_fixture "$d" 1 1; seed_link "$d"; run_apply "$d"
check "the apply re-rendered the unit" "1" "$(grep -c '^Restart=on-failure$' "$d/home/.config/systemd/user/sotd.service" || true)"
check "the apply moved the enable link onto the boot drop-in" "dropin none" "$(wiring "$d")"
run_apply "$d" --rollback
check "the rollback keeps the drop-in, which starts the unit whatever its text" "dropin none" "$(wiring "$d")"
check "the unit is byte-equal to the original" "same" "$(cmp -s "$d/unit.orig" "$d/home/.config/systemd/user/sotd.service" && echo same || echo differ)"
check "the wrapper is byte-equal to the original" "same" "$(cmp -s "$d/wrap.orig" "$d/home/.local/bin/sot-launch" && echo same || echo differ)"
check "the wrapper is executable" "yes" "$([ -x "$d/home/.local/bin/sot-launch" ] && echo yes || echo no)"
linux_check "a daemon-reload followed the restore" "2" "$(reloads "$d")"
check "the checkout flipped back" "$d/prev-co" "$(readlink "$d/prefix/repo/current")"

# ---------------------------------------------------------------------------
case_start "failed_apply_keeps_old_record_and_pending"
d="$WORK/ap7"; mk_apply_fixture "$d" 1 1
chmod 0555 "$d/home/.local/bin"
run_apply "$d"
chmod 0755 "$d/home/.local/bin"
check "install.json still names the old tag" "1" "$(grep -c '"tag": "v9.9.8"' "$d/prefix/install.json" || true)"
check "install.json still names the old commit" "1" "$(grep -c '"commit": "aaa"' "$d/prefix/install.json" || true)"
check "the pending pointer is still armed" "yes" "$([ -f "$d/prefix/updates/pending-$TGT.json" ] && echo yes || echo no)"
run_apply "$d"
check "a second apply installs the new tag" "1 new-sotd no" \
    "$(grep -c '"tag": "v9.9.9"' "$d/prefix/install.json" || true) $(cat "$d/prefix/bin/sotd") $([ -f "$d/prefix/updates/pending-$TGT.json" ] && echo yes || echo no)"

# ---------------------------------------------------------------------------
# A pointer that no longer fits the install (UPDATER-ARMS-INSTALLED): sot-apply asks the installed sotd's `update
# pointer` before it re-executes a staged copy or applies anything. Exit 3 refuses; any other answer leaves today's
# tag check in charge. The stand-in answers with the exit code it is given; the real sotd (SOT_TEST_SOTD) answers from
# the pointer and install.json it finds.
mk_pointer_sotd() {  # <dir> <exit> <line>
    printf '#!/bin/sh\n[ "$1 $2" = "update pointer" ] || exit 1\necho "%s"\nexit %s\n' "$3" "$2" > "$1/prefix/bin/sotd"
    chmod 0755 "$1/prefix/bin/sotd"
}
installed_tag() { sed -n 's/^ *"tag": *"\([^"]*\)".*/\1/p' "$1/prefix/install.json" | head -1; }
armed() { [ -f "$1/prefix/updates/pending-$TGT.json" ] && echo armed || echo gone; }

case_start "a_refused_pointer_is_dropped_and_nothing_is_applied"
d="$WORK/pg1"; mk_apply_fixture "$d" 1 1; mk_pointer_sotd "$d" 3 "refuse: armed over 9.9.7 but the install is now 9.9.8; it no longer fits"
cp "$d/prefix/bin/sotd" "$d/sotd.orig"
run_apply "$d"
check "the install still names its tag" "v9.9.8" "$(installed_tag "$d")"
check "sotd is not replaced" "same" "$(cmp -s "$d/sotd.orig" "$d/prefix/bin/sotd" && echo same || echo differ)"
check "the pointer is dropped" "gone" "$(armed "$d")"
check "it says why" "1" "$(grep -c 'will not apply (armed over 9.9.7 but the install is now 9.9.8' "$d/out" || true)"
check "no rollback state is written" "no" "$([ -e "$d/prefix/updates/last-good-$TGT.json" ] && echo yes || echo no)"

case_start "the_refusal_runs_above_the_staged_copys_re_exec"
# The pointer's own release ships a sot-apply the installed one would run next. A refused pointer never reaches it.
for code in 3 0; do
    d="$WORK/pg2-$code"; mk_apply_fixture "$d" 1 1; mk_pointer_sotd "$d" "$code" "refuse: x"
    printf '#!/bin/sh\necho ran > "%s/reexec"\nexit 0\n' "$d" > "$d/prefix/updates/v9.9.9-$TGT/sot-v9.9.9-$TGT/sot-apply"
    chmod 0755 "$d/prefix/updates/v9.9.9-$TGT/sot-v9.9.9-$TGT/sot-apply"
    run_apply "$d"
    check "sotd says exit $code: the staged copy $([ "$code" = 3 ] && echo is not run || echo is run)" \
        "$([ "$code" = 3 ] && echo no || echo yes)" "$([ -e "$d/reexec" ] && echo yes || echo no)"
done

case_start "a_prefix_or_home_the_renders_refuse_is_dropped_before_the_flip"
# The rendered unit and wrapper cannot carry the path, so the apply refuses before the flip, drops the pointer and exits 0:
# no window launch loops on a try-restart.
for which in prefix home; do
    if [ "$which" = prefix ]; then d="$WORK"'/pn3$x'; ch='$'; else d="$WORK/pn3h"; ch=';'; fi
    mk_apply_fixture "$d" 1 1
    cp "$d/prefix/bin/sotd" "$d/sotd.orig"
    ah="$d/home"; [ "$which" = prefix ] || { mv "$d/home" "$d/h;m"; ah="$d/h;m"; }
    rm -rf "${d:?}/prefix/updates/.lock"; AP_RC=0
    ( HOME="$ah" PATH="$d/stubs:$TOOLS" STUB_LOG="$d/log" STUB_SOCKET="$d/sot.sock" "$d/prefix/bin/sot-apply" ) > "$d/out" 2>&1 || AP_RC=$?
    check "a $which holding $ch: the apply exits 0" "0" "$AP_RC"
    check "the line names the $which and $ch" "1 1" "$(grep -cF "ERROR: the $which " "$d/out" || true) $(grep -cF " holds $ch," "$d/out" || true)"
    check "the pointer is dropped" "gone" "$(armed "$d")"
    check "the install still names its tag, sotd is not replaced" "v9.9.8 same" \
        "$(installed_tag "$d") $(cmp -s "$d/sotd.orig" "$d/prefix/bin/sotd" && echo same || echo differ)"
    check "the checkout did not flip" "$d/prev-co" "$(readlink "$d/prefix/repo/current")"
done

case_start "a_sotd_that_does_not_answer_leaves_the_tag_check_in_charge"
# 1 is an older sotd's answer to the unknown word, 2 is a sotd that has `update` and not `pointer`, 4 is anything else.
for code in 0 1 2 4; do
    d="$WORK/pg3-$code"; mk_apply_fixture "$d" 1 1; mk_pointer_sotd "$d" "$code" "refuse: x"
    run_apply "$d"
    check "sotd exits $code: the pointer applies as before" "v9.9.9 gone" "$(installed_tag "$d") $(armed "$d")"
done
d="$WORK/pg3-none"; mk_apply_fixture "$d" 1 1; rm -f "$d/prefix/bin/sotd"
run_apply "$d"
check "no sotd at all: the pointer applies as before" "v9.9.9 gone" "$(installed_tag "$d") $(armed "$d")"
d="$WORK/pg3-tag"; mk_apply_fixture "$d" 1 1; mk_pointer_sotd "$d" 1 ""
sed 's/"v9.9.8"/"v9.9.9"/; s/"9.9.8"/"9.9.9"/' "$d/prefix/install.json" > "$d/install.new"; mv "$d/install.new" "$d/prefix/install.json"
run_apply "$d"
check "an install already at the pointer's tag is still cleared by the tag check" "gone" "$(armed "$d")"

# The real sotd, when the caller names it: the same pointer, now judged from its own content.
# PREP names what the stage's prepare did: full (the default), light (no Julia environments) or none (no prepared.json).
real_apply() {  # <name> <installed tag> <pointer version> [<over-version> <over-commit>]: sets RD
    RD="$WORK/$1"; mk_apply_fixture "$RD" 1 1
    local u="$RD/prefix/updates"
    # The staged copy is named for the pointer's version; its files are the fixture's.
    mv "$u/v9.9.9-$TGT/sot-v9.9.9-$TGT.tar.gz" "$u/v9.9.9-$TGT/sot-$3-$TGT.tar.gz"
    mv "$u/v9.9.9-$TGT/sot-v9.9.9-$TGT" "$u/v9.9.9-$TGT/sot-$3-$TGT"
    [ "$3" = 9.9.9 ] || mv "$u/v9.9.9-$TGT" "$u/v$3-$TGT"
    f() { sed -n 's/^ *"'"$1"'": *"\([^"]*\)".*/\1/p' "$u/pending-$TGT.json" | head -1; }
    local sha co cm; sha="$(f asset_sha256)"; co="$(f checkout)"; cm="$(f commit)"
    printf '{\n  "schema": 1,\n  "repo": "kalidke/ship-of-tools",\n  "tag": "v%s",\n  "version": "%s",\n  "target": "%s",\n  "asset": "sot-%s-%s.tar.gz",\n  "asset_sha256": "%s",\n  "checkout": "%s",\n  "commit": "%s",\n  "armed_at": 1' \
        "$3" "$3" "$TGT" "$3" "$TGT" "$sha" "$co" "$cm" > "$u/pending-$TGT.json"
    [ -z "${4:-}" ] || printf ',\n  "armed_over_version": "%s",\n  "armed_over_commit": "%s"' "$4" "$5" >> "$u/pending-$TGT.json"
    printf '\n}\n' >> "$u/pending-$TGT.json"
    [ "${PREP:-full}" = none ] || printf '{\n  "schema": 1,\n  "repo": "kalidke/ship-of-tools",\n  "tag": "v%s",\n  "version": "%s",\n  "target": "%s",\n  "asset": "sot-%s-%s.tar.gz",\n  "asset_sha256": "%s",\n  "checkout": "%s",\n  "commit": "%s",\n  "julia_instantiated": %s,\n  "mathjax_deps": false,\n  "prepared_at": 1\n}\n' \
            "$3" "$3" "$TGT" "$3" "$TGT" "$sha" "$co" "$cm" "$([ "${PREP:-full}" = full ] && echo true || echo false)" > "$u/v$3-$TGT/prepared.json"
    printf '{\n  "schema": 1,\n  "prefix": "%s",\n  "tag": "%s",\n  "version": "%s",\n  "commit": "aaa",\n  "service": "systemd"\n}\n' \
        "$RD/prefix" "$2" "${2#v}" > "$RD/prefix/install.json"
    cp "$SOT_TEST_SOTD" "$RD/prefix/bin/sotd"; chmod 0755 "$RD/prefix/bin/sotd"
    cp "$RD/prefix/bin/sotd" "$RD/sotd.orig"
}
case_start "the_real_sotd_judges_the_pointer_from_its_content"
# SOT_TEST_SOTD names the sotd; unset, this tree's debug build (CARGO_TARGET_DIR, else rust/target) when it is there.
[ -n "${SOT_TEST_SOTD:-}" ] || SOT_TEST_SOTD="${CARGO_TARGET_DIR:-$SRC/rust/target}/debug/sotd"
if [ ! -x "$SOT_TEST_SOTD" ]; then
    printf '    SKIP the real sotd cases (no built sotd at %s; build it with cargo build -p sot-backend --bin sotd)\n' "$SOT_TEST_SOTD"
else
    # Installed 9.9.10, a pointer for 9.9.9 that records nothing: the daemon that armed it predates the record.
    real_apply pr1 v9.9.10 9.9.9
    run_apply "$RD"; sed 's/^/      | /' "$RD/out"
    check "below the install: nothing is downgraded" "v9.9.10 same" "$(installed_tag "$RD") $(cmp -s "$RD/sotd.orig" "$RD/prefix/bin/sotd" && echo same || echo differ)"
    check "below the install: the pointer is dropped" "gone" "$(armed "$RD")"
    check "below the install: the reason is the version judgement" "1" "$(grep -c 'will not apply (9.9.9 is not above the installed 9.9.10)' "$RD/out" || true)"
    real_apply pr2 v9.9.8 9.9.9
    run_apply "$RD"
    check "above the install: a pointer with no record applies" "v9.9.9 gone" "$(installed_tag "$RD") $(armed "$RD")"
    real_apply pr3 v9.9.8 9.9.9 9.9.7 aaa
    run_apply "$RD"
    check "armed over another install: refused" "v9.9.8 gone" "$(installed_tag "$RD") $(armed "$RD")"
    check "armed over another install: the reason names the record" "1" "$(grep -c 'will not apply (armed over 9.9.7 but the install is now 9.9.8' "$RD/out" || true)"
    real_apply pr4 v9.9.8 9.9.9 9.9.8 bbb
    run_apply "$RD"
    check "armed over the same version at another commit: refused" "v9.9.8 gone" "$(installed_tag "$RD") $(armed "$RD")"
    real_apply pr5 v9.9.8 9.9.9 9.9.8 aaa
    run_apply "$RD"
    check "armed over this install: applies" "v9.9.9 gone" "$(installed_tag "$RD") $(armed "$RD")"
    real_apply pr6 v9.9.8 9.9.9
    # Every field the script reads, none the Rust arm adds (schema, repo, version): complete to the shell, unreadable to sotd.
    grep -v -e '"schema"' -e '"repo"' -e '"version"' -e '"armed_at"' "$RD/prefix/updates/pending-$TGT.json" > "$RD/ptr.new"
    mv "$RD/ptr.new" "$RD/prefix/updates/pending-$TGT.json"
    run_apply "$RD"
    check "a pointer the sotd cannot read is dropped, not applied" "v9.9.8 gone" "$(installed_tag "$RD") $(armed "$RD")"
    check "it says the pointer is unreadable" "1" "$(grep -c 'will not apply (unreadable pointer: ' "$RD/out" || true)"
    real_apply pr7 v9.9.8-dev+abc 9.9.9
    run_apply "$RD"
    check "a pointer with no record onto a dev build: refused" "v9.9.8-dev+abc gone" "$(installed_tag "$RD") $(armed "$RD")"
    check "a pointer with no record onto a dev build: the reason names the dev build" "1" "$(grep -c 'is a dev build' "$RD/out" || true)"
    # shellcheck source=installer-apply-fit.sh
    . "$(dirname "${BASH_SOURCE[0]}")/installer-apply-fit.sh"
fi

# ---------------------------------------------------------------------------
case_start "partial_wrapper_write_restores"
d="$WORK/ap8"; mk_apply_fixture "$d" 1 1
cat > "$d/stubs/cat" <<'CAT'
#!/bin/sh
# A disk that fills mid-write: half the text lands, then the write fails.
in="$(head -c 1000000)"
printf '%s' "$in" | head -c "$((${#in} / 2))"
exit 1
CAT
chmod +x "$d/stubs/cat"
run_apply "$d"
check "the apply fails at the re-render and restores" "1" "$(grep -c 're-rendering the unit or wrapper failed .* restoring previous binaries' "$d/out" || true)"
check "the old wrapper is byte-identical" "same" "$(cmp -s "$d/wrap.orig" "$d/home/.local/bin/sot-launch" && echo same || echo differ)"
check "the pending pointer is kept" "yes" "$([ -f "$d/prefix/updates/pending-$TGT.json" ] && echo yes || echo no)"
check "no temp file is left beside the wrapper" "0" "$(find "$d/home/.local/bin" -name 'sot-launch.new*' | wc -l | tr -d ' ')"

# ---------------------------------------------------------------------------
# A disk that fills during a backup copy: half the bytes land, then cp fails.
# Every other copy is the real cp.
half_cp_stub() {  # <dir> <destination glob>
    cat > "$1/stubs/cp" <<CP
#!/bin/sh
for a; do src="\$dst"; dst="\$a"; done
case "\$dst" in
    $2) head -c "\$((\$(wc -c < "\$src") / 2))" "\$src" > "\$dst"; exit 1 ;;
esac
exec "$TOOLS/cp" "\$@"
CP
    chmod +x "$1/stubs/cp"
}
for which in unit wrapper; do
    case_start "partial_${which}_backup_keeps_the_originals"
    d="$WORK/ap9-$which"; mk_apply_fixture "$d" 1 1
    case "$which" in
        unit) half_cp_stub "$d" '*/sotd.service.prev-*' ;;
        wrapper) half_cp_stub "$d" '*/sot-launch.prev-*' ;;
    esac
    run_apply "$d"
    check "$which: the apply fails at the backup" "1" "$(grep -c 'backing up the unit or wrapper failed' "$d/out" || true)"
    check "$which: the unit is byte-identical" "same" "$(cmp -s "$d/unit.orig" "$d/home/.config/systemd/user/sotd.service" && echo same || echo differ)"
    check "$which: the wrapper is byte-identical" "same" "$(cmp -s "$d/wrap.orig" "$d/home/.local/bin/sot-launch" && echo same || echo differ)"
    check "$which: no partial backup is left" "0" "$(find "$d/prefix" "$d/home" -name '*.new*' | wc -l | tr -d ' ')"
done

# ---------------------------------------------------------------------------
for k in sot sotd; do
    case_start "failed_${k}_install_restores_only_this_apply"
    # Backups left by an older apply, then installing $k fails after every
    # binary before it was replaced.
    d="$WORK/ap10-$k"; mk_apply_fixture "$d" 1 1
    printf 'older-sot\n' > "$d/prefix/bin/sot.prev"; printf 'older-sotd\n' > "$d/prefix/bin/sotd.prev"
    cp "$d/prefix/bin/sot-apply" "$d/apply.orig"
    cat > "$d/stubs/install" <<INST
#!/bin/sh
for a; do dst="\$a"; done
case "\$dst" in */bin/$k.new) exit 1 ;; esac
exec "$TOOLS/install" "\$@"
INST
    chmod +x "$d/stubs/install"
    run_apply "$d"
    check "$k: the apply fails installing $k" "1" "$(grep -c "installing $k failed" "$d/out" || true)"
    check "$k: every binary has its pre-apply bytes" "old-sot old-sotd same" \
        "$(cat "$d/prefix/bin/sot") $(cat "$d/prefix/bin/sotd") $(cmp -s "$d/apply.orig" "$d/prefix/bin/sot-apply" && echo same || echo differ)"
    check "$k: install.json still names the old version" "1" "$(grep -c '"version": "9.9.8"' "$d/prefix/install.json" || true)"
done

# ---------------------------------------------------------------------------
case_start "backup_failure_replaces_nothing"
# An older apply's backups, then the second binary's backup fails half-written.
d="$WORK/ap11"; mk_apply_fixture "$d" 1 1
printf 'older-sot\n' > "$d/prefix/bin/sot.prev"; printf 'older-sotd\n' > "$d/prefix/bin/sotd.prev"
printf 'older-unit\n' > "$d/prefix/updates/sotd.service.prev-$TGT"
printf 'older-wrap\n' > "$d/prefix/updates/sot-launch.prev-$TGT"
cp "$d/prefix/bin/sot-apply" "$d/apply.orig"
half_cp_stub "$d" '*/bin/sotd.prev*'
run_apply "$d"
check "the apply exits non-zero" "yes" "$([ "$AP_RC" -ne 0 ] && echo yes || echo no)"
check "the failed backup is named" "1" "$(grep -c 'backing up sotd failed' "$d/out" || true)"
check "no installed file changed" "old-sot old-sotd same same same" \
    "$(cat "$d/prefix/bin/sot") $(cat "$d/prefix/bin/sotd") $(cmp -s "$d/apply.orig" "$d/prefix/bin/sot-apply" && echo same || echo differ) $(cmp -s "$d/unit.orig" "$d/home/.config/systemd/user/sotd.service" && echo same || echo differ) $(cmp -s "$d/wrap.orig" "$d/home/.local/bin/sot-launch" && echo same || echo differ)"
check "the old backup set is intact" "older-sot older-sotd older-unit older-wrap" \
    "$(cat "$d/prefix/bin/sot.prev") $(cat "$d/prefix/bin/sotd.prev") $(cat "$d/prefix/updates/sotd.service.prev-$TGT") $(cat "$d/prefix/updates/sot-launch.prev-$TGT")"
check "no .new is left" "0" "$(find "$d/prefix" "$d/home" -name '*.new*' | wc -l | tr -d ' ')"
check "repo/current and install.json are unchanged" "$d/prev-co 1" \
    "$(readlink "$d/prefix/repo/current") $(grep -c '"tag": "v9.9.8"' "$d/prefix/install.json" || true)"

# ---------------------------------------------------------------------------
case_start "rollback_copy_failure_fails_the_rollback"
d="$WORK/ap12"; mk_apply_fixture "$d" 1 1; run_apply "$d"
check "the apply installed the new tag" "1" "$(grep -c '"tag": "v9.9.9"' "$d/prefix/install.json" || true)"
cp "$d/prefix/install.json" "$d/record.applied"
half_cp_stub "$d" '*/bin/sotd|*/bin/sotd.new.*'
run_apply "$d" --rollback
check "the rollback exits non-zero" "yes" "$([ "$AP_RC" -ne 0 ] && echo yes || echo no)"
check "the error names the binary" "1" "$(grep -cF "could not restore $d/prefix/bin/sotd " "$d/out" || true)"
check "install.json is unchanged" "same" "$(cmp -s "$d/record.applied" "$d/prefix/install.json" && echo same || echo differ)"
check "repo/current is unchanged" "$d/co" "$(readlink "$d/prefix/repo/current")"
check "no rollback-complete line" "0" "$(grep -c 'rollback complete' "$d/out" || true)"

# ---------------------------------------------------------------------------
case_start "one_copy_helper"
helper_text() { sed -n '/^sot_install_copy() {/,/^}/p' "$1"; }
check "the library defines sot_install_copy" "yes" "$([ -n "$(helper_text "$LIB")" ] && echo yes || echo no)"
# sot-apply.sh carries the one byte-identical copy: it copies before it reads the library of the commit it applies.
check "sot-apply.sh's sot_install_copy is byte-identical to the library's" "$(helper_text "$LIB")" "$(helper_text "$SRC/scripts/sot-apply.sh")"
check "no other script defines it" "2" "$(grep -rl '^sot_install_copy() {' "$SRC/scripts" | wc -l | tr -d ' ')"
# A stale <dst>.new another writer left (or still writes) is never reused.
d="$WORK/copy-stale"; mkdir -p "$d"
printf 'source\n' > "$d/src"; printf 'old\n' > "$d/dst"; printf 'junk\n' > "$d/dst.new"
sot_install_copy "$d/src" "$d/dst"
check "the destination equals the source" "source" "$(cat "$d/dst")"
check "a stale dst.new is untouched" "junk" "$(cat "$d/dst.new" 2>/dev/null || echo gone)"
check "no temp file of this copy is left" "0" "$(find "$d" -name 'dst.new.*' | wc -l | tr -d ' ')"

# ---------------------------------------------------------------------------
case_start "remote_wrapper_failed_write_keeps_the_old"
# The launcher that dials the plan (render_remote_launch): a cat that fails before writing a byte.
d="$WORK/remote-wrap"; mkdir -p "$d/stubs"
printf '#!/bin/sh\necho old-wrapper\n' > "$d/sot-launch"; cp "$d/sot-launch" "$d/wrap.orig"
printf '#!/bin/sh\nexit 1\n' > "$d/stubs/cat"; chmod +x "$d/stubs/cat"
rc=0; ( PATH="$d/stubs:$PATH"; render_remote_launch /opt/sot be-alias "$d/sot-launch" ) 2>/dev/null || rc=$?
check "the failed write returns 1" "1" "$rc"
check "the old remote wrapper is byte-identical" "same" "$(cmp -s "$d/wrap.orig" "$d/sot-launch" && echo same || echo differ)"
check "no temp file is left beside it" "0" "$(find "$d" -name 'sot-launch.new*' | wc -l | tr -d ' ')"
render_remote_launch /opt/sot be-alias "$d/sot-launch"
check "a good write names the alias and is executable" "1 yes" \
    "$(grep -c '^export SOT_HOST="be-alias"$' "$d/sot-launch" || true) $([ -x "$d/sot-launch" ] && echo yes || echo no)"

# ---------------------------------------------------------------------------
case_start "the dev launcher's git pull carries the stall bound"
# A stand-in git records its argv and answers the pull with success; the launcher then goes on, finds no sotd, and
# execs the stub frontend. The pull must not be able to hold the window back on a stalled network.
d="$WORK/launch-pull"; mkdir -p "$d/repo/scripts/lib" "$d/repo/.git" "$d/home" "$d/stubs"
cp "$(dirname "$0")/../launch-sot.sh" "$d/repo/scripts/launch-sot.sh"
cp "$(dirname "$0")/../lib/sot-hosts.sh" "$(dirname "$0")/../lib/sot-daemon.sh" "$d/repo/scripts/lib/"
printf '#!/bin/sh\necho "git $*" >> "%s/git.log"\n' "$d" > "$d/stubs/git"
printf '#!/bin/sh\necho fe >> "%s/git.log"\n' "$d" > "$d/fe"
chmod +x "$d/stubs/git" "$d/fe"
( HOME="$d/home" PATH="$d/stubs:$TOOLS" SOT_PREFIX= SOT_FRONTEND_BIN="$d/fe" bash "$d/repo/scripts/launch-sot.sh" ) >/dev/null 2>&1 || true
check "the launcher pulled, then started the frontend" "1 1" "$(grep -c ' pull ' "$d/git.log" || true) $(grep -c '^fe$' "$d/git.log" || true)"
check "the pull carries the stall bound" "0" \
    "$(grep ' pull ' "$d/git.log" | grep -vc -e '-c http.lowSpeedLimit=1000 -c http.lowSpeedTime=60' || true)"

# ---------------------------------------------------------------------------
case_start "the ensure replaces a daemon an update left on the old binary (sot_daemon_current)"
# An apply that ran while the daemon ran swaps the binary and leaves the process (a Windows field box, 2026-10-16: an
# rc1 daemon kept serving after rc9.13 landed, and every launch kept it because its pipe answered). The stand-in sotd is
# a daemon that listens on its --socket and logs its pid, a bridge that connects, and a same-version that answers
# STUB_THEIRS (exit 3) or nothing (exit 0). The stand-in ps lists this account's processes as the old daemon's: the
# ensure finds it there by its socket and ends it by its pid.
d="$WORK/stale"; mkdir -p "$d/bin" "$d/home" "$d/prefix"
cat > "$d/bin/sotd" <<'PY'
#!/usr/bin/env python3
import os, socket, sys
a = sys.argv[1:]
if a[:1] == ["stdio-bridge"]:
    try:
        socket.socket(socket.AF_UNIX).connect(a[2][len("unix:"):])
    except OSError:
        sys.exit(1)
    sys.exit(0)
if a[:1] == ["same-version"]:
    if os.environ.get("STUB_RACE"):
        # another launch replaces the daemon while this one's probe is out: it ends the old one, starts a new one
        # and the account's process list now names the new one
        import subprocess, time
        n = sum(1 for l in open(os.environ["STUB_LOG"]) if l.startswith("daemon "))
        old = int([l for l in open(os.environ["STUB_LOG"]) if l.startswith("daemon ")][-1].split()[1])
        os.kill(old, 15)
        for _ in range(100):
            try:
                os.kill(old, 0)
                time.sleep(0.05)
            except OSError:
                break
        subprocess.Popen([sys.argv[0], "--socket", os.environ["STUB_SOCKET"]], stdout=subprocess.DEVNULL,
                         stderr=subprocess.DEVNULL, start_new_session=True)
        for _ in range(100):
            if sum(1 for l in open(os.environ["STUB_LOG"]) if l.startswith("daemon ")) > n:
                break
            time.sleep(0.05)
        new = [l for l in open(os.environ["STUB_LOG"]) if l.startswith("daemon ")][-1].split()[1]
        open(os.environ["STUB_PSLIST"], "w").write("%s sotd %s --socket %s --label sot\n" % (new, sys.argv[0], os.environ["STUB_SOCKET"]))
    if os.environ.get("STUB_THEIRS"):
        print(os.environ["STUB_THEIRS"])
        sys.exit(4 if os.environ.get("STUB_LEASED") else 3)
    sys.exit(0)
path = a[a.index("--socket") + 1]
if os.path.exists(path):
    os.unlink(path)
s = socket.socket(socket.AF_UNIX)
s.bind(path)
s.listen(8)
with open(os.environ["STUB_LOG"], "a") as log:
    log.write("daemon %d\n" % os.getpid())
while True:
    s.accept()[0].close()
PY
chmod +x "$d/bin/sotd"
: > "$d/log"
( STUB_LOG="$d/log" "$d/bin/sotd" --socket "$d/s.sock" --project-root "$d/home" --label sot >/dev/null 2>&1 & )
for _ in $(seq 1 100); do grep -q '^daemon ' "$d/log" && break; sleep 0.05; done
old="$(sed -n 's/^daemon //p' "$d/log" | head -1)"
# Before it, a daemon (a pid no process has) on a socket that only begins with this one, which must be left alone.
printf '999999999 sotd %s/bin/sotd --socket %s/s.sock.old\n%s sotd %s/bin/sotd --socket %s/s.sock --project-root %s/home --label sot\n' \
    "$d" "$d" "$old" "$d" "$d" "$d" > "$d/pslist"
printf '#!/bin/sh\ncat "%s/pslist"\n' "$d" > "$d/bin/ps"
chmod +x "$d/bin/ps"
state() {  # <pid>: alive, or gone once it has exited (a zombie not yet reaped is gone)
    case "$(ps -o stat= -p "$1" 2>/dev/null)" in ''|Z*) echo gone ;; *) echo alive ;; esac
}
stale_ensure() {  # [STUB_THEIRS]: run the ensure once; its rc and stderr in ST_RC and ST_ERR
    ST_RC=0
    ( HOME="$d/home" PATH="$d/bin:$TOOLS" STUB_LOG="$d/log" STUB_THEIRS="${1:-}" STUB_RACE="${2:-}" STUB_LEASED="${3:-}" \
        STUB_SOCKET="$d/s.sock" STUB_PSLIST="$d/pslist" \
        bash -c '. "$1"; sot_daemon_ensure "$2" "$3" "$4"; rc=$?; printf %s "$SOT_ENSURE_NOTICE" > "$5"; exit $rc' ensure "$LIB" "$d/prefix" "$d/bin/sotd" "$d/s.sock" "$d/notice" \
        2> "$d/err" ) || ST_RC=$?
    ST_ERR="$(cat "$d/err")"
    ST_NOTICE="$(cat "$d/notice")"
}
stale_ensure
check "a daemon of this version is kept" "0 1 alive" \
    "$ST_RC $(grep -c '^daemon ' "$d/log") $(state "$old")"
stale_ensure 0.6.6-rc1
new="$(sed -n 's/^daemon //p' "$d/log" | tail -1)"
check "a daemon of another version is ended and this version's started in its place" "0 gone 2 up" \
    "$ST_RC $(state "$old") $(grep -c '^daemon ' "$d/log") $(sot_socket_open "$d/bin/sotd" "$d/s.sock" && echo up || echo down)"
case "$ST_ERR" in
    *"the backend on $d/s.sock runs 0.6.6-rc1, not the version of $d/bin/sotd: replacing it"*) check "the ensure says which version it replaced" ok ok ;;
    *) check "the ensure says which version it replaced" "a replacing line" "$ST_ERR" ;;
esac
case "$ST_ERR" in
    *"their REPLs restart"*) check "the ensure says the rows keep running and the REPLs restart" ok ok ;;
    *) check "the ensure says the rows keep running and the REPLs restart" "the cost sentence" "$ST_ERR" ;;
esac
case "$ST_NOTICE" in
    *"was replaced; its rows keep running and their REPLs restart"*) check "the launch notice names the replace and its cost" ok ok ;;
    *) check "the launch notice names the replace and its cost" "the cost sentence" "$ST_NOTICE" ;;
esac
# A window holds a lease: the daemon of another version is kept, and the launch says it is out of date.
ps_before="$(grep -c '^daemon ' "$d/log")"
printf '%s sotd %s/bin/sotd --socket %s/s.sock --label sot\n' "$new" "$d" "$d" > "$d/pslist"
kill "$new" 2>/dev/null || true
for _ in $(seq 1 100); do sot_socket_open "$d/bin/sotd" "$d/s.sock" || break; sleep 0.05; done
( STUB_LOG="$d/log" "$d/bin/sotd" --socket "$d/s.sock" --project-root "$d/home" --label sot >/dev/null 2>&1 & )
for _ in $(seq 1 100); do [ "$(grep -c '^daemon ' "$d/log")" -gt "$ps_before" ] && break; sleep 0.05; done
leased="$(sed -n 's/^daemon //p' "$d/log" | tail -1)"
printf '%s sotd %s/bin/sotd --socket %s/s.sock --label sot\n' "$leased" "$d" "$d" > "$d/pslist"
stale_ensure 0.6.6-rc1 "" 1
check "a daemon of another version is kept while a window holds a lease, and the launch says so" "0 alive daemon out of date, restart when ready" \
    "$ST_RC $(state "$leased") $ST_NOTICE"
kill "$leased" 2>/dev/null || true
for _ in $(seq 1 100); do sot_socket_open "$d/bin/sotd" "$d/s.sock" || break; sleep 0.05; done
( STUB_LOG="$d/log" "$d/bin/sotd" --socket "$d/s.sock" --project-root "$d/home" --label sot >/dev/null 2>&1 & )
for _ in $(seq 1 100); do [ "$(grep -c '^daemon ' "$d/log")" -gt "$((ps_before + 1))" ] && break; sleep 0.05; done
new="$(sed -n 's/^daemon //p' "$d/log" | tail -1)"
# Two launches: the second one's probe is out while the first replaces the daemon. It named the old daemon before it
# asked, so it ends nothing and the first launch's successor lives.
printf '%s sotd %s/bin/sotd --socket %s/s.sock --label sot\n' "$new" "$d" "$d" > "$d/pslist"
n0="$(grep -c '^daemon ' "$d/log")"
stale_ensure 0.6.6-rc1 1
race="$(sed -n 's/^daemon //p' "$d/log" | tail -1)"
check "a launch racing a replace leaves the successor alive" "0 $((n0 + 1)) alive up" \
    "$ST_RC $(grep -c '^daemon ' "$d/log") $(state "$race") $(sot_socket_open "$d/bin/sotd" "$d/s.sock" && echo up || echo down)"
for p in "$old" "$new" "$leased" "$race"; do kill "$p" 2>/dev/null || true; done

# An owned unit whose restart stops the daemon and then fails: the replace is an attempted end, so the ensure's start
# and wait loop takes over (its `systemctl start`) instead of returning 0 with nothing running.
mkdir -p "$d/oprefix/bin" "$d/home/.config/systemd/user"
cp "$d/bin/sotd" "$d/oprefix/bin/sotd"
printf '{\n  "service": "systemd"\n}\n' > "$d/oprefix/install.json"
printf '[Service]\nExecStart=%s/oprefix/bin/sotd --x\n' "$d" > "$d/home/.config/systemd/user/sotd.service"
: > "$d/log"
( STUB_LOG="$d/log" "$d/bin/sotd" --socket "$d/s.sock" --project-root "$d/home" --label sot >/dev/null 2>&1 & )
for _ in $(seq 1 100); do grep -q '^daemon ' "$d/log" && break; sleep 0.05; done
old="$(sed -n 's/^daemon //p' "$d/log" | head -1)"
cat > "$d/bin/systemctl" <<SC
#!/bin/sh
echo "\$*" >> "$d/sc.log"
case "\$*" in
    *"show -p MainPID"*) echo "$old" ;;
    *restart*) kill "$old"; sleep 0.5; exit 1 ;;
esac
exit 1
SC
chmod +x "$d/bin/systemctl"
: > "$d/sc.log"
ST_RC=0
( HOME="$d/home" PATH="$d/bin:$TOOLS" STUB_LOG="$d/log" STUB_THEIRS=0.6.6-rc1 \
    bash -c '. "$1"; SOT_LAUNCH_WAIT_S=2; sot_daemon_ensure "$2" "$3" "$4"' ensure "$LIB" "$d/oprefix" "$d/oprefix/bin/sotd" "$d/s.sock" \
    2> "$d/err" ) || ST_RC=$?
check "a replace that fails to restart hands over to the start and wait loop" "1 gone started" \
    "$ST_RC $(state "$old") $(grep -q '^--user start sotd.service' "$d/sc.log" && echo started || echo not-started)"
kill "$old" 2>/dev/null || true

# ---------------------------------------------------------------------------
case_start "comm follows the checkout's commit after an apply (sot_comm_refresh, the rendered wrapper)"
# The 0.6.5 -> 0.6.6 apply changed the comm protocol and nothing refreshed the scripts (review of the stale-daemon work).
# The rule is the one sot-freshness.ps1 states: update comm when its install is missing or its VERSION stamp names another
# commit than the checkout's. A julia stand-in logs each update_comm it is asked for.
d="$WORK/comm"; mkdir -p "$d/bin" "$d/co" "$d/ch/bin" "$d/prefix/updates" "$d/home"
printf '#!/bin/sh\necho "julia $*" >> "%s/julia.log"\n' "$d" > "$d/bin/julia"; chmod +x "$d/bin/julia"
( cd "$d/co" && git init -q . && : > f && git add f && git -c user.name=t -c user.email=t@t commit -q -m c )
commit="$(cd "$d/co" && git rev-parse --short=9 HEAD)"
refresh() {  # <stamp-or-"none"> [comm dir]: the number of update_comm calls this refresh made
    : > "$d/julia.log"
    [ "$1" = none ] && rm -f "$d/ch/VERSION" || printf '%s\n' "$1" > "$d/ch/VERSION"
    ( HOME="$d/home" SOT_COMM_HOME="${2:-$d/ch}" PATH="$d/bin:$TOOLS" bash -c '. "$1"; sot_comm_refresh "$2"' x "$LIB" "$d/co" ) >/dev/null 2>&1 || true
    grep -c 'update_comm' "$d/julia.log" || true
}
check "a stamp naming the checkout's commit runs nothing" "0" "$(refresh "$commit")"
check "a -dirty stamp of the checkout's commit runs nothing" "0" "$(refresh "$commit-dirty")"
check "a stamp naming another commit updates comm" "1" "$(refresh abcdef123)"
check "no stamp updates comm" "1" "$(refresh none)"
check "a missing comm install updates comm" "1" "$(refresh "$commit" "$d/absent")"
mkdir -p "$d/nogit"
: > "$d/julia.log"
( HOME="$d/home" SOT_COMM_HOME="$d/ch" PATH="$d/bin:$TOOLS" bash -c '. "$1"; sot_comm_refresh "$2"' x "$LIB" "$d/nogit" ) >/dev/null 2>&1 || true
check "a checkout whose commit cannot be read decides nothing" "0" "$(grep -c 'update_comm' "$d/julia.log" || true)"
render_sot_launch "$d/prefix" "$TGT" "$d/launch" >/dev/null 2>&1 || true
check "the rendered wrapper refreshes comm after the apply and before the ensure" "refresh<ensure" \
    "$(awk '/sot_comm_refresh/ && !r { r = NR } /sot_daemon_ensure/ && !e { e = NR } /^if apply_pending/ && !a { a = NR } END { print (a && r > a && r < e) ? "refresh<ensure" : "a=" a " r=" r " e=" e }' "$d/launch")"
# pack-build.sh without a musl compiler: a gnu sotd only from a host whose glibc is within the fleet floor. The script
# is copied into a throwaway tree so it cd's there; PATH holds stubs only, so no musl compiler is found. Its first check
# refuses every host but Linux x86_64, so its cases are that platform's: elsewhere they print SKIP lines.
mk_pack_fixture() {  # <dir> <glibc-version>: <dir>/tree/scripts/pack-build.sh, <dir>/bin with stub getconf
    local d="$1"
    mkdir -p "$d/tree/scripts" "$d/bin"
    cp "$SRC/scripts/pack-build.sh" "$d/tree/scripts/"
    printf '#!/bin/sh\necho "glibc %s"\n' "$2" > "$d/bin/getconf"; chmod 0755 "$d/bin/getconf"
    for t in uname dirname sort tail cut; do ln -s "$(command -v "$t")" "$d/bin/$t"; done
}
run_pack() {  # <dir>: sets PK_RC and PK_OUT
    local d="$1"
    PK_RC=0
    PK_OUT="$(env -i PATH="$d/bin" /bin/bash "$d/tree/scripts/pack-build.sh" "$d/out" 2>&1)" || PK_RC=$?
}
pack_check() {  # <description> <expected> <actual>: check on Linux x86_64; elsewhere a SKIP line
    if [ "$(uname -sm)" = "Linux x86_64" ]; then check "$@"; else printf '    SKIP %s (pack-build.sh runs on Linux x86_64 only)\n' "$1"; fi
}
case_start "pack_build_refuses_a_too_new_glibc_without_musl"
d="$WORK/pk-new"; mk_pack_fixture "$d" 2.39; run_pack "$d"
pack_check "the pack exits non-zero" "yes" "$([ "$PK_RC" -ne 0 ] && echo yes || echo no)"
pack_check "it names the glibc floor route" "1" "$(printf '%s\n' "$PK_OUT" | grep -c 'glibc <= 2.31' || true)"
pack_check "it does not ask for a package install" "0" "$(printf '%s\n' "$PK_OUT" | grep -c 'install musl' || true)"
pack_check "nothing was packed" "gone" "$([ -e "$d/out" ] && echo exists || echo gone)"
case_start "pack_build_passes_the_glibc_gate_at_the_floor"
d="$WORK/pk-ok"; mk_pack_fixture "$d" 2.31; run_pack "$d"
pack_check "the glibc gate does not refuse" "0" "$(printf '%s\n' "$PK_OUT" | grep -c 'newer than the fleet' || true)"
pack_check "it goes on to the tree checks, which this git-less PATH stops" "yes" "$(printf '%s\n' "$PK_OUT" | grep -q 'git' && echo yes || echo no)"
case_start "pack_build_names_an_unreadable_glibc_without_musl"
d="$WORK/pk-bad-getconf"; mk_pack_fixture "$d" 2.31; printf '#!/bin/sh\nexit 1\n' > "$d/bin/getconf"; run_pack "$d"
pack_check "the pack exits non-zero when getconf fails" "yes" "$([ "$PK_RC" -ne 0 ] && echo yes || echo no)"
pack_check "it names the unreadable glibc when getconf fails" "1" "$(printf '%s\n' "$PK_OUT" | grep -c 'no readable glibc version' || true)"
d="$WORK/pk-no-getconf"; mk_pack_fixture "$d" 2.31; rm -f "$d/bin/getconf"; run_pack "$d"
pack_check "the pack exits non-zero when getconf is absent" "yes" "$([ "$PK_RC" -ne 0 ] && echo yes || echo no)"
pack_check "it names the unreadable glibc when getconf is absent" "1" "$(printf '%s\n' "$PK_OUT" | grep -c 'no readable glibc version' || true)"
case_start "pack_build_orders_glibc_versions_by_number"
for v in 2.4 2.9; do
    d="$WORK/pk-order-$v"; mk_pack_fixture "$d" "$v"; run_pack "$d"
    pack_check "glibc $v is older than the floor, so the gate does not refuse it" "0" "$(printf '%s\n' "$PK_OUT" | grep -c 'newer than the fleet' || true)"
done
case_start "pack_build_takes_the_musl_branch_when_a_musl_compiler_is_present"
d="$WORK/pk-musl"; mk_pack_fixture "$d" 2.39; printf '#!/bin/sh\nexit 0\n' > "$d/bin/musl-gcc"; chmod 0755 "$d/bin/musl-gcc"; run_pack "$d"
pack_check "with a musl compiler the glibc gate does not refuse 2.39" "0" "$(printf '%s\n' "$PK_OUT" | grep -c 'newer than the fleet' || true)"
pack_check "it goes on to the tree checks, which this git-less PATH stops" "yes" "$(printf '%s\n' "$PK_OUT" | grep -q 'git' && echo yes || echo no)"

# shellcheck source=installer-handoff.sh
. "$(dirname "$0")/installer-handoff.sh"

# ---------------------------------------------------------------------------
printf '\n'
if [ "$fails" -eq 0 ]; then
    printf 'installer-apply: all checks passed\n'
else
    printf 'installer-apply: %d check(s) FAILED\n' "$fails" >&2
    exit 1
fi
