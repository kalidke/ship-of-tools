#!/bin/sh
# sot-apply — fast, OFFLINE apply of an armed pending update (ADR 0030 Phase
# C3). Consumes <prefix>/updates/pending-<target>.json (written by the stage
# → prepare → arm pipeline): re-verifies the staged tree (per-file digests
# from files.sha256, archive digest, prepared worktree commit + cleanliness),
# swaps binaries (keeping .prev), flips the repo/current symlink, rewrites
# install.json, records last-good + a just-applied health marker, clears the
# pointer, prunes old versions. All transaction state is namespaced by
# TARGET: a shared $HOME serves several platforms from one updates root.
#
# FAIL-OPEN BY CONTRACT: every problem exits 0 with a "sot-apply:" log line.
# A verification failure BEFORE mutation leaves everything untouched (and
# clears a pointer whose stage is damaged, so the daemon re-stages instead
# of looping). A failure AFTER mutation restores the previous binaries and
# symlinks before exiting. Invoked by:
#   - sot-launch (all Unix roles) — which stops/restarts the daemon around it;
#   - systemd:  ExecStartPre=-<prefix>/bin/sot-apply  (daemon already stopped);
#   - sotd's update.apply op (arms + exits; the restart path runs this).
#
# No network, no Julia, no npm — those all happened at prepare time.
# Installed to <prefix>/bin/ by install.sh and shipped inside release
# archives; keep POSIX sh (macOS dash/bash).

set -u

log() { echo "sot-apply: $*" >&2; }

# Prefix = parent of this script's bin dir.
SELF="$0"
case "$SELF" in */*) ;; *) SELF="$(command -v -- "$0" || echo "$0")";; esac
BIN_DIR="$(cd "$(dirname -- "$SELF")" && pwd)"
PREFIX="$(dirname -- "$BIN_DIR")"
UPDATES="$PREFIX/updates"

# ---- host target (never trust the pointer's word for what THIS box is) ------
case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)  TARGET=linux-x86_64 ;;
    Darwin-arm64)  TARGET=macos-aarch64 ;;
    *) log "platform $(uname -s)-$(uname -m) not in the release matrix — nothing to apply"; exit 0 ;;
esac

PENDING="$UPDATES/pending-$TARGET.json"
LASTGOOD="$UPDATES/last-good-$TARGET.json"
MARKER="$UPDATES/just-applied-$TARGET"

sha_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" 2>/dev/null | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" 2>/dev/null | cut -d' ' -f1
    fi
}

# Everything below (rollback included) runs under the staging lock shared
# with the Rust updater — arm/apply/rollback are one critical section.
LOCK="$UPDATES/.lock"
[ -d "$UPDATES" ] || exit 0
if ! mkdir "$LOCK" 2>/dev/null; then
    log "staging lock held — skipping this launch"
    exit 0
fi
printf '%s@%s#sh\n' "$$" "$(hostname 2>/dev/null || echo unknown)" > "$LOCK/owner" 2>/dev/null || true
cleanup() { rm -f "$LOCK/owner" 2>/dev/null; rmdir "$LOCK" 2>/dev/null; }
trap cleanup EXIT INT TERM

# ---- rollback mode -----------------------------------------------------------
# sot-apply --rollback: restore the last-good transaction after a crash-loop
# (invoked by sot-launch's supervisor, gated there on a FRESH just-applied
# marker). Restores .prev binaries, flips repo/current back, marks the failed
# version bad so it is never re-armed, rewrites install.json.
if [ "${1:-}" = "--rollback" ]; then
    [ -f "$LASTGOOD" ] || { log "rollback requested but no last-good state — nothing to do"; exit 0; }
    lgfield() { sed -n 's/^ *"'"$1"'": *"\([^"]*\)".*/\1/p' "$LASTGOOD" | head -1; }
    LG_TAG="$(lgfield tag)"
    LG_CHECKOUT="$(lgfield checkout)"
    BAD_TAG="$(sed -n 's/^ *"tag": *"\([^"]*\)".*/\1/p' "$PREFIX/install.json" 2>/dev/null | head -1)"
    [ -n "$LG_CHECKOUT" ] && [ -d "$LG_CHECKOUT" ] || {
        log "last-good checkout '$LG_CHECKOUT' missing — cannot roll back"; exit 0; }
    if [ "$BAD_TAG" = "$LG_TAG" ]; then
        log "install already at last-good $LG_TAG — nothing to roll back"; exit 0
    fi
    log "ROLLING BACK to $LG_TAG (marking $BAD_TAG bad for $TARGET)"
    for b in sot sotd sot-capsule sot-apply; do
        [ -f "$PREFIX/bin/$b.prev" ] && cp -p "$PREFIX/bin/$b.prev" "$PREFIX/bin/$b"
    done
    ln -sfn "$LG_CHECKOUT" "$PREFIX/repo/current"
    ln -sfn "$LG_CHECKOUT" "$PREFIX/julia/current" 2>/dev/null
    case "$BAD_TAG" in
        v[0-9]*) : > "$UPDATES/bad-$BAD_TAG-$TARGET" ;;
    esac
    if [ -f "$PREFIX/install.json" ] && [ -n "$LG_TAG" ]; then
        sed -e 's|"version": *"[^"]*"|"version": "'"${LG_TAG#v}"'"|' \
            -e 's|"tag": *"[^"]*"|"tag": "'"$LG_TAG"'"|' \
            "$PREFIX/install.json" > "$PREFIX/install.json.tmp" \
            && mv -f "$PREFIX/install.json.tmp" "$PREFIX/install.json"
    fi
    rm -f "$PENDING" "$MARKER"
    log "rollback complete — running $LG_TAG"
    exit 0
fi

[ -f "$PENDING" ] || exit 0

# ---- parse the pointer (our own pretty-printed JSON: one key per line) -------
field() { sed -n 's/^ *"'"$1"'": *"\([^"]*\)".*/\1/p' "$PENDING" | head -1; }
TAG="$(field tag)"
PTR_TARGET="$(field target)"
CHECKOUT="$(field checkout)"
COMMIT="$(field commit)"
ASSET="$(field asset)"
ASSET_SHA="$(field asset_sha256)"

drop_pending() { rm -f "$PENDING"; }

case "$TAG" in
    v[0-9]*) ;;
    *) log "pending tag '$TAG' fails validation — dropping pointer"; drop_pending; exit 0 ;;
