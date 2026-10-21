#!/usr/bin/env bash
# install.sh -- install or update Ship of Tools on Linux or macOS (scripts/CLAUDE.md, "Installers"): the bootstrap of
# `sotd install`.
#
#   curl -fsSL <raw-url>/scripts/install.sh | bash -s -- --local
#   bash ~/.local/share/sot/repo/current/scripts/install.sh    # UPDATE this box: "update sot from the repo"
#   options: [--local | --backend <ssh-alias> | --be-only] [--version vX.Y.Z | --archive <folder>] [--prefix <dir>]
#            [--hub <ssh-alias>] [--no-service] [--force-role-change] [--restart-daemon]
#   SOT_INSTALL_TAG=<tag> ./scripts/install.sh ...   # run this body on that tag's release, skipping the prelude
#
# The prelude runs the target tag's own install.sh: the tag --version names, else the channel the prefix runs (no
# install or a stable release: GitHub's latest stable release; a pre-release or a build of the line: the newest
# published release), told it is an implicit run. The body fetches that release's SHA256SUMS, archive and COMMIT (or
# copies them from the --archive folder, which skips the prelude), checks every file against SHA256SUMS, unpacks the
# archive and runs its own `sotd install` with the same arguments. That does the install, refuses what it must, and
# prints the one verdict: `PASS: ...`, `ONE STEP LEFT: ...` (exit 0) or `FAIL: <check>: <detail>` (exit 1, 2 for a
# refusal). `sotd install --help` says what each option does. Interrupted or failed, run it again: a repeat repairs.
set -euo pipefail

REPO="${SOT_INSTALL_REPO:-kalidke/ship-of-tools}"
PREFIX="${SOT_PREFIX:-$HOME/.local/share/sot}"

# A run that stops before `sotd install` prints its own verdict line: `FAIL: <step>: <detail>`.
STEP=install
VERDICT_PRINTED=0
fail() {  # <exit code> <detail>
    VERDICT_PRINTED=1
    printf 'FAIL: %s: %s\n' "$STEP" "$2"
    exit "$1"
}
die() { fail 1 "$*"; }
# The comm library's two cleanup-trap helpers, copied: this script does not load the library
# (comm/lib/comm-lib-base.sh, cleanup traps).
sot_cleanup_signals() {
    local s; _SOT_SIG=""
    for s in HUP:129 INT:130 TERM:143; do trap "trap '' HUP INT TERM; _SOT_SIG=${s%:*}; set +e; exit ${s#*:}" "${s%:*}"; done
}
sot_end_by_signal() { [ -z "${_SOT_SIG:-}" ] || { trap - "$_SOT_SIG"; exec sh -c 'kill -s "$1" "$$"' sh "$_SOT_SIG"; }; }
# The one exit trap: it removes the run's scratch (WORK, and the prelude's TMP_INSTALLER) and, when the run ends with no
# verdict line (a command that failed under set -e, an interrupt before sotd install ran), prints the FAIL line itself.
WORK="" TMP_INSTALLER=""
on_exit() {
    local rc=$?
    trap '' HUP INT TERM
    [ -z "$TMP_INSTALLER" ] || rm -f "${TMP_INSTALLER:?}"
    [ -z "$WORK" ] || rm -rf "${WORK:?}"
    [ "$VERDICT_PRINTED" = 1 ] || printf 'FAIL: %s: stopped (exit %s)\n' "$STEP" "$rc"
    sot_end_by_signal
}
# curl gives up on a connect past 15 s or a transfer under 1000 B/s for 60 s.
CURL_BOUND=(--connect-timeout 15 --speed-limit 1000 --speed-time 60)

