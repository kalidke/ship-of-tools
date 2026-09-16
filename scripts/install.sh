#!/usr/bin/env bash
# install.sh — Ship of Tools installer for Linux/macOS (ADR 0030 §5).
#
#   curl -fsSL <raw-url>/scripts/install.sh | bash -s -- --local
#   ./scripts/install.sh --local                     # all-in-one on this box
#   ./scripts/install.sh --backend <ssh-alias>       # FE here → remote BE
#   ./scripts/install.sh --be-only                   # headless backend/canary
#   [--version vX.Y.Z] [--prefix <dir>] [--port <n>] [--no-service]
#   [--hub <ssh-alias>]     # this box does NOT share the hub's home: fetch
#                           # its hosts.toml (`sotd topology sync`) once staged
#   [--force-role-change]  # consent to installing over another prefix's live daemon
#                                                    # default: latest release
#   SOT_INSTALL_TAG=<tag> ./scripts/install.sh ...   # run THIS checkout's body
#
# Role: a declared hosts.toml naming this host (host_name()) wins — its
# daemon/frontend flags say what gets installed and enabled here, no
# --local/--backend/--be-only needed. Those flags are the fallback for a box
# with no entry yet (a brand-new user, or one not sharing the hub's home).
#
# What it does (idempotent; re-run to upgrade):
#   1. preflight — arch/glibc floor for the FE, tar/curl present (gh or
#      $GITHUB_TOKEN are OPTIONAL: authed calls dodge API rate limits)
#   2. download the release artifacts + verify SHA256SUMS
#   3. lay out $PREFIX (~/.local/share/sot): bin/ updates/ repo/current
#   4. REPO CHECKOUT at the release tag (ADR 0030 addendum: the repo IS the
#      manual and the resource tree; blobless partial clone = full history
#      for blame, only the tag's tree downloaded; supersedes the curated
#      julia bundle) + juliaup + Pkg.instantiate inside the checkout
#   5. config in ~/.config/sot: settings.toml stub if missing; hosts.toml is
#      read (role) and, with --hub, fetched — never written here
#   6. agent comm resources: ~/.sot-comm plus Claude/Codex skills
#   7. backend roles: install+enable the systemd --user sotd unit
#   8. FE roles: ~/.local/bin/sot-launch wrapper + app/desktop entry
#
# Development machines DON'T use this release installer — they run from a checkout.
set -euo pipefail

REPO="${SOT_INSTALL_REPO:-kalidke/ship-of-tools}"
PREFIX="${SOT_PREFIX:-$HOME/.local/share/sot}"
CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}/sot"
ROLE="" VERSION="" BE_ALIAS="" HUB_ALIAS="" PORT=18743 NO_SERVICE=0 FORCE_ROLE_CHANGE=0
GLIBC_FLOOR_FE="2.35"

