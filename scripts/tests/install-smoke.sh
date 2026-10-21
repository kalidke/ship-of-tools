#!/usr/bin/env bash
# install-smoke.sh -- install.sh against the artifacts a release run built (Linux and macOS): release.yml's install
# jobs call it, and it runs off CI the same way.
#
#   install-smoke.sh SCENARIO ARTIFACTS CHECKOUT [BEFORE-TAG]
#
# ARTIFACTS holds the run's sot-<version>-<target>.tar.gz; CHECKOUT is the tree whose scripts/install.sh is the entry
# under test. SCENARIO:
#   fresh-local    a fresh install (--local; Linux adds --no-service), the entry's PASS line, then a repair run
#   fresh-be-only  the same with --be-only --no-service (Linux; a hosted container has no user manager for the unit)
#   update-entry   BEFORE-TAG installed by its own installer, then this entry with the run's artifacts, then a repair run
#   update-apply   BEFORE-TAG installed, the run's release staged and armed by BEFORE-TAG's own sotd (its updater), the
#                  installed sot-apply run as the unit's ExecStartPre runs it: the staged release's own apply must run, and
#                  leave the wrapper, comm and install.json at the new release, then the entry's repair run
#   update-implicit  BEFORE-TAG installed (pinned), then the entry with NO arguments against the real repository, as a
#                  lab box runs it: the channel resolves to BEFORE-TAG itself (this run's release is still a draft), so the
#                  run is a repair run at BEFORE-TAG that clears the pin; then the run's release, staged by the new sotd,
#                  must say `newer` and arm. The run's artifacts reach only that stage.
# Every install but update-implicit's one run names its target: --archive <folder> for the run's artifacts, --version
# for BEFORE-TAG (the fetch of that tag's own installer is the entry's prelude). The folder --archive takes is made here from the artifacts:
# SHA256SUMS, COMMIT and the tarball, which keeps its name when that carries the binaries' own version and is repacked
# under it otherwise (a branch run names its artifacts 0.0.0-ci.<sha>; the binaries carry the real, +-marked version).
#
# Every step ends with one verdict line and the script judges it: PASS, else FAIL (a ONE STEP LEFT is a FAIL here).
# Environment: SOT_SMOKE_HOME (the scratch $HOME; default a fresh folder), SOT_SMOKE_REPO (owner/name; default
# $GITHUB_REPOSITORY), SOT_SMOKE_COMMIT (default HEAD of CHECKOUT), SOT_SMOKE_KEEP=1 (keep the scratch folders).
# Bash 3.2 (macOS): no mapfile, no associative arrays.
set -euo pipefail

say() { printf 'install-smoke: %s\n' "$*"; }
die() { printf 'install-smoke: FAIL: %s\n' "$*" >&2; exit 1; }
[ $# -ge 3 ] && [ $# -le 4 ] || { echo "usage: install-smoke.sh SCENARIO ARTIFACTS CHECKOUT [BEFORE-TAG]" >&2; exit 2; }
SCENARIO="$1"
ARTIFACTS="$(cd "$2" && pwd)" || die "no such artifacts folder: $2"
CHECKOUT="$(cd "$3" && pwd)" || die "no such checkout: $3"
BEFORE="${4:-}"
[ -f "$CHECKOUT/scripts/install.sh" ] || die "$CHECKOUT holds no scripts/install.sh"
case "$SCENARIO" in
    fresh-local|fresh-be-only) ;;
    update-entry|update-apply|update-implicit) [ -n "$BEFORE" ] || die "$SCENARIO needs BEFORE-TAG (scripts/tests/release-before.sh names it)" ;;
    *) die "unknown scenario '$SCENARIO'" ;;
esac

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) TARGET=linux-x86_64 ROLE_FLAGS="--local --no-service" ;;
    Darwin-arm64) TARGET=macos-aarch64 ROLE_FLAGS="--local" ;;
    *) die "no release artifact for $(uname -s)-$(uname -m)" ;;
