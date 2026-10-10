#!/usr/bin/env bash
# install.sh — Ship of Tools installer for Linux/macOS (ADR 0030 §5).
#
#   curl -fsSL <raw-url>/scripts/install.sh | bash -s -- --local
#   ./scripts/install.sh --local                     # all-in-one on this box
#   ./scripts/install.sh --backend <ssh-alias>       # FE here → remote BE
#   ./scripts/install.sh --be-only                   # headless backend/canary
#   [--version vX.Y.Z] [--prefix <dir>] [--no-service]
#   [--hub <ssh-alias>]     # this box does NOT share the hub's home: fetch
#                           # its hosts.toml (`sotd topology sync`) once unpacked
#   [--force-role-change]  # consent to installing over another prefix's live daemon, over an existing
#                          # install from a host hosts.toml runs no sotd on, or to recording less than
#                          # install.json records
#                                                    # default: latest release
#   SOT_INSTALL_TAG=<tag> ./scripts/install.sh ...   # run THIS checkout's body
#
# Role: a declared hosts.toml naming this host (host_name()) wins — its
# daemon/frontend flags say what gets installed and enabled here (a frontend
# runs its own local daemon too), no --local/--backend/--be-only needed. Those flags are the fallback for a box
# with no entry yet (a brand-new user, or one not sharing the hub's home).
#
# What it does (idempotent; re-run to upgrade):
#   1. preflight — arch/glibc floor for the FE, tar/curl present
#   2. download the release artifacts from the fixed release URL + verify SHA256SUMS
#   3. unpack them; resolve the role (the declared topology, else the flags)
#      with the unpacked sotd, and run the ownership, host and install-record
#      gates before anything is written under $PREFIX; then lay out $PREFIX
#      (~/.local/share/sot): bin/ updates/ repo/current
#   4. REPO CHECKOUT at the release tag (ADR 0030 addendum: the repo IS the
#      manual and the resource tree; blobless partial clone = full history
#      for blame, only the tag's tree downloaded; supersedes the curated
#      julia bundle) + juliaup + Pkg.instantiate inside the checkout
#   5. config: delegate this box's folder-trust declaration to the offline owner;
#      hosts.toml is read (role) and, with --hub, fetched — never written here
#   6. agent comm resources: ~/.sot-comm plus Claude/Codex skills
#   7. backend roles: install+enable the systemd --user sotd unit
#   8. FE roles: ~/.local/bin/sot-launch wrapper + app/desktop entry
#
# Development machines DON'T use this release installer — they run from a checkout.
set -euo pipefail

REPO="${SOT_INSTALL_REPO:-kalidke/ship-of-tools}"
PREFIX="${SOT_PREFIX:-$HOME/.local/share/sot}"
CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}/sot"


ROLE="" VERSION="" BE_ALIAS="" HUB_ALIAS="" NO_SERVICE=0 FORCE_ROLE_CHANGE=0
GLIBC_FLOOR_FE="2.35"

