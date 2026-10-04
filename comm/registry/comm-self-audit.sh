#!/usr/bin/env bash
# comm-self-audit.sh — do the identity slots agree with the rows they are keyed
# by? One slot per session lives at `$SOT_COMM_HOME/self/<host>__<key>.txt`; a
# capsule row's key is its workspace id (`ws-<slug>-<hex>`, the slug derived
# from the row's label), while the handle and the `repo=`/`root=` lines inside
# are derived from the shell's cwd. Nothing compares the two at rest, so a slot
# can carry a DIFFERENT project's handle than the row it is keyed by — after
# which that row reads mail addressed to another session (the inbox is keyed by
# handle). comm-lib.sh's write-side guard stops new ones; this finds the ones
# already on disk, which is the only way one is noticed before misrouted mail
# is.
#
# The verdict per workspace-keyed slot:
#   agree    the key's slug and the slot's repo= slugify to the same string
#   benign   one is the other plus a suffix at a `-`/`_` boundary — a
#            deliberately suffixed row (a codex `-cx` row), a path-
#            disambiguated name, a test fixture
#   DIFFERS  neither — the defect: this slot names another project
#   no repo= a legacy slot with no repo= line; no evidence either way
#
# Slots keyed by anything other than a workspace id (the numeric tmux-era
# keys, and the shared `__nopane.txt` slot every no-pane shell on a host
# writes by design) carry no project claim in their key and are not judged.
#
# Usage: comm-self-audit.sh [-v]     (-v also lists the slots that agree)
# Exit: 0 when no slot names a different project, 1 when at least one does,
#       2 on a usage error or an unreadable slot directory.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=comm-lib.sh
source "$SCRIPT_DIR/comm-lib.sh"

VERBOSE=0
case "${1:-}" in
    "")        ;;
    -v)        VERBOSE=1 ;;
    -h|--help) sed -n '2,27p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *)         echo "comm-self-audit.sh: unknown argument '$1' (see --help)" >&2; exit 2 ;;
esac

[ -d "$SELF_DIR" ] || { echo "comm-self-audit.sh: no identity slots at '$SELF_DIR' — nothing joined on this box yet" >&2; exit 2; }

# A repo basename has to go through the daemon's label→slug rule before it can
# be compared with a key's slug at all (`MyPackage.jl` keys a row as
# `mypackage_jl`). That rule is `sot_slug` in the comm-lib.sh sourced above — a
# char-by-char mirror of Rust `slug()` (rust/protocol/src/topology/endpoint.rs)
# with its own tests. This script carried a second, sed-based copy, which agreed
# with `sot_slug` over every real `repo=` on this box but not in general: a
# literal repeated dash survives the daemon's keep-branch (`alpha- beta` →
# `alpha-beta` there, `alpha--beta` in the copy) and an empty label is `default`
# there and `''` in the copy. Either divergence makes a HEALTHY slot print
# DIFFERS and this script exit 1 — the cry-wolf outcome the false-positive rule
# below exists to prevent. One rule, one implementation, and it is the daemon's.

# 0 when $2 is $1 plus a suffix starting at a `-`/`_` boundary. The boundary is
# what keeps this from excusing a real disagreement: `alpha` is a bare prefix of
# `alphatools` and those are two different projects, while `alpha-cx` and
# `alpha-<parentdir>` continue their repo's slug at a separator.
suffixed_at_boundary() {
    local short="$1" long="$2" rest
    [ "${#short}" -lt "${#long}" ] || return 1
    rest="${long#"$short"}"
    [ "$rest" != "$long" ] || return 1
    case "$rest" in -*|_*) return 0 ;; *) return 1 ;; esac
}

agree=0; benign=0; differs=0; unknown=0; skipped=0
for slot in "$SELF_DIR"/*__*.txt; do
    [ -f "$slot" ] || continue
    key="${slot##*__}"; key="${key%.txt}"
    case "$key" in ws-?*) ;; *) skipped=$((skipped + 1)); continue ;; esac
    # `ws-<slug>-<hex>`: the slug is everything between the prefix and the
    # last dash-separated field (the per-creation suffix), so a slug that
    # contains dashes of its own survives intact.
    rest="${key#ws-}"
    slug="${rest%-*}"
    [ -n "$slug" ] && [ "$slug" != "$rest" ] || { skipped=$((skipped + 1)); continue; }
    name="$(sed -n '1p' "$slot" 2>/dev/null)"
    repo_line="$(sed -n '2p' "$slot" 2>/dev/null)"
    case "$repo_line" in
        repo=?*) repo="${repo_line#repo=}" ;;
        *)       unknown=$((unknown + 1))
                 [ "$VERBOSE" = 1 ] && echo "no repo=  $key  (@${name:-?})"
                 continue ;;
    esac
    repo_slug="$(sot_slug "$repo")"
    if [ "$slug" = "$repo_slug" ]; then
        agree=$((agree + 1))
        [ "$VERBOSE" = 1 ] && echo "agree     $key  (@${name:-?}, repo=$repo)"
    # One direction only: a row's LABEL continues its repo's slug (`alpha-cx`,
    # `alpha-<parentdir>` for a row whose repo is `alpha`), which is how the
    # variant and path-disambiguated rows are named. The reverse — a key
    # `ws-alpha-…` holding `repo=alpha-tools` — never occurred over this box's
    # 161 slots, has no example in the rule above and no test, and excusing it
    # would wave through exactly the cross-project slot this audit exists to
    # find: `alpha` and `alpha-tools` are two different checkouts.
    elif suffixed_at_boundary "$repo_slug" "$slug"; then
        benign=$((benign + 1))
        [ "$VERBOSE" = 1 ] && echo "benign    $key  (@${name:-?}, repo=$repo — suffixed, same project)"
    else
        differs=$((differs + 1))
        echo "DIFFERS   $key  names @${name:-?} with repo=$repo (key slug '$slug')"
    fi
done

echo "comm-self-audit: $agree agree, $benign benign, $differs differ, $unknown without a repo= line, $skipped not workspace-keyed"
[ "$differs" -eq 0 ] || {
    echo "A slot that names a different project sends that row's mail to the wrong reader. It self-heals the next time its own session runs comm-context.sh (the read side discards a handle whose root= doesn't match), so prefer letting the row start over hand-editing the file." >&2
    exit 1
}
exit 0