# The version the prefix runs, for the channel: its install.json's (one key per line), else its sotd's own.
installed_version() {  # <prefix>
    local v=""
    [ ! -f "$1/install.json" ] || v="$(sed -n -E 's/^ *"version": *"([^"]*)".*/\1/p' "$1/install.json" | sed -n 1p)"
    if [ -z "$v" ] && [ -x "$1/bin/sotd" ]; then
        v="$("$1/bin/sotd" --version 2>/dev/null | sed -n 1p | awk '{ print $2 }')" || v=""
    fi
    printf '%s' "$v"
}
# The newest published release of any kind: its tag, else a failure that prints why. The listing is an API call; it
# carries GITHUB_TOKEN (or GH_TOKEN) when one is set, since GitHub limits anonymous calls per network address.
newest_release() {
    local out code tok auth=() rc=0
    tok="${GITHUB_TOKEN:-${GH_TOKEN:-}}"
    [ -z "$tok" ] || auth=(-H "Authorization: Bearer $tok")
    out="$(curl "${CURL_BOUND[@]}" -fsSL -w '\n%{http_code}' -H 'Accept: application/vnd.github+json' ${auth[@]+"${auth[@]}"} "https://api.github.com/repos/$REPO/releases?per_page=1")" || rc=$?
    code="${out##*$'\n'}"
    if [ "$rc" != 0 ]; then
        case "$code" in
            403|429)
                if [ -n "$tok" ]; then printf 'GitHub refused the request (HTTP %s, its rate limit) even with the token in GITHUB_TOKEN or GH_TOKEN; retry in an hour, or name the release with --version <tag>, which needs no listing\n' "$code"
                else printf 'GitHub refused the anonymous request (HTTP %s, its rate limit for this network address); retry in an hour, set GITHUB_TOKEN to send a token, or name the release with --version <tag>, which needs no listing\n' "$code"; fi ;;
        esac
        return 1
    fi
    printf '%s\n' "${out%$'\n'*}" | grep -o '"tag_name": *"[^"]*"' | sed -n 1p | sed -E 's/.*: *"([^"]*)"/\1/'
}

trap on_exit EXIT
sot_cleanup_signals

# The three options the bootstrap reads; every option goes on to sotd install as given, which reads them all.
STEP=options
VERSION="" ARCHIVE="" prev=""
for a in "$@"; do
    case "$prev" in
        --version) VERSION="$a" ;;
        --archive) ARCHIVE="$a" ;;
        --prefix) PREFIX="$a" ;;
    esac
    prev="$a"
done
case "$prev" in --version|--archive|--prefix) fail 2 "$prev needs a value" ;; esac

# ---- the prelude: run the target tag's own installer, not this one's body ----------------------------------------
if [ -z "${SOT_INSTALL_TAG:-}" ] && [ -z "$ARCHIVE" ]; then
    STEP=target
    command -v curl >/dev/null || die "curl is required"
    tag="$VERSION" implicit=0
    if [ -z "$tag" ]; then
        implicit=1
        installed="$(installed_version "$PREFIX")"
        case "$installed" in
            *+*|*-*) tag="$(newest_release)" || die "could not list the releases of $REPO${tag:+: $tag}" ;;
            *)
                loc="$(curl "${CURL_BOUND[@]}" -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest")" \
                    || die "could not resolve the latest release of $REPO"
                tag="${loc##*/}" ;;
        esac
    fi
    case "$tag" in v[0-9]*) ;; *) die "could not resolve a release tag (got '$tag')" ;; esac
    TMP_INSTALLER="$(mktemp "${TMPDIR:-/tmp}/tmp.XXXXXXXXXX")"
    url="https://raw.githubusercontent.com/$REPO/$tag/scripts/install.sh"
    curl "${CURL_BOUND[@]}" -fsSL -o "$TMP_INSTALLER" "$url" || die "could not fetch $url"
    # The tag's own installer prints the verdict of what it did, a refusal or an interrupt included.
    VERDICT_PRINTED=1
    SOT_INSTALL_TAG="$tag" SOT_INSTALL_IMPLICIT="$implicit" bash "$TMP_INSTALLER" "$@"
    exit
fi

# ---- the body: the pack, checked, then its own sotd install ----------------------------------------------------------
STEP=preflight
case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) TARGET=linux-x86_64 ;;
    Darwin-arm64) TARGET=macos-aarch64 ;;
    Linux-*|Darwin-*) die "no prebuilt release for $(uname -s) $(uname -m) (Linux x86_64 and macOS on Apple silicon have one); build from source" ;;
    MINGW*|MSYS*|CYGWIN*) die "this installer covers Linux and macOS; on Windows run scripts/install.ps1 (docs/INSTALL-AGENT.md)" ;;
    *) die "unsupported system $(uname -s); Linux and macOS: this installer, Windows: scripts/install.ps1" ;;
esac
for t in tar sed awk; do command -v "$t" >/dev/null || die "$t is required"; done
if command -v sha256sum >/dev/null; then SUM=(sha256sum); elif command -v shasum >/dev/null; then SUM=(shasum -a 256); else die "sha256sum or shasum is required"; fi
WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-install.XXXXXX")"