say()  { printf '\033[1;36m==\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }

# Refuse characters a shell-embedded path (systemd unit ExecStart, JSON
# manifest, sed substitution, launcher heredocs) cannot carry safely.
# Explicit rejection over silent corruption. Shared by --prefix and the
# project root ($HOME) — deploy/sotd.service's ExecStart now embeds both
# inside a shell string, where a stray quote breaks the unit.
reject_unsafe_path_chars() {  # <label> <value>
    case "$2" in
        *[\&\|\;\"\'\\\`]*|*' '*|*'	'*)
            die "unsupported characters in $1 '$2' — no spaces, quotes, backslashes, or shell metacharacters" ;;
    esac
}

# ---- what this box knows about itself --------------------------------------------
# hosts.toml is never written here (D1/D9, dev/output/topology-plan.md §C/§D):
# the hub owns the one canonical copy (`sotd topology apply`); every other box
# either shares its home (the file is simply there) or fetches a copy
# (`--hub`, below). The installer only ever READS it, to ask "does this list
# name ME" — and derives what to install and enable from the answer instead
# of a role flag or a Q&A.

# ExecStart's binary path from a `systemctl --user cat sotd.service` unit.
# Two shapes: the old direct form (ExecStart=<bin>/sotd ...) and the current
# one that sources ~/.bashrc through a shell before exec'ing (ExecStart=/bin/bash
# -c '...; exec "<bin>/sotd" ...'). One regex covers both: an optional
# `exec "` prefix — present only in the wrapped form — before the path, which
# runs to the next space or quote either way. Pure (stdin -> stdout), so it is
# testable without systemctl or a real unit file (scripts/tests/installer-state.sh).
installer_unit_owner_path() {
    sed -n -E 's/^ExecStart=(.*exec ")?([^ "]+)"?.*/\2/p' | head -1
}

# The sotd binary path from the unit CURRENTLY RUNNING for this user on this
# host, or empty when none is. A shared-home cluster (four boxes, one NFS
# $HOME) puts every host's `sotd.service` FILE in the same
# ~/.config/systemd/user — `systemctl --user cat` finds it no matter which
# host wrote it, so file presence alone cannot tell "another host owns this"
# from "nothing is running here"; that used to refuse a fresh install on
# every box but the one that ran the original install. `is-active` is
# per-host state systemd keeps outside that shared file — the only honest
# signal that installing here would step on a live process.
installer_running_daemon_bin() {
    command -v systemctl >/dev/null 2>&1 || return 0
    systemctl --user is-active --quiet sotd.service 2>/dev/null || return 0
    systemctl --user cat sotd.service 2>/dev/null | installer_unit_owner_path
}

# "allow" | "refuse:<why>" — pure, so the decision is testable without
# systemctl or a real prefix. A binary running from THIS prefix is an
# upgrade, not a collision.
installer_running_daemon_decision() {  # <running-bin> <prefix> <force>
    local running="$1" prefix="$2" force="$3"
    [ -z "$running" ] && { printf 'allow'; return; }
    [ "$force" = 1 ] && { printf 'allow'; return; }
    if [ "${running#"$prefix"/}" != "$running" ]; then
        printf 'allow'
        return
    fi
    printf 'refuse:the sotd.service running for this user runs %s; this install targets %s/bin/sotd' "$running" "$prefix"
}

# The owner prefix a sot-launch wrapper's content embeds, or empty when it
# matches none of the three known shapes — fail-closed: an unrecognized
# wrapper's owner is never assumed to be THIS install. Shapes: today's
# all-in-one wrapper (PENDING="<prefix>/updates/pending-..."), today's
# remote-backend wrapper (SOT_FRONTEND_BIN="<prefix>/bin/sot"), and the
# legacy one-liner (exec "<prefix>/bin/sot" ...). Pure (stdin -> stdout).
installer_wrapper_owner_prefix() {
    sed -n -E \
        -e 's#^PENDING="(.+)/updates/pending-.*"$#\1#p' \
        -e 's#^export SOT_FRONTEND_BIN="(.+)/bin/sot"$#\1#p' \
        -e 's#^exec "(.+)/bin/sot".*#\1#p' \
        | head -1
}

# The exec target of a desktop entry's `Exec=` line or an app bundle's
# `exec "..."` shim — the shape both the Linux .desktop file and the macOS
# app's Contents/MacOS/sot-launch use to launch the real sot-launch wrapper.
# Pure (stdin -> stdout).
installer_integration_exec_target() {
    sed -n -E -e 's/^Exec=(.*)$/\1/p' -e 's/^exec "(.*)"$/\1/p' | head -1
}

# "allow" | "refuse:<why>" | "unresolvable:<why>" for one on-disk
# integration file whose content-derived owner prefix is <owner> (empty
# when it could not be identified — installer_wrapper_owner_prefix found no
# known shape, or a desktop/app launcher's exec target isn't this install's
# wrapper path). Mirrors installer_running_daemon_decision's allow/refuse
# shape; "unresolvable" is a third outcome --force-role-change must not
# waive — this isn't a DIFFERENT owner to consent past, it's no identifiable
# owner at all, so overwriting it could be corrupting THIS install's own file
# just as easily as taking over someone else's.
installer_integration_decision() {  # <file> <owner-or-empty> <prefix> <force>
    local file="$1" owner="$2" prefix="$3" force="$4"
    [ -z "$owner" ] && { printf 'unresolvable:%s exists but its owner could not be determined — move it aside and re-run' "$file"; return; }
    [ "$owner" = "$prefix" ] && { printf 'allow'; return; }
    [ "$force" = 1 ] && { printf 'allow'; return; }
    printf 'refuse:%s belongs to the install at %s; this install targets %s' "$file" "$owner" "$prefix"
}

# "allow" | "refuse:<why>" | "unresolvable:<why>" — the whole ownership gate
# as one decision, so it runs (and is tested) in one place instead of a
# refusal path per file. <running-bin> is installer_running_daemon_bin's
# output (a parameter, not a call, so this stays callable with no live
# systemd session — tests pass a synthetic value). The service is checked
# unconditionally: the disable step in "6. config" and the enable step in
# "7. backend service" can each touch a unit this run doesn't otherwise own,
# regardless of <want-frontend>. The wrapper/desktop/app files are checked
# only when <want-frontend> is 1 and only for their own <os> — exactly the
# files "8. FE launcher" would otherwise write. Read-only: it opens files to
# read them and nothing else, which is what makes "a refused install changes
# nothing under $HOME" true by construction rather than by care taken at
# each call site.
installer_ownership_gate() {  # <running-bin> <home> <prefix> <os> <want-frontend> <force>
    local running="$1" home="$2" prefix="$3" os="$4" want_frontend="$5" force="$6"
    local decision wrapper file
    decision="$(installer_running_daemon_decision "$running" "$prefix" "$force")"
    [ "$decision" = allow ] || { printf '%s' "$decision"; return; }
    [ "$want_frontend" = 1 ] || { printf 'allow'; return; }
    wrapper="$home/.local/bin/sot-launch"
    if [ -f "$wrapper" ]; then
        decision="$(installer_integration_decision "$wrapper" "$(installer_wrapper_owner_prefix < "$wrapper")" "$prefix" "$force")"
        [ "$decision" = allow ] || { printf '%s' "$decision"; return; }
    fi
    if [ "$os" = Linux ]; then
        file="$home/.local/share/applications/ship-of-tools.desktop"
        if [ -f "$file" ] && [ "$(installer_integration_exec_target < "$file")" != "$wrapper" ]; then
            printf 'unresolvable:%s exists but does not launch this install'"'"'s wrapper — move it aside and re-run' "$file"
            return
        fi
    fi
    if [ "$os" = Darwin ]; then
        file="$home/Applications/Ship of Tools.app/Contents/MacOS/sot-launch"
        if [ -f "$file" ] && [ "$(installer_integration_exec_target < "$file")" != "$wrapper" ]; then
            printf 'unresolvable:%s exists but does not launch this install'"'"'s wrapper — move it aside and re-run' "$file"
            return
        fi
    fi
    printf 'allow'
}

# Mirrors sot_log::state_dir::host_name() (rust/log/src/state_dir.rs): this
# box's own name, needed to look itself up in the declared topology one step
# before any staged sotd has run to say it out loud. Not a second parser —
# the grammar itself is read by `sotd topology status` (installer_topology_role
# below); this is the same three-line env-or-hostname resolution that
# function documents.
installer_self_host() {
    if [ -n "${SOT_SELF_HOST:-}" ]; then
        printf '%s' "$SOT_SELF_HOST"
        return
    fi
    hostname 2>/dev/null | cut -d. -f1 | tr '[:upper:]' '[:lower:]'
}

# "daemon:0|1 frontend:0|1" for <self> in `sotd topology status`'s output —
# the one parser (rust/protocol/src/topology.rs); this reads its plain-line
# table, not hosts.toml itself, so it stays a consumer, not a second parser.
# "none" when the table doesn't list self: no hosts.toml yet, or one that
# doesn't name this box — both are the same "fall back to flags" signal to
# the caller. Pure, so the decision is testable against canned status text.
installer_topology_role() {  # <status-table-text> <self-host>
    printf '%s\n' "$1" | awk -v self="$2" '
        NR == 1 { next }  # header line "HOST DECLARED"
        $1 == self && NF >= 2 {
            split($2, w, ",")
            d = 0; f = 0
            for (i in w) {
                if (w[i] == "daemon") d = 1
                if (w[i] == "frontend") f = 1
            }
            printf "daemon:%d frontend:%d", d, f
            found = 1
            exit
        }
        END { if (!found) print "none" }
    '
}

# The same "daemon:0|1 frontend:0|1" shape, for the flags fallback (no
# topology entry for this host yet) — one parse path serves both sources.
installer_role_from_flags() {  # <role: local|remote|be-only>
    case "$1" in
        local) printf 'daemon:1 frontend:1' ;;
        be-only) printf 'daemon:1 frontend:0' ;;
        remote) printf 'daemon:0 frontend:1' ;;
    esac
}

