#!/bin/sh
# sot-apply — fast, OFFLINE apply of an armed pending update (ADR 0030 Phase
# C3). Consumes <prefix>/updates/pending.json (written by the stage → prepare
# → arm pipeline): re-verifies the staged archive digest and the prepared
# worktree's commit, swaps binaries (keeping .prev), flips the repo/current
# symlink, rewrites install.json, clears the pointer, prunes old versions.
#
# FAIL-OPEN BY CONTRACT: every problem exits 0 with a "sot-apply:" log line
# and leaves the CURRENT version untouched — a broken update path must never
# prevent a launch (same rule as the Windows supervisor). Only a fully
# verified transaction changes anything. Invoked by:
#   - sot-launch (all Unix roles), before starting anything;
#   - systemd:  ExecStartPre=-<prefix>/bin/sot-apply  (the '-' double-guards);
#   - sotd's update.apply op (arms + exits; the restart path runs this).
#
# No network, no Julia, no npm — those all happened at prepare time.
# Installed to <prefix>/bin/ by install.sh; keep POSIX sh (macOS dash/bash).

set -u

log() { echo "sot-apply: $*" >&2; }

# Prefix = parent of this script's bin dir.
SELF="$0"
case "$SELF" in */*) ;; *) SELF="$(command -v -- "$0" || echo "$0")";; esac
BIN_DIR="$(cd "$(dirname -- "$SELF")" && pwd)"
PREFIX="$(dirname -- "$BIN_DIR")"
UPDATES="$PREFIX/updates"
PENDING="$UPDATES/pending.json"

# ---- rollback mode -----------------------------------------------------------
# sot-apply --rollback: restore the last-good transaction after a crash-loop
# (invoked by sot-launch's supervisor). Restores .prev binaries, flips
# repo/current back to the recorded checkout, marks the failed version bad so
# it is never re-armed, and rewrites install.json. Same fail-open contract.
if [ "${1:-}" = "--rollback" ]; then
    LG="$UPDATES/last-good.json"
    [ -f "$LG" ] || { log "rollback requested but no last-good.json — nothing to do"; exit 0; }
    lgfield() { sed -n 's/^ *"'"$1"'": *"\([^"]*\)".*/\1/p' "$LG" | head -1; }
    LG_TAG="$(lgfield tag)"
    LG_CHECKOUT="$(lgfield checkout)"
    BAD_TAG="$(sed -n 's/^ *"tag": *"\([^"]*\)".*/\1/p' "$PREFIX/install.json" 2>/dev/null | head -1)"
    [ -n "$LG_CHECKOUT" ] && [ -d "$LG_CHECKOUT" ] || {
        log "last-good checkout '$LG_CHECKOUT' missing — cannot roll back"; exit 0; }
    if [ "$BAD_TAG" = "$LG_TAG" ]; then
        log "install already at last-good $LG_TAG — nothing to roll back"; exit 0
    fi
    log "ROLLING BACK to $LG_TAG (marking $BAD_TAG bad)"
    for b in sot sotd; do
        [ -f "$PREFIX/bin/$b.prev" ] && cp -p "$PREFIX/bin/$b.prev" "$PREFIX/bin/$b"
    done
    ln -sfn "$LG_CHECKOUT" "$PREFIX/repo/current"
    ln -sfn "$LG_CHECKOUT" "$PREFIX/julia/current" 2>/dev/null
    case "$BAD_TAG" in
        v[0-9]*) : > "$UPDATES/bad-$BAD_TAG" ;;
    esac
    if [ -f "$PREFIX/install.json" ] && [ -n "$LG_TAG" ]; then
        sed -e 's|"version": *"[^"]*"|"version": "'"${LG_TAG#v}"'"|' \
            -e 's|"tag": *"[^"]*"|"tag": "'"$LG_TAG"'"|' \
            "$PREFIX/install.json" > "$PREFIX/install.json.tmp" \
            && mv -f "$PREFIX/install.json.tmp" "$PREFIX/install.json"
    fi
    rm -f "$UPDATES/pending.json"
    log "rollback complete — running $LG_TAG"
    exit 0
fi

[ -f "$PENDING" ] || exit 0

# ---- staging lock (mkdir; shared with the Rust updater) ----------------------
LOCK="$UPDATES/.lock"
if ! mkdir "$LOCK" 2>/dev/null; then
    log "staging lock held — skipping apply this launch"
    exit 0
fi
printf '%s@%s\n' "$$" "$(hostname 2>/dev/null || echo unknown)" > "$LOCK/owner" 2>/dev/null || true
cleanup() { rm -f "$LOCK/owner" 2>/dev/null; rmdir "$LOCK" 2>/dev/null; }
trap cleanup EXIT INT TERM

# ---- parse the pointer (our own pretty-printed JSON: one key per line) -------
field() { sed -n 's/^ *"'"$1"'": *"\([^"]*\)".*/\1/p' "$PENDING" | head -1; }
TAG="$(field tag)"
TARGET="$(field target)"
CHECKOUT="$(field checkout)"
COMMIT="$(field commit)"
ASSET="$(field asset)"
ASSET_SHA="$(field asset_sha256)"

case "$TAG" in
    v[0-9]*) ;;
    *) log "pending tag '$TAG' fails validation — ignoring pointer"; exit 0 ;;