esac
case "$TAG" in
    */*|*..*|*\\*) log "pending tag '$TAG' contains path material — dropping pointer"; drop_pending; exit 0 ;;
esac
[ "$PTR_TARGET" = "$TARGET" ] || {
    log "pending pointer is for target '$PTR_TARGET', this host is $TARGET — dropping"; drop_pending; exit 0; }
[ -n "$CHECKOUT" ] && [ -n "$COMMIT" ] && [ -n "$ASSET" ] && [ -n "$ASSET_SHA" ] || {
    log "pending pointer is missing fields — dropping"; drop_pending; exit 0; }

# Stage dirs are keyed <tag>-<target> (shared roots serve several platforms).
READY="$UPDATES/$TAG-$TARGET"
TOP="${ASSET%.tar.gz}"; TOP="${TOP%.zip}"
STAGED="$READY/$TOP"

# Already at this version? Clear the stale pointer and move on.
CUR_TAG="$(sed -n 's/^ *"tag": *"\([^"]*\)".*/\1/p' "$PREFIX/install.json" 2>/dev/null | head -1)"
if [ "$CUR_TAG" = "$TAG" ]; then
    log "install is already at $TAG — clearing stale pending pointer"
    drop_pending
    exit 0
fi

# ---- verify the whole transaction BEFORE touching anything -------------------
# A damaged stage clears the pointer AND the stage dir: the daemon's next
# cycle re-stages fresh instead of auto-exiting into the same failure forever.
drop_damaged() {
    log "$1 — dropping pointer and damaged stage so the updater re-stages"
    drop_pending
    rm -rf "$READY"
    exit 0
}

[ -f "$READY/manifest.json" ] || drop_damaged "no ready manifest for $TAG"
[ -f "$STAGED/sot" ] || drop_damaged "staged sot missing"
[ -f "$STAGED/sotd" ] || drop_damaged "staged sotd missing"

# Archive digest (cheap) + per-file digests of the ACTUAL binaries we are
# about to install (the extracted tree is mutable independently of the
# archive).
GOT="$(sha_file "$READY/$ASSET")"
[ -n "$GOT" ] && [ "$GOT" = "$ASSET_SHA" ] || drop_damaged "staged archive digest mismatch"
if [ -f "$READY/files.sha256" ]; then
    if command -v sha256sum >/dev/null 2>&1; then
        (cd "$READY" && sha256sum -c --quiet files.sha256 >/dev/null 2>&1) \
            || drop_damaged "staged file digests do not verify"
    elif command -v shasum >/dev/null 2>&1; then
        (cd "$READY" && shasum -a 256 -c files.sha256 >/dev/null 2>&1) \
            || drop_damaged "staged file digests do not verify"
    fi
else
    log "note: stage has no files.sha256 (pre-C4 stage) — archive digest only"
fi

# Prepared worktree: exact commit, and no modified tracked files.
[ -d "$CHECKOUT" ] || { log "prepared checkout $CHECKOUT missing — dropping pointer"; drop_pending; exit 0; }
HEAD="$(git -C "$CHECKOUT" rev-parse HEAD 2>/dev/null)"
[ "$HEAD" = "$COMMIT" ] || { log "prepared checkout HEAD ($HEAD) != pinned commit ($COMMIT) — dropping pointer"; drop_pending; exit 0; }
DIRTY="$(git -C "$CHECKOUT" status --porcelain -uno 2>/dev/null)"
[ -z "$DIRTY" ] || { log "prepared checkout has modified tracked files — dropping pointer"; drop_pending; exit 0; }