STEP=download
if [ -n "$ARCHIVE" ]; then
    ARCHIVE="$(cd "$ARCHIVE" 2>/dev/null && pwd)" || fail 2 "--archive: no such folder"
    [ -f "$ARCHIVE/SHA256SUMS" ] || die "$ARCHIVE has no SHA256SUMS"
    cp "$ARCHIVE/SHA256SUMS" "$WORK/SHA256SUMS" || die "cannot copy SHA256SUMS from $ARCHIVE"
else
    command -v curl >/dev/null || die "curl is required"
    VERSION="${VERSION:-$SOT_INSTALL_TAG}"
    case "$VERSION" in v[0-9]*) ;; *) fail 2 "--version takes a release tag (vX.Y.Z), not '$VERSION'" ;; esac
    base="https://github.com/$REPO/releases/download/$VERSION"
    curl "${CURL_BOUND[@]}" -fsSL -o "$WORK/SHA256SUMS" "$base/SHA256SUMS" || die "SHA256SUMS not found on release $VERSION"
fi
# The archive is the one SHA256SUMS names for this system; its name is compared as text (a + is no pattern).
names="$(sed -n -E "s/^[0-9a-fA-F]{64} [ *](sot-.+-$TARGET\.tar\.gz)\$/\1/p" "$WORK/SHA256SUMS")"
[ -n "$names" ] && [ "$(printf '%s\n' "$names" | wc -l | tr -d ' ')" = 1 ] || die "SHA256SUMS must name exactly one sot-<version>-$TARGET.tar.gz"
ver="${names#sot-}"; ver="${ver%-"$TARGET".tar.gz}"
[[ "$ver" =~ ^[0-9A-Za-z.+-]+$ ]] || die "unsafe version '$ver' in SHA256SUMS"
ASSETS=("$names" COMMIT)
if [ -n "$ARCHIVE" ]; then
    [ ! -f "$ARCHIVE/LIBC" ] || ASSETS+=(LIBC)
    for a in "${ASSETS[@]}"; do
        cp "$ARCHIVE/$a" "$WORK/$a" || die "cannot copy $a from $ARCHIVE"
    done
else
    printf 'downloading %s assets of %s\n' "${#ASSETS[@]}" "$VERSION"
    for a in "${ASSETS[@]}"; do
        curl "${CURL_BOUND[@]}" -fsSL -o "$WORK/$a" "$base/$a" || die "asset $a not found on release $VERSION"
    done
fi
# Every file copied or downloaded is one SHA256SUMS lists, and matches it, before anything of the pack runs.
STEP=verify
for a in "${ASSETS[@]}"; do
    awk -v n="$a" '{ f = $0; if (sub(/^[0-9a-fA-F]+ [ *]/, "", f) && f == n) ok = 1 } END { exit !ok }' "$WORK/SHA256SUMS" \
        || die "$a is not listed in SHA256SUMS; every file of the pack must be checksummed"
done
( cd "$WORK" && "${SUM[@]}" -c --ignore-missing SHA256SUMS ) || die "checksum verification FAILED"

STEP=unpack
tar -xzf "$WORK/$names" -C "$WORK" || die "cannot unpack $names"
SOTD="$WORK/sot-$ver-$TARGET/sotd"
if ! err="$("$SOTD" --version 2>&1 >/dev/null)"; then
    printf '%s\n' "$err"
    die "the unpacked sotd cannot run on this host (its error is above)"
fi
help="$("$SOTD" install --help 2>/dev/null)" || help=""
case "$help" in
    *"Usage: sotd install"*) ;;
    *) die "release $ver predates sotd install: install it with its own installer (the one-liner without SOT_INSTALL_TAG, or that tag's scripts/install.sh)" ;;
esac

# sotd install prints the verdict, a stop by a signal included (exit 128 plus the signal); one that ends any other way
# without exit 0, 1 or 2 is named here.
STEP=install
VERDICT_PRINTED=1
rc=0
"$SOTD" install --pack "$WORK" "$@" || rc=$?
case "$rc" in
    0|1|2|129|130|143) ;;
    *) printf 'FAIL: install: sotd install ended with exit %s and no verdict\n' "$rc"; rc=1 ;;
esac
exit "$rc"