esac
case "$TAG" in
    */*|*..*|*\\*) log "pending tag '$TAG' contains path material — ignoring pointer"; exit 0 ;;
esac
[ -n "$TARGET" ] && [ -n "$CHECKOUT" ] && [ -n "$COMMIT" ] && [ -n "$ASSET" ] && [ -n "$ASSET_SHA" ] || {
    log "pending pointer is missing fields — ignoring"; exit 0; }
case "$TARGET" in
    */*|*..*|*\\*) log "pending target '$TARGET' contains path material — ignoring pointer"; exit 0 ;;
esac

# Stage dirs are keyed <tag>-<target> (shared-\$HOME machines of different
# platforms share one updates root).
READY="$UPDATES/$TAG-$TARGET"
TOP="${ASSET%.tar.gz}"; TOP="${TOP%.zip}"
STAGED="$READY/$TOP"

# Already at this version? Clear the stale pointer and move on.
CUR_TAG="$(sed -n 's/^ *"tag": *"\([^"]*\)".*/\1/p' "$PREFIX/install.json" 2>/dev/null | head -1)"
if [ "$CUR_TAG" = "$TAG" ]; then
    log "install is already at $TAG — clearing stale pending pointer"
    rm -f "$PENDING"
    exit 0
fi

# ---- verify the whole transaction BEFORE touching anything -------------------
[ -f "$READY/manifest.json" ] || { log "no ready manifest for $TAG — not applying"; exit 0; }
[ -d "$CHECKOUT" ] || { log "prepared checkout $CHECKOUT missing — not applying"; exit 0; }
HEAD="$(git -C "$CHECKOUT" rev-parse HEAD 2>/dev/null)"
[ "$HEAD" = "$COMMIT" ] || {
    log "prepared checkout HEAD ($HEAD) != pinned commit ($COMMIT) — not applying"; exit 0; }

# Re-verify the staged archive digest (mutable-dir defense; cheap, offline).
if command -v sha256sum >/dev/null 2>&1; then
    GOT="$(sha256sum "$READY/$ASSET" 2>/dev/null | cut -d' ' -f1)"
elif command -v shasum >/dev/null 2>&1; then
    GOT="$(shasum -a 256 "$READY/$ASSET" 2>/dev/null | cut -d' ' -f1)"
else
    GOT=""
fi
if [ -z "$GOT" ] || [ "$GOT" != "$ASSET_SHA" ]; then
    log "staged archive digest mismatch or unreadable ($READY/$ASSET) — not applying"
    exit 0
fi

NEW_SOT="$STAGED/sot"
NEW_SOTD="$STAGED/sotd"
[ -f "$NEW_SOTD" ] || { log "staged sotd missing — not applying"; exit 0; }

# ---- record last-good for rollback ------------------------------------------
PREV_CHECKOUT="$(readlink "$PREFIX/repo/current" 2>/dev/null || echo "")"
{
    printf '{\n'
    printf '  "tag": "%s",\n' "$CUR_TAG"
    printf '  "checkout": "%s"\n' "$PREV_CHECKOUT"
    printf '}\n'
} > "$UPDATES/last-good.json.tmp" && mv -f "$UPDATES/last-good.json.tmp" "$UPDATES/last-good.json"

# ---- the flip (binaries, then pointers) -------------------------------------
# sot-apply itself is in the list: the new-inode mv is safe against the
# RUNNING copy of this script (sh keeps its fd on the old inode; same trick
# as the launcher self-update prelude).
for b in sot sotd sot-apply; do
    [ -f "$STAGED/$b" ] || continue
    [ -f "$PREFIX/bin/$b" ] && cp -p "$PREFIX/bin/$b" "$PREFIX/bin/$b.prev" 2>/dev/null
    if ! install -m 0755 "$STAGED/$b" "$PREFIX/bin/$b.new" 2>/dev/null \
       || ! mv -f "$PREFIX/bin/$b.new" "$PREFIX/bin/$b"; then
        log "installing $b failed — restoring previous binaries"
        for r in sot sotd sot-apply; do
            [ -f "$PREFIX/bin/$r.prev" ] && cp -p "$PREFIX/bin/$r.prev" "$PREFIX/bin/$r"
        done
        exit 0
    fi
    # macOS: curl'd staged files carry no quarantine attr, but strip
    # defensively (a browser-downloaded sideload does).
    command -v xattr >/dev/null 2>&1 && xattr -d com.apple.quarantine "$PREFIX/bin/$b" 2>/dev/null
done

ln -sfn "$CHECKOUT" "$PREFIX/repo/current"
mkdir -p "$PREFIX/julia" && ln -sfn "$CHECKOUT" "$PREFIX/julia/current"

# ---- rewrite install.json (preserve role/prefix/config/service) --------------
VERSION="${TAG#v}"
if [ -f "$PREFIX/install.json" ]; then
    sed -e 's|"version": *"[^"]*"|"version": "'"$VERSION"'"|' \
        -e 's|"tag": *"[^"]*"|"tag": "'"$TAG"'"|' \
        -e 's|"commit": *"[^"]*"|"commit": "'"$COMMIT"'"|' \
        "$PREFIX/install.json" > "$PREFIX/install.json.tmp" \
        && mv -f "$PREFIX/install.json.tmp" "$PREFIX/install.json"
fi

rm -f "$PENDING"

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

log "APPLIED $TAG (previous: ${CUR_TAG:-unknown}; rollback state in updates/last-good.json)"
exit 0