esac
[ "$SCENARIO" != fresh-be-only ] || ROLE_FLAGS="--be-only --no-service"
[ "$SCENARIO" != fresh-be-only ] || [ "$TARGET" = linux-x86_64 ] || die "fresh-be-only is a Linux scenario"

# Nothing of the caller's own reaches the install: its SOT_ variables (the comm home, a pinned tag) and XDG folders would
# send the files somewhere other than the scratch $HOME.
for v in $(compgen -v SOT_); do
    case "$v" in SOT_SMOKE_*) ;; *) unset "$v" ;; esac
done
unset XDG_CONFIG_HOME XDG_STATE_HOME XDG_DATA_HOME JULIA_LOAD_PATH JULIA_PROJECT

SMOKE="$(mktemp -d "${TMPDIR:-/tmp}/sot-smoke.XXXXXX")"
SMOKE="$(cd "$SMOKE" && pwd)"
export HOME="${SOT_SMOKE_HOME:-$SMOKE/home}"
mkdir -p "$HOME"
HOME="$(cd "$HOME" && pwd)"
export SOT_INSTALL_REPO="${SOT_SMOKE_REPO:-${GITHUB_REPOSITORY:-kalidke/ship-of-tools}}"
PREFIX="$HOME/.local/share/sot"
cleanup() {
    [ "${SOT_SMOKE_KEEP:-0}" = 1 ] || rm -rf "${SMOKE:?}"
}
trap cleanup EXIT

sha_of() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }

# ---- the folder --archive takes, made from the artifacts ------------------------------------------------------------
COMMIT="${SOT_SMOKE_COMMIT:-$(git -C "$CHECKOUT" rev-parse HEAD)}"
[[ "$COMMIT" =~ ^[0-9a-f]{40}$ ]] || die "the commit '$COMMIT' is not a 40-digit id"
SHORT="${COMMIT:0:9}"
set -- "$ARTIFACTS"/sot-*-"$TARGET".tar.gz
[ $# -eq 1 ] && [ -f "$1" ] || die "$ARTIFACTS must hold exactly one sot-<version>-$TARGET.tar.gz"
TARBALL="$1"
mkdir -p "$SMOKE/x" "$SMOKE/pack"
tar -xzf "$TARBALL" -C "$SMOKE/x"
set -- "$SMOKE"/x/sot-*-"$TARGET"
[ $# -eq 1 ] && [ -d "$1" ] || die "$TARBALL must hold exactly one sot-<version>-$TARGET folder"
TOP="$(basename "$1")"
VER="$("$SMOKE/x/$TOP/sotd" --version | awk '{ print $2 }')"
[ -n "$VER" ] || die "$TOP/sotd does not say its version"
case "$("$SMOKE/x/$TOP/sotd" --version)" in
    "sotd $VER ($SHORT "*) ;;
    *) die "$TOP/sotd says '$("$SMOKE/x/$TOP/sotd" --version)', which is not $VER at $SHORT (the commit of $CHECKOUT)" ;;
esac
# The packed apply is the staged release's own: the installed apply hands off to it only when it is executable, and the
# archive carries the mode of the file the runner checked out.
tar -tvzf "$TARBALL" | awk -v want="$TOP/sot-apply" '$NF == want { found = 1; if (substr($1, 4, 1) == "x") ok = 1 } END { exit !(found && ok) }' \
    || die "$TARBALL packs sot-apply without its execute bit: no installed apply could hand off to it"
NAME="sot-$VER-$TARGET"
if [ "$TOP" = "$NAME" ]; then
    cp "$TARBALL" "$SMOKE/pack/$NAME.tar.gz"
else
    say "repacking $TOP as $NAME (the binaries' version)"
    mv "$SMOKE/x/$TOP" "$SMOKE/x/$NAME"
    tar -czf "$SMOKE/pack/$NAME.tar.gz" -C "$SMOKE/x" "$NAME"
fi
printf '%s\n' "$COMMIT" > "$SMOKE/pack/COMMIT"
( cd "$SMOKE/pack" && sha_of "$NAME.tar.gz" COMMIT > SHA256SUMS )
PACK="$SMOKE/pack"
NEW_SOTD="$SMOKE/x/$NAME/sotd"
say "pack of $VER at $SHORT for $TARGET in $PACK"