say()  { printf '\033[1;36m==\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }

# Byte-identical to scripts/lib/sot-daemon.sh's (a test pins it): the first
# copies below run before the checkout that holds the library exists.
sot_install_copy() {  # <src> <dst> [mode]
    if cp -p "$1" "$2.new.$$" && { [ -z "${3:-}" ] || chmod "$3" "$2.new.$$"; } && mv -f "$2.new.$$" "$2"; then
        return 0
    fi
    rm -f "${2:?}.new.$$"
    return 1
}

# Refuse characters a shell-embedded path (systemd unit ExecStart, JSON
# manifest, sed substitution, launcher heredocs) cannot carry safely, plus
# a newline, which turns one generated service or manifest line into two.
# Explicit rejection over silent corruption. Shared by --prefix and the
# project root ($HOME) — deploy/sotd.service's ExecStart now embeds both
# inside a shell string, where a stray quote breaks the unit.
reject_unsafe_path_chars() {  # <label> <value>
    case "$2" in
        *[\&\|\;\"\'\\\`]*|*' '*|*'	'*|*$'\n'*)
            die "unsupported characters in $1 '$2' — no spaces, quotes, backslashes, newlines, or shell metacharacters" ;;
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

# What install.json's `service` records for this run: "systemd" when the run
# installs the unit (Linux, a daemon here, no --no-service), else "none".
installer_service_record() {  # <os> <want-daemon 0|1> <no-service 0|1>
    if [ "$1" = Linux ] && [ "$2" = 1 ] && [ "$3" = 0 ]; then printf systemd; else printf none; fi
}

# "allow" | "refuse:<why>": under a declared topology, a run may not record
# less than the install record already says the install runs: a daemon
# ("daemon": true) or its systemd unit ("service": "systemd"). A home several
# hosts share holds one install and one record, serving the hosts that run
# sotd; a host that runs no daemon there would rewrite both. On a home of its
# own the same run is this box's role change. Either way it takes
# --force-role-change. With no hosts.toml nothing is declared and the record
# is this box's alone. Pure, so the decision is testable without a release.
installer_record_decision() {  # <install.json> <topology-declared 0|1> <new-daemon 0|1> <new-service> <force 0|1>
    if [ "$2" != 1 ] || [ "$5" = 1 ] || [ ! -f "$1" ]; then printf allow; return; fi
    if [ "$3" = 0 ] && grep -q '"daemon": *true' "$1"; then
        printf 'refuse:%s records an install that runs a daemon, and this run would record none' "$1"
        return
    fi
    if [ "$4" != systemd ] && grep -q '"service": *"systemd"' "$1"; then
        printf 'refuse:%s records an install whose daemon runs as a systemd unit, and this run would record none' "$1"
        return
    fi
    printf allow
}

# "allow" | "refuse:<why>": the shared install is written only from a host the
# declared topology runs sotd on. Under a readable hosts.toml that runs none
# here (this host unlisted, or listed with neither key), a run over an install
# already at the prefix is refused, whatever role its flags ask for: on a home
# several hosts share, that install serves the hosts that run sotd, and the
# layout would replace their binaries and rollback state. A first install, no
# hosts.toml, an unreadable one (installer_record_decision then decides) and
# --force-role-change are allowed. Reads the prefix only.
installer_host_decision() {  # <topology-readable 0|1> <topology-role> <prefix> <force 0|1>
    case "$2" in *"daemon:1"*) printf allow; return ;; esac
    if [ "$1" != 1 ] || [ "$4" = 1 ] || { [ ! -f "$3/install.json" ] && [ ! -e "$3/bin/sotd" ]; }; then
        printf allow; return
    fi
    printf 'refuse:hosts.toml runs no sotd on this host, and %s already holds an install' "$3"
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

# Mirrors sot_log::host::state_dir::host_name() (rust/log/src/host/state_dir.rs): this
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

# "daemon:0|1 frontend:0|1" — what this box installs for <self>'s entry in
# `sotd topology status`'s output, the one parser (rust/protocol/src/topology/
# mod.rs); this reads its plain-line table, not hosts.toml itself, so it
# stays a consumer, not a second parser. A `frontend` entry installs a daemon
# too, `daemon` declared or not: every box that runs a window runs its own
# private local daemon, as on Windows. `daemon = true` adds only that other
# boxes dial it through the hub, a list this installer never writes. "none"
# when the table doesn't list self: no hosts.toml yet, or one that doesn't
# name this box — both are the same "fall back to flags" signal to the
# caller. Pure, so the decision is testable against canned status text.
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
            if (f) d = 1
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

# Write the FE-only wrapper for PREFIX that dials BE-ALIAS over SSH into DEST:
# beside DEST under a name of this shell's own ($$), then moved in, so a
# failed write leaves the old wrapper whole and returns 1.
installer_render_remote_launch() {  # <prefix> <be-alias> <dest>
    local prefix="$1" be_alias="$2" tmp="$3.new.$$"
    cat > "$tmp" <<EOF || { rm -f "${tmp:?}"; return 1; }
#!/usr/bin/env bash
# FE-only install -> remote BE over SSH (key auth required, ADR 0030 §5).
# Item 2 follow-up: this used to be a second, hand-maintained copy of
# scripts/launch-sot.sh's tunnel-open + backend-ensure + frontend-invoke
# logic (one fixed tunnel, no per-host support) -- it now delegates to the
# pinned checkout's own copy instead, so the two never drift.
#
# Kept from the old heredoc (install-layout-specific; no equivalent in
# launch-sot.sh itself, which only knows git pull / cargo build, not
# sot-apply's staged $prefix/repo/versions flip): applying an armed
# pending update (staged by the frontend's own self-check) before every
# launch. Exit-75 respawn and crash-loop rollback are DROPPED, not kept --
# launch-sot.sh has never had them for the plain Unix launcher either
# (that's Windows-only today, ADR 0017 / relaunch-sot.ps1), so this
# wrapper now matches every other Unix launch path instead of being the
# one with more supervision than the rest.
#
# SOT_REMOTE_REPO is deliberately left UNSET: this install has no local
# knowledge of the remote's checkout (never had one -- the old heredoc
# only ever queried the remote's installed sotd directly).
if [ -x "$prefix/bin/sot-apply" ]; then
    APPLY_OUT="\$("$prefix/bin/sot-apply" 2>&1)"
    [ -n "\$APPLY_OUT" ] && printf '%s\n' "\$APPLY_OUT" >&2
fi
export SOT_HOST="$be_alias"
export SOT_FRONTEND_BIN="$prefix/bin/sot"
export SOT_NO_UPDATE=1
exec "$prefix/repo/current/scripts/launch-sot.sh" "\$@"
EOF
    chmod +x "$tmp" && mv -f "$tmp" "$3" || { rm -f "${tmp:?}"; return 1; }
}

installer_retire_tmux_unit() {  # <systemd-user-dir> — v0.6.0 deleted the tmux
    # runtime: retire the keeper unit earlier installs enabled (ADR 0038,
    # superseded). No-op if the unit was never installed.
    unit="$1/sot-tmux.service"
    [ -f "$unit" ] || return 0
    systemctl --user disable --now sot-tmux.service 2>/dev/null || true
    rm -f "${unit:?}"
}

# Step 6: a resolution with no daemon here must not leave a previously
# installed LOCAL backend running (a wrong-topology remnant). Only
# `--backend <host>` and a topology entry with neither flag resolve that way;
# a box that runs a window resolves a daemon of its own
# (installer_topology_role). Under a declared topology the unit file and its
# enable link may serve another host that shares this home, so they stay: the
# unit's host pin (`sotd topology pin`) keeps it from starting here, and this
# host's own run is stopped. With no hosts.toml the unit is this box's alone,
# and it is disabled.
# Whether this box has a hosts.toml that `sotd topology status` cannot read: such a file is still a declared
# topology, and a retire under it must never disable a unit that may serve another host. Only a missing file (status
# says "no hosts.toml at") means this box is alone. Prints status's error when it is unreadable.
installer_topology_unreadable() {  # <sotd>
    local err
    err="$("$1" topology status 2>&1 >/dev/null)" && return 1
    case "$err" in *"no hosts.toml at"*) return 1 ;; esac
    printf '%s\n' "$err"
}

installer_retire_local_service() {  # <want-daemon 0|1> <prefix> <topology-declared 0|1>
    [ "$1" = 0 ] && command -v systemctl >/dev/null 2>&1 || return 0
    if [ "$3" = 1 ]; then
        [ -f "$HOME/.config/systemd/user/sotd.service" ] || return 0
        "$2/bin/sotd" topology pin --dir "$HOME/.config/systemd/user" \
            || say "WARNING: sotd topology pin failed: sotd.service is not pinned to the hosts that run sotd"
        systemctl --user stop sotd.service 2>/dev/null || true
        systemctl --user daemon-reload 2>/dev/null || true
        say "stopped this host's sotd.service; the declared topology runs no daemon here"
    elif systemctl --user is-enabled sotd.service >/dev/null 2>&1; then
        systemctl --user disable --now sotd.service || true
        say "disabled the local sotd.service from a previous all-in-one install"
    fi
}

# Step 7 on Linux: render, enable and start the sotd unit for PREFIX from
# TEMPLATE. No JULIA_DEPOT_PATH (or any other) config is written here: owner
# ruling 2026-09-02 forbids writing/overwriting depot config anywhere. The
# unit itself (deploy/sotd.service) sources ~/.bashrc before exec'ing sotd, so
# the daemon inherits whatever the owner's shell profile exports. A prior
# install's forbidden drop-in is healed unconditionally near the top of this
# script (step 0), not here.
installer_enable_local_service() {  # <prefix> <template> <socket>
    mkdir -p "$HOME/.config/systemd/user"
    installer_retire_tmux_unit "$HOME/.config/systemd/user"
    render_sotd_unit "$1" "$2" "$HOME/.config/systemd/user/sotd.service"
    # The unit and its enable link live in the home, which other hosts may
    # share: pin it to the hosts that run sotd before any manager reloads it.
    "$1/bin/sotd" topology pin --dir "$HOME/.config/systemd/user" \
        || say "WARNING: sotd topology pin failed: sotd.service is not pinned to the hosts that run sotd"
    systemctl --user daemon-reload
    systemctl --user enable --now sotd.service
    loginctl enable-linger "${USER:-$(id -un)}" 2>/dev/null || true
    say "sotd running: $(systemctl --user is-active sotd.service) (socket $3)"
}

# Step 8's wrapper: the all-in-one one, which ensures this box's own daemon
# (sot_daemon_ensure) before every window, unless this install names an
# explicit remote backend to dial over SSH (--backend <alias>, or its
# interactive equivalent) — the one window that runs without a local daemon.
installer_render_wrapper() {  # <prefix> <target> <be-alias-or-empty> <dest>
    if [ -z "$3" ]; then
        render_sot_launch "$1" "$2" "$4"
    else
        installer_render_remote_launch "$1" "$3" "$4"
    fi
}

# Both installer paths delegate folder-trust declaration to the agents-owned offline command;
# a failure is reported and never described as a successful declaration.
installer_declare_trust() {  # <absolute-sotd-path> <prefix>
    local binary="$1" prefix="$2" output result
    case "$binary" in
        /*) ;;
        *) say "WARN: folder trust not declared - sotd path is not absolute"; return 0 ;;
    esac
    if output="$("$binary" trust declare "$prefix" 2>&1)"; then
        case "$output" in
            Declared) say "folder trust declared" ;;
            Kept) say "folder trust kept" ;;
            *) say "WARN: folder trust not declared - unexpected command outcome" ;;
        esac
    else
        result=$?
        say "WARN: folder trust not declared (exit $result)"
        [ -z "$output" ] || printf '%s\n' "$output" >&2
    fi
    return 0
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
    tmp="$(mktemp)"; trap 'rm -f "${tmp:?}"' EXIT
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
    rm -f "${HOME:?}/.config/systemd/user/sotd.service.d/depot.conf"
    command -v systemctl >/dev/null 2>&1 && systemctl --user daemon-reload 2>/dev/null
fi

while [ $# -gt 0 ]; do
    case "$1" in
        --local) ROLE=local ;;
        --backend) ROLE=remote; BE_ALIAS="${2:?--backend needs an ssh alias}"; shift ;;
        --be-only) ROLE=be-only ;;
        # This box does not share the hub's home: fetch its hosts.toml
        # (`sotd topology sync`) with the unpacked sotd, below.
        --hub) HUB_ALIAS="${2:?--hub needs an ssh alias}"; shift ;;
        --version) VERSION="${2:?}"; shift ;;
        --prefix) PREFIX="${2:?}"; shift ;;
        # Skip the systemd unit install/enable: the caller supervises sotd
        # itself (e.g. a systemd-run --user transient unit). A shared home
        # needs no such flag: the unit is pinned to the hosts that run sotd
        # (`sotd topology pin`).
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

# Finding 2 (v0.6.5 macOS field report): the daemon correctly REFUSES to
# put capsule records on a remote filesystem (`rust/log/src/host/volume.rs`
# `preflight_volume`'s statfs denylist), but today the only place that
# says so is the backend's own journal -- from the frontend it just looks
# like the retry blink, "supervisor lane not answering", forever. Surfacing
# the real reason in the pane itself needs a design pass (deferred); for
# now, warn here, at install time, on the two directories that matter:
# $HOME (the default project root) and the state dir the daemon will
# actually use. Linux only -- `stat -f -c %T` is GNU coreutils, and this
# whole failure mode is Linux/Windows-only today anyway (capsule mode
# isn't wired up on macOS -- see `rust/log/src/host/volume.rs`'s
# `preflight_volume` non-Linux-unix arm). Warn, never abort: a remote
# home is a real, working (if degraded) setup for everything except
# capsule rows.
if [ "$OS" = Linux ] && command -v stat >/dev/null 2>&1; then
    # Mirrors `REMOTE_FS_TYPES`' names in host/volume.rs (NFS, SMB, CIFS, SMB2,
    # 9p, FUSE) as GNU `stat -f -c %T` actually spells them.
    check_remote_fs() {
        local label="$1" dir="$2"
        local fstype
        fstype="$(stat -f -c %T "$dir" 2>/dev/null)" || return 0
        case "$fstype" in
            nfs|nfs4|cifs|smb2|fuse.sshfs|9p)
                say "WARNING: $label ($dir) is on a remote filesystem ($fstype)."
                say "  Capsule session records cannot live on a remote filesystem"
                say "  (ADR 0043 decision 23). Set XDG_STATE_HOME to a local-disk"
                say "  directory in the environment sotd starts from -- for a"
                say "  systemd install: systemctl --user set-environment"
                say "  XDG_STATE_HOME=/path/on/local/disk, or export it in"
                say "  ~/.bashrc (which the unit sources) -- then restart sotd."
                say "  Whichever directory you choose, sotd requires it to be owned by"
                say "  you with mode 700 (it refuses to start otherwise) and does not"
                say "  back it up -- pick a path that persists across reboots, not /tmp."
                ;;
        esac
    }
    check_remote_fs '$HOME' "$HOME"
    check_remote_fs 'the state dir' "${XDG_STATE_HOME:-$HOME/.local/state}"
fi
for t in curl tar; do command -v "$t" >/dev/null || die "$t is required"; done
# The glibc floor for the frontend binary is checked further down, once the
# role is resolved (WANT_FRONTEND) — that now needs the declared topology,
# read via the sotd unpacked below, so it can't run this early any more.

# Downloader: the repo is public, so unauthenticated curl against the fixed
# release-download URL works — no API call, no auth.
WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-install.XXXXXX")"; trap 'rm -rf "${WORK:?}"' EXIT

# VERSION is always known here: the prelude resolves and re-execs first.
VERSION="${VERSION:-${SOT_INSTALL_TAG:-}}"
VER="${VERSION#v}"
say "installing Ship of Tools $VERSION into $PREFIX"

# This body sources the tag's scripts/lib/sot-daemon.sh; a release older than
# that library has none. Ask for it BEFORE the first copy, unit or symlink
# (only the depot-config heal above has run). The one-liner never pairs this
# body with an old tree: it runs the tag's own installer. curl's exit 22 is an
# HTTP error (the file is absent); any other failure is the network.
lib_rc=0
curl -fsSI -o /dev/null "https://raw.githubusercontent.com/$REPO/$VERSION/scripts/lib/sot-daemon.sh" || lib_rc=$?
[ "$lib_rc" -ne 22 ] \
    || die "release $VERSION predates this installer (no scripts/lib/sot-daemon.sh); install it with its own installer: run the one-liner without SOT_INSTALL_TAG, or that tag's scripts/install.sh"
[ "$lib_rc" -eq 0 ] \
    || die "cannot reach raw.githubusercontent.com to check release $VERSION (curl exit $lib_rc); nothing was changed, try again"

# ---- 2. download + verify ----------------------------------------------------
ASSETS=("SHA256SUMS" "sot-$VER-$TARGET.tar.gz")

dl() {
    curl -fsSL -o "$WORK/$1" "https://github.com/$REPO/releases/download/$VERSION/$1" \
        || die "asset $1 not found on release $VERSION"
}
say "downloading ${#ASSETS[@]} assets"
for a in "${ASSETS[@]}"; do dl "$a"; done
if command -v sha256sum >/dev/null; then
    ( cd "$WORK" && sha256sum -c --ignore-missing SHA256SUMS ) || die "checksum verification FAILED"
else
    ( cd "$WORK" && shasum -a 256 -c --ignore-missing SHA256SUMS ) || die "checksum verification FAILED"
fi

# ---- 3. unpack ---------------------------------------------------------------
# Unpacked into $WORK only: the role resolution and the gates below run the
# unpacked sotd, so a refused install has written nothing under $PREFIX.
mkdir -p "$CONFIG"
tar -xzf "$WORK/sot-$VER-$TARGET.tar.gz" -C "$WORK"
BINDIR="$WORK/sot-$VER-$TARGET"
# Every answer the gates read comes from this sotd, so one that cannot run here
# (a TMPDIR mounted noexec) stops the install instead of reading as a topology.
"$BINDIR/sotd" --version >/dev/null 2>&1 \
    || die "the unpacked sotd cannot run from $WORK (is ${TMPDIR:-/tmp} mounted noexec?); set TMPDIR to a folder that allows execution"

# ---- heal a pre-0.6 hosts.toml (finding 3a, v0.6.5 macOS field report) -----------
# The old grammar (`default_host` at top level) is a loud parse error under
# the current one (rust/protocol/src/topology/mod.rs), not a silently-kept
# file -- an installer that preserved one across an upgrade left the box
# with NO topology plan at all, which is what then walked
# `scripts/launch-sot.sh` into the bash-3.2 unbound-array crash (finding
# 3b, fixed separately). Move it aside, never delete it, so `--hub` above
# or a plain `sotd topology sync --hub <alias>` writes the current
# grammar fresh on next launch.
if [ -f "$CONFIG/hosts.toml" ] && grep -q '^default_host' "$CONFIG/hosts.toml" 2>/dev/null; then
    OLD_HOSTS="$CONFIG/hosts.toml.v1-$(date +%Y%m%d%H%M%S)"
    mv "$CONFIG/hosts.toml" "$OLD_HOSTS"
    say "pre-0.6 hosts.toml moved aside to $OLD_HOSTS; the hub sync writes the current grammar on next launch (or run: sotd topology sync --hub <alias>)"
fi

# ---- role resolution: the declared topology, else flags -------------------------
# D9 (dev/output/topology-plan.md §C/§D): the hub's hosts.toml is canonical —
# when it names this host, that entry's daemon/frontend flags decide what's
# installed and enabled here, no --local/--backend/--be-only or Q&A needed;
# a frontend entry installs its own local daemon too (installer_topology_role).
# A box with no entry yet (a brand-new user) falls back to the role flag /
# interactive choice below. --hub fetches a copy first, for a box that does
# not share the hub's home; a box that does already has the file, no fetch
# needed.
if [ -n "$HUB_ALIAS" ]; then
    "$BINDIR/sotd" topology sync --hub "$HUB_ALIAS" || die "topology sync --hub $HUB_ALIAS failed"
fi
SELF_HOST="$(installer_self_host)"
WANT_DAEMON=0
WANT_FRONTEND=0
TOPO_ROLE=none
TOPO_DECLARED=0
TOPO_READABLE=0
UNREADABLE=""
if STATUS_OUT="$("$BINDIR/sotd" topology status 2>/dev/null)"; then
    TOPO_DECLARED=1
    TOPO_READABLE=1
    TOPO_ROLE="$(installer_topology_role "$STATUS_OUT" "$SELF_HOST")"
elif UNREADABLE="$(installer_topology_unreadable "$BINDIR/sotd")"; then
    TOPO_DECLARED=1
    say "WARNING: hosts.toml could not be read ($UNREADABLE); the role comes from the flags, and the shared sotd.service is never disabled from here"
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
if [ "$TOPO_READABLE" = 1 ] && [ "$TOPO_ROLE" = none ] && [ "$WANT_DAEMON" = 1 ]; then
    say "WARNING: hosts.toml declares no daemon on '$SELF_HOST', so sotd.service's host pin keeps it from starting here; declare the host (daemon or frontend) in hosts.toml"
fi

# A coding agent isn't part of this installer's business, but a daemon role
# with neither one on PATH will start sessions that go nowhere — warn, don't
# die. Fallback dirs mirror resolve_claude/resolve_ccx in
# rust/backend/src/agents/argv.rs.
if [ "$WANT_DAEMON" = 1 ] && ! command -v claude >/dev/null 2>&1 \
    && ! command -v ccx >/dev/null 2>&1 \
    && [ ! -x "$HOME/.local/bin/claude" ] && [ ! -x "$HOME/.claude/local/claude" ] \
    && [ ! -x "$HOME/.local/bin/ccx" ]; then
    say "WARNING: neither claude nor ccx (the Codex launcher) found on PATH — install and log in a coding agent on this machine before running sessions"
fi

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
# Run now that the role is known and before the first write under the prefix
# or ~/.local/bin (the layout and the launcher) — a refused install changes
# nothing there.
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
HOST_DECISION="$(installer_host_decision "$TOPO_READABLE" "$TOPO_ROLE" "$PREFIX" "$FORCE_ROLE_CHANGE")"
case "$HOST_DECISION" in
    refuse:*)
        printf '\033[1;31mERROR:\033[0m %s\n' "${HOST_DECISION#refuse:}" >&2
        printf '       Declare this host in hosts.toml (frontend for a machine that runs a window, daemon for one other machines dial; a machine with a home of its own is declared in the hub'"'"'s hosts.toml and re-runs with --hub <hub alias> to fetch it) and re-run, or run the installer on a host hosts.toml already declares. --force-role-change overrides this check.\n' >&2
        exit 2 ;;
esac
RECORD_DECISION="$(installer_record_decision "$PREFIX/install.json" "$TOPO_DECLARED" "$WANT_DAEMON" \
    "$(installer_service_record "$OS" "$WANT_DAEMON" "$NO_SERVICE")" "$FORCE_ROLE_CHANGE")"
case "$RECORD_DECISION" in
    refuse:*)
        printf '\033[1;31mERROR:\033[0m %s\n' "${RECORD_DECISION#refuse:}" >&2
        printf '       If this host runs the daemon, drop --no-service. If hosts.toml could not be read, fix it (sotd topology status names the line). If this machine'"'"'s own role changed, re-run with --force-role-change.\n' >&2
        exit 2 ;;
esac

# ---- 3b. layout, now that the gates allow it --------------------------------
mkdir -p "$PREFIX/bin" "$PREFIX/updates" "$PREFIX/repo"
# sot-capsule is sotd's capsule-runtime pair (ADR 0042 L1a): sotd resolves it
# next to its own executable, so it must land in the same directory. Archives
# that predate the capsule runtime lack it, hence the skip for it alone.
for b in sot sotd sot-capsule; do
    [ "$b" = sot-capsule ] && [ ! -f "$BINDIR/$b" ] && continue
    if [ -f "$PREFIX/bin/$b" ]; then
        sot_install_copy "$PREFIX/bin/$b" "$PREFIX/bin/$b.prev" || die "backing up $PREFIX/bin/$b failed"
    fi
    sot_install_copy "$BINDIR/$b" "$PREFIX/bin/$b" 0755 || die "installing $PREFIX/bin/$b failed"
    # Gatekeeper: strip any quarantine attr (browser downloads carry it).
    [ "$OS" = Darwin ] && xattr -d com.apple.quarantine "$PREFIX/bin/$b" 2>/dev/null || true
done
# The offline apply/rollback script (Phase C3). Newer releases ship it in the
# archive; otherwise it lands from the checkout below.
if [ -f "$BINDIR/sot-apply" ]; then
    sot_install_copy "$BINDIR/sot-apply" "$PREFIX/bin/sot-apply" 0755 || die "installing $PREFIX/bin/sot-apply failed"
fi
# A manual installer run is a NEW transaction: stale rollback state from a
# previous auto-apply must not pair old last-good pointers with these fresh
# .prev binaries (a later crash-loop rollback would mix versions).
rm -f "${PREFIX:?}"/updates/last-good-*.json "${PREFIX:?}"/updates/just-applied-* 2>/dev/null || true
say "binaries: $("$PREFIX/bin/sotd" --version)"
DEFAULT_SOCKET="$("$PREFIX/bin/sotd" session-socket-path sot)"

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
    # Public repo: plain https clone, no auth needed.
    git clone --filter=blob:none --no-checkout "https://github.com/$REPO" "$BASE" \
        || die "base clone failed"
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
. "$CHECKOUT/scripts/lib/sot-daemon.sh" || die "cannot read $CHECKOUT/scripts/lib/sot-daemon.sh"
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
    rm -rf "${REPO_DIR:?}/current.pre-versioned"
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
    sot_install_copy "$CHECKOUT/scripts/sot-apply.sh" "$PREFIX/bin/sot-apply" 0755 || die "installing $PREFIX/bin/sot-apply failed"
fi
# Keep the previously-active version dir for rollback; prune everything else.
for v in "$REPO_DIR/versions"/*; do
    [ -d "$v" ] || continue
    case "$(basename "$v")" in
        "$VERSION") ;;
        "$PREV_VERSION") ;;
        *)
            say "pruning old version dir $(basename "$v")"
            git -C "$BASE" worktree remove --force "$v" 2>/dev/null || rm -rf "${v:?}"
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
            rm -f "${CHECKOUT:?}/$env/Manifest.toml"
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
# above.
installer_retire_local_service "$WANT_DAEMON" "$PREFIX" "$TOPO_DECLARED"
# The owner creates or upgrades settings and keeps any existing trust answer.
installer_declare_trust "$PREFIX/bin/sotd" "$HOME"

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
    installer_enable_local_service "$PREFIX" "$BINDIR/sotd.service" "$DEFAULT_SOCKET"
fi

# ---- 8. FE launcher -------------------------------------------------------------
if [ "$WANT_FRONTEND" = 1 ]; then
    mkdir -p "$HOME/.local/bin"
    installer_render_wrapper "$PREFIX" "$TARGET" "$BE_ALIAS" "$HOME/.local/bin/sot-launch" \
        || die "writing $HOME/.local/bin/sot-launch failed"
    chmod +x "$HOME/.local/bin/sot-launch"
    if [ "$OS" = Darwin ]; then
        # A minimal .app bundle so the FE launches from Launchpad/Spotlight/
        # Dock like a real app. Locally-created bundles carry no quarantine
        # attr, so Gatekeeper doesn't object. The icon is generated from the
        # checkout's logo with sips+iconutil (both ship with macOS).
        APP="$HOME/Applications/Ship of Tools.app"
        mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
        cat > "$WORK/Info.plist" <<EOF
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
        sot_install_copy "$WORK/Info.plist" "$APP/Contents/Info.plist" 0644 || die "writing $APP/Contents/Info.plist failed"
        cat > "$WORK/sot-launch.app" <<EOF
#!/usr/bin/env bash
exec "$HOME/.local/bin/sot-launch"
EOF
        sot_install_copy "$WORK/sot-launch.app" "$APP/Contents/MacOS/sot-launch" 0755 || die "writing $APP/Contents/MacOS/sot-launch failed"
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
SERVICE="$(installer_service_record "$OS" "$WANT_DAEMON" "$NO_SERVICE")"
COMMIT="$(git -C "$CHECKOUT" rev-parse HEAD 2>/dev/null || echo unknown)"
# Temp file plus rename: a heredoc straight onto the live path truncates it
# first, so an interrupt would leave the machine with no readable manifest.
installer_manifest_json "$PREFIX" "$CONFIG" "$SERVICE" "${VERSION#v}" "$VERSION" "$COMMIT" "$(date -u +%FT%TZ)" "$HUB_ALIAS" "$WANT_DAEMON" "$WANT_FRONTEND" \
    > "$PREFIX/install.json.new.$$"
mv "$PREFIX/install.json.new.$$" "$PREFIX/install.json"
say "wrote $PREFIX/install.json (schema 1, daemon=$WANT_DAEMON frontend=$WANT_FRONTEND, service=$SERVICE)"

say "DONE — Ship of Tools $VERSION installed (daemon=$WANT_DAEMON frontend=$WANT_FRONTEND)."
[ -n "$BE_ALIAS" ] && say "reminder: key-based ssh to '$BE_ALIAS' is required (ssh $BE_ALIAS true)"
exit 0