# Minimal JSON string escape (backslash + double quote) so an exotic prefix
# or hub alias can't produce a manifest that parses wrong.
json_str() { printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'; }

# $PREFIX/install.json (schema 1) body. No `role` key: this schema no longer
# records one — a box the declared topology names asks the list again next
# run. `daemon`/`frontend` are NOT that role reborn: they record WHAT THIS
# INSTALL ACTUALLY INSTALLED (whichever source decided it — the list, or the
# flags — at this run), a plain fact about the install, not a live source of
# truth and nothing a user is meant to hand-edit. A reader asks the declared
# topology FIRST (it can change with no reinstall) and falls back to these
# two bits only for a listless box with no entry to ask (rust/backend/src/
# update.rs's `backend_role_wanted`, rust/frontend/src/selfupdate.rs's
# `backend_owns_updates_here`). `hub` is the plan's one local declaration,
# for a box that does not share the hub's home; empty string when this box
# has none. Extracted to a pure function so scripts/tests/installer-state.sh
# can check its shape without a real install.
installer_manifest_json() {  # <prefix> <config> <service> <version> <tag> <commit> <installed_at> <hub> <daemon 0|1> <frontend 0|1>
    local prefix="$1" config="$2" service="$3" version="$4" tag="$5" commit="$6" installed_at="$7" hub="$8"
    local daemon_json=false frontend_json=false
    [ "$9" = 1 ] && daemon_json=true
    [ "${10}" = 1 ] && frontend_json=true
    cat <<EOF
{
  "schema": 1,
  "prefix": "$(json_str "$prefix")",
  "config": "$(json_str "$config")",
  "service": "$service",
  "version": "$version",
  "tag": "$tag",
  "commit": "$commit",
  "installed_at": "$installed_at",
  "hub": "$(json_str "$hub")",
  "daemon": $daemon_json,
  "frontend": $frontend_json
}
EOF
}

installer_retire_tmux_unit() {  # <systemd-user-dir> — v0.6.0 deleted the tmux
    # runtime: retire the keeper unit earlier installs enabled (ADR 0038,
    # superseded). No-op if the unit was never installed.
    unit="$1/sot-tmux.service"
    [ -f "$unit" ] || return 0
    systemctl --user disable --now sot-tmux.service 2>/dev/null || true
    rm -f "$unit"
}

# scripts/tests/installer-state.sh sources this file to exercise the
# functions above in isolation. Nothing else sets this, `curl | bash`
# included.
if [ "${SOT_INSTALL_SOURCE_ONLY:-}" = 1 ]; then return 0; fi

# ---- prelude: run the FETCHED tag's own installer, not main's body -------
# The one-liner always fetches this file from main, whose body targets the
# release line under development and can drift from what "latest" needs
# (v0.5.10 needed tmux; main's body no longer checks). SOT_INSTALL_TAG unset
# means: resolve the tag, then run THAT tag's own install.sh, args untouched
# (set = skip: the pinned run, and how a checkout runs its own body). Old
# tags have no prelude; they resolve "latest" themselves the same way.
if [ -z "${SOT_INSTALL_TAG:-}" ]; then
    command -v curl >/dev/null || die "curl is required"
    tag="" prev=""
    for a in "$@"; do [ "$prev" = --version ] && tag="$a"; prev="$a"; done
    if [ -z "$tag" ]; then
        loc="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest")" \
            || die "could not resolve the latest release of $REPO"
        tag="${loc##*/}"
    fi
    case "$tag" in v[0-9]*) ;; *) die "could not resolve a release tag (got '$tag')" ;; esac
    tmp="$(mktemp)"; trap 'rm -f "$tmp"' EXIT
    url="https://raw.githubusercontent.com/$REPO/$tag/scripts/install.sh"
    curl -fsSL -o "$tmp" "$url" || die "could not fetch $url"
    SOT_INSTALL_TAG="$tag" bash "$tmp" "$@"
    exit
fi

# ---- 0. heal a forbidden depot config (owner ruling 2026-09-02: no depot
# path is ever set, derived, or hardcoded anywhere in this repo) -----------
# A prior install (PR #161) wrote a systemd drop-in carrying the installing
# shell's JULIA_DEPOT_PATH into the unit. Remove it UNCONDITIONALLY, for
# every role and every --no-service combination — not only the managed-
# local-service path that used to own this cleanup — so a remote or
# --no-service reinstall heals too. Reload only when the file existed.
if [ -f "$HOME/.config/systemd/user/sotd.service.d/depot.conf" ]; then
    rm -f "$HOME/.config/systemd/user/sotd.service.d/depot.conf"
    command -v systemctl >/dev/null 2>&1 && systemctl --user daemon-reload 2>/dev/null
fi

while [ $# -gt 0 ]; do
    case "$1" in
        --local) ROLE=local ;;
        --backend) ROLE=remote; BE_ALIAS="${2:?--backend needs an ssh alias}"; shift ;;
        --be-only) ROLE=be-only ;;
        # This box does not share the hub's home: fetch its hosts.toml
        # (`sotd topology sync`) after staging, below.
        --hub) HUB_ALIAS="${2:?--hub needs an ssh alias}"; shift ;;
        --version) VERSION="${2:?}"; shift ;;
        --prefix) PREFIX="${2:?}"; shift ;;
        --port) PORT="${2:?}"; shift ;;
        # Skip the systemd unit install/enable — for shared-home deployments
        # (a user-level unit file + its enable symlink live in $HOME, so on an
        # shared home they'd apply to EVERY machine). The caller supervises
        # sotd itself (e.g. systemd-run --user transient unit, per-machine).
        --no-service) NO_SERVICE=1 ;;
        # Consent to reconfiguring an installation that is already here. See
        # the role gate below for what it protects and why a role flag alone
        # is not consent.
        --force-role-change) FORCE_ROLE_CHANGE=1 ;;
        *) echo "unknown flag: $1" >&2; exit 2 ;;
    esac
    shift