# ---- running an install and judging its verdict -----------------------------------------------------------------------
# run <label> <command...>: the output goes to the log and the screen; the exit code is RC, the last line LAST.
run() {
    local label="$1"; shift
    say "--- $label: $*"
    RC=0
    "$@" > "$SMOKE/$label.out" 2>&1 || RC=$?
    cat "$SMOKE/$label.out"
    LAST="$(grep -v '^[[:space:]]*$' "$SMOKE/$label.out" | tail -1)"
    return 0
}
# verdict <label> <expected-pass-version>: the last line is PASS for that version.
verdict() {
    local label="$1" ver="$2"
    case "$LAST" in
        "PASS: Ship of Tools $ver at $SHORT on this computer"*) [ "$RC" = 0 ] || die "$label: PASS with exit $RC" ;;
        *) die "$label: the last line is '$LAST' (exit $RC), not PASS for $ver at $SHORT" ;;
    esac
    say "$label: ${LAST%%;*}"
}
installed_json() { sed -n -E "s/^ *\"$1\": *\"([^\"]*)\".*/\1/p" "$PREFIX/install.json" | head -1; }
installed_flag() { sed -n -E "s/^ *\"$1\": *(true|false).*/\1/p" "$PREFIX/install.json" | head -1; }
expect_eq() {  # <what> <got> <want>
    [ "$2" = "$3" ] || die "$1 is '$2', not '$3'"
}

# The entry, as a person runs it from the checkout; the run's artifacts only ever by --archive.
entry() { bash "$CHECKOUT/scripts/install.sh" "$@"; }

# BEFORE-TAG by its own installer: this entry's prelude fetches the tag's install.sh and runs it with these arguments.
install_before() {
    local vb="${BEFORE#v}"
    # shellcheck disable=SC2086
    run before entry --version "$BEFORE" $ROLE_FLAGS
    [ "$RC" = 0 ] || die "before: $BEFORE's installer ended with exit $RC; its last line: $LAST"
    expect_eq "before: install.json version" "$(installed_json version)" "$vb"
    say "before: $BEFORE is installed"
}
# The state every leg ends in: install.json, the checkout, the binaries, the installed apply, the wrapper and comm are the
# new release's. The wrapper is what the new release's library renders (the installed checkout's own); a release that
# changes nothing in it leaves the release before's, which is right, so no leg asks for a different one.
state_is_new() {  # <label>
    local want="$SMOKE/wrapper.want" fe=true
    [ "$SCENARIO" != fresh-be-only ] || fe=false
    expect_eq "$1: install.json version" "$(installed_json version)" "$VER"
    expect_eq "$1: install.json commit" "$(installed_json commit)" "$COMMIT"
    expect_eq "$1: install.json daemon" "$(installed_flag daemon)" true
    expect_eq "$1: install.json frontend" "$(installed_flag frontend)" "$fe"
    expect_eq "$1: the checkout repo/current names" "$(git -C "$PREFIX/repo/current" rev-parse HEAD)" "$COMMIT"
    case "$("$PREFIX/bin/sotd" --version)" in "sotd $VER ($SHORT "*) ;; *) die "$1: bin/sotd is not $VER at $SHORT" ;; esac
    expect_eq "$1: the comm stamp" "$(sed -e 's/-dirty$//' -e 1q "$HOME/.sot-comm/VERSION")" "$SHORT"
    cmp -s "$PREFIX/bin/sot-apply" "$SMOKE/x/$NAME/sot-apply" || die "$1: bin/sot-apply is not the release's own"
    [ "$fe" = true ] || return 0
    ( . "$PREFIX/repo/current/scripts/lib/sot-daemon.sh" && render_sot_launch "$PREFIX" "$TARGET" "$want" ) \
        || die "$1: render_sot_launch of the installed checkout failed"
    cmp -s "$want" "$HOME/.local/bin/sot-launch" || die "$1: $HOME/.local/bin/sot-launch is not what $VER's render_sot_launch writes"
}
repair_run() {
    run repair entry --archive "$PACK"
    verdict repair "$VER"
    grep -q 'a repair run' "$SMOKE/repair.out" || die "repair: the run did not say it is a repair run"
}