# ---- record last-good (the pre-apply state) for rollback ---------------------
PREV_CHECKOUT="$(readlink "$PREFIX/repo/current" 2>/dev/null || echo "")"
{
    printf '{\n'
    printf '  "tag": "%s",\n' "$CUR_TAG"
    printf '  "checkout": "%s"\n' "$PREV_CHECKOUT"
    printf '}\n'
} > "$LASTGOOD.tmp" && mv -f "$LASTGOOD.tmp" "$LASTGOOD"

# ---- the flip: binaries, then pointers — all-or-restore ----------------------
restore_previous() {
    log "$1 — restoring previous binaries and pointers"
    for r in sot sotd sot-capsule sot-apply; do
        [ -f "$PREFIX/bin/$r.prev" ] && cp -p "$PREFIX/bin/$r.prev" "$PREFIX/bin/$r"
    done
    if [ -n "$PREV_CHECKOUT" ]; then
        ln -sfn "$PREV_CHECKOUT" "$PREFIX/repo/current" 2>/dev/null
        ln -sfn "$PREV_CHECKOUT" "$PREFIX/julia/current" 2>/dev/null
    fi
    # Pending stays: the stage verified clean, so the failure is local
    # (permissions, disk); retrying at the next launch is safe and fail-open.
    exit 0
}

# sot-apply itself is in the list: the new-inode mv is safe against the
# RUNNING copy of this script (sh keeps its fd on the old inode).
for b in sot sotd sot-capsule sot-apply; do
    [ -f "$STAGED/$b" ] || continue
    [ -f "$PREFIX/bin/$b" ] && cp -p "$PREFIX/bin/$b" "$PREFIX/bin/$b.prev" 2>/dev/null
    if ! install -m 0755 "$STAGED/$b" "$PREFIX/bin/$b.new" 2>/dev/null \
       || ! mv -f "$PREFIX/bin/$b.new" "$PREFIX/bin/$b"; then
        restore_previous "installing $b failed"
    fi
    # macOS: curl'd staged files carry no quarantine attr, but strip
    # defensively (a browser-downloaded sideload does).
    command -v xattr >/dev/null 2>&1 && xattr -d com.apple.quarantine "$PREFIX/bin/$b" 2>/dev/null
done

ln -sfn "$CHECKOUT" "$PREFIX/repo/current" || restore_previous "flipping repo/current failed"
[ "$(readlink "$PREFIX/repo/current")" = "$CHECKOUT" ] || restore_previous "repo/current did not flip"
mkdir -p "$PREFIX/julia" && ln -sfn "$CHECKOUT" "$PREFIX/julia/current" \
    || restore_previous "flipping julia/current failed"

# ---- rewrite install.json (preserve role/prefix/config/service) --------------
VERSION="${TAG#v}"
if [ -f "$PREFIX/install.json" ]; then
    if ! sed -e 's|"version": *"[^"]*"|"version": "'"$VERSION"'"|' \
             -e 's|"tag": *"[^"]*"|"tag": "'"$TAG"'"|' \
             -e 's|"commit": *"[^"]*"|"commit": "'"$COMMIT"'"|' \
             "$PREFIX/install.json" > "$PREFIX/install.json.tmp" \
       || ! mv -f "$PREFIX/install.json.tmp" "$PREFIX/install.json"; then
        restore_previous "rewriting install.json failed"
    fi
fi

# Success: arm the crash-loop health window, clear the pointer.
: > "$MARKER"
drop_pending

# ---- prune: keep the new and previous version dirs ---------------------------
PREV_KEEP="$(basename "$PREV_CHECKOUT" 2>/dev/null)"
for v in "$PREFIX/repo/versions"/*; do
    [ -d "$v" ] || continue
    case "$(basename "$v")" in
        "$TAG"|"$PREV_KEEP") ;;
        *)
            git -C "$PREFIX/repo/base" worktree remove --force "$v" 2>/dev/null || rm -rf "$v"
            ;;
    esac
done
git -C "$PREFIX/repo/base" worktree prune 2>/dev/null

# Old staged release dirs FOR THIS TARGET (keep the just-applied one for
# re-verification; other targets' stages belong to other machines).
for d in "$UPDATES"/v[0-9]*-"$TARGET"; do
    [ -d "$d" ] || continue
    [ "$(basename "$d")" = "$TAG-$TARGET" ] || rm -rf "$d"
done

log "APPLIED $TAG (previous: ${CUR_TAG:-unknown}; rollback state in $(basename "$LASTGOOD"))"
exit 0