done
# Canonicalize the prefix (a relative one produces a repo/current symlink
# whose target resolves from repo/, i.e. a broken link), then reject unsafe
# characters in both the prefix and the project root — deploy/sotd.service's
# ExecStart embeds @SOT_PROJECT_ROOT@ ($HOME) inside a shell string too, so a
# stray quote there breaks the unit exactly like it would in the prefix.
case "$PREFIX" in
    /*) ;;
    *) PREFIX="$(pwd)/$PREFIX" ;;
esac
reject_unsafe_path_chars "prefix" "$PREFIX"
reject_unsafe_path_chars 'project root ($HOME)' "$HOME"

# ---- 1. preflight ------------------------------------------------------------
OS="$(uname -s)"
case "$OS" in
    Linux)
        [ "$(uname -m)" = x86_64 ] || die "the linux prebuilt is x86_64 only (this is $(uname -m)) — build from source"
        TARGET="linux-x86_64" ;;
    Darwin)
        [ "$(uname -m)" = arm64 ] || die "the macOS prebuilt is Apple Silicon only (this is $(uname -m)) — Intel Macs build from source"
        TARGET="macos-aarch64"
        say "macOS support is EXPERIMENTAL — both roles work; please report anything broken" ;;
    MINGW*|MSYS*|CYGWIN*)
        die "this installer covers Linux and macOS. Windows uses source setup unless a release zip exists; see docs/INSTALL-AGENT.md section 2b" ;;
    *)  die "unsupported OS $OS. Linux/macOS: this installer; Windows: docs/INSTALL-AGENT.md section 2b" ;;
esac
for t in curl tar; do command -v "$t" >/dev/null || die "$t is required"; done
# The glibc floor for the frontend binary is checked further down, once the
# role is resolved (WANT_FRONTEND) — that now needs the declared topology,
# read via the sotd just staged below, so it can't run this early any more.

# Downloader: for a public repo, unauthenticated curl works. gh (authed) is
# preferred when present, and $GITHUB_TOKEN is honored purely to dodge the
# unauthenticated API rate limit (60 req/h per IP).
FETCH=curl
if command -v gh >/dev/null && gh auth status >/dev/null 2>&1; then
    FETCH=gh
fi

# The curl path parses GitHub's JSON — needs jq (the gh path doesn't).
[ "$FETCH" = curl ] && { command -v jq >/dev/null || die "jq is required (or install+auth gh)"; }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-install.XXXXXX")"; trap 'rm -rf "$WORK"' EXIT
gh_api() {  # gh_api <endpoint> <outfile> — API GET to a file (token optional)
    curl -fsSL ${GITHUB_TOKEN:+-H "Authorization: Bearer $GITHUB_TOKEN"} \
         -H "Accept: application/vnd.github+json" \
         -o "$2" "https://api.github.com/repos/$REPO/$1"
}

# A pinned run already has the tag the prelude resolved — never re-hit the
# releases API for it.
VERSION="${VERSION:-${SOT_INSTALL_TAG:-}}"
if [ -z "$VERSION" ]; then
    if [ "$FETCH" = gh ]; then
        VERSION="$(gh api "repos/$REPO/releases/latest" --jq .tag_name)"
    else
        gh_api "releases/latest" "$WORK/latest.json"
        VERSION="$(jq -r .tag_name "$WORK/latest.json")"
    fi
fi
VER="${VERSION#v}"
say "installing Ship of Tools $VERSION into $PREFIX"

# ---- 2. download + verify ----------------------------------------------------
ASSETS=("SHA256SUMS" "sot-$VER-$TARGET.tar.gz")

dl() {
    if [ "$FETCH" = gh ]; then
        gh release download "$VERSION" -R "$REPO" -p "$1" -D "$WORK"
    else
        [ -f "$WORK/release.json" ] || gh_api "releases/tags/$VERSION" "$WORK/release.json"
        url="$(jq -r --arg n "$1" '.assets[] | select(.name == $n) | .url' "$WORK/release.json")"
        [ -n "$url" ] && [ "$url" != null ] || die "asset $1 not found on release $VERSION"
        curl -fsSL ${GITHUB_TOKEN:+-H "Authorization: Bearer $GITHUB_TOKEN"} -H "Accept: application/octet-stream" -o "$WORK/$1" "$url"
    fi
}
say "downloading ${#ASSETS[@]} assets"
for a in "${ASSETS[@]}"; do dl "$a"; done
if command -v sha256sum >/dev/null; then
    ( cd "$WORK" && sha256sum -c --ignore-missing SHA256SUMS ) || die "checksum verification FAILED"
else
    ( cd "$WORK" && shasum -a 256 -c --ignore-missing SHA256SUMS ) || die "checksum verification FAILED"
fi

# ---- 3. layout ---------------------------------------------------------------
mkdir -p "$PREFIX/bin" "$PREFIX/updates" "$PREFIX/repo" "$CONFIG"
tar -xzf "$WORK/sot-$VER-$TARGET.tar.gz" -C "$WORK"
BINDIR="$WORK/sot-$VER-$TARGET"
# sot-capsule is sotd's capsule-runtime pair (ADR 0042 L1a): sotd resolves it
# next to its own executable, so it must land in the same directory. Archives
# that predate the capsule runtime lack it, hence the skip for it alone.
for b in sot sotd sot-capsule; do
    [ "$b" = sot-capsule ] && [ ! -f "$BINDIR/$b" ] && continue
    [ -f "$PREFIX/bin/$b" ] && cp "$PREFIX/bin/$b" "$PREFIX/bin/$b.prev"
    install -m 0755 "$BINDIR/$b" "$PREFIX/bin/$b"
    # Gatekeeper: strip any quarantine attr (browser downloads carry it).
    [ "$OS" = Darwin ] && xattr -d com.apple.quarantine "$PREFIX/bin/$b" 2>/dev/null || true
done
# The offline apply/rollback script (Phase C3). Newer releases ship it in the
# archive; otherwise it lands from the checkout below.
[ -f "$BINDIR/sot-apply" ] && install -m 0755 "$BINDIR/sot-apply" "$PREFIX/bin/sot-apply"
# A manual installer run is a NEW transaction: stale rollback state from a
# previous auto-apply must not pair old last-good pointers with these fresh
# .prev binaries (a later crash-loop rollback would mix versions).
rm -f "$PREFIX"/updates/last-good-*.json "$PREFIX"/updates/just-applied-* 2>/dev/null || true
say "binaries: $("$PREFIX/bin/sotd" --version)"
DEFAULT_SOCKET="$("$PREFIX/bin/sotd" session-socket-path sot)"

# ---- role resolution: the declared topology, else flags -------------------------
# D9 (dev/output/topology-plan.md §C/§D): the hub's hosts.toml is canonical —
# when it names this host, that entry's daemon/frontend flags decide what's
# installed and enabled here, no --local/--backend/--be-only or Q&A needed.
# A box with no entry yet (a brand-new user) falls back to the role flag /
# interactive choice below. --hub fetches a copy first, for a box that does
# not share the hub's home; a box that does already has the file, no fetch
# needed.
if [ -n "$HUB_ALIAS" ]; then
    "$PREFIX/bin/sotd" topology sync --hub "$HUB_ALIAS" || die "topology sync --hub $HUB_ALIAS failed"
fi
SELF_HOST="$(installer_self_host)"
WANT_DAEMON=0
WANT_FRONTEND=0
TOPO_ROLE=none
if STATUS_OUT="$("$PREFIX/bin/sotd" topology status 2>/dev/null)"; then
    TOPO_ROLE="$(installer_topology_role "$STATUS_OUT" "$SELF_HOST")"
fi
if [ "$TOPO_ROLE" != none ]; then
    [ -n "$ROLE" ] && say "note: --$ROLE given, but the declared topology names this host — the topology wins"
    # The topology wins outright: an ssh alias from an overridden --backend
    # flag must not leak into the FE launcher choice below (step 8).
    BE_ALIAS=""
    RESOLVED="$TOPO_ROLE"
    say "role: the declared topology names '$SELF_HOST' ($RESOLVED) — installing/enabling accordingly"
else
    # No role flag → interactive Q&A (matches the sot-setup experience; found
    # missing by the first real laptop install). Prompts read /dev/tty so
    # this also works under `curl | bash`. No TTY and no flags → hard error.
    if [ -z "$ROLE" ]; then
        # A real open-probe: -r/-w pass on the device node even in ttyless
        # contexts (cron, CI, piped bash) where opening it then fails.
        if (: < /dev/tty) 2>/dev/null && (: > /dev/tty) 2>/dev/null; then
            {
                echo "No declared topology names this machine ($SELF_HOST) — where should Ship of Tools run?"
                echo "  1) all on this machine        (frontend + backend here)"
                echo "  2) frontend here, backend on another machine over SSH"
                echo "  3) backend only on this machine (headless server)"
                printf "Choose [1-3]: "
            } > /dev/tty
            read -r choice < /dev/tty
            case "$choice" in
                1) ROLE=local ;;
                2) ROLE=remote
                   printf "SSH alias/hostname of the backend machine (key-based auth required): " > /dev/tty
                   read -r BE_ALIAS < /dev/tty
                   [ -n "$BE_ALIAS" ] || die "backend host is required for the remote layout"
                   printf "Verifying ssh to '%s'... " "$BE_ALIAS" > /dev/tty
                   ssh -o BatchMode=yes -o ConnectTimeout=8 "$BE_ALIAS" true 2>/dev/null \
                       && echo "ok" > /dev/tty \
                       || { echo "FAILED" > /dev/tty; die "key-based ssh to '$BE_ALIAS' doesn't work (ssh-copy-id first, or fix ~/.ssh/config)"; } ;;
                3) ROLE=be-only ;;
                *) die "no such choice: '$choice'" ;;
            esac
            printf "Local tunnel port [%s]: " "$PORT" > /dev/tty
            read -r p < /dev/tty
            [ -n "$p" ] && PORT="$p"
        else
            echo "pick a role: --local | --backend <alias> | --be-only (no TTY for interactive setup)" >&2
            exit 2
        fi
    fi
    RESOLVED="$(installer_role_from_flags "$ROLE")"
    say "role: no topology entry for '$SELF_HOST' — using $ROLE ($RESOLVED)"
fi
case "$RESOLVED" in *"daemon:1"*) WANT_DAEMON=1 ;; esac
case "$RESOLVED" in *"frontend:1"*) WANT_FRONTEND=1 ;; esac

if [ "$OS" = Linux ] && [ "$WANT_FRONTEND" = 1 ]; then
    # No pipelines here: `... | head | grep || echo 0` SIGPIPEs ldd under
    # pipefail and APPENDS a bogus "0" to a good match, making the floor
    # check fail on EVERY machine (the first real laptop install hit this).
    # Capture the whole output, then parse from the variable.
    ldd_out="$(ldd --version 2>/dev/null || true)"
    glibc="$(printf '%s\n' "$ldd_out" | sed -n '1s/.*[^0-9.]\([0-9][0-9]*\.[0-9][0-9]*\)[[:space:]]*$/\1/p')"
    [ -n "$glibc" ] || glibc=0
    lowest="$(printf '%s\n%s\n' "$GLIBC_FLOOR_FE" "$glibc" | sort -V | sed -n 1p)"
    [ "$lowest" = "$GLIBC_FLOOR_FE" ] \
        || die "the frontend binary needs glibc >= $GLIBC_FLOOR_FE (this box: $glibc). The backend (musl, --be-only) runs anywhere."
fi

# ---- ownership gate ------------------------------------------------------------
# Run now that the role is known and before the first write under $HOME (the
# mkdir of ~/.local/bin included) — a refused install changes nothing there.
# installer_ownership_gate is read-only, so this cannot be the source of a
# stray write; see it above for what --force-role-change does and does not
# waive.
GATE_DECISION="$(installer_ownership_gate "$(installer_running_daemon_bin)" "$HOME" "$PREFIX" "$OS" "$WANT_FRONTEND" "$FORCE_ROLE_CHANGE")"
case "$GATE_DECISION" in
    refuse:*)
        printf '\033[1;31mERROR:\033[0m %s\n' "${GATE_DECISION#refuse:}" >&2
        printf '       Installing here would replace or disable another install'"'"'s files. Re-run with --force-role-change if that is what you want.\n' >&2
        exit 2 ;;
    unresolvable:*)
        printf '\033[1;31mERROR:\033[0m %s\n' "${GATE_DECISION#unresolvable:}" >&2
        exit 2 ;;
esac

# ---- 4. the repo checkout — manual, resources, julia code (ADR 0030 add.) -----
# The checkout IS the product's resource tree and its help system:
# resource_dir resolves julia/kernel, julia/repl, sidecars, and examples from
# $PREFIX/repo/current, and the FE Terminal's agent reads docs/ + ADRs +
# source as the manual.
#
# Phase-C layout (versioned, transactional — Codex-reviewed design):
#   repo/base            blobless --no-checkout clone (the fetch target)
#   repo/versions/<tag>  detached git worktree pinned at that release
#   repo/current         SYMLINK to the active version dir
# The auto-updater prepares new version dirs off the live tree and applying
# an update is a pointer flip; this installer produces the same layout (and
# migrates the pre-Phase-C in-place clone). READ-ONLY BY CONVENTION: a dirty
# tree refuses to move (fail loud).
REPO_DIR="$PREFIX/repo"
BASE="$REPO_DIR/base"
CHECKOUT="$REPO_DIR/versions/$VERSION"
CURRENT="$REPO_DIR/current"
command -v git >/dev/null || die "git is required (the install includes a repo checkout)"
if [ ! -d "$BASE" ]; then
    say "creating base clone (blobless, no checkout)"
    if [ "$FETCH" = gh ]; then
        git -c "credential.helper=!gh auth git-credential" \
            clone --filter=blob:none --no-checkout "https://github.com/$REPO" "$BASE" \
            || die "base clone failed"
    else
        # Public repo: plain https clone, no auth needed.
        git clone --filter=blob:none --no-checkout "https://github.com/$REPO" "$BASE" \
            || die "base clone failed"
    fi
fi
# --force on the tag fetch: a release tag force-moved upstream (e.g. the
# public-flip history rewrite) otherwise makes the whole fetch abort with
# "would clobber existing tag", blocking every upgrade re-run (issue #4).
# The checkouts are READ-ONLY BY CONVENTION, so force-updating tags is safe.
git -C "$BASE" fetch --tags --force origin || die "fetch failed"
want="$(git -C "$BASE" rev-parse "refs/tags/$VERSION^{commit}" 2>/dev/null)" \
    || die "tag $VERSION not present after fetch"
if [ -d "$CHECKOUT" ]; then
    if [ -n "$(git -C "$CHECKOUT" status --porcelain 2>/dev/null)" ]; then
        die "the checkout at $CHECKOUT has local changes — commit/stash/revert, then re-run (updates refuse to move a dirty tree)"
    fi
else
    say "adding version worktree $VERSION"
    git -C "$BASE" worktree prune
    git -C "$BASE" worktree add --detach "$CHECKOUT" "$want" || die "worktree add failed"
fi
# Enforce HEAD == the tag's recorded commit (fresh AND reused worktree): a
# moved tag, wrong ref, or half-checkout must fail HERE, not at first use.
have="$(git -C "$CHECKOUT" rev-parse HEAD)"
[ "$have" = "$want" ] || die "checkout HEAD ($have) != $VERSION commit ($want) — refusing"
# Flip repo/current to this version. Migration from the pre-Phase-C layout:
# current used to BE the clone (a plain dir) — refuse if dirty, then delete
# it (read-only by convention; its only untracked files are julia Manifests,
# regenerated by instantiate below).
PREV_VERSION=""
if [ -L "$CURRENT" ]; then
    PREV_VERSION="$(basename "$(readlink "$CURRENT")")"
elif [ -d "$CURRENT" ]; then
    # Probe separately and LOUDLY: an unreadable or non-git dir must not
    # fall through to deletion on empty command-substitution output.
    git -C "$CURRENT" rev-parse --git-dir >/dev/null 2>&1 \
        || die "repo/current exists but is not a readable git checkout — refusing to migrate (inspect $CURRENT)"
    if [ -n "$(git -C "$CURRENT" status --porcelain)" ]; then
        die "the old-layout checkout at $CURRENT has local changes — commit/stash/revert, then re-run"
    fi
    # Preserve, don't delete: the old clone is moved aside recoverably; a
    # later successful run can clean it up manually.
    say "migrating pre-versioned layout (old clone preserved at repo/current.pre-versioned)"
    rm -rf "$REPO_DIR/current.pre-versioned"
    mv "$CURRENT" "$REPO_DIR/current.pre-versioned"
fi
ln -sfn "$CHECKOUT" "$CURRENT"
# Compat: older pre-clone binaries resolve resources via julia/current
# (the retired bundle's mount point). Point it at the checkout — repo-shaped
# either way — so the clone-based install works with any binary generation.
mkdir -p "$PREFIX/julia"
ln -sfn "$CHECKOUT" "$PREFIX/julia/current"
# sot-apply from the checkout when the release archive predates shipping it.
if [ ! -f "$PREFIX/bin/sot-apply" ] && [ -f "$CHECKOUT/scripts/sot-apply.sh" ]; then
    install -m 0755 "$CHECKOUT/scripts/sot-apply.sh" "$PREFIX/bin/sot-apply"
fi
# Keep the previously-active version dir for rollback; prune everything else.
for v in "$REPO_DIR/versions"/*; do
    [ -d "$v" ] || continue
    case "$(basename "$v")" in
        "$VERSION") ;;
        "$PREV_VERSION") ;;
        *)
            say "pruning old version dir $(basename "$v")"
            git -C "$BASE" worktree remove --force "$v" 2>/dev/null || rm -rf "$v"
            ;;
    esac
done
git -C "$BASE" worktree prune 2>/dev/null || true

# ---- 5. Julia + agent comm -----------------------------------------------------
chan="1.12"
if [ -x "$HOME/.juliaup/bin/juliaup" ]; then
    export PATH="$HOME/.juliaup/bin:$PATH"
fi
if command -v julia >/dev/null && julia -e 'exit(VERSION >= v"1.12" ? 0 : 1)' >/dev/null 2>&1; then
    :
elif command -v juliaup >/dev/null 2>&1; then
    say "installing Julia channel $chan with juliaup"
    juliaup add "$chan" >/dev/null 2>&1 || true
    export PATH="$HOME/.juliaup/bin:$PATH"
else
    say "installing Julia (juliaup, channel $chan)"
    curl -fsSL https://install.julialang.org | sh -s -- --yes --default-channel "$chan"
    export PATH="$HOME/.juliaup/bin:$PATH"
fi
command -v julia >/dev/null || die "Julia install failed; julia is still not on PATH"
julia_run() {
    julia "+$chan" "$@" 2>/dev/null || julia "$@"
}

say "installing agent comm resources (sot-comm, Claude/Codex skills)"
julia_run --project="$CHECKOUT" -e 'using ShipTools; ShipTools.update_comm()' \
    || die "ShipTools.update_comm() failed"

if [ "$WANT_DAEMON" = 1 ]; then
    # A previous install's instantiate wrote Manifest.toml into these env dirs
    # (untracked — envs fresh-resolve at the tag by design), and a tag move
    # keeps the file. A stale manifest predating a newly added dep fails
    # instantiate with "project and manifest out of sync" (field report
    # 2026-08-11: Sockets, added to julia/repl in #67). Drop UNTRACKED
    # leftovers only — deleting a file the current tag tracks (old tags
    # shipped julia/pluto/Manifest.toml) would dirty the read-only checkout
    # and break the next upgrade's dirty-tree refusal.
    for env in julia/kernel julia/repl julia/pluto; do
        if [ -f "$CHECKOUT/$env/Manifest.toml" ] \
           && ! git -C "$CHECKOUT" ls-files --error-unmatch "$env/Manifest.toml" >/dev/null 2>&1; then
            say "dropping stale $env/Manifest.toml (fresh resolve at this tag)"
            rm -f "$CHECKOUT/$env/Manifest.toml"
        fi
    done
    say "instantiating julia envs (first run takes a few minutes)"
    julia_run --project="$CHECKOUT/julia/kernel" -e 'using Pkg; Pkg.instantiate()'
    julia_run --project="$CHECKOUT/julia/repl" -e 'using Pkg; Pkg.instantiate()'
    julia_run --project="$CHECKOUT/julia/pluto" -e 'using Pkg; Pkg.instantiate(); Pkg.precompile(); using Pluto'

    # MathJax sidecar (math rendering in markdown previews). Its node deps
    # are NOT in the repo — without them every math.render dies with
    # "mathjax sidecar terminated" (bit a live deployment 2026-07-10).
    # Best-effort: a box without node still installs fine, math previews
    # just show raw LaTeX until the deps land.
    if command -v npm >/dev/null 2>&1; then
        say "installing MathJax sidecar deps (npm ci)"
        (cd "$CHECKOUT/rust/backend/sidecars/mathjax" && npm ci --silent) \
            || say "WARN: npm ci failed in sidecars/mathjax — math rendering unavailable until you run it manually"
    else
        say "WARN: node/npm not found — math rendering in markdown previews needs it."
        say "      Install node, then run: (cd $CHECKOUT/rust/backend/sidecars/mathjax && npm ci)"
    fi
fi

# ---- 6. config -----------------------------------------------------------------
# hosts.toml is never written here — see "what this box knows about itself"
# above. A resolution that does not want a daemon here must not leave a
# previously-installed LOCAL backend running (a wrong-topology remnant):
# disable it, don't just orphan it.
if [ "$WANT_DAEMON" = 0 ] && command -v systemctl >/dev/null 2>&1 && systemctl --user is-enabled sotd.service >/dev/null 2>&1; then
    systemctl --user disable --now sotd.service || true
    say "disabled the local sotd.service from a previous all-in-one install"
fi
[ -f "$CONFIG/settings.toml" ] || printf '# Ship of Tools settings — see .sot/settings.toml.example in the repo\n' > "$CONFIG/settings.toml"

# ---- 7. backend service --------------------------------------------------------
if [ "$WANT_DAEMON" = 1 ] && [ "$NO_SERVICE" = 1 ]; then
    say "skipping systemd unit (--no-service) — supervise sotd yourself, e.g.:"
    say "  systemd-run --user --unit=sotd-canary -p Restart=always $PREFIX/bin/sotd --project-root \$HOME --label sot"
fi
if [ "$OS" = Darwin ] && [ "$WANT_DAEMON" = 1 ]; then
    # No launchd wiring yet (roadmap): the local-role launcher below starts
    # sotd on demand; be-only Macs run it by hand.
    NO_SERVICE=1
    say "macOS: no service manager wiring yet — the sot-launch wrapper starts sotd on demand"
    [ "$WANT_FRONTEND" = 0 ] && say "  be-only: start it with  $PREFIX/bin/sotd --project-root ~ --label sot"
fi
if [ "$OS" = Linux ] && [ "$WANT_DAEMON" = 1 ] && [ "$NO_SERVICE" = 0 ]; then
    mkdir -p "$HOME/.config/systemd/user"
    installer_retire_tmux_unit "$HOME/.config/systemd/user"
    sed -e "s|@SOT_BIN@|$PREFIX/bin/sotd|" \
        -e "s|@SOT_APPLY@|$PREFIX/bin/sot-apply|" \
        -e "s|@SOT_PROJECT_ROOT@|$HOME|" \
        "$BINDIR/sotd.service" > "$HOME/.config/systemd/user/sotd.service"
    # No JULIA_DEPOT_PATH (or any other) config is written here: owner ruling
    # 2026-09-02 forbids writing/overwriting depot config anywhere. The unit
    # itself (deploy/sotd.service) sources ~/.bashrc before exec'ing sotd, so
    # the daemon inherits whatever the owner's shell profile exports. A prior
    # install's forbidden drop-in is healed unconditionally near the top of
    # this script (step 0), not here.
    systemctl --user daemon-reload
    systemctl --user enable --now sotd.service
    loginctl enable-linger "$USER" 2>/dev/null || true
    say "sotd running: $(systemctl --user is-active sotd.service) (socket $DEFAULT_SOCKET)"
fi

# ---- 8. FE launcher -------------------------------------------------------------
if [ "$WANT_FRONTEND" = 1 ]; then
    mkdir -p "$HOME/.local/bin"
    # The all-in-one launcher (own daemon on demand) unless this install
    # names an explicit remote backend to dial over SSH (--backend <alias>,
    # or its interactive equivalent) — that shape has no topology-derived
    # equivalent yet, so a listed frontend-only host also gets this one.
    if [ -z "$BE_ALIAS" ]; then
        cat > "$HOME/.local/bin/sot-launch" <<EOF
#!/usr/bin/env bash
# All-in-one launcher: apply any armed pending update (offline pointer flip,
# fail-open), start the backend on demand if its per-user socket is missing
# (macOS has no service wiring yet; Linux normally has the systemd unit),
# then SUPERVISE the frontend: exit-75 respawn (ADR 0017 on Unix) and
# crash-loop rollback of a just-applied update (ADR 0030 Phase C3).
PENDING="$PREFIX/updates/pending-$TARGET.json"
MARKER="$PREFIX/updates/just-applied-$TARGET"
stop_daemon() { pkill -u "\$(id -u)" -f "$PREFIX/bin/sotd" 2>/dev/null && sleep 1; }
# Single apply owner (ADR 0030 Phase C): on systemd installs the apply runs
# ONLY inside ExecStartPre (daemon stopped, whole install — FE binary
# included — flips together); a try-restart triggers it. Launcher-managed
# daemons (macOS / --no-service) are stopped FIRST, then sot-apply runs here.
apply_pending() {
    [ -f "\$PENDING" ] || return 0
    [ -x "$PREFIX/bin/sot-apply" ] || return 0
    if command -v systemctl >/dev/null 2>&1 && systemctl --user is-active sotd.service >/dev/null 2>&1; then
        echo "pending update armed — restarting sotd so ExecStartPre applies it" >&2
        systemctl --user try-restart sotd.service || true
    else
        stop_daemon
        APPLY_OUT="\$("$PREFIX/bin/sot-apply" 2>&1)"
        [ -n "\$APPLY_OUT" ] && printf '%s\n' "\$APPLY_OUT" >&2
    fi
}
apply_pending
SOCKET="\$("$PREFIX/bin/sotd" session-socket-path sot)"
socket_open() {
    [ -S "\$SOCKET" ] || return 1
    if command -v nc >/dev/null 2>&1; then
        nc -U "\$SOCKET" </dev/null >/dev/null 2>&1 &
        pid=\$!
        sleep 1
        if kill -0 "\$pid" 2>/dev/null; then
            kill "\$pid" 2>/dev/null || true
            wait "\$pid" 2>/dev/null || true
            return 0
        fi
        wait "\$pid"
        return \$?
    fi
    # Minimal installs may not have nc. A socket file is the best available
    # probe; the frontend will still fail loud if the connect cannot complete.
    return 0
}
start_daemon_if_needed() {
    if ! socket_open; then
        rm -f "\$SOCKET" 2>/dev/null || true
        nohup "$PREFIX/bin/sotd" --project-root "\$HOME" --label sot >/tmp/sotd.log 2>&1 </dev/null &
        i=0; while [ \$i -lt 40 ]; do socket_open && break; sleep 0.25; i=\$((i+1)); done
        socket_open || { echo "ERROR: backend did not open \$SOCKET; see /tmp/sotd.log" >&2; exit 1; }
    fi
}
start_daemon_if_needed
FAILS=0; ROLLED=0
while :; do
    START="\$(date +%s)"
    "$PREFIX/bin/sot" --socket "\$SOCKET"
    RC=\$?
    NOW="\$(date +%s)"
    RUNTIME=\$((NOW - START))
    # A healthy run closes the crash-loop health window.
    [ "\$RUNTIME" -ge 60 ] && rm -f "\$MARKER" 2>/dev/null
    if [ "\$RC" -eq 75 ]; then
        # ADR-0017 self-relaunch: pick up any staged update, then respawn.
        apply_pending
        start_daemon_if_needed
        FAILS=0
        continue
    fi
    if [ "\$RC" -ne 0 ] && [ "\$RUNTIME" -le 10 ]; then
        FAILS=\$((FAILS + 1))
        if [ "\$FAILS" -ge 2 ]; then
            # Roll back ONLY inside the just-applied health window — an
            # unrelated crash weeks later must not downgrade a healthy
            # release.
            if [ "\$ROLLED" -eq 0 ] && [ -f "\$MARKER" ] \
               && [ -n "\$(find "\$MARKER" -mmin -30 2>/dev/null)" ]; then
                echo "frontend crash-looped inside the post-update window — rolling back" >&2
                stop_daemon
                [ -x "$PREFIX/bin/sot-apply" ] && "$PREFIX/bin/sot-apply" --rollback >&2
                start_daemon_if_needed
                ROLLED=1; FAILS=0
                continue
            fi
            exit "\$RC"
        fi
        continue
    fi
    exit "\$RC"
done
EOF
    else
        cat > "$HOME/.local/bin/sot-launch" <<EOF
#!/usr/bin/env bash
# FE-only install -> remote BE over SSH (key auth required, ADR 0030 §5).
# Item 2 follow-up: this used to be a second, hand-maintained copy of
# scripts/launch-sot.sh's tunnel-open + backend-ensure + frontend-invoke
# logic (one fixed tunnel, no per-host support) -- it now delegates to the
# pinned checkout's own copy instead, so the two never drift.
#
# Kept from the old heredoc (install-layout-specific; no equivalent in
# launch-sot.sh itself, which only knows git pull / cargo build, not
# sot-apply's staged $PREFIX/repo/versions flip): applying an armed
# pending update (staged by the frontend's own self-check) before every
# launch. Exit-75 respawn and crash-loop rollback are DROPPED, not kept --
# launch-sot.sh has never had them for the plain Unix launcher either
# (that's Windows-only today, ADR 0017 / relaunch-sot.ps1), so this
# wrapper now matches every other Unix launch path instead of being the
# one with more supervision than the rest.
#
# SOT_REMOTE_SOCKET and SOT_LEGACY_FORWARDS, if a caller sets them in its
# own environment before running sot-launch, pass through unchanged --
# launch-sot.sh reads both itself, so no explicit forwarding is needed
# here. SOT_REMOTE_REPO is deliberately left UNSET: this install has no
# local knowledge of the remote's checkout (never had one -- the old
# heredoc only ever queried the remote's installed sotd directly), and
# sot_ensure_remote_host's repo-optional path (scripts/launch-sot.sh) is
# exactly that behavior.
if [ -x "$PREFIX/bin/sot-apply" ]; then
    APPLY_OUT="\$("$PREFIX/bin/sot-apply" 2>&1)"
    [ -n "\$APPLY_OUT" ] && printf '%s\n' "\$APPLY_OUT" >&2
fi
export SOT_HOST="$BE_ALIAS"
export SOT_TCP_PORT="$PORT"
export SOT_FRONTEND_BIN="$PREFIX/bin/sot"
export SOT_NO_UPDATE=1
exec "$PREFIX/repo/current/scripts/launch-sot.sh" "\$@"
EOF
    fi
    chmod +x "$HOME/.local/bin/sot-launch"
    if [ "$OS" = Darwin ]; then
        # A minimal .app bundle so the FE launches from Launchpad/Spotlight/
        # Dock like a real app. Locally-created bundles carry no quarantine
        # attr, so Gatekeeper doesn't object. The icon is generated from the
        # checkout's logo with sips+iconutil (both ship with macOS).
        APP="$HOME/Applications/Ship of Tools.app"
        mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
        cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleName</key><string>Ship of Tools</string>
    <key>CFBundleIdentifier</key><string>dev.ship-of-tools.sot</string>
    <key>CFBundleExecutable</key><string>sot-launch</string>
    <key>CFBundleIconFile</key><string>sot</string>
    <key>NSHighResolutionCapable</key><true/>
</dict></plist>
EOF
        cat > "$APP/Contents/MacOS/sot-launch" <<EOF
#!/usr/bin/env bash
exec "$HOME/.local/bin/sot-launch"
EOF
        chmod +x "$APP/Contents/MacOS/sot-launch"
        LOGO="$CHECKOUT/logo.png"
        if [ -f "$LOGO" ] && command -v sips >/dev/null && command -v iconutil >/dev/null; then
            ICONSET="$WORK/sot.iconset"; mkdir -p "$ICONSET"
            for sz in 16 32 128 256 512; do
                sips -z "$sz" "$sz" "$LOGO" --out "$ICONSET/icon_${sz}x${sz}.png" >/dev/null 2>&1 || true
                sips -z "$((sz*2))" "$((sz*2))" "$LOGO" --out "$ICONSET/icon_${sz}x${sz}@2x.png" >/dev/null 2>&1 || true
            done
            iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/sot.icns" 2>/dev/null                 && say "app icon generated from the checkout logo"                 || say "icon generation failed (cosmetic) — bundle works without it"
        fi
        say "FE launcher: sot-launch + 'Ship of Tools' in ~/Applications (Launchpad/Spotlight)"
    else
    mkdir -p "$HOME/.local/share/applications"
    # Icon parity with the other two platforms: Windows points its .lnk at
    # logo.ico and macOS generates an .icns from logo.png, but this entry had
    # no Icon= key at all, so Linux was the one platform whose launcher showed
    # a generic placeholder. Install into the hicolor theme (the freedesktop
    # lookup path) AND keep an absolute-path copy under $PREFIX as the
    # fallback Icon= value, so the entry still resolves on a desktop with no
    # icon theme cache. $PREFIX is stable across repo moves; the checkout is
    # not — pointing Icon= into the clone would break the moment it is moved.
    ICON_SRC="$CHECKOUT/logo.png"
    ICON_VALUE="ship-of-tools"
    if [ -f "$ICON_SRC" ]; then
        cp -f "$ICON_SRC" "$PREFIX/logo.png" 2>/dev/null || true
        if mkdir -p "$HOME/.local/share/icons/hicolor/256x256/apps" 2>/dev/null &&
           cp -f "$ICON_SRC" "$HOME/.local/share/icons/hicolor/256x256/apps/ship-of-tools.png" 2>/dev/null; then
            command -v gtk-update-icon-cache >/dev/null 2>&1 &&
                gtk-update-icon-cache -q -t -f "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
        else
            ICON_VALUE="$PREFIX/logo.png"
        fi
    else
        ICON_VALUE="$PREFIX/logo.png"
    fi
    cat > "$HOME/.local/share/applications/ship-of-tools.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Ship of Tools
Comment=Agentic Julia development environment
Exec=$HOME/.local/bin/sot-launch
Icon=$ICON_VALUE
Terminal=false
Categories=Development;IDE;
StartupWMClass=sot
EOF
    command -v update-desktop-database >/dev/null 2>&1 &&
        update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
    say "FE launcher: sot-launch (+ desktop entry, icon $ICON_VALUE)"
    fi
fi

# ---- 8b. pick up the new version in a RUNNING daemon ---------------------------
# An installer update swaps binaries + flips repo/current, but a running sotd
# keeps executing the old binary until restarted. Restart it here so the
# update takes effect now, not at the next reboot. (Launcher-started daemons
# — macOS / --no-service — are restarted by the next sot-launch; say so.)
if [ "$OS" = Linux ] && [ "$WANT_DAEMON" = 1 ] && [ "$NO_SERVICE" = 0 ] \
   && systemctl --user is-active sotd.service >/dev/null 2>&1; then
    say "restarting sotd to pick up the new version"
    systemctl --user try-restart sotd.service || true
elif [ "$WANT_DAEMON" = 1 ]; then
    say "note: a running sotd keeps the old version until restarted (next sot-launch restarts it if its socket is gone; or stop it manually)"
fi

# ---- 9. install manifest -------------------------------------------------------
# $PREFIX/install.json (schema 1) — how the binaries find their own install
# instead of guessing from XDG env vars: the updater resolves its staging root
# (and, in later phases, the checkout and bin dirs) from `prefix`. Written
# LAST so it always describes a completed install; an update re-run refreshes
# it. Read by sot-updater's InstallManifest (rust/updater/src/manifest.rs) —
# keep the two in sync.
SERVICE="none"
[ "$OS" = Linux ] && [ "$WANT_DAEMON" = 1 ] && [ "$NO_SERVICE" = 0 ] && SERVICE="systemd"
COMMIT="$(git -C "$CHECKOUT" rev-parse HEAD 2>/dev/null || echo unknown)"
# Temp file plus rename: a heredoc straight onto the live path truncates it
# first, so an interrupt would leave the machine with no readable manifest.
installer_manifest_json "$PREFIX" "$CONFIG" "$SERVICE" "${VERSION#v}" "$VERSION" "$COMMIT" "$(date -u +%FT%TZ)" "$HUB_ALIAS" "$WANT_DAEMON" "$WANT_FRONTEND" \
    > "$PREFIX/install.json.new"
mv "$PREFIX/install.json.new" "$PREFIX/install.json"
say "wrote $PREFIX/install.json (schema 1, daemon=$WANT_DAEMON frontend=$WANT_FRONTEND, service=$SERVICE)"

# A shared-home Linux cluster used to set SOT_RELAY_ENDPOINT with a
# per-host `case` in the shell profile (ADR 0028); `sotd topology
# relay-endpoint` now derives the right value on every box (hub's own
# socket, a frontend's forward tunnel, or the reverse-tunnel socket) from
# hosts.toml, so that block becomes this one-liner. The profile is the
# maintainer's own file outside this repo — paste it by hand, this script
# never edits it. The fallback keeps a box working (same value ADR 0028
# used before this) if the command fails or sotd isn't on PATH yet; it
# runs on every non-interactive ssh, so it stays quiet either way.
if [ "$OS" = Linux ]; then
    say "shell profile: replace any per-host SOT_RELAY_ENDPOINT case block with:"
    say '  export SOT_RELAY_ENDPOINT="$(sotd topology relay-endpoint 2>/dev/null)"'
    say '  : "${SOT_RELAY_ENDPOINT:=tcp:127.0.0.1:18743}"'
fi

say "DONE — Ship of Tools $VERSION installed (daemon=$WANT_DAEMON frontend=$WANT_FRONTEND)."
[ -n "$BE_ALIAS" ] && say "reminder: key-based ssh to '$BE_ALIAS' is required (ssh $BE_ALIAS true)"
exit 0