case "$SCENARIO" in
    fresh-local|fresh-be-only)
        # shellcheck disable=SC2086
        run fresh entry --archive "$PACK" $ROLE_FLAGS
        verdict fresh "$VER"
        state_is_new fresh
        repair_run
        state_is_new repair ;;
    update-entry)
        install_before
        run update entry --archive "$PACK"
        verdict update "$VER"
        grep -q "moves the install from ${BEFORE#v} to $VER" "$SMOKE/update.out" || die "update: the run did not say it moves the install from ${BEFORE#v}"
        state_is_new update
        repair_run
        state_is_new repair ;;
    update-implicit)
        install_before
        vb="${BEFORE#v}"
        # A release before that predates the pin records none: then only the repair half is proved, and the line says so.
        PINNED_BEFORE="$(installed_flag pinned)"
        if [ "$PINNED_BEFORE" = true ]; then say "before: $BEFORE pinned the install, as a named run does"
        else say "note: $BEFORE records no pin, so this leg cannot prove the pin's removal"; fi
        # No arguments: the prelude resolves the channel against the real repository, runs that tag's own installer as an
        # implicit run, and the role comes from the record.
        run implicit entry
        case "$LAST" in
            "PASS: Ship of Tools $vb at "*) [ "$RC" = 0 ] || die "implicit: PASS with exit $RC" ;;
            *) die "implicit: the last line is '$LAST' (exit $RC), not PASS for $vb (the channel must resolve to $BEFORE)" ;;
        esac
        case "$LAST" in *"pinned at"*) die "implicit: the run pinned the install: $LAST" ;; esac
        grep -q 'a repair run' "$SMOKE/implicit.out" || die "implicit: the run did not say it is a repair run at $vb"
        expect_eq "implicit: install.json version" "$(installed_json version)" "$vb"
        expect_eq "implicit: install.json pinned" "$(installed_flag pinned)" ""
        # The pin is gone, so an update would be admitted: the run's release must read as newer and arm.
        run stage "$NEW_SOTD" update stage "$PACK" --prefix "$PREFIX"
        [ "$RC" = 0 ] || die "stage: sotd update stage ended with exit $RC; its last line: $LAST"
        grep -q "armed .*(installed .*: newer)" "$SMOKE/stage.out" || die "stage: the run's release did not read as newer than $vb: $LAST"
        [ -f "$PREFIX/updates/pending-$TARGET.json" ] || die "stage: no pointer is armed" ;;
    update-apply)
        install_before
        # The run's release staged and armed by the release before's own sotd, as its daemon's updater does; then the
        # INSTALLED apply, as the unit's ExecStartPre (or the wrapper) runs it. It hands off to the staged copy, which is
        # the new release's own.
        run stage "$PREFIX/bin/sotd" update stage "$PACK" --prefix "$PREFIX"
        [ "$RC" = 0 ] || die "stage: sotd update stage ended with exit $RC; its last line: $LAST"
        [ -f "$PREFIX/updates/pending-$TARGET.json" ] || die "stage: no pointer is armed"
        run apply "$PREFIX/bin/sot-apply"
        [ "$RC" = 0 ] || die "apply: the installed sot-apply ended with exit $RC"
        grep -q 're-executing the staged sot-apply' "$SMOKE/apply.out" || die "apply: the installed apply did not hand off to the staged release's own"
        grep -q "sot-apply: APPLIED v$VER" "$SMOKE/apply.out" || die "apply: the staged apply did not say APPLIED v$VER"
        [ ! -e "$PREFIX/updates/pending-$TARGET.json" ] || die "apply: the pointer is still armed"
        state_is_new apply
        repair_run
        state_is_new repair ;;
esac
say "PASS $SCENARIO: $VER at $SHORT on $TARGET"
