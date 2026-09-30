#!/usr/bin/env bash
# comm-lib.sh — shared helpers for sot-comm. SOURCED, not executed.
# Implements the v1 protocol (see comm/PROTOCOL.md). Runtime data lives under
# $SOT_COMM_HOME (default ~/.sot-comm).

# _sot_is_windows — the ONE shared platform test (Codex review, PR1 round 2
# finding 6: Windows-specific defaults/guards must live HERE, not duplicated
# per-caller — a caller-side workaround dies with that process, so a later,
# separately-invoked script (e.g. a retried invocation) never
# sees it and falls through to Linux-only logic that has no role on Windows).
# comm-session-skill.sh keeps its own tiny copy (it does not source this file, by
# design). comm-watch.sh used to as well, and mirroring is how it came to poll a
# different file from the one comm-poll.sh read — it now sources this library like
# everything else (2026-09-27). Every script that sources comm-lib.sh calls this
# one instead of re-deriving it.
_sot_is_windows() {
    case "${OS:-}" in Windows_NT) return 0 ;; esac
    case "${OSTYPE:-}" in msys*|cygwin*|win32) return 0 ;; esac
    case "$(uname -s 2>/dev/null || true)" in MINGW*|MSYS*|CYGWIN*) return 0 ;; esac
    return 1
}

# _sot_is_msys — a git-bash/Cygwin shell, which is NARROWER than
# _sot_is_windows on purpose: $OS=Windows_NT is an environment variable (a
# test fixture sets it, a native Windows shell inherits it), while the tier
# gated on this one needs the msys USERLAND — /proc/<pid>/winpid, `ps -W`,
# and a powershell.exe on PATH. Asking the wrong question here would spawn a
# PowerShell on a box that has none.
_sot_is_msys() {
    case "${OSTYPE:-}" in msys*|cygwin*) return 0 ;; esac
    case "$(uname -s 2>/dev/null || true)" in MINGW*|MSYS*|CYGWIN*) return 0 ;; esac
    return 1
}

PROTOCOL_VERSION=1

COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
REGISTRY="$COMM_HOME/registry.json"
INBOX_DIR="$COMM_HOME/inbox"
SELF_DIR="$COMM_HOME/self"
READ_DIR="$COMM_HOME/read"
# The registry lock (see with_lock): a FILE naming its holder. `_SOT_REG_LOCK`
# is the name every lock function uses; LOCKDIR is kept for the suites that
# seize the lock, and comm-wake.sh reassigns it for its own lock.
_SOT_REG_LOCK="$COMM_HOME/.registry.lock"
LOCKDIR="$_SOT_REG_LOCK"

now_iso() { date -u +%Y-%m-%dT%H:%M:%SZ; }

# sot_require_tools PATH_NAME TOOL... — say, on stderr, one line per TOOL that
# is not on PATH, and return nonzero if any is missing. A session whose jq,
# flock or perl is missing never sees its mail and used to be told nothing:
# each path that reads mail (comm-poll, session start, the Stop hook) checks
# the tools IT runs before first use and shows this line to the session.
sot_require_tools() {
    local path_name="$1" t rc=0
    shift
    for t in "$@"; do
        command -v "$t" >/dev/null 2>&1 && continue
        printf 'sot-comm: cannot %s: %s is missing (install it)\n' "$path_name" "$t" >&2
        rc=1
    done
    return "$rc"
}

# sot_jq ARGS... — run jq, but normalise ITS OWN OUTPUT so a caller that
# captures a single field (command substitution) or splits several
# records (mapfile / `while read`) never keeps a stray carriage return
# glued to the value. On Windows, a native (non-MSYS) `jq.exe` opens
# stdout in text mode and rewrites every \n it writes to \r\n; bash's own
# consumption idioms keep that \r — command substitution strips only the
# final \n, and mapfile/`read` split on \n only — so a handle, host,
# workspace id or registry root read back this way never compares equal
# to the clean string it should match (field report, 2026-09-27: a
# handle read back as "admiral-kitt\r" made every send from that box
# report "no such handle" for a delivery that had already landed). Exit
# status is jq's OWN, captured before the cleanup pipe runs, so this is a
# drop-in replacement for `jq` even in a boolean `-e` test.
#
# Use this ONLY where the extracted value is an IDENTIFIER — a handle, a
# host, a workspace id, a protocol version, a phase/state tag, a cursor
# offset, a filename component, or anything else fed into a comparison.
# A field that is free-text CONTENT (a message body, a status summary a
# human wrote) keeps calling `jq` directly: the same text-mode rewrite can
# add a \r before a newline the sender typed on purpose, and stripping it
# there would silently rewrite what they wrote instead of fixing a
# comparison.
sot_jq() {
    # STREAMED, not captured: an earlier body took jq's output through a
    # command substitution, which strips every trailing newline, so a
    # `while read` consumer silently lost its LAST record on every platform --
    # comm-list.sh printed 14 of 15 registered agents. Streaming also means jq
    # can now see a downstream close, so no caller of this ends its pipeline in
    # `head`: the filter picks the one value it wants instead.
    command jq "$@" | tr -d '\r'
    return "${PIPESTATUS[0]}"
}

# _sot_windows_local_pipe — the LOCAL daemon's named pipe, resolved and
# proven live (ADR 0042 amendment, decision 5, corrected 2026-09-07): asks
# the daemon binary itself for its pipe path — the SAME query
# scripts/sot-local-daemon.ps1 makes (`sotd.exe session-socket-path local`)
# — so this can never derive a different name than the one the launcher's
# own daemon binds. Then proves it live with a bounded connect-then-close
# probe, mirroring that script's Test-SotPipeOpen exactly: a pipe NAME can
# persist under \\.\pipe\ while a dead client still holds a handle to it,
# so a resolvable name alone is not evidence anything is listening. Prints
# the \\.\pipe\... path and returns 0 only when both checks pass; nothing
# printed, nonzero return otherwise. Windows-only — callers gate with
# _sot_is_windows first.
# _sot_windows_sotd_exe — the sotd executable on a Windows box: SOTD_BIN when
# set, else the RUNNING daemon's own path (an installed %LOCALAPPDATA%\sot\bin\
# sotd.exe or a dev build under a checkout's target dir -- ask the OS, not a
# fixed install path), else the install path. Prints it; 1 when none exists.
_sot_windows_sotd_exe() {
    local daemon_exe="${SOTD_BIN:-}"
    if [ -z "$daemon_exe" ] || [ ! -f "$daemon_exe" ]; then
        daemon_exe="$(powershell.exe -NoProfile -NonInteractive -Command \
            "(Get-Process -Name sotd -ErrorAction SilentlyContinue | Select-Object -First 1).Path" 2>/dev/null \
            | tr -d '\r' | head -n1)"
        [ -n "$daemon_exe" ] || daemon_exe="${LOCALAPPDATA:-}/sot/bin/sotd.exe"
    fi
    [ -f "$daemon_exe" ] || return 1
    printf '%s\n' "$daemon_exe"
}
_sot_windows_local_pipe() {
    command -v powershell.exe >/dev/null 2>&1 || return 1
    local daemon_exe
    daemon_exe="$(_sot_windows_sotd_exe)" || return 1
    local raw
    raw="$("$daemon_exe" session-socket-path local 2>/dev/null | head -n1 | tr -d '\r')"
    case "$raw" in
        '\\'*'pipe'*) : ;;
        *) return 1 ;;
    esac
    local name="${raw##*\\}"
    [ -n "$name" ] || return 1
    # The name is interpolated into a PowerShell single-quoted literal:
    # refuse anything outside the daemon's own charset rather than escape it.
    case "$name" in *[!A-Za-z0-9._-]*) return 1 ;; esac
    powershell.exe -NoProfile -NonInteractive -Command "
        \$c = New-Object System.IO.Pipes.NamedPipeClientStream('.', '$name', [System.IO.Pipes.PipeDirection]::InOut)
        try { \$c.Connect(500); exit 0 } catch { exit 1 } finally { \$c.Dispose() }
    " >/dev/null 2>&1 || return 1
    printf '%s\n' "$raw"
}

# _sot_is_plain_host_name — the shell twin of `sot_protocol`'s Rust grammar
# (`topology::is_plain_host_name` / `ssh_bridge::SshRecipe::new`): first
# character an ASCII lowercase letter or digit, the rest lowercase
# letters, digits, `.`, `_`, `-`. Two implementations of one grammar, not
# one shared: a value from THIS shell's own environment never passed
# through the Rust one. A value that DID come from `hosts.toml` always
# passes here too — C2's producer checks it at parse and at apply.
_sot_is_plain_host_name() {
    case "$1" in
        [a-z0-9]*) case "$1" in *[!a-z0-9._-]*) return 1 ;; esac ;;
        *) return 1 ;;
    esac
}

# _sot_emit_endpoint VALUE — main's ruling, 2026-09-29: every endpoint value
# leaves this file through this ONE gate. Prints VALUE and returns 0 only
# when its scheme is one THIS version can dial -- `unix:`, `pipe:`, `ssh:`
# -- and, for `ssh:`, only when both halves of `ssh:<target>[/<host>]` are
# plain host names (`<target>` is an argv element a bare `ssh` reads --
# a leading `-` would be read as an OPTION; `<host>` is interpolated into
# the remote command STRING a shell on the far end parses). For anything
# else -- an empty value, an unknown scheme, a value this version used to
# speak (`tcp:`) but no longer does, a malformed `ssh:` target/host -- it
# prints NOTHING, writes one line naming the discarded value to stderr,
# and returns nonzero. An empty value is a silent nonzero: that is
# ordinary control flow (no source had an answer), not a fault, so it
# gets no stderr line.
#
# A WHITELIST of what this version dials, not a `tcp:` blacklist -- that
# is what closes the whole class of stale-endpoint bugs rather than one
# member: it also discards a scheme a newer `sotd` invents, a `tunnel`
# line pasted into a variable, and a truncated value. Because this is the
# only `printf` that leaves either resolver below, no source can hand a
# caller an undialable value, and no caller's own scheme switch ever sees
# one.
_sot_emit_endpoint() {
    local value="$1"
    [ -n "$value" ] || return 1
    case "$value" in
        unix:*|pipe:*)
            printf '%s\n' "$value"
            return 0
            ;;
        ssh:*)
            local rest="${value#ssh:}" target host
            case "$rest" in
                */*) target="${rest%%/*}"; host="${rest#*/}" ;;
                *) target="$rest"; host="" ;;
            esac
            if _sot_is_plain_host_name "$target" && { [ -z "$host" ] || _sot_is_plain_host_name "$host"; }; then
                printf '%s\n' "$value"
                return 0
            fi
            ;;
    esac
    printf 'comm-lib: discarding an endpoint this version cannot dial: %s\n' "$value" >&2
    return 1
}

# _sot_sotd_bin — the one binary-finding ladder for a caller that only
# needs `sotd`'s PATH, no live socket: `SOTD_BIN`, `command -v sotd`,
# `~/.local/share/sot/bin/sotd`, `~/.local/bin/sotd` -- a bare `sotd`
# fails silently in a daemon-spawned capsule, whose PATH lacks
# `~/.local/bin`. `sot_daemon_endpoint`'s own `_try_sotd_socket_bin`
# cannot reuse this: its four calls also require `[ -S "$sock" ]` on a
# LIVE socket, a question this ladder's own caller
# (`_sot_planned_relay_endpoint`) does not ask.
_sot_sotd_bin() {
    local candidate
    for candidate in "${SOTD_BIN:-}" "$(command -v sotd 2>/dev/null || true)" \
                      "$HOME/.local/share/sot/bin/sotd" "$HOME/.local/bin/sotd"; do
        [ -n "$candidate" ] && [ -x "$candidate" ] && { printf '%s\n' "$candidate"; return 0; }
    done
    # LAST candidate (BLOCKER 3): a live `sotd`'s own binary, read out of
    # /proc/<pid>/exe -- a source-built box (`rust/target/release/sotd`, no
    # release install, `SOTD_BIN` unset, neither `~/.local` path present)
    # fell through this whole ladder to nothing before this line existed,
    # and this ladder's own caller (`_sot_planned_relay_endpoint`) no
    # longer falls through further to `sot_daemon_endpoint` (main's ruling,
    # pinned at test-join-disambiguation.sh:2023-2053: never the local
    # daemon for a question about the hub's endpoint) -- finding the
    # BINARY here and asking IT `topology relay-endpoint` is still a
    # planned answer, not the local daemon's own socket, so that ruling
    # stays met. `sot_daemon_endpoint` (below) guards the identical pgrep
    # loop the same way; pgrep is not on a stock git-bash PATH and must
    # never be reached for on Windows.
    if ! _sot_is_windows; then
        while IFS= read -r line; do
            local pid="${line%% *}"
            case "$pid" in ''|*[!0-9]*) continue ;; esac
            [ -r "/proc/$pid/exe" ] || continue
            candidate="$(readlink "/proc/$pid/exe" 2>/dev/null || true)"
            [ -n "$candidate" ] && [ -x "$candidate" ] || continue
            # basename gate (round-2 lane fix): unlike `sot_daemon_endpoint`'s
            # loop below, this candidate is never executed to prove itself
            # (there is no socket-serving process to query yet -- that's the
            # whole point of this ladder rung), so "readable and executable"
            # alone matches ANY process pgrep's substring search turned up --
            # a `journalctl -fu sotd` or a `tail -f .../sotd.log` with a lower
            # pid than the real daemon's own. `continue` to the next pid
            # instead of returning the first plausible one.
            case "${candidate##*/}" in
                sotd|sotd.exe) ;;
                *) continue ;;
            esac
            printf '%s\n' "$candidate"; return 0
        done < <(pgrep -af 'sotd' 2>/dev/null || true)
    fi
    return 1
}

# _sot_planned_relay_endpoint — `<sotd> topology relay-endpoint` on BOTH
# platforms, naming its binary the way the rest of this file does:
# `_sot_windows_sotd_exe` on Windows, `_sot_sotd_bin`'s ladder elsewhere.
# Drops the old `2>/dev/null`: `sotd`'s own failure line already names the
# fix ("no hosts.toml at ... run `sotd topology sync --hub <alias>`"), so
# the honest thing is to let it through rather than re-explain it. Exit
# status is `sotd`'s OWN (`${PIPESTATUS[0]}`, `sot_jq`'s own idiom above),
# not `tr`'s, so a caller can tell "no answer" from "answered empty".
_sot_planned_relay_endpoint() {
    local bin
    if _sot_is_windows; then
        bin="$(_sot_windows_sotd_exe)" || return 1
    else
        bin="$(_sot_sotd_bin)" || return 1
    fi
    "$bin" topology relay-endpoint | head -n1 | tr -d '\r'
    return "${PIPESTATUS[0]}"
}

# _sot_ssh_control — this process's own ControlPath, when the
# connection-sharing trio is even worth trying (main's ruling,
# isolation-plan.md §3 C10): `$XDG_RUNTIME_DIR/sot-comm-ssh-%C` and
# NOTHING else, where `%C` is ssh's own per-target hash. That directory is
# per-uid, private, and on local disk. Prints nothing when
# `XDG_RUNTIME_DIR` is unset -- a fallback under `$HOME` (an earlier draft
# of this helper) is wrong twice over on a shared-home fleet: the home is
# shared across boxes while `%C` hashes only user and target (two boxes
# collide on one path), and a unix socket on a network home is unusable
# for multiplexing anyway. Declining to share beats sharing the wrong
# socket.
_sot_ssh_control() {
    [ -n "${XDG_RUNTIME_DIR:-}" ] && printf '%s/sot-comm-ssh-%%C\n' "$XDG_RUNTIME_DIR"
}

# _sot_ssh_sharing_ok — once per process (cached in $_SOT_SSH_SHARING),
# decides whether this box's ssh build accepts the ControlMaster trio
# WITHOUT a network round trip: `ssh -G` parses the options and prints
# the effective configuration without connecting. On Windows, git-bash's
# MSYS ssh multiplexes but Win32-OpenSSH's support is unverified -- and
# an ssh that REJECTS an unsupported ControlMaster (rather than ignoring
# it) would be a Windows box that cannot mail at all, exactly the outcome
# C10 exists to prevent. So this decides it locally instead of trusting
# either platform's reputation: nonzero here ("Bad configuration option"
# from a build that refuses them) drops the trio for the rest of this
# process; zero passes it. One probe, no network, no guess.
_SOT_SSH_SHARING=""
_sot_ssh_sharing_ok() {
    local control
    control="$(_sot_ssh_control)" || return 1
    [ -n "$control" ] || return 1
    if [ -z "$_SOT_SSH_SHARING" ]; then
        if ssh -G -o ControlMaster=auto -o ControlPath="$control" -o ControlPersist=600 \
               localhost >/dev/null 2>&1; then
            _SOT_SSH_SHARING=1
        else
            _SOT_SSH_SHARING=0
        fi
    fi
    [ "$_SOT_SSH_SHARING" = 1 ]
}

# sot_ssh_bridge TARGET [HOST] [TIMEOUT_SECS] — stdin → that daemon; its
# replies → stdout. The one child every `ssh:` scheme switch spawns
# (C10): `ssh <target> '<PATH prelude>; sotd stdio-bridge
# [--host <host>]'`, the option set and prelude literally the ones the
# hub's own relay unit runs (`rust/protocol/src/topology.rs`) and C3
# spawns identically from Rust (`rust/protocol/src/ssh_bridge.rs`) -- kept
# as this file's own implementation, not shared code, because shell
# cannot call into that crate. The connection-sharing trio is part of THIS
# helper, not an optional extra: without it "one authentication per host"
# (isolation-plan.md §10) is false as specified, since each send would be
# a full login on every platform rather than only on Windows -- applied
# through this one place so it is written once, not at each call site.
#
# THE BOUND LIVES HERE, not at the call site. `timeout N sot_ssh_bridge …`
# looked right and never ran: `timeout` is coreutils and `execvp`s its
# argument, so it never sees a shell function even after `export -f` --
# every one of the four call sites that tried it died at 127 with no
# output (reproduced: `timeout 1 f` on an exported function). A caller
# that wants a bound passes it as this THIRD POSITIONAL parameter, never
# an environment variable (`VAR=x func` scoping in bash is a quirk nobody
# should have to remember) -- this wraps its OWN `ssh` in `timeout` when
# the bound is non-empty, and runs unbounded, exactly as before, when it
# is empty.
sot_ssh_bridge() {
    local target="$1" host="${2:-}" secs="${3:-}"
    local remote='export PATH="$HOME/.local/share/sot/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; sotd stdio-bridge'
    [ -n "$host" ] && remote="$remote --host $host"
    local opts=(-T -o BatchMode=yes -o ServerAliveInterval=15 -o ServerAliveCountMax=3)
    if _sot_ssh_sharing_ok; then
        opts+=(-o ControlMaster=auto -o ControlPath="$(_sot_ssh_control)" -o ControlPersist=600)
    fi
    if [ -n "$secs" ]; then
        timeout "$secs" ssh "${opts[@]}" "$target" "$remote"
    else
        ssh "${opts[@]}" "$target" "$remote"
    fi
}

# sot_relay_endpoint [EXPLICIT] — the endpoint for comm RELAY traffic (send):
# where the HANDLES live. On a Windows box this
# is the box's own ssh child to the hub — never the local daemon's pipe,
# which has no route to a handle on another host and drops the frame
# without a word (2026-09-08: every cross-host send from a Windows session
# went dark the day discovery became pipe-first). Workspace ops, spawn and
# sot-fe keep sot_daemon_endpoint's pipe-first order: those really do
# target the local daemon. An explicit endpoint always wins, as everywhere
# else -- but a refusal here is a MISS, not a death: this resolver
# continues to its next source (main's ruling; contrast
# sot_daemon_endpoint's explicit arm below, which is fatal).
#
# Two lines, both through the one gate: an explicit value, else whatever
# `sotd topology relay-endpoint` answers for THIS box (`sotd` always has
# an answer once it exists -- its own endpoint on a box that never
# declared a topology, the plan's endpoint on one that did, its own error
# line and nothing else on a file that names a hub without this box).
# NEVER falls through to `sot_daemon_endpoint`: that would silently
# resolve THIS box's own daemon for a question about the hub's, which is
# the 2026-09-08 cross-host regression pinned at
# `comm/core/tests/test-join-disambiguation.sh:2023-2053` -- a failed
# resolution is no endpoint, never the local daemon, on either platform.
sot_relay_endpoint() {
    _sot_emit_endpoint "${1:-}" && return 0
    _sot_emit_endpoint "$(_sot_planned_relay_endpoint)" && return 0
    return 1
}

# sot_daemon_endpoint [EXPLICIT] — resolve the control socket endpoint used by
# comm relay/spawn/FE commands: the daemon on THIS box, or the one the
# caller named. Explicit endpoints keep their old behavior EXCEPT one
# change main ruled on 2026-09-29: a refused explicit value is FATAL here
# (`return`, not `exit` -- every one of the eight callers reads this
# inside a command substitution, where `exit` would end only the subshell
# and hand the caller an empty string with a status it might not test; all
# eight DO test it and exit 1 with their own message, read line by line,
# not assumed). A session holding a stale `SOT_FE_ENDPOINT`/
# `SOT_SPAWN_ENDPOINT=tcp:...` now fails loudly where it used to succeed
# by accident, reaching this box's own daemon nobody named -- the ruling:
# on the control plane a wrong box is worse than a stopped command.
sot_daemon_endpoint() {
    local explicit="${1:-}"
    if [ -n "$explicit" ]; then
        _sot_emit_endpoint "$explicit" && return 0
        printf 'comm-lib: refusing to substitute a local daemon for the endpoint you named: %s\n' "$explicit" >&2
        return 1
    fi
    if [ -n "${SOT_SOCKET:-}" ]; then
        _sot_emit_endpoint "unix:$SOT_SOCKET" && return 0
    fi

    # ADR 0042 amendment (2026-09-07): on a Windows box the LOCAL daemon
    # only ever listens on its named pipe -- discovery asks for it FIRST.
    # A probe miss (no local daemon running) is simply no endpoint (C10):
    # there is no tunnel left to fall back to, and guessing this box's own
    # pipe for what might be a remote question is exactly the 2026-09-08
    # regression `sot_relay_endpoint`'s own doc names.
    if _sot_is_windows; then
        local pipe_path
        if pipe_path="$(_sot_windows_local_pipe)"; then
            _sot_emit_endpoint "pipe:$pipe_path" && return 0
        fi
        return 1
    fi

    # Normal socket-only mode: the daemon may have only --label on argv, so
    # there is no transport flag to scrape. Query the same binary family the
    # installer/launcher uses. The default label is the product backend label;
    # override with SOT_BACKEND_LABEL for a non-default session.
    local label="${SOT_BACKEND_LABEL:-sot}"
    local bin sock
    _try_sotd_socket_bin() {
        local candidate="$1"
        [ -n "$candidate" ] || return 1
        [ -x "$candidate" ] || return 1
        sock="$("$candidate" session-socket-path "$label" 2>/dev/null || true)"
        [ -n "$sock" ] && [ -S "$sock" ] || return 1
        _sot_emit_endpoint "unix:$sock"
    }

    _try_sotd_socket_bin "${SOTD_BIN:-}" && return 0
    bin="$(command -v sotd 2>/dev/null || true)"
    _try_sotd_socket_bin "$bin" && return 0
    _try_sotd_socket_bin "$HOME/.local/share/sot/bin/sotd" && return 0
    _try_sotd_socket_bin "$HOME/.local/bin/sotd" && return 0

    if ! _sot_is_windows; then
        while IFS= read -r line; do
            local pid="${line%% *}"
            case "$pid" in ''|*[!0-9]*) continue ;; esac
            [ -r "/proc/$pid/exe" ] || continue
            bin="$(readlink "/proc/$pid/exe" 2>/dev/null || true)"
            _try_sotd_socket_bin "$bin" && return 0
        done < <(pgrep -af 'sotd' 2>/dev/null || true)
    fi

    # LAST resort — a development daemon launched with an explicit --socket.
    # Below the canonical session socket on purpose (2026-09-08): a lane's
    # test daemon (`--socket /tmp/sotrt-*/...`) scraped from argv hijacked
    # every comm script's discovery while the real daemon sat on its
    # label-derived socket, so despawn "found no workspace" and the row
    # survived. A scratch daemon is targeted explicitly (SOT_RELAY_ENDPOINT
    # / --endpoint), never by luck of process order. `--tcp` scraping is
    # GONE (dead since 0.4.0 -- `sotd` rejects `--tcp` outright,
    # `rust/backend/src/main.rs:446-450`). pgrep is not on a stock git-bash
    # PATH and must never be reached for on Windows.
    if ! _sot_is_windows; then
        local line
        while IFS= read -r line; do
            case "$line" in
                *comm-relay*|*comm-spawn*|*comm-despawn*|*comm-watch*|*comm-poll*|*sot-fe*|*sot-nav*)
                    continue
                    ;;
            esac
            if [[ "$line" =~ --socket[[:space:]]+([^[:space:]]+) ]]; then
                _sot_emit_endpoint "unix:${BASH_REMATCH[1]}" && return 0
            fi
        done < <(pgrep -af 'sotd' 2>/dev/null || true)
    fi

    return 1
}

ensure_home() {
    mkdir -p "$COMM_HOME" "$INBOX_DIR" "$SELF_DIR" "$READ_DIR"
    if [ ! -f "$REGISTRY" ]; then
        printf '{"protocol_version": %s, "agents": {}}\n' "$PROTOCOL_VERSION" > "$REGISTRY"
    fi
}

# with_lock CMD [ARGS...] — run CMD holding the registry lock. CMD may be a
# shell function defined in this sourced lib.
#
# The lock is the FILE $COMM_HOME/.registry.lock, one line naming its holder
# (B1b): `name:machine:boot:pidns:pid:start` — the holder's host name,
# /etc/machine-id, the kernel's boot_id, its pid namespace, its pid and that
# pid's start tick (field 22 of /proc/<pid>/stat); a field that cannot be read
# is `-`, which never equals anything. It is made by link(2) of a temp file
# that already holds the line, so it never exists without its holder, and a
# link never replaces anything. The daemon's `comm_registry_lock.rs` takes
# the same lock with the same record.
#
# Proof of death is Linux-only, and only from the holder's own machine: the
# same boot and pid namespace with the pid gone or its start changed, or the
# same machine-id AND host name with another boot. That assumes a machine-id
# is unique to one machine: a cloned image sharing one would read a live
# clone as rebooted. No timeout proves anything, so a live, frozen or
# unprovable holder is never forced (Codex review, PR #148 F2): the waiter
# FAILS CLOSED at the bound, naming the holder and the one-line recovery.
#
# The reclaim (`_sot_lock_step`) runs on the FIRST failed take, before any
# sleep, so a zero-try heartbeat and a ~1 s touch reach it too: prove the
# holder D dead, take the marker `.registry.lock.reclaim.<D>` (only its
# creator acts; markers are kept forever), settle 1 s for D's orphaned
# children and in-flight calls, re-read the lock fresh, and remove it only if
# it still names D. A removed lock is retaken at once, and the settle is not
# counted as a try. The bound is SOT_LOCK_MAX_TRIES polls of 50 ms: 10 s for
# every ruled write, about 1 s (20) for the best-effort `last_seen` touches of
# send, poll and spawn, and 0 for the heartbeat.
#
# Release is TRAP-based, not a plain post-command `rm` (Codex review F2
# second half / F7): a caller's `set -e` aborts the WHOLE SCRIPT the moment
# `"$@"` fails, at that exact statement — skipping every line after it in
# this function, including a plain `rm` written below the call. That
# leaked the lock forever on any callee failure (a corrupt registry.json
# making `registry_put`'s jq fail, for example). An EXIT trap still fires
# on that abort, so the lock comes off either way.
#
# The PRIOR EXIT trap (if any) is saved and restored, not just cleared:
# bash has one EXIT trap per shell, not a stack, and a caller may already
# have its own (e.g. comm-spawn.sh's provisional-row rollback) active
# around a with_lock call — blindly clearing it here would silently
# disarm the caller's cleanup for the rest of the script. This restore now
# runs on EVERY path, including a directly-failing "$@" (Codex review PR
# #148 round 2, finding 4): a bare `"$@"` statement under the caller's
# `set -e` used to abort the WHOLE SCRIPT right there, skipping every line
# below it in this function — the lock still came off (its own release
# trap fired on that abort), but the restore of the CALLER's prior trap
# never ran, silently losing it for the rest of the script. Capturing the
# callee's status via `if "$@"; then :; else rc=$?; fi` — the standard
# idiom for "run this and don't let -e kill us on failure" — means release
# and restore always execute before this function returns, on every path.
SOT_LOCK_MAX_TRIES=200   # ~10s at the 0.05s poll below
with_lock() {
    local tries=0 took
    # Test seam (F10): let a test PROVE a background waiter has reached its
    # first lock attempt, instead of racing it with a sleep. Touched once,
    # right before that attempt; unset (the default) this is a no-op.
    #
    # `touch --`, NOT `: > FILE` (Codex review round 2, finding 6): a bare
    # `>` redirect TRUNCATES whatever already sits at that path — if this
    # var ever leaked into a production environment pointed at a real
    # file, every with_lock call would zero it. `touch` only updates/
    # creates, and unlike `>` it doesn't attempt to OPEN-FOR-WRITE (which
    # would block forever against a FIFO with no reader, right here in the
    # lock's own hot path) — and `|| true` keeps a bad path from tripping
    # this function's own `set -e`-sensitive callers.
    [ -n "${SOT_COMM_TEST_LOCK_BARRIER:-}" ] && { touch -- "$SOT_COMM_TEST_LOCK_BARRIER" 2>/dev/null || true; }
    _sot_lock_self_id
    while :; do
        if _sot_lock_take "$_SOT_REG_LOCK"; then break; else took=$?; fi
        if [ "$took" = 2 ]; then
            echo "ERROR: registry lock $_SOT_REG_LOCK cannot be taken: $_SOT_LOCK_WHY" >&2
            return 1
        fi
        _sot_lock_step && continue
        tries=$((tries + 1))
        if [ "$tries" -gt "$SOT_LOCK_MAX_TRIES" ]; then
            _sot_lock_fail_text >&2
            return 1
        fi
        sleep 0.05
    done
    # Lock acquired — guarantee release via EXIT trap (see header comment),
    # preserving whatever EXIT trap the caller already had.
    local prev_trap rc=0
    prev_trap="$(trap -p EXIT)"
    trap 'rm -f "${_SOT_REG_LOCK:?}" 2>/dev/null || true' EXIT
    if "$@"; then
        :
    else
        rc=$?
    fi
    rm -f "${_SOT_REG_LOCK:?}" 2>/dev/null || true
    if [ -n "$prev_trap" ]; then
        eval "$prev_trap"
    else
        trap - EXIT
    fi
    return $rc
}

# _sot_lock_self_id — set _SOT_LOCK_ID to THIS process's holder record, and
# _SOT_LOCK_SELF to `name:machine:boot:pidns` where this shell can prove a
# death (Linux, with a /proc that is its own), else to "". Call it in the
# process that will hold the lock, never as `id=$(_sot_lock_self_id)`: a
# command substitution is a subshell, and its pid dies at once, so every
# waiter on this machine would reclaim a live holder (review S5).
_sot_lock_self_id() {
    local name machine=- boot=- pidns=- pid="${BASHPID:-$$}" start=- self_pid="" ns=""
    name="$(sot_host 2>/dev/null)" || name=""
    name="${name//[!A-Za-z0-9._-]/_}"
    name="${name:--}"
    _SOT_LOCK_SELF="" _SOT_LOCK_HOLDER="" _SOT_LOCK_WHO="" _SOT_LOCK_WHY="it was released just now"
    if [ "$(uname -s 2>/dev/null)" = Linux ]; then
        read -r self_pid _ 2>/dev/null </proc/self/stat || true
    fi
    if [ "$self_pid" = "$pid" ]; then
        IFS= read -r machine 2>/dev/null </etc/machine-id || true
        IFS= read -r boot 2>/dev/null </proc/sys/kernel/random/boot_id || true
        ns="$(readlink "/proc/$pid/ns/pid" 2>/dev/null)" || ns=""
        case "$ns" in 'pid:['*']') pidns="${ns#pid:[}"; pidns="${pidns%]}" ;; esac
        if _sot_lock_start "$pid"; then start="$_SOT_LOCK_START"; fi
        machine="${machine:--}"; boot="${boot:--}"; pidns="${pidns:--}"
        _SOT_LOCK_SELF="$name:$machine:$boot:$pidns"
    fi
    _SOT_LOCK_ID="$name:$machine:$boot:$pidns:$pid:$start"
}

# _sot_lock_start PID — set _SOT_LOCK_START to field 22 of /proc/PID/stat,
# read after the LAST `)` because the command name may hold spaces and
# parentheses (challenge_unix.rs's process_start_ticks, the same parse).
_sot_lock_start() {
    local line="" f=()
    { IFS= read -r line </proc/"$1"/stat; } 2>/dev/null || [ -n "$line" ] || return 1
    read -r -a f <<<"${line##*)}" || true
    [[ "${f[19]:-}" =~ ^[0-9]+$ ]] || return 1
    _SOT_LOCK_START="${f[19]}"
}

# _sot_lock_take TARGET — one attempt to create TARGET holding _SOT_LOCK_ID:
# 0 taken, 1 held (TARGET exists), 2 an error named in _SOT_LOCK_WHY. `link`,
# never `ln`: `ln` into an existing directory, an older peer's mkdir lock,
# makes TARGET/tmp and "succeeds", which gives two holders (review B1). File
# names map ':' to '.', because Windows reads a ':' in a name as a stream
# (review B2); the record itself keeps its colons.
_sot_lock_take() {
    local tmp="$_SOT_REG_LOCK.tmp.${_SOT_LOCK_ID//:/.}.0" err n
    if ! { printf '%s\n' "$_SOT_LOCK_ID" >|"$tmp"; } 2>/dev/null; then
        _SOT_LOCK_WHY="cannot write $tmp"
        return 2
    fi
    if err="$(LC_ALL=C link "$tmp" "$1" 2>&1)"; then
        rm -f "${tmp:?}"
        return 0
    fi
    # A retransmitted LINK on NFSv3 answers "exists" for this call's own
    # link, so my temp file's link count, read after an open, decides
    # (review S4) — never "the target names my ID", which Rust threads share.
    n="$({ : <"$tmp"; } 2>/dev/null; stat -c %h "$tmp" 2>/dev/null)" || n=""
    rm -f "${tmp:?}"
    [ "$n" = 2 ] && return 0
    # Held is the link's own EEXIST, never "the target exists now": a holder
    # can release between the two, which would read as an error.
    case "$err" in *"File exists"*) return 1 ;; esac
    _SOT_LOCK_WHY="${err:-link failed}"
    return 2
}

# _sot_lock_fresh PATH — read PATH's record into _SOT_LOCK_READ, fresh from
# the server. Opening the folder first forces its GETATTR (close-to-open): a
# changed folder drops every cached lookup beneath it, so the record's own
# open looks it up on the wire; a plain stat or readlink was seen to stay
# stale on the shared home. The folder open is required only where a death
# can be proved. 0 = a record of six fields with a numeric pid; else 1.
_sot_lock_fresh() {
    _SOT_LOCK_READ=""
    { : <"${1%/*}"; } 2>/dev/null || [ -z "${_SOT_LOCK_SELF:-}" ] || return 1
    { IFS= read -r _SOT_LOCK_READ <"$1"; } 2>/dev/null || true
    [[ "$_SOT_LOCK_READ" =~ ^[^:]*:[^:]*:[^:]*:[^:]*:[0-9]+:[^:]*$ ]]
}

# _sot_lock_judge ID — set _SOT_LOCK_VERDICT to DEAD, ALIVE or UNPROVABLE,
# and _SOT_LOCK_WHY to the reason a person reads.
_sot_lock_judge() {
    local name machine boot pidns pid start me_name="" me_machine="" me_boot="" me_pidns=""
    IFS=: read -r name machine boot pidns pid start <<<"$1" || true
    IFS=: read -r me_name me_machine me_boot me_pidns <<<"${_SOT_LOCK_SELF:-}" || true
    _SOT_LOCK_VERDICT=UNPROVABLE
    if [ -z "${_SOT_LOCK_SELF:-}" ]; then
        _SOT_LOCK_WHY="this box cannot prove a death"
    elif [ "$boot" != - ] && [ "$pidns" != - ] && [ "$boot" = "$me_boot" ] && [ "$pidns" = "$me_pidns" ]; then
        if _sot_lock_start "$pid"; then
            if [ "$start" != - ] && [ "$_SOT_LOCK_START" != "$start" ]; then
                _SOT_LOCK_VERDICT=DEAD; _SOT_LOCK_WHY="its pid now names another process"
            else
                _SOT_LOCK_VERDICT=ALIVE; _SOT_LOCK_WHY="it is running"
            fi
        elif [ ! -e "/proc/$pid" ]; then
            _SOT_LOCK_VERDICT=DEAD; _SOT_LOCK_WHY="it has exited"
        else
            _SOT_LOCK_WHY="its /proc entry cannot be read"
        fi
    elif [ "$machine" != - ] && [ "$boot" != - ] && [ "$me_boot" != - ] && [ "$name" != - ] \
        && [ "$machine" = "$me_machine" ] && [ "$name" = "$me_name" ] && [ "$boot" != "$me_boot" ]; then
        _SOT_LOCK_VERDICT=DEAD; _SOT_LOCK_WHY="its machine has rebooted since"
    elif [ "$machine" = - ] || [ "$machine" != "$me_machine" ] || [ "$name" != "$me_name" ]; then
        _SOT_LOCK_WHY="it is on another machine"
    else
        _SOT_LOCK_WHY="it is in another pid namespace"
    fi
}

# _sot_lock_vouch — 0 when the comm home's mount makes the fresh read fresh:
# a local filesystem, or NFS without `nocto`. Reached only on Linux, except
# by comm-registry-lock-clear.sh, whose person vouches elsewhere.
_sot_lock_vouch() {
    local fs="" opts=""
    [ -n "${_SOT_LOCK_SELF:-}" ] || return 0
    read -r fs opts <<<"$(findmnt -n -o FSTYPE,OPTIONS -T "${_SOT_REG_LOCK%/*}" 2>/dev/null)" || true
    case "$fs" in
        ext2|ext3|ext4|xfs|btrfs|zfs|f2fs|tmpfs|nfs|nfs4) ;;
        *) _SOT_LOCK_WHY="the comm home's filesystem (${fs:-unknown}) does not prove a fresh read"; return 1 ;;
    esac
    case ",$opts," in
        *,nocto,*) _SOT_LOCK_WHY="the comm home is mounted nocto, so no read of it is proved fresh"; return 1 ;;
    esac
}

# _sot_lock_step [--forced] — one reclaim attempt against the lock as it
# stands. 0 = this step saw the lock go, retake at once; 1 = not (a lock
# released since the take is a try, and keeps the last holder named), with
# _SOT_LOCK_HOLDER (the ID the lock names, "" for none), _SOT_LOCK_WHO (the
# ID that blocks) and _SOT_LOCK_WHY set for the FAILED line. The chain runs
# D, then the creator of reclaim.<D> if that one is dead too, and so on;
# every step past a marker needs its creator proved dead, so the live process
# holding the chain's last marker is the only one with authority over "the
# lock names a member of the chain". A marker naming me is one I took earlier
# in this wait. --forced (the clear command) takes a person's word for an
# unprovable holder, never against a proof that it is alive.
_sot_lock_step() {
    local x chain=() m c
    if ! _sot_lock_fresh "$_SOT_REG_LOCK"; then
        [ -e "$_SOT_REG_LOCK" ] || return 1
        _SOT_LOCK_HOLDER=""; _SOT_LOCK_WHO=""
        if [ -d "$_SOT_REG_LOCK" ]; then
            _SOT_LOCK_WHY="held by an older version that records no holder"
        else
            _SOT_LOCK_WHY="its record names no holder (${_SOT_LOCK_READ:-empty})"
        fi
        return 1
    fi
    _SOT_LOCK_HOLDER="$_SOT_LOCK_READ"; _SOT_LOCK_WHO=""; x="$_SOT_LOCK_READ"
    while :; do
        chain+=("$x")
        [ "${#chain[@]}" -gt 1 ] && [ "$x" = "$_SOT_LOCK_ID" ] && break
        _sot_lock_judge "$x"
        [ "${1:-}" = --forced ] && [ "$_SOT_LOCK_VERDICT" = UNPROVABLE ] && _SOT_LOCK_VERDICT=DEAD
        if [ "$_SOT_LOCK_VERDICT" != DEAD ]; then
            _SOT_LOCK_WHO="$x"
            [ "$x" = "$_SOT_LOCK_HOLDER" ] \
                || _SOT_LOCK_WHY="it is dead, but its reclaim by ${x%%:*} pid $(_sot_lock_field "$x" 5) did not finish: $_SOT_LOCK_WHY"
            return 1
        fi
        if [ "${#chain[@]}" = 1 ] && ! _sot_lock_vouch; then _SOT_LOCK_WHO="$x"; return 1; fi
        m="$_SOT_REG_LOCK.reclaim.${x//:/.}"
        if _sot_lock_take "$m"; then break; else c=$?; fi
        if [ "$c" = 2 ] || ! _sot_lock_fresh "$m"; then
            _SOT_LOCK_WHO="$x"; _SOT_LOCK_WHY="its reclaim marker $m cannot be read${_SOT_LOCK_WHY:+ ($_SOT_LOCK_WHY)}"
            return 1
        fi
        x="$_SOT_LOCK_READ"
    done
    sleep "${SOT_COMM_TEST_LOCK_SETTLE:-1}"
    if ! _sot_lock_fresh "$_SOT_REG_LOCK"; then
        [ -e "$_SOT_REG_LOCK" ] || return 0
        _SOT_LOCK_WHY="its record changed during the reclaim"
        return 1
    fi
    for x in "${chain[@]}"; do
        if [ "$_SOT_LOCK_READ" = "$x" ]; then
            rm -f "${_SOT_REG_LOCK:?}"
            return 0
        fi
    done
    _SOT_LOCK_HOLDER="$_SOT_LOCK_READ"; _SOT_LOCK_WHY="it was taken again during the reclaim"
    return 1
}

_sot_lock_field() {  # ID N — the ID's Nth colon field
    local f=()
    IFS=: read -r -a f <<<"$1" || true
    printf '%s' "${f[$2 - 1]:--}"
}

# _sot_lock_fail_text — the one FAILED line for the lock as _sot_lock_step
# last saw it: the holder's host, pid and start tick, and the recovery.
_sot_lock_fail_text() {
    local age="unknown" mtime="" who="${_SOT_LOCK_WHO:-${_SOT_LOCK_HOLDER:-}}"
    mtime="$(stat -c %Y "$_SOT_REG_LOCK" 2>/dev/null)" || mtime=""
    [ -n "$mtime" ] && age="$(( $(date +%s) - mtime ))s"
    if [ -z "${_SOT_LOCK_HOLDER:-}" ]; then
        echo "ERROR: registry lock $_SOT_REG_LOCK still held ($age old): $_SOT_LOCK_WHY. If its holder is dead, remove $_SOT_REG_LOCK by hand and retry."
        return 0
    fi
    echo "ERROR: registry lock $_SOT_REG_LOCK is held by ${_SOT_LOCK_HOLDER%%:*} pid $(_sot_lock_field "$_SOT_LOCK_HOLDER" 5) start $(_sot_lock_field "$_SOT_LOCK_HOLDER" 6) ($age old): $_SOT_LOCK_WHY. If it is dead, run any comm command on ${who%%:*}, or run comm-registry-lock-clear.sh."
}

# --- registry mutators (call inside with_lock) ---
registry_put() {  # name objJSON
    # F7 (Codex review): never write an empty/blank handle — a derivation
    # bug or a corrupt-registry jq failure upstream is an ERROR, not a
    # claim of "". Last line of defense regardless of how a caller got here.
    if [ -z "$1" ]; then
        echo "registry_put: refusing to write an empty/blank handle" >&2
        return 1
    fi
    jq --arg n "$1" --argjson o "$2" '.agents[$n] = $o' "$REGISTRY" \
        > "$REGISTRY.tmp" && mv "$REGISTRY.tmp" "$REGISTRY"
}
registry_del() {  # name
    jq --arg n "$1" 'del(.agents[$n])' "$REGISTRY" \
        > "$REGISTRY.tmp" && mv "$REGISTRY.tmp" "$REGISTRY"
}
registry_touch() {  # name — bump last_seen if present
    local ts; ts="$(now_iso)"
    jq --arg n "$1" --arg t "$ts" \
        'if .agents[$n] then .agents[$n].last_seen = $t else . end' \
        "$REGISTRY" > "$REGISTRY.tmp" && mv "$REGISTRY.tmp" "$REGISTRY"
}

# --- the inbox append (0031 B1) ---
# sot_inbox_append HANDLE — THE one place a script appends a frame to an
# inbox. One JSON line on stdin. 0 = filed; 1 = nothing appended, with the
# reason on stdout for the caller to print after `FAILED -> @h: `.
#
# The lock is the kernel's file lock (flock) on the sidecar
# `inbox/<handle>.lock`, which the daemon's `comm.file` filer takes too. The
# OS releases it when its holder dies, so there is no reclaim; a frozen holder
# only makes the next sender wait, and a sender that cannot take the lock
# within SOT_INBOX_LOCK_WAIT_SECS appends NOTHING and fails — a `filed` for an
# append that did not happen is a false success. The inbox is opened inside
# the lock and closed before it is released: correctness is the lock plus
# close-to-open consistency, never O_APPEND's offset across boxes.
#
# A lock excludes only writers that go through ONE lock manager: an NFSv3 and
# an NFSv4 lock on one export exclude nothing, and a local flock on a disk
# other hosts mount does not exclude their NFS locks. So a script appends
# locally only when flock(1) and perl exist, this is Linux, and the identity it
# computes for $INBOX_DIR is byte-equal to line 1 of
# `$COMM_HOME/inbox-lock-manager`, which only the folder's hub writes, at
# startup, by an exclusive create (line 2 is the writing machine's id). On
# NFSv3 or an unknown mount the identity is `none@<machine-id>`: the folder is
# bound to the one machine that wrote the record, whose processes share its
# one kernel lock, and every other machine computes a different string.
# Anything else — no record, a bare `none` record, another machine's
# `none@…` (NFSv3, an unknown mount, an NFS v4 mount whose `local_lock` is not
# `none`), another host mounting the daemon's local disk — hands the frame to
# the daemon that owns this comm folder as `comm.file`, and a daemon that does
# not answer is FAILED. A daemon makes the same check at each filing: a guest
# on the hub's folder forwards what it cannot prove to the hub, and the hub
# refuses it with the recovery named.
#
# An unterminated tail a dead writer left is CUT back to the last newline under
# the lock (a note on stderr says how many bytes), never ended. Readers count
# newline-terminated lines only, so a cut never moves a cursor, and two guards
# cover a line a failed append then cuts back: where a writer would append
# locally the reader's count-and-read takes a shared lock bounded by
# SOT_INBOX_READ_WAIT_SECS (a lock held past it means try again, exit 75; any
# other lock fault, a lock file that will not open or any flock error, is named
# and the read runs unlocked) — comm-poll and comm-wake let go before they
# show anything, while comm-watch holds the lock as it prints (B3 replaces
# it) — and on every host the cursor keeps
# `<count> <crc>-<len>` of the last line the reader READ (hashed from the bytes
# it holds, never re-read from the file), so a reader steps back one line when
# a cut-back removed it.
SOT_INBOX_LOCK_WAIT_SECS="${SOT_INBOX_LOCK_WAIT_SECS:-10}"
_sot_have_flock() { command -v flock >/dev/null 2>&1; }
_sot_findmnt() { findmnt "$@"; }
_sot_machine_id() { local m=""; { read -r m < /etc/machine-id; } 2>/dev/null; printf '%s' "$m"; }
# sot_inbox_lock_identity DIR — the lock manager an append to DIR goes
# through, by the rule comm_inbox.rs's record uses: `nfs4 <source>`,
# `local <machine-id>` on a local block filesystem, else `none@<machine-id>`,
# this machine's lock alone (bare `none` with no machine id, which never
# matches) — so an NFS v4 mount without `local_lock=none` (its lock stays on
# the client) is local only on the machine that wrote the record. The last
# line of `findmnt -T` is the mount on top when one is stacked over another.
sot_inbox_lock_identity() {  # DIR
    local fs="" opts="" src="" mid
    read -r fs opts src < <(_sot_findmnt -n -o FSTYPE,FS-OPTIONS,SOURCE -T "$1" 2>/dev/null | tail -n 1)
    case "$fs" in
        nfs4) case ",$opts," in
                  *,vers=4.*,*) case ",$opts," in
                      *,local_lock=none,*) [ -z "$src" ] || { printf 'nfs4 %s\n' "$src"; return 0; } ;;
                  esac ;;
              esac ;;
        ext2|ext3|ext4|xfs|btrfs|zfs|f2fs)
            mid="$(_sot_machine_id)"
            [ -z "$mid" ] || { printf 'local %s\n' "$mid"; return 0; } ;;
    esac
    mid="$(_sot_machine_id)"
    [ -z "$mid" ] || { printf 'none@%s\n' "$mid"; return 0; }
    printf 'none\n'
}
# _sot_inbox_lock_ours DIR — prints this script's sot_inbox_lock_identity for
# DIR when it is the lock manager every other writer of the inbox takes (the
# record's line 1), and nothing otherwise: no flock(1), no perl, not Linux, or
# a missing, empty or bare `none` record. The cheap tests come first, so a box
# that can never take the lock runs no findmnt.
_sot_inbox_lock_ours() {  # DIR
    local rec="" id
    _sot_have_flock && command -v perl >/dev/null 2>&1 && [ "$(uname -s 2>/dev/null)" = Linux ] || return 0
    { IFS= read -r rec < "$COMM_HOME/inbox-lock-manager"; } 2>/dev/null
    [ -n "$rec" ] && [ "$rec" != none ] || return 0
    id="$(sot_inbox_lock_identity "$1")"
    [ "$id" != "$rec" ] || printf '%s\n' "$id"
}
# _sot_flock_wait MODE SECS ID — the lock on fd 9 (MODE -x or -s) within SECS,
# chosen by ID, the identity _sot_inbox_lock_ours printed.
# The Linux NFSv4 client retries a blocked lock with a backoff that doubles
# from 100 ms, so a local writer re-takes the lock before a remote waiter's
# next retry and a blocking waiter can sleep past a free lock: under `nfs4 ` a
# non-blocking try is repeated every 15-25 ms until the bound. NLM (v3) and one machine's own
# kernel lock (`local …`, `none@…`) wake a blocked waiter on release, so those
# block, bounded. 75 = the bound passed with the lock held elsewhere; any
# other non-zero is flock's own error, never a held lock, and flock names it on
# stderr (`flock: 9: <strerror>`).
_sot_flock_wait() {  # MODE SECS ID
    local rc end
    case "$3" in
        "nfs4 "*)
            end=$(( $(date +%s%N) + $2 * 1000000000 ))
            while :; do
                rc=0
                flock -n -E 75 "$1" 9 || rc=$?
                [ "$rc" -eq 75 ] || return "$rc"
                [ "$(date +%s%N)" -lt "$end" ] || return 75
                sleep "0.0$((15 + RANDOM % 11))"
            done ;;
        *) flock "$1" -w "$2" -E 75 9 ;;
    esac
}
# _sot_append_whole FILE LINE — under the caller's lock, LINE goes in whole or
# not at all, in ONE perl process on ONE descriptor: the length before is a
# seek to its end (a path stat can be answered from the attribute cache and
# cut away a line another host filed).
# An unterminated tail (a writer that died mid-line, or NULs after a client
# crash) is CUT back to the last newline first, in blocks on that descriptor,
# and the cut is noted on stderr: everything past the last newline was
# written by a writer that never answered `filed`, so nothing kept is lost.
# 0 means written, fsynced and closed, and any failure cuts FILE back to the
# length after that cut, on the same descriptor. LINE goes on stdin, never
# argv.
_sot_append_whole() {  # FILE LINE
    printf '%s\n' "$2" | perl -e '
        use strict; use Fcntl qw(O_RDWR O_APPEND O_CREAT SEEK_SET SEEK_END); use IO::Handle;
        my $f = shift; my $buf = do { local $/; <STDIN> }; my ($fh, $len);
        sub fail { my $e = "$!"; truncate($fh, $len) if defined $len; print STDERR "$f: $e\n"; exit 1 }
        sub readat { my ($at, $n) = @_; my $got = "";
            defined sysseek($fh, $at, SEEK_SET) or fail();
            while (length($got) < $n) { my $r = sysread($fh, $got, $n - length($got), length($got)) // fail(); $r > 0 or fail() }
            $got }
        sysopen($fh, $f, O_RDWR | O_APPEND | O_CREAT, 0666) or fail();
        defined($len = sysseek($fh, 0, SEEK_END)) or fail();
        $len += 0;   # sysseek says "0 but true" for 0
        if ($len > 0 && readat($len - 1, 1) ne "\n") {
            my ($end, $cut) = ($len, 0);
            while ($end > 0) {
                my $start = $end > 4096 ? $end - 4096 : 0;
                my $i = rindex(readat($start, $end - $start), "\n");
                if ($i >= 0) { $cut = $start + $i + 1; last }
                $end = $start;
            }
            truncate($fh, $cut) or fail();
            my ($h) = $f =~ m{([^/]*)\.jsonl$};
            print STDERR "note: cut " . ($len - $cut) . " bytes of an unterminated line a dead writer left in \@$h\x27s inbox\n";
            $len = $cut;
        }
        for (my $off = 0; $off < length $buf;) { $off += syswrite($fh, $buf, length($buf) - $off, $off) // fail() }
        $fh->sync or fail();
        close($fh) or fail();
    ' "$1"
}
# A reader that polls after a writer's `write` but before its `fsync` fails
# would count a line the writer then cuts back, and its line-count cursor would
# sit one past the end and skip the next message. Where a writer would append
# locally (the test above) a reader therefore counts and reads under a SHARED
# lock on the writers' `inbox/<h>.lock`, waiting at most
# SOT_INBOX_READ_WAIT_SECS. The lock descriptor is opened read-write everywhere
# (`9<>`): the Linux NFS client refuses a shared lock on one without read
# access. sot_inbox_read_lock takes it on fd 9 of the calling
# shell (never a subshell: the reader's counters must survive) and returns 0
# when held or when no lock applies, 75 when the bound passed with the lock
# held elsewhere — which means try again, never a skip. Any other lock fault
# is not "try again": a lock file that will not open, or any other flock error,
# returns 0 unheld, with SOT_INBOX_READ_WARNING naming it (flock's code and its
# own stderr text) for the caller to show where its session sees it. flock runs
# inside $(…) to catch that text: the lock is on the open file description,
# which the calling shell's fd 9 still holds. An unheld reader, like every
# reader elsewhere, is covered by the cursor's line hash (sot_cursor_write): a
# line may show twice, none is lost. comm-poll reads its batch under the lock,
# lets go, then shows it: a slow display never holds off a writer.
SOT_INBOX_READ_WAIT_SECS="${SOT_INBOX_READ_WAIT_SECS:-3}"
sot_inbox_read_lock() {  # HANDLE
    local rc=0 id err
    SOT_INBOX_READ_WARNING=""
    id="$(_sot_inbox_lock_ours "$COMM_HOME/inbox")"
    [ -n "$id" ] || return 0
    if ! { exec 9<> "$COMM_HOME/inbox/$1.lock"; } 2>/dev/null; then
        SOT_INBOX_READ_WARNING="WARNING: the inbox lock for @$1 failed (cannot open its lock file) — reading without it; a line may show twice, none is lost"
        return 0
    fi
    err="$(_sot_flock_wait -s "$SOT_INBOX_READ_WAIT_SECS" "$id" 2>&1)" || rc=$?
    [ "$rc" -ne 0 ] || return 0
    exec 9>&-
    [ "$rc" -ne 75 ] || return 75
    err="${err##*$'\n'}"
    [ -n "$err" ] && err="$rc: ${err##*: }" || err="code $rc"
    SOT_INBOX_READ_WARNING="WARNING: the inbox lock for @$1 failed ($err) — reading without it; a line may show twice, none is lost"
}
sot_inbox_read_unlock() { exec 9>&-; }
# sot_inbox_read_warning_log HANDLE — for a reader that loops (comm-wake,
# comm-watch; B3 replaces both): after sot_inbox_read_lock, the warning goes to
# stderr only when it changes, and one line says when it clears, so a lasting
# fault is one log line rather than one per tick.
sot_inbox_read_warning_log() {  # HANDLE
    [ "$SOT_INBOX_READ_WARNING" != "${_SOT_INBOX_WARNING_LOGGED:-}" ] || return 0
    if [ -n "$SOT_INBOX_READ_WARNING" ]; then
        printf '%s\n' "$SOT_INBOX_READ_WARNING" >&2
    else
        printf 'the inbox lock fault for @%s has cleared\n' "$1" >&2
    fi
    _SOT_INBOX_WARNING_LOGGED="$SOT_INBOX_READ_WARNING"
}
sot_inbox_append() {  # HANDLE
    local h="$1" line err rc=0 id
    line="$(cat)"
    id="$(_sot_inbox_lock_ours "$INBOX_DIR")"
    if [ -z "$id" ]; then
        _sot_inbox_append_via_daemon "$h" "$line"
        return
    fi
    # 75 is flock's own conflict exit (-E), so a lock that was never taken is
    # told apart from an append that failed under it.
    err="$( { ( _sot_flock_wait -x "$SOT_INBOX_LOCK_WAIT_SECS" "$id" || exit $?
                _sot_append_whole "$INBOX_DIR/$h.jsonl" "$line"
              ) 9<> "$INBOX_DIR/$h.lock"; } 2>&1 )" || rc=$?
    case "$rc" in
        0)  [ -z "$err" ] || printf '%s\n' "$err" >&2   # the cut's note
            return 0 ;;
        75) printf 'the inbox lock for @%s was held for %ss — nothing was appended\n' \
                "$h" "$SOT_INBOX_LOCK_WAIT_SECS" ;;
        *)  printf 'the append failed: %s\n' "${err##*$'\n'}" ;;   # the last line: a cut's note may precede it
    esac
    return 1
}
# The daemon that owns this comm folder: this box's own when there is one,
# else the relay endpoint (the hub, for a shared-home box that runs no
# daemon). One route, chosen once: an endpoint that does not answer is FAILED.
_sot_inbox_append_via_daemon() {  # HANDLE LINE
    local ENDPOINT
    ENDPOINT="$(sot_daemon_endpoint 2>/dev/null)" || ENDPOINT=""
    [ -n "$ENDPOINT" ] || { ENDPOINT="$(sot_relay_endpoint 2>/dev/null)" || ENDPOINT=""; }
    if [ -z "$ENDPOINT" ]; then
        printf 'this box cannot take the inbox lock itself, and no daemon is reachable to file it\n'
        return 1
    fi
    sot_comm_file "$1" "$2" || return 1
}

# sot_comm_file HANDLE LINE — THE one `comm.file` request and its verdict, for
# the guard's daemon route above and comm-relay.sh's send_frame alike. LINE is
# the inbox line (`from`, `to`, `msg`); ENDPOINT comes from the caller's scope.
# 0 = filed; otherwise the reason is on stdout for the caller to print after
# `FAILED -> @h: `, and the status is 2 when the daemon does not list HANDLE
# (`not_here`), 1 for everything else. The response line decides, in this
# order: an `error` is FAILED in the daemon's own words (keyed on its
# presence, never on `code` — an older daemon refuses the unknown op with no
# code); `ok` is filed whatever the transport's exit status or stderr say; no
# response is FAILED, and only then does the transport's stderr give the why.
sot_comm_file() {  # HANDLE LINE
    local h="$1" frame resp reason code err diag window
    # The daemon may wait the whole inbox-lock bound before it files, so a read
    # window no longer than that reports FAILED for a line that WAS filed, and
    # the sender resends it: the lock wait plus 10s for the transport's setup.
    window=$(( SOT_INBOX_LOCK_WAIT_SECS + 10 ))
    [ "${SOT_SEND_TIMEOUT:-0}" -gt "$window" ] 2>/dev/null && window="$SOT_SEND_TIMEOUT"
    # The text reaches jq on stdin, never argv (the MSYS2 guard, sot_jq_rawfile).
    # A broadcast copy (the line's own `to` empty) must stay one after filing.
    frame="$(printf '%s' "$2" | jq -c --arg t "$h" \
        '{v:1,id:1,kind:"req",op:"comm.file",payload:{from:.from,to:$t,text:.msg,broadcast:(.to == "")}}')" || {
        printf 'the frame could not be built\n'; return 1; }
    err="$(mktemp "${XDG_RUNTIME_DIR:-/tmp}/sot-comm-file-XXXXXX")" || err=""
    resp="$(SOT_SEND_TIMEOUT="$window" sot_oneshot_request "$frame" comm.file 2>"${err:-/dev/null}")" || resp=""
    diag=""
    if [ -n "$err" ]; then diag="$(tr '\n' ' ' < "$err")"; rm -f "${err:?}"; fi
    reason="$(printf '%s' "$resp" | sot_jq -r '.payload.error // empty' 2>/dev/null)" || reason=""
    if [ -n "$reason" ]; then
        printf '%s\n' "$reason"
        code="$(printf '%s' "$resp" | sot_jq -r '.payload.code // empty' 2>/dev/null)" || code=""
        [ "$code" = not_here ] && return 2
        return 1
    fi
    # `-n` first: `jq -e` over empty input exits 0.
    if [ -n "$resp" ] && printf '%s' "$resp" | jq -e '.payload.ok == true' >/dev/null 2>&1; then
        return 0
    fi
    diag="${diag% }"
    printf 'the daemon did not answer at %s%s\n' "$ENDPOINT" "${diag:+: $diag}"
    return 1
}

# sot_fmt_age SECONDS -> "12s"/"6m"/"32m"/"8h"/"3d" (no "ago" suffix -- every
# caller supplies its own wording, since the same figure reads differently in
# a success line vs a timeout message). A negative age (clock skew) floors to
# 0 rather than printing garbage.
sot_fmt_age() {
    local s="$1"
    [ "$s" -lt 0 ] 2>/dev/null && s=0
    if   [ "$s" -lt 60 ];    then echo "${s}s"
    elif [ "$s" -lt 3600 ];  then echo "$((s / 60))m"
    elif [ "$s" -lt 86400 ]; then echo "$((s / 3600))h"
    else                          echo "$((s / 86400))d"
    fi
}

# sot_recipient_note HANDLE — one factual clause about a registry entry's
# recipient, for a sender's ack line (messaging ruling, 2026-09-26: "a sender
# cannot tell working from waiting-on-its-human from gone"). Reads ONLY the
# entry the sender already has open (.agents[$1] in $REGISTRY) -- no second
# file, no daemon call. Prints nothing and returns 1 the instant a needed
# fact is absent or unparseable: an annotation is a courtesy, never a guess.
#
# A STALE heartbeat (last_seen older than SOT_COMM_STALE_SECS, default 600 --
# the same threshold comm-list.sh already uses) overrides every other clause,
# because a stamp from a dead session is the misleading one.
#
# Absent that, `state` decides (comm-status.sh's own reduction, ADR 0044
# amendment): `working` gets the turn-boundary wording; the state that
# actually means "stopped, waiting on its own human to answer" is `blocked`
# (a `question` is set and `floor` is absent) -- NOT `floor == "user"`, which
# instead co-occurs with `working` (floor present => state "working"; the
# value only records who started the live turn -- comm-status.sh:97-112, the
# ADR 0044 amendment table). Every other state prints its own word
# unembellished (idle, done, waiting, or a future state this helper has never
# heard of) -- silence beats a confident wrong summary, but a real word beats
# no word.
sot_recipient_note() {
    local h="$1" row state summary status_at last_seen now
    row="$(jq -c --arg n "$h" '.agents[$n] // empty' "$REGISTRY" 2>/dev/null)" || row=""
    [ -n "$row" ] && [ "$row" != "null" ] || return 1
    state="$(printf '%s' "$row" | jq -r 'if (.state|type)=="string" then .state else empty end' 2>/dev/null)" || state=""
    [ -n "$state" ] || return 1
    summary="$(printf '%s' "$row" | jq -r 'if (.summary|type)=="string" then .summary else empty end' 2>/dev/null)" || summary=""
    status_at="$(printf '%s' "$row" | jq -r 'if (.status_at|type)=="string" then .status_at else empty end' 2>/dev/null)" || status_at=""
    last_seen="$(printf '%s' "$row" | jq -r 'if (.last_seen|type)=="string" then .last_seen else empty end' 2>/dev/null)" || last_seen=""
    now="$(date -u +%s)"

    if [ -n "$last_seen" ]; then
        local seens hb_age
        seens="$(date -u -d "$last_seen" +%s 2>/dev/null)" || seens=""
        if [ -n "$seens" ] && [ "$seens" -gt 0 ] 2>/dev/null; then
            hb_age=$((now - seens))
            if [ "$hb_age" -ge "${SOT_COMM_STALE_SECS:-600}" ]; then
                echo "no heartbeat for $(sot_fmt_age "$hb_age") -- may be gone"
                return 0
            fi
        fi
    fi

    [ -n "$status_at" ] || return 1
    local sats sat_age
    sats="$(date -u -d "$status_at" +%s 2>/dev/null)" || sats=""
    [ -n "$sats" ] && [ "$sats" -gt 0 ] 2>/dev/null || return 1
    sat_age="$(sot_fmt_age $((now - sats)))"

    case "$state" in
        working)
            echo "working, stamped ${sat_age} ago -- reply expected at its turn boundary" ;;
        blocked)
            local q; q="$(printf '%s' "$summary" | jq -Rr '.[0:60]' 2>/dev/null)" || q=""
            echo "needs its own user, stamped ${sat_age} ago: \"${q}\"" ;;
        *)
            echo "${state}, stamped ${sat_age} ago" ;;
    esac
}

# sot_handle_live HANDLE — rc 0 when the registry's .agents[HANDLE].last_seen is
# fresh (within SOT_COMM_STALE_SECS, default 600 — the threshold
# sot_recipient_note and comm-list.sh use), else rc 1. An absent row, an absent
# or unparseable last_seen all mean not live: it never fabricates liveness. Asks
# the heartbeat only (no daemon round trip), so comm-join.sh's stranding warning
# works on a box with no daemon reachable.
sot_handle_live() {
    local last_seen seens
    last_seen="$(jq -r --arg n "$1" '.agents[$n].last_seen // empty | if type=="string" then . else empty end' "$REGISTRY" 2>/dev/null)" || return 1
    [ -n "$last_seen" ] || return 1
    seens="$(date -u -d "$last_seen" +%s 2>/dev/null)" || return 1
    [ "$seens" -gt 0 ] 2>/dev/null || return 1
    [ $(( $(date -u +%s) - seens )) -lt "${SOT_COMM_STALE_SECS:-600}" ]
}

# registry_del_if_provisional NAME WANT_ROOT WANT_NONCE — conditionally
# delete NAME's row, but ONLY if it's STILL provably the exact provisional
# row identified by WANT_ROOT + WANT_NONCE (status "spawning" is implied —
# a provisional row is always spawning; a real join or an explicit
# claimant always overwrites both root and status/removes the nonce as it
# writes a normal row). Call under with_lock. Exists so a spawn's rollback
# can never delete a NEWER row that has since replaced the provisional one
# (Codex review PR #148 round 2, finding 1 — reproduced by the reviewer:
# an unconditional `registry_del "$NAME"` deleted a live `status:"idle"`
# row the child had already written for real, turning a successful join
# into `null`). Returns:
#   0 — deleted (it was still ours)
#   1 — deletion itself failed (registry_del's jq/mv step)
#   2 — NOT deleted: the row no longer matches what was claimed (or
#       WANT_NONCE/NAME is empty) — left untouched; this is the common,
#       expected outcome once a real join has happened, not an error
registry_del_if_provisional() {
    local name="$1" want_root="$2" want_nonce="$3"
    local cur_status cur_root cur_nonce
    [ -n "$name" ] && [ -n "$want_nonce" ] || return 2
    cur_status="$(sot_jq -r --arg n "$name" '.agents[$n].status // ""' "$REGISTRY" 2>/dev/null)"
    cur_root="$(sot_jq -r --arg n "$name" '.agents[$n].root // ""' "$REGISTRY" 2>/dev/null)"
    cur_nonce="$(sot_jq -r --arg n "$name" '.agents[$n].nonce // ""' "$REGISTRY" 2>/dev/null)"
    if [ "$cur_status" != "spawning" ] || [ "$cur_root" != "$want_root" ] || [ "$cur_nonce" != "$want_nonce" ]; then
        return 2
    fi
    registry_del "$name"
}

# --- self-file writer (shared by comm-join.sh and comm-context.sh's
# read-side self-heal) ---

# sot_write_self_file SELF_FILE NAME REPO ROOT — write the v2 self-file
# format (identity line, then `repo=`, then `root=`) to SELF_FILE via a
# same-directory temp file + checked `mv`, never an in-place `>`
# truncation (Codex review round-1 finding 3: the old in-place write left a
# read-only self-file silently unwritten — the redirection failed, nothing
# checked its exit status, and the caller went on to print a success
# message anyway — and could leave a torn/zero-byte file if interrupted
# mid-write). The temp file lives NEXT TO SELF_FILE so the final `mv` is a
# same-filesystem rename: atomic, no partial-write window a concurrent
# reader could observe.
#
# Both write sites — this self-heal path in comm-context.sh and the
# ordinary join-write in comm-join.sh — route through this ONE function so
# there is a single place that gets the atomicity right, rather than two
# copies that could drift.
#
# No cross-process lock: the self-file's "nopane" slot is deliberately
# SHARED across every no-workspace shell on a host (see
# comm-context.sh's nopane note) and is last-writer-wins BY DESIGN — every
# read of it is independently re-validated (against root=, or against
# repo=/the registry for a legacy file), so a slot two shells raced to
# write is caught on its next read rather than silently trusted either
# way. Serializing the write here would only slow down an already-safe
# race, not close a real hazard.
#
# Returns 0 only once SELF_FILE has been VERIFIABLY replaced with the new
# content; nonzero (with a reason on stderr) otherwise, and the original
# SELF_FILE is left untouched (the failed temp file is cleaned up, never
# left as e.g. a stray `.tmp.*` sibling). Callers MUST treat a nonzero
# return as "did not persist" — never report success on it. A refusal by the
# agreement guard below is return 3 specifically, so a caller can tell "this
# slot belongs to another project" from "the disk said no".
#
# sot_self_file_project_conflict SELF_FILE REPO ROOT — 0 (and one line on
# stderr naming the incumbent) when SELF_FILE already holds an identity
# claimed for a DIFFERENT project than (REPO, ROOT), 1 otherwise. This is the
# WRITE side of the read-side staleness matrix in comm-context.sh: the slot
# path is keyed by the workspace row in the ENVIRONMENT while the identity
# written into it is derived from the shell's CWD, the two derivations are
# never compared, and so any process whose $SOT_WORKSPACE_ID names one row
# while its cwd sits in another row's repo used to write that repo's handle
# into the other row's slot — after which the other session reads a handle
# that is not its own and directed mail is filed for the wrong reader (the
# inbox is keyed by handle, so the read-side matrix catches it only AFTER the
# misdelivery). `root=` decides when the slot has one; `repo=` decides for a
# legacy slot; a slot with neither (the ancient one-line format) is no
# evidence and never a conflict. The SHARED `$HOST__nopane.txt` slot is
# exempt: it is last-writer-wins BY DESIGN (see the note above), every
# no-pane shell on the host writes it whatever project it sits in, and a
# guard there would refuse ordinary use.
sot_self_file_project_conflict() {
    local self_file="$1" repo="$2" root="$3"
    case "$self_file" in *__nopane.txt) return 1 ;; esac
    [ -f "$self_file" ] || return 1
    local -a lines; mapfile -t lines < "$self_file" 2>/dev/null || return 1
    local name="${lines[0]:-}" repo_line="${lines[1]:-}" root_line="${lines[2]:-}" claim=""
    [ -n "$name" ] || return 1
    case "$root_line" in root=?*) claim="root='${root_line#root=}'"
        [ "${root_line#root=}" = "$root" ] && return 1 ;;
    esac
    if [ -z "$claim" ]; then
        case "$repo_line" in repo=?*) claim="repo='${repo_line#repo=}' (legacy slot, no root=)"
            [ "${repo_line#repo=}" = "$repo" ] && return 1 ;;
        esac
    fi
    [ -n "$claim" ] || return 1
    echo "the identity slot '$self_file' already names @$name, claimed for $claim" >&2
    return 0
}
sot_write_self_file() {
    local self_file="$1" name="$2" repo="$3" root="$4" repin="${5:-0}" tmp
    if [ "$repin" != 1 ] && sot_self_file_project_conflict "$self_file" "$repo" "$root"; then
        echo "sot_write_self_file: REFUSING to write '$name' (repo='$repo', root='$root') over it — a slot keyed by one row must not come to name another project's session, or that row reads mail addressed to this one. If this row really is '$repo' now, re-run the join with --repin." >&2
        return 3
    fi
    tmp="$(mktemp "${self_file}.tmp.XXXXXX" 2>/dev/null)" || {
        echo "sot_write_self_file: could not create a temp file next to '$self_file' (directory missing or not writable?)" >&2
        return 1
    }
    if ! printf '%s\nrepo=%s\nroot=%s\n' "$name" "$repo" "$root" > "$tmp" 2>/dev/null; then
        echo "sot_write_self_file: write to temp file '$tmp' failed (disk full? permissions?)" >&2
        rm -f "${tmp:?}" 2>/dev/null
        return 1
    fi
    if ! mv -f "$tmp" "$self_file" 2>/dev/null; then
        echo "sot_write_self_file: could not move '$tmp' into place at '$self_file'" >&2
        rm -f "${tmp:?}" 2>/dev/null
        return 1
    fi
    return 0
}

# --- owned lifetimes: who a watcher belongs to -------------------
#
# sot_owner_pid — the pid of the nearest ancestor whose command is `claude` or
# `codex`, walking up from $PPID. Lives HERE, not in one caller, because every
# process that outlives a turn has to end with the agent it serves, and a flag
# a caller can forget leaves exactly one leg ownerless (the Codex watch leg was
# that leg). A caller still directly attached to the agent may pass the pid it
# already knows; anything spawned without one discovers it the same way here.
# `ps -o comm=` is tried at each hop, then /proc; if neither answers the walk
# stops and prints nothing (rc 1) — which is a REFUSAL at the call site, never
# an untethered process.
#
# Three ways to read a name, tried in turn: `ps -o comm=`, then
# `<pid>/comm`, then `Name:` from `<pid>/status` for a procfs that has the
# second but not the first. A leading path and a trailing `.exe` are stripped
# before the match, so a name that matched before still matches.
#
# None of that reaches an agent on git-bash, and the tier below is why: msys
# procfs does not cross the Windows process boundary, so this walk stops at
# the first hop there whatever it reads the name from.
sot_owner_pid() {
    _sot_owner_pid_proc && return 0
    # The /proc walk is the whole answer everywhere but git-bash, where it
    # stops at the first hop: msys procfs does not cross the Windows process
    # boundary, so a real chain of bash.exe -> bash.exe -> claude.exe reads as
    # one line, "Name: bash", and the ancestor that IS there is invisible
    # (measured on a Windows box, 2026-09-28). Only then is the Windows tier
    # worth a spawn — and only where its userland exists.
    _sot_is_msys || return 1
    _sot_owner_pid_windows
}

# _sot_owner_pid_proc — the portable walk: `ps -o comm=`, then /proc. This is
# the ONLY tier on Linux and macOS, unchanged, and it still answers first on
# git-bash for a chain that never leaves msys (a claude started from the shell
# itself).
_sot_owner_pid_proc() {
    local pid="${PPID:-}" comm ppid
    while [ -n "$pid" ] && [ "$pid" != "1" ]; do
        comm="$(ps -o comm= -p "$pid" 2>/dev/null | tr -d ' ')"
        if [ -z "$comm" ] && [ -r "/proc/$pid/comm" ]; then
            comm="$(tr -d ' \t\n' < "/proc/$pid/comm" 2>/dev/null)"
        fi
        if [ -z "$comm" ] && [ -r "/proc/$pid/status" ]; then
            comm="$(awk '/^Name:/{print $2; exit}' "/proc/$pid/status" 2>/dev/null)"
        fi
        [ -n "$comm" ] || return 1
        comm="${comm##*/}"
        case "${comm%.exe}" in
            claude|codex) printf '%s\n' "$pid"; return 0 ;;
        esac
        ppid="$(ps -o ppid= -p "$pid" 2>/dev/null | tr -d ' ')"
        if [ -z "$ppid" ] && [ -r "/proc/$pid/status" ]; then
            ppid="$(awk '/^PPid:/{print $2}' "/proc/$pid/status" 2>/dev/null)"
        fi
        [ -n "$ppid" ] && [ "$ppid" != "$pid" ] || break
        pid="$ppid"
    done
    return 1
}

# sot_pid_alive PID — is that process still there? One helper, because the
# answer is NOT `kill -0` everywhere and the exception is invisible until it
# bites: `sot_owner_pid`'s git-bash tier legitimately returns a SYNTHETIC
# Cygwin pid for a process msys did not start (the claude.exe above a capsule
# row), and msys `kill -0` answers 1 for it — so every owner tether read its
# live agent as dead, armed, and exited on its first tick (measured on a
# Windows box, 2026-09-28: pid 73528 maps to WINPID 7992, the real claude.exe,
# and `kill -0 73528` fails).
#
# `kill -0` FIRST and always: on Linux and macOS that is the whole function
# and nothing else runs. The `ps -W` fallback is reached only after a failure
# and only on msys, so the hot path — a watcher asks this every two seconds —
# is unchanged off Windows and is one cheap fork on it. Never PowerShell or
# tasklist here, whatever the walk itself may cost once at startup.
#
# EITHER column matches: `ps -W` lists the msys pid in column 1 and the
# Windows pid in column 4, and `_sot_msys_pid_of` can legitimately hand back a
# WINPID when no msys pid exists for it.
sot_pid_alive() {
    local pid="${1:-}"
    [[ "$pid" =~ ^[0-9]+$ ]] || return 1
    kill -0 "$pid" 2>/dev/null && return 0
    _sot_is_msys || return 1
    ps -W 2>/dev/null | awk -v p="$pid" '$1 == p || $4 == p { found = 1; exit } END { exit !found }'
}

# --- the git-bash tier: one spawn, and the pid namespaces kept straight -----
#
# TWO PID NAMESPACES. An msys pid is not a Windows pid, and handing one to a
# Win32 query walks an unrelated process tree — which is WORSE than failing,
# because it answers confidently with a pid that owns nothing here. So every
# crossing is mapped explicitly: /proc/<pid>/winpid going out, the WINPID
# column of `ps -W` coming back, and nothing is returned that was not mapped.
#
# ONE SPAWN, not one per hop: the whole ancestor chain is walked inside a
# single PowerShell invocation over one `Get-CimInstance Win32_Process`
# snapshot (never `wmic`, which current Windows no longer ships).
#
# FAIL CLOSED AND FAST: no PowerShell, a refusal, a hang, an unmappable pid —
# every one of them returns non-zero, which lands the box exactly where it is
# today (the bootstrap prints MONITOR). This is on the bootstrap's hot path,
# so the call is bounded by `timeout` where one exists.

# _sot_winpid_of MSYS_PID — the Windows pid for an msys pid.
_sot_winpid_of() {
    local pid="${1:-}" w=""
    [[ "$pid" =~ ^[0-9]+$ ]] || return 1
    [ -r "/proc/$pid/winpid" ] && w="$(tr -dc '0-9' < "/proc/$pid/winpid" 2>/dev/null)"
    [ -n "$w" ] || w="$(ps -W 2>/dev/null | awk -v p="$pid" '$1 == p { print $4; exit }')"
    [[ "$w" =~ ^[0-9]+$ ]] || return 1
    printf '%s\n' "$w"
}

# _sot_msys_pid_of WIN_PID — the msys pid for a Windows pid, or the Windows pid
# itself when `ps -W` lists it without an msys one (a pure Win32 process is
# listed under its own winpid, which is the pid this shell's `kill` names it
# by). The caller needs a pid `kill -0` can ask about, never a raw handle.
_sot_msys_pid_of() {
    local w="${1:-}" m
    [[ "$w" =~ ^[0-9]+$ ]] || return 1
    m="$(ps -W 2>/dev/null | awk -v w="$w" '$4 == w { print $1; exit }')"
    [[ "$m" =~ ^[0-9]+$ ]] || m="$w"
    printf '%s\n' "$m"
}

# _sot_owner_pid_windows — the nearest claude/codex ancestor across the Windows
# boundary, as a pid THIS shell can signal. Prints nothing (rc 1) on any doubt.
_sot_owner_pid_windows() {
    local start ps_bin timeout_bin win
    start="$(_sot_winpid_of "${PPID:-}")" || return 1
    ps_bin="$(command -v powershell.exe 2>/dev/null || command -v pwsh.exe 2>/dev/null)" || return 1
    # NO BOUND, NO TIER. A corrupt WMI repository leaves Get-CimInstance
    # blocked for as long as it likes, and this runs on the session-start
    # path, so an unbounded call is a session that never starts. Without a
    # `timeout` binary there is nothing to bound it with, so the tier refuses
    # and the box keeps the Monitor — the same answer every other failure here
    # gives.
    timeout_bin="$(command -v timeout 2>/dev/null)" || return 1
    win="$(SOT_WALK_FROM="$start" "$timeout_bin" 10 "$ps_bin" -NoProfile -NonInteractive -Command '
$id = [int]$env:SOT_WALK_FROM
$map = @{}
Get-CimInstance Win32_Process -Property ProcessId,ParentProcessId,Name,CreationDate |
    ForEach-Object { $map[[int]$_.ProcessId] = $_ }
for ($i = 0; $i -lt 64 -and $map.ContainsKey($id); $i++) {
    $p = $map[$id]
    $n = ($p.Name -replace "\.exe$","").ToLower()
    if ($n -eq "claude" -or $n -eq "codex") { "$($p.ProcessId)"; break }
    $next = [int]$p.ParentProcessId
    if ($next -eq $id -or -not $map.ContainsKey($next)) { break }
    # A Windows pid is small, recycled hard, and ParentProcessId is NOT
    # cleared when the parent exits -- so a chain with no agent in it can
    # climb into a STRANGER whose pid was reused, and this would return it
    # with full confidence. A parent that started after its child is that
    # stranger; stop rather than answer.
    if ($map[$next].CreationDate -gt $p.CreationDate) { break }
    $id = $next
}' 2>/dev/null | tr -d '\r' | tr -dc '0-9')"
    [[ "$win" =~ ^[0-9]+$ ]] || return 1
    _sot_msys_pid_of "$win"
}

# sot_watcher_pid_for HANDLE — the live watcher pid recorded in
# state/<handle>.watch, verified BY IDENTITY, else nothing (rc 1). `kill -0`
# alone is not enough to act on: these markers live on a shared home and
# survive reboots, so a REUSED pid would let a teardown kill an unrelated
# process and let the start-time mutex refuse a legitimate watcher forever. The
# recorded pid must still BE a watcher for this handle — its command line names
# one of the watcher scripts and the handle itself. Anything else (gone,
# different command, no way to read one) means the marker is STALE: rc 1, and
# the caller proceeds as if it were absent.
sot_watcher_pid_for() {
    local handle="$1" pid
    pid="$(sed -n '1p' "$COMM_HOME/state/$handle.watch" 2>/dev/null)"
    sot_pid_is_watcher_for "$pid" "$handle" || return 1
    printf '%s\n' "$pid"
}

# sot_wake_watcher_pid_for HANDLE — the same marker read, narrowed to a live
# `comm-wake.sh`. The marker is SHARED by all three watcher scripts, so a
# guard that used the broad read above would refuse a ping start because a
# MONITOR's marker is there — the same wrong answer by the other door.
sot_wake_watcher_pid_for() {
    local handle="$1" pid
    pid="$(sed -n '1p' "$COMM_HOME/state/$handle.watch" 2>/dev/null)"
    sot_pid_is_wake_watcher_for "$pid" "$handle" || return 1
    printf '%s\n' "$pid"
}

# sot_pid_is_watcher_for PID HANDLE — is THIS pid a live watcher for HANDLE?
# The test sot_watcher_pid_for always applied to the marker's pid, lifted out
# so it can be applied to a pid found any other way. Liveness AND identity:
# the marker outlives reboots on a shared home, so a reused pid would let a
# teardown kill an unrelated process and let a start-time mutex refuse a
# legitimate watcher forever.
#
# The script must be what the process IS, not something its command line
# MENTIONS — only the first two arguments are looked at, and by basename. A
# watcher runs as `bash /path/comm-wake.sh <handle> ...` (its shebang puts the
# script in argv[1]), so those two fields are where the answer lives. The
# substring test this replaces was harmless while the only pid asked about
# came from our own marker, and became unsafe the moment a SCAN asked it about
# every pid on the box: any shell whose command line happened to carry both
# the script name and the handle — a grep, an editor, the session's own
# tooling — then counted as a live watcher and would refuse a legitimate
# start, i.e. leave the session deaf. Measured while building this, not
# theorised.
# BROAD: any of the three scripts that write the shared marker. This is the
# one the marker's own consumers use, because a Monitor's marker must read as
# LIVE there — narrow it and `_survived` would call a healthy Monitor stale,
# remove its marker and report the wrong thing.
sot_pid_is_watcher_for() {
    _sot_pid_is_watcher "${1:-}" "${2:-}" any
}

# NARROW: a live `comm-wake.sh` and nothing else. This is the one the START
# GUARD uses, because the guard exists to stop two PING watchers — a Monitor
# running beside one costs a doubled notice, while refusing to start costs a
# deaf session, and nobody re-arms a Monitor after this release. The two are
# deliberately different tests; folding them together reintroduces exactly
# that (measured on a Windows box, 2026-09-28: the guard counted a live
# comm-watch.sh as the handle's watcher).
sot_pid_is_wake_watcher_for() {
    _sot_pid_is_watcher "${1:-}" "${2:-}" wake
}

# _sot_pid_is_watcher PID HANDLE any|wake — liveness AND identity: the marker
# outlives reboots on a shared home, so a reused pid would let a teardown kill
# an unrelated process and let a start guard refuse a legitimate watcher
# forever.
_sot_pid_is_watcher() {
    local pid="${1:-}" handle="${2:-}" mode="${3:-any}" args rest field ok=0
    [[ "$pid" =~ ^[0-9]+$ ]] || return 1
    sot_pid_alive "$pid" || return 1
    if [ -r "/proc/$pid/cmdline" ]; then
        args="$(tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null)"
    else
        args="$(ps -o args= -p "$pid" 2>/dev/null)"
    fi
    [ -n "$args" ] || return 1
    rest="${args#* }"
    for field in "${args%% *}" "${rest%% *}"; do
        case "${field##*/}" in
            comm-wake.sh) ok=1 ;;
            comm-watch.sh|codex-watch.sh) [ "$mode" = any ] && ok=1 ;;
        esac
    done
    [ "$ok" = 1 ] || return 1
    case "$args" in *"$handle"*) ;; *) return 1 ;; esac
    return 0
}

# sot_pid_starttime PID — when the process started, in a unit comparable
# BETWEEN TWO PIDS ON THIS BOX (field 22 of /proc/<pid>/stat, ticks since
# boot). Prints nothing where there is no procfs; callers must treat that as
# "unknown", never as zero. Read from the field AFTER the last ')' because a
# process name can contain spaces and parentheses.
sot_pid_starttime() {
    local pid="${1:-}" stat
    [[ "$pid" =~ ^[0-9]+$ ]] || return 1
    [ -r "/proc/$pid/stat" ] || return 1
    stat="$(cat "/proc/$pid/stat" 2>/dev/null)" || return 1
    stat="${stat#*) }"
    printf '%s\n' "$stat" | awk '{ print $20 }' | tr -dc '0-9'
    printf '\n'
}

# sot_live_wake_watcher_for HANDLE [SELF_PID] — the pid of a live
# `comm-wake.sh` for HANDLE in the PROCESS TABLE, or nothing (rc 1). Narrow on
# purpose (see sot_pid_is_wake_watcher_for): its two callers are the start
# guard and the bootstrap's did-it-come-up check, and both ask the same
# question — does this row have a PING watcher.
#
# WHY THE PROCESS TABLE and not the record we keep: the defect this exists for
# was two watchers running side by side for seventeen hours, one of them named
# by no file at all — and every start consulted only the pid the marker named,
# so the unrecorded one was invisible to every future start and unreapable by
# every cleanup. A file can be missing, overwritten or judged stale; the
# process table cannot.
#
# TWO EXCLUSIONS, both load-bearing:
#   * SELF. This scan runs INSIDE a watcher, whose own command line carries
#     both the script name and the handle, so a naive scan always finds itself
#     and the watcher refuses to start every single time.
#   * A CANDIDATE THAT STARTED AFTER US, in `older` mode (the default, and
#     the START GUARD's question: is one ALREADY running that I must yield
#     to). The bootstrap asks a different question after spawning one -- does
#     this row have a ping watcher AT ALL -- and the answer there is a process
#     that is NEWER than the asker by definition, so it passes `any`. One
#     scan, two questions, named at the call site rather than duplicated.
#     Two starts racing for one handle can
#     each see the other as a live watcher and both refuse, leaving the
#     session with no watcher at all -- deafness, which is worse than the
#     double wake this guard exists to prevent. Only an EARLIER process
#     refuses us, so of any two racing starts exactly one proceeds; the tie
#     (two starts in the same clock tick) breaks on the lower pid, which is
#     arbitrary but is a total order, which is all this needs. Where no start
#     time is available (no procfs) the rule falls back to refusing on ANY
#     live watcher: the safe direction, and never the platform this was
#     written for.
sot_live_wake_watcher_for() {
    local handle="$1" self="${2:-$$}" mode="${3:-older}" mine pid theirs
    mine=""
    [ "$mode" = older ] && mine="$(sot_pid_starttime "$self" 2>/dev/null)"
    _sot_scan_pids | while read -r pid; do
        [ "$pid" = "$self" ] && continue
        sot_pid_is_wake_watcher_for "$pid" "$handle" || continue
        if [ -n "$mine" ]; then
            theirs="$(sot_pid_starttime "$pid" 2>/dev/null)"
            [ -n "$theirs" ] || { printf '%s\n' "$pid"; return 0; }
            if [ "$theirs" -gt "$mine" ]; then continue; fi
            if [ "$theirs" -eq "$mine" ] && [ "$pid" -gt "$self" ]; then continue; fi
        fi
        printf '%s\n' "$pid"
        return 0
    done | { read -r pid && { printf '%s\n' "$pid"; return 0; }; return 1; }
}

# _sot_scan_pids — every pid on this box, one per line. /proc where there is
# one (Linux AND git-bash, whose procfs lists every msys process), `ps`
# elsewhere.
_sot_scan_pids() {
    local d
    if [ -r /proc/self/cmdline ]; then
        for d in /proc/[0-9]*; do printf '%s\n' "${d##*/}"; done
        return 0
    fi
    ps -A -o pid= 2>/dev/null | tr -dc '0-9\n'
}

# --- the read cursor: a LINE OFFSET into inbox/<handle>.jsonl ----------------
#
# read/<handle>.cursor holds the NUMBER of inbox lines the recipient has been
# shown. It used to hold the newest-shown `ts`, and those stamps are
# second-resolution while every comparison was strictly-greater: a frame filed
# in the same second as one already read was never shown and never announced,
# while the sender printed a success line — a false-positive acknowledgement,
# the one thing this design may not produce. A count cannot lose a frame that
# way.
#
# sot_cursor_offset HANDLE — that offset. Three rules, each one a way this could
# otherwise go silently deaf:
#
#   * A LEGACY ts cursor is converted in memory (never written here — only a real
#     comm-poll.sh advances the cursor) by counting the lines BEFORE THE FIRST
#     one whose ts is greater than it. Not "every line at or below it": stamps
#     are only in order if every sender's clock agrees, and with a skewed clock
#     across hosts (or two frames in one second) that count would step PAST an
#     unread frame already on disk and it would never be shown. Stopping at the
#     first greater line inherits no loss at all.
#   * An unparseable line counts as read and never as a boundary. A torn append
#     is realistic on a shared filesystem, and one must not be able to freeze
#     the cursor: that makes a handle permanently deaf while its senders keep
#     printing a success line.
#   * An offset PAST the end of the inbox is 0. Production only appends, so this
#     means the file was cleared, truncated or restored by hand — exactly the
#     moment nobody suspects the cursor, and left as-is the handle never sees
#     another message. The one exception: a hashed cursor exactly one past the
#     end is the last line read, cut back with nothing filed since — one step
#     back, like any other cut-back.
#
# Anything unreadable yields 0. On doubt this biases LOW: showing a frame twice
# is tolerable where dropping one is not.
sot_cursor_offset() {
    local handle="$1" cur n cnt hash="" total
    # $COMM_HOME, not the source-time $READ_DIR: comm-wake.sh re-derives its
    # home inside its own main, and a helper reading a different one than its
    # caller is a silently wrong answer.
    cur="$(cat "$COMM_HOME/read/$handle.cursor" 2>/dev/null || true)"
    [ -n "$cur" ] || { printf '0\n'; return 0; }
    # `<count>` (every cursor written before the hash existed) or
    # `<count> <hash>`; anything else is the legacy ts form.
    cnt="${cur%% *}"
    [ "$cnt" = "$cur" ] || hash="${cur#* }"
    case "$cnt" in
        ''|*[!0-9]*) ;;
        *)
            total="$(sot_inbox_lines "$handle")"
            if [ "$cnt" -gt "$total" ]; then
                if [ "$cnt" -eq $((total + 1)) ] && [ -n "$hash" ]; then
                    printf "note: the last line read from @%s's inbox was cut back; reading from the line before it\n" "$handle" >&2
                    printf '%s\n' "$total"
                else
                    printf '0\n'
                fi
                return 0
            fi
            # A hash says which line the cursor consumed last. If line CNT is
            # no longer it, a cut-back removed it (the append that wrote it
            # failed after a reader counted it), and a cut-back removes at most
            # that one line: one step back is exact.
            if [ "$cnt" -gt 0 ] && [ -n "$hash" ] \
                && [ "$(sot_line_hash "$COMM_HOME/inbox/$handle.jsonl" "$cnt")" != "$hash" ]; then
                printf "note: the last line read from @%s's inbox was cut back; reading from the line before it\n" "$handle" >&2
                cnt=$((cnt - 1))
            fi
            printf '%s\n' "$cnt"; return 0 ;;
    esac
    n="$(sot_jq -Rrs --arg cur "$cur" '
        [ split("\n")[] | select(length > 0)
          | (((fromjson? // {}) | (.ts // "")) > $cur) ] as $past
        | ($past | index(true)) // ($past | length)' \
        "$COMM_HOME/inbox/$handle.jsonl" 2>/dev/null)" || n=0
    [[ "$n" =~ ^[0-9]+$ ]] || n=0
    _sot_clamp_offset "$handle" "$n"
}

# _sot_hash_stdin — THE line hash: `<crc>-<len>` (cksum, POSIX, in git-bash
# too) of stdin without its newlines and NULs. A NUL is dropped because bash
# drops it from a reader's copy, so the file's bytes and the copy hash alike.
_sot_hash_stdin() { tr -d '\n\000' | cksum | awk '{print $1 "-" $2}'; }

# sot_line_hash FILE N — the line hash of line N of FILE.
sot_line_hash() {
    sed -n "${2}p" "$1" 2>/dev/null | _sot_hash_stdin
}

# sot_cursor_write HANDLE COUNT LINE — the read cursor: `<count> <hash of
# LINE>`, or just `0`. LINE is the bytes of line COUNT as the caller read them
# (no newline), never re-read from the file: a line shown and then cut back
# must not have its hash taken from the line filed in its place. The hash is
# _sot_hash_stdin's, the one sot_line_hash takes of the file, so a line holding
# a NUL (bash has already dropped it from LINE) hashes the same on both sides.
sot_cursor_write() {
    local h="$1" n="$2"
    if [ "$n" -gt 0 ]; then
        printf '%s %s' "$n" "$(printf '%s' "$3" | _sot_hash_stdin)" > "$COMM_HOME/read/$h.cursor"
    else
        printf '0' > "$COMM_HOME/read/$h.cursor"
    fi
}

# _sot_clamp_offset HANDLE N — N, or 0 when it points past the end of the inbox.
_sot_clamp_offset() {
    local total; total="$(sot_inbox_lines "$1")"
    if [ "$2" -gt "$total" ]; then printf '0\n'; else printf '%s\n' "$2"; fi
}

# sot_file_lines PATH — PATH's line count, 0 when it is absent or unreadable.
# The invariant every inbox reader leans on: this counts newline-TERMINATED
# lines only (`wc -l`), and readers read `sed -n "a,${count}p"` up to that
# count, so an unterminated tail a dead writer left is never counted, never
# read, and its later cut never moves a cursor.
# THE line counter: every inbox reader needs one, and each copy was a chance
# to get the two quiet parts wrong. Readability is tested FIRST because the
# SHELL, not wc, prints "No such file" for `< missing` — before wc's own
# 2>/dev/null can suppress it, into whatever the caller's stderr happens to be
# (a durable watcher log, a bootstrap's one-line-per-outcome contract). And the
# count is stripped of the leading spaces a BSD `wc` pads it with, so callers
# can compare it as a number without each one remembering to.
sot_file_lines() {
    local n=""
    [ -r "$1" ] && n="$(wc -l < "$1" 2>/dev/null | tr -d ' ')"
    [[ "$n" =~ ^[0-9]+$ ]] || n=0
    printf '%s\n' "$n"
}

# sot_inbox_lines HANDLE — the inbox's line count (0 when absent).
sot_inbox_lines() {
    sot_file_lines "$COMM_HOME/inbox/$1.jsonl"
}

# --- the WINDOWS FRONTEND inbox: a second file, a second cursor --------------
#
# On Windows nothing writes inbox/<handle>.jsonl:
# the frontend files every inbound relay frame into its own fe-inbox.jsonl
# (comm-watch.sh's Windows branch), so THAT file is the
# mail. Every reader was blind to it, and a Windows session went an hour without
# seeing two messages while comm-poll.sh printed "No new messages" from a
# per-handle file three weeks stale (field report, 2026-09-27). Two facts make it
# a different file rather than the same one under another name:
#
#   * TWO SCHEMAS. A frontend line carries `.text`; a per-handle line carries
#     `.msg`. Rewriting at this boundary is what keeps every reader downstream
#     single-schema instead of learning both.
#   * ONE FILE PER BOX, SHARED BY EVERY HANDLE. The frontend appends every frame
#     from every connection whatever its `to`, so admission is `.to == this
#     handle` STRICTLY — unlike the per-handle inbox, where a legacy line with no
#     `.to` at all is treated as directed, because that file is already this
#     handle's alone.
#
# The cursor is a LINE OFFSET like the per-handle one but in its OWN file,
# read/<handle>.fe.cursor: the two inboxes have unrelated line counts, so one
# shared cursor would silence whichever file is shorter. It has no legacy `ts`
# form to migrate — it was born a count.
#
# The platform test lives HERE and nowhere else (review, 2026-09-27: stop adding
# a Windows branch to one more reader). A reader asks for the path; off Windows
# it gets nothing and its frontend arm is simply inert.

# sot_fe_inbox_path — the frontend inbox, or NOTHING on every non-Windows box.
# Mirrors comm-watch.sh's Windows branch exactly, including the fallback chain
# for a box with no %LOCALAPPDATA%.
sot_fe_inbox_path() {
    _sot_is_windows || return 0
    printf '%s/sot/fe-inbox.jsonl\n' "${LOCALAPPDATA:-${XDG_STATE_HOME:-$HOME/.local/state}}"
}

# sot_fe_inbox_lines — the frontend inbox's line count (0 when absent, and off
# Windows).
sot_fe_inbox_lines() {
    local fe
    fe="$(sot_fe_inbox_path)"
    [ -n "$fe" ] || { printf '0\n'; return 0; }
    sot_file_lines "$fe"
}

# sot_fe_cursor_offset HANDLE — this handle's frontend read offset. 0 when
# unset, unreadable, non-numeric, or PAST the end of the file: production only
# appends, so an offset past the end means the file was cleared, truncated or
# restored by hand — exactly the moment nobody suspects the cursor, and left
# as-is this handle never sees another frontend frame.
# The FE cursor stays count-only, with no line hash and no read lock: the
# frontend that writes fe-inbox.jsonl never cuts a line back.
sot_fe_cursor_offset() {
    local cur total
    cur="$(cat "$COMM_HOME/read/$1.fe.cursor" 2>/dev/null || true)"
    [[ "$cur" =~ ^[0-9]+$ ]] || cur=0
    total="$(sot_fe_inbox_lines)"
    [ "$cur" -gt "$total" ] && cur=0
    printf '%s\n' "$cur"
}

# sot_fe_unread_lines HANDLE — the frontend lines past HANDLE's frontend cursor
# that are addressed to HANDLE, one compact JSON object per line, rewritten into
# the per-handle schema (`.text` -> `.msg`). Nothing off Windows.
#
# An unparseable line is SKIPPED, never fatal: a torn append is realistic and the
# reader advances its cursor past it, so it counts as read exactly like a torn
# per-handle line — one bad line must not be able to pin a cursor and leave a
# handle permanently deaf. Provenance filters (self-echo, __selftest__) stay with
# the readers, which already apply them to the per-handle schema this output now
# shares.
sot_fe_unread_lines() {
    local handle="$1" fe pos total
    fe="$(sot_fe_inbox_path)"
    [ -n "$fe" ] && [ -r "$fe" ] || return 0
    pos="$(sot_fe_cursor_offset "$handle")"
    total="$(sot_fe_inbox_lines)"
    [ "$total" -gt "$pos" ] || return 0
    sed -n "$((pos + 1)),${total}p" "$fe" 2>/dev/null \
        | jq -Rc --arg me "$handle" '
            (fromjson? // empty) | select(type == "object")
            | select((.to // "") == $me)
            | . + {msg: (.msg // .text // "")}' 2>/dev/null || true
}

# --- live delivery into a workspace row (comm-send.sh, comm-bootstrap.sh,
# codex-watch.sh) ---
#
# sot_pty_input WORKSPACE_ID DATA_B64 — one `pty.input` request (enter:true)
# to the daemon at ENDPOINT (caller's scope); prints the response line.
# The daemon types the text into the row's capsule and appends Enter, and
# reports `enter_sent`. This is the only live-delivery path: a message
# reaches a session by its workspace row or stays in the durable inbox.
sot_pty_input() {
    local wsid="$1" data="$2" frame
    # base64 can begin with "/" (MSYS2 path conversion): --rawfile, never --arg.
    local _data_file; _data_file="$(sot_jq_rawfile "$data")" || return 1
    frame="$(jq -nc --arg w "$wsid" --rawfile d "$_data_file" \
        '{v:1,id:1,kind:"req",op:"pty.input",payload:{workspace_id:$w,data_b64:$d,enter:true}}')"
    local rc=$?
    rm -f "${_data_file:?}"
    [ "$rc" -eq 0 ] || return 1
    # ~18s is the daemon's own worst case for one enter=true write.
    SOT_SEND_TIMEOUT="${SOT_SEND_TIMEOUT:-20}" sot_oneshot_request "$frame" "pty.input"
}

# sot_pty_screen WORKSPACE_ID — one `pty.screen` request (no scrollback,
# current screen only) to the daemon at ENDPOINT (caller's scope); prints
# the response line. Lifted out of sot-fe's send_pty_screen (ADR 0042
# amendment) so comm-wake.sh's prompt-free gate and sot-fe share the one
# implementation instead of two frame-builders drifting apart.
sot_pty_screen() {
    local wsid="$1" frame
    frame="$(jq -nc --arg w "$wsid" '{v:1,id:1,kind:"req",op:"pty.screen",payload:{workspace_id:$w}}')"
    # No local default here (fixed 2026-09-17): sot_oneshot_request's own
    # fallback chain is SOT_SEND_TIMEOUT -> SEND_TIMEOUT -> 10. Presetting
    # SOT_SEND_TIMEOUT=10 here shadowed a caller-set SEND_TIMEOUT (sot-fe
    # screen --timeout N exports SEND_TIMEOUT, then its own error text quotes
    # SEND_TIMEOUT), so the flag was silently ignored. Let the callee's own
    # fallback apply unmangled.
    sot_oneshot_request "$frame" "pty.screen"
}

# sot_pty_input_gated WORKSPACE_ID DATA_B64 — sot_pty_input, but only into a
# row that is sitting at a FREE prompt. Typing plus Enter submits a turn, so a
# row with a dialog, a menu or a half-typed draft on screen must never be
# typed into: the keystrokes would land in whatever is open. The test is
# `sot_prompt_free` above — the CURSOR's position, not the line's text — so the
# sender's poke (comm-send.sh) and the ping watcher share ONE implementation
# instead of two that drift. Returns 0 typed, 1 screen read but NOT free (also an
# accepted-but-not-ok row — nothing was typed either way), 2 no reply at all
# (transport error/empty response, kept distinct so a caller can tell a busy
# row from a dead daemon), 3 the daemon does not have this row at all. 3 is
# separate from 1 because they are different facts about different worlds: 1 is
# a row that IS there with something on its screen, 3 is a row that is not
# there. Both arrive as a non-empty reply that is not a free prompt, and
# collapsing them made a destroyed row report as a session sitting at a dialog
# — a default dressed as a signal from a row that no longer exists. Checked on
# BOTH replies: the row can be destroyed between the screen read and the input.
# sot_prompt_free SCREEN_JSON — 0 when the row is sitting at an EMPTY input
# line with the cursor at its start. THE prompt test: both the sender's poke
# (sot_pty_input_gated) and the ping watcher's gate read it, so "free prompt"
# cannot come to mean two different things.
#
# It is the CURSOR that decides, not the text. pty.screen returns plain text
# with every attribute stripped (capsule_workspace.rs maps the vt100 rows
# through trim_end), so a grey PROMPT SUGGESTION — ghost text Claude Code
# draws after the insertion point on an empty input — is byte-identical to a
# half-typed human draft. Matching the text alone therefore held every wake
# for as long as a suggestion sat on screen: a row went deaf for a day with a
# live watcher and no signal (2026-09-25). The cursor separates them with no
# new protocol: ghost text leaves the cursor AT the input start, typed text
# pushes it past what was typed. Anchoring on the cursor's own line also
# closes the opposite hole — a bare `❯` anywhere on screen (in output, or
# behind a permission dialog) used to open the gate.
#
# FREE means all of: a cursor is present; its row indexes a real line; that
# line carries `❯` with nothing but spaces before it; and the cursor column
# is at the insertion point. That last part does NOT guess which convention
# the renderer uses, because guessing is unsound in both directions. A renderer
# that draws a separator (`❯ text` — the measured one: a live row showed the
# glyph at column 0 and the cursor at column 2, with a NON-BREAKING space
# between) puts an empty input's cursor at glyph+2; one that draws none
# (`❯text`) puts it at glyph+1, and puts a ONE-CHARACTER DRAFT at glyph+2.
# So glyph+2 alone is not "empty" and neither is the pair: accept both columns
# blindly and a one-character draft on a no-separator prompt reads FREE and
# gets submitted, which is the exact harm this gate exists to prevent.
# The screen already carries the answer, so READ it instead: glyph+1 is always
# the insertion point, and glyph+2 is the insertion point only when the cell
# between glyph and cursor is a separator — a space, a non-breaking space, a
# tab, or ABSENT (the backend trims trailing whitespace, so an empty prompt
# under the separator convention arrives as a bare `❯` with nothing at
# glyph+1). A typed character there is not a separator, and the gate holds.
# The all-spaces prefix test above is what makes the byte offset `index`
# returns safe to reuse as a codepoint index here: an all-ASCII prefix has
# both the same. Everything else is NOT free: no
# cursor (never happens on a healthy capsule row — the field has existed
# since the op was born, and the only other runtime answers with an error
# payload and no lines at all), a row out of range, an error payload, a
# malformed reply, or an empty string. The glyph is written `❯` so the
# jq PROGRAM stays pure ASCII across the git-bash leg's native jq.
sot_prompt_free() {
    # jq exits 0 on EMPTY stdin, which would read as "free" — a screen we
    # never saw is never a free prompt.
    [ -n "$1" ] || return 1
    printf '%s' "$1" | jq -e '
        (.payload // {}) as $p
        | ($p.lines // []) as $L
        | ($p.cursor // {}) as $k
        | ($k.row // -1) as $r
        | ($k.col // -1) as $c
        | if ($r|type) != "number" or ($c|type) != "number"
             or $r < 0 or $r >= ($L|length) then false
          else ($L[$r]) as $line
             | ($line | index("\u276f")) as $g
             | ($g != null)
               and (($line[0:$g] | test("[^ ]")) | not)
               and ( $c == $g + 1
                     or ( $c == $g + 2
                          and ( $line[$g+1:$g+2]
                                | . == "" or . == " " or . == "\u00a0" or . == "\t" ) ) )
          end
    ' >/dev/null 2>&1
}

# sot_wake_row HANDLE — the LIVE workspace row that declares HANDLE, asked of
# the daemon at $ENDPOINT at the moment of asking. This is the wake path's ONE
# authority for handle->row: the registry's own `workspace_id` and the ping
# watcher's environment are both copies taken once at join time, and a session
# that continues in another row keeps waking the row it used to be in
# (2026-09-28: one ping held ~27h). The daemon owns rows and re-learns the
# binding on every `agent.join` (ADR 0046), so ask IT.
# Prints the id and returns 0 for exactly one match; 1 when the daemon answered
# and no live row declares it; 2 when there was no usable answer; 3 when two or
# more rows declare it. Two rows CAN declare one handle: `set_agent_handle`
# writes one row and clears no other. 1 and 3 are REFUSALS -- a wake aimed by a
# guess types into someone else's session, which is the harm the prompt gate
# exists to prevent.
# $ENDPOINT is the caller's, exactly as sot_pty_screen and sot_pty_input
# already take it; this never resolves an endpoint itself.
sot_wake_row() {
    local h="$1" resp ids id count=0 first=""
    [ -n "$h" ] || return 2
    resp="$(sot_oneshot_request '{"v":1,"id":1,"kind":"req","op":"workspace.list","payload":{}}' workspace.list 2>/dev/null)" || return 2
    [ -n "$resp" ] || return 2
    # The array check is what separates rc 2 (garbage) from rc 1 (a real empty
    # answer). Without it a malformed reply reads as "no live row" and the
    # watcher exits on a transport hiccup.
    printf '%s' "$resp" | jq -e '(.payload.workspaces | type) == "array"' >/dev/null 2>&1 || return 2
    ids="$(printf '%s' "$resp" | jq -r --arg h "$h" \
        '.payload.workspaces[] | select((.agent_handle // "") == $h) | .workspace_id' 2>/dev/null)" || return 2
    # Counted in this shell, not a pipeline: no `grep -c`, no `wc -l` on a
    # possibly-empty string, and no subshell that would lose $count.
    while IFS= read -r id; do
        [ -n "$id" ] || continue
        count=$((count + 1))
        [ "$count" -eq 1 ] && first="$id"
    done <<EOF
$ids
EOF
    case "$count" in
        0) return 1 ;;
        1) printf '%s\n' "$first"; return 0 ;;
        *) return 3 ;;
    esac
}

# sot_row_gone RESP — 0 when RESP is the daemon's refusal for a workspace it
# does not have (`code: "unknown_workspace"`, the pty.screen and pty.input arms
# both answer it), 1 otherwise, including an empty RESP: a reply we never saw
# is not evidence the row is gone.
sot_row_gone() {
    [ -n "$1" ] || return 1
    printf '%s' "$1" | jq -e '(.payload.code // "") == "unknown_workspace"' >/dev/null 2>&1
}

sot_pty_input_gated() {
    local wsid="$1" data="$2" screen resp
    screen="$(sot_pty_screen "$wsid" 2>/dev/null)"
    [ -n "$screen" ] || return 2
    sot_row_gone "$screen" && return 3
    sot_prompt_free "$screen" || return 1
    resp="$(sot_pty_input "$wsid" "$data" 2>/dev/null || true)"
    [ -n "$resp" ] || return 2
    sot_row_gone "$resp" && return 3
    printf '%s' "$resp" | jq -e '.payload.ok == true' >/dev/null 2>&1 || return 1
    return 0
}

# sot_capsule_workspace_id — print the row id THIS SHELL'S IDENTITY names,
# or print nothing and return 1 when it names none. The identity is the
# pinned $SOT_COMM_SELF_FILE, which comm-context.sh names
# "<host>__<workspace_id>.txt", so the row id travels with the identity
# instead of with the environment. $SOT_WORKSPACE_ID answers only where no
# self file is pinned — there the slot comm-context.sh derives is keyed by
# that same ambient id, so the two agree by construction. "nopane" is the
# literal placeholder comm-context.sh writes for a non-capsule shell, never
# a real id — treated the same as absent.
#
# The order is load-bearing and ran the other way until 2026-09-28. A test
# or a lane pins its own scratch identity but inherits $SOT_WORKSPACE_ID
# from the session that launched it, so the ambient id let every hermetic
# suite declare its throwaway handle into the live row it happened to run
# inside — ninety-two such declarations in one morning, and the last one
# left that row naming a handle no status lookup could resolve, which the
# owner saw as a permanently grey badge on a session that was working fine.
# An identity that does not name a row has no row to declare into.
sot_capsule_workspace_id() {
    local base="${SOT_COMM_SELF_FILE:-}"
    if [ -z "$base" ]; then
        [ -n "${SOT_WORKSPACE_ID:-}" ] || return 1
        printf '%s\n' "$SOT_WORKSPACE_ID"
        return 0
    fi
    base="$(basename "$base")"
    case "$base" in
        *__*.txt) ;;
        *) return 1 ;;
    esac
    local id="${base#*__}"
    id="${id%.txt}"
    [ -n "$id" ] && [ "$id" != "nopane" ] || return 1
    printf '%s\n' "$id"
}
# --- MSYS2 argv-conversion guard for jq values that can legitimately
# start with "/" ---
#
# capsule-comm-identity fix, field-measured on a Windows git-bash box: when
# a NATIVE (non-MSYS) jq.exe is invoked, MSYS2's argv-to-Windows-path
# conversion rewrites any ARGV ELEMENT that starts with "/" into a Windows
# path before jq ever sees it — verified through the real relay, a message
# beginning "/sot-session-start ..." arrived in the peer's inbox mangled
# into a filesystem path. The rule is exact and easy to miss in ad hoc
# testing: only the FIRST character of the whole argument matters (a bare
# "/" corrupts too); "./", "~/", "//server", and every MID-string slash
# are untouched; on a MULTI-LINE value only the FIRST line is mangled —
# every later line survives intact, which is why this read as an isolated
# typo rather than a systematic corruption. `--arg NAME "$value"` passes
# $value as its own argv element, so any of the THREE values in this
# codebase that can genuinely start with "/" — a message body
# (comm-send.sh, comm-relay.sh) and a project root (comm-join.sh) — must
# never go through `--arg`. Every OTHER `--arg` (handles, hosts, ids,
# timestamps) is drawn from a restricted charset that can never start
# with "/" and is untouched by this fix.
#
# sot_jq_rawfile VALUE — writes VALUE verbatim (no added newline) to a
# fresh temp file and prints its path, for use as `jq --rawfile NAME
# <path> ...` in place of `--arg NAME "$VALUE"`: the risky VALUE now lives
# in the file's CONTENT, read by jq via fread — never an argv element, so
# never subject to the conversion. The file PATH argument itself is left
# as an ordinary argument and SHOULD still convert when it starts with
# "/" (that's the well-behaved half of the same MSYS2 mechanism — it's
# what lets a native jq.exe find the file at all), so no
# MSYS2_ARG_CONV_EXCL or similar exclusion is involved. The caller owns
# the returned path and MUST `rm -f` it once jq has run.
sot_jq_rawfile() {
    local f
    f="$(mktemp "${TMPDIR:-/tmp}/sot-comm-jq.XXXXXX" 2>/dev/null)" || {
        echo "sot_jq_rawfile: could not create a temp file for a jq --rawfile value" >&2
        return 1
    }
    if ! printf '%s' "$1" > "$f" 2>/dev/null; then
        echo "sot_jq_rawfile: write to temp file '$f' failed" >&2
        rm -f "${f:?}" 2>/dev/null
        return 1
    fi
    printf '%s' "$f"
}

# --- sender identity: NAME resolved is not enough, it must be ROUTABLE ---
#
# Codex review round-2 finding 4/C: comm-send.sh, comm-relay.sh, and
# comm-bootstrap.sh each carried their OWN "resolved identity" check, and
# each treated a merely NONEMPTY $NAME as good enough. That's insufficient:
# a self-file can resolve NAME locally (it passed comm-context.sh's own
# root=/repo= validation) while the registry itself has no matching row
# for it at all (evicted, or a failed registry write) or — worse — a row
# whose root belongs to a DIFFERENT project (this handle was reclaimed
# elsewhere). Either way, sending under it stamps a from-handle a reply
# can't route back to, or routes it to the wrong session. "Resolved" now
# means ROUTABLE: NAME nonempty AND a registry row for it exists AND (that
# row's root is empty — a legacy row, allowed during the migration window
# — OR it matches this project's canonical root).
#
# ONE helper, called by all three scripts, replacing three separately
# drifting diagnostic essays with the invariant plus the exact recovery
# command. Must be called BEFORE any endpoint/socket/transport resolution
# (Codex review round-2 SHOULD-FIX 2) so an unresolved sender always sees
# THIS refusal, never an unrelated daemon/socket error.
#
# Prints nothing and returns 0 if routable. Prints ONE refusal line and
# returns 1 otherwise. Depends on NAME/PROJECT_ROOT already being set by
# `eval "$(comm-context.sh)"` — call after that, never before.
sot_require_routable_identity() {
    if [ -z "${NAME:-}" ]; then
        echo "ERROR: your sot-comm identity did not resolve — refusing to send with no verifiable from-handle (a reply would silently misroute). Join first: comm-join.sh --name <canonical-handle> (never a bare comm-join.sh if you previously held one — see the sot-session-start skill's recovery recipe)." >&2
        return 1
    fi
    local reg_status reg_root qname qdir
    IFS=$'\t' read -r reg_status reg_root <<< "$(sot_registry_entry_status "$NAME")"
    # %q-quote the handle AND the executable path (Codex review round-3
    # finding 7): a raw $NAME/$SCRIPT_DIR interpolation produces a wrong
    # or unsafe copy-paste command for a handle or install path containing
    # spaces/metacharacters. $SCRIPT_DIR is this HELPER's caller's own
    # directory (every caller sources comm-lib.sh after setting it).
    qname="$(printf '%q' "$NAME")"
    qdir="$(printf '%q' "${SCRIPT_DIR:-.}")"
    if [ "$reg_status" != "present" ]; then
        echo "ERROR: your sot-comm identity '@$NAME' has no registry row — refusing to send with an unroutable from-handle (a reply would silently misroute). Reclaim it: $qdir/comm-join.sh --name $qname" >&2
        return 1
    fi
    if [ -n "$reg_root" ] && [ "$reg_root" != "${PROJECT_ROOT:-}" ]; then
        echo "ERROR: your sot-comm identity '@$NAME' is registered to a DIFFERENT project's root ('$reg_root') — refusing to send with a misrouting from-handle. Reclaim it: $qdir/comm-join.sh --name $qname" >&2
        return 1
    fi
    return 0
}

# --- derived-handle disambiguation (ADR 0028 addendum: "derived vs
# explicit") --- single home for the algorithm; comm-join.sh and
# comm-spawn.sh both call sot_derive_handle. This is ONLY for a name that
# comes from DERIVATION (the default <basename>-<host>): a caller must never
# route an explicit --name, $SOT_COMM_NAME, or an already-joined self-file
# identity through here — those stay verbatim, unconditionally.

# sot_canonical_path PATH — absolute, symlink-resolved path (the
# "canonical project root" the disambiguation compares), or NOTHING on
# stdout plus a nonzero return if one can't be established (Codex review
# F8). NEVER falls back to an unresolved/relative path: two callers that
# each `cd` into a differently-spelled relative path (or the SAME literal
# "./foo" from two different directories) would otherwise compare as
# identical roots, defeating the whole disambiguation.
sot_canonical_path() {
    local p="$1" out
    if command -v realpath >/dev/null 2>&1; then
        if out="$(realpath -- "$p" 2>/dev/null)" && [ -n "$out" ]; then
            printf '%s\n' "$out"
            return 0
        fi
    fi
    if command -v readlink >/dev/null 2>&1; then
        if out="$(readlink -f -- "$p" 2>/dev/null)" && [ -n "$out" ]; then
            printf '%s\n' "$out"
            return 0
        fi
    fi
    echo "sot_canonical_path: could not resolve a canonical path for '$p' (no working realpath or readlink -f) — refusing to record a relative/unresolved project root" >&2
    return 1
}

# sot_hash6 STR — first 6 hex chars of sha256(STR); stable per input, used
# only for the last-resort hash-qualified handle tier. NO fallback when
# sha256sum/shasum are both missing (Codex review F8 / simplicity audit):
# the earlier `cksum` fallback was a variable-length decimal CRC, not a
# hash6 — installing sha256sum later would silently change that root's
# tier-3 handle. Fail loudly instead; the caller surfaces this as a hard
# error asking for an explicit --name.
#
# Both the pipeline's exit status AND the shape of its output are checked
# (Codex review PR #148 round 2, finding 5): the previous version ran
# `return 0` unconditionally after each pipeline, so an INSTALLED-but-
# FAILING sha256sum (confirmed: one that exits 23) still "succeeded" with
# an EMPTY hash, producing a `<base>--<host>` handle instead of a loud
# failure. `rc=$?` right after the pipeline reflects its real exit status
# under this shell's `pipefail` (both callers set it); the regex is the
# stronger, direct check — it also catches a tool that exits 0 but emits
# garbage, which an exit-code check alone would miss.
sot_hash6() {
    local out rc
    if command -v sha256sum >/dev/null 2>&1; then
        out="$(printf '%s' "$1" | sha256sum | cut -c1-6)"; rc=$?
        if [ "$rc" -eq 0 ] && [[ "$out" =~ ^[0-9a-f]{6}$ ]]; then
            printf '%s\n' "$out"
            return 0
        fi
    fi
    if command -v shasum >/dev/null 2>&1; then
        out="$(printf '%s' "$1" | shasum -a 256 | cut -c1-6)"; rc=$?
        if [ "$rc" -eq 0 ] && [[ "$out" =~ ^[0-9a-f]{6}$ ]]; then
            printf '%s\n' "$out"
            return 0
        fi
    fi
    echo "sot_hash6: no working sha256sum/shasum produced a valid 6-hex-character digest — cannot compute a stable tier-3 handle qualifier" >&2
    return 1
}

# sot_slug LABEL — bash mirror of rust/backend/src/paths.rs::slug (Codex
# review PR #148 round 2, finding 3): lowercase; '.' -> '_' BEFORE the
# keep-check; a RUN of characters outside [a-z0-9_-] collapses to a single
# '-' (a LITERAL '-'/'_'/alnum in the input is pushed as-is and never
# collapsed, even if repeated — matching Rust's keep/else branch split
# exactly, not a blanket dash-collapse); trailing '-' trimmed; empty ->
# "default". Verified against every example in that function's own doc
# comment (MyPackage.jl -> mypackage_jl, "Foo Bar" -> foo-bar, /abs/path
# -> abs-path, "  " -> default) plus literal-repeated-dash and leading-
# junk cases. Needed because workspace.create's same-slug path is an
# intentional metadata-refresh idempotence, not an error — two labels
# that only differ by case, or by a dot vs underscore, resolve to the
# SAME workspace and must be caught as a collision too, not just a
# byte-identical label match.
sot_slug() {
    local label="$1" out="" last_dash=false i len ch c
    len=${#label}
    for (( i = 0; i < len; i++ )); do
        ch="${label:i:1}"
        c="$(printf '%s' "$ch" | tr '[:upper:]' '[:lower:]')"
        [ "$c" = "." ] && c="_"
        case "$c" in
            [a-z0-9_-])
                out="${out}${c}"
                if [ "$c" = "-" ]; then last_dash=true; else last_dash=false; fi
                ;;
            *)
                if [ "$last_dash" = false ] && [ -n "$out" ]; then
                    out="${out}-"
                    last_dash=true
                fi
                ;;
        esac
    done
    while [[ "$out" == *- ]]; do out="${out%-}"; done
    [ -z "$out" ] && out="default"
    printf '%s\n' "$out"
}

# sot_sanitize_component STR [MAXLEN=20] — reduce STR to the
# workspace.create charset [A-Za-z0-9._-] (every other byte becomes '-',
# runs of '-' collapse, leading/trailing '-' trimmed) and clamp it to
# MAXLEN (Codex review F4): a repo/parentdir basename can contain spaces,
# Unicode, or shell metacharacters, none of which workspace.create's name
# validator (`rust/backend/src/handlers.rs`, `valid_name`) accepts — and an
# unsanitized basename reaching the `--no-workspace` launcher string is a
# shell-injection vector. Applied to EVERY raw piece (basename, parentdir,
# host) BEFORE composing a candidate, never to the assembled candidate
# afterward, so composed separators can't be reintroduced or hidden behind
# a length overflow. MAXLEN defaults to 20 so the worst-case composed
# candidate (20 + "-" + 20 + "-" + 20 = 62) stays inside workspace.create's
# 64-char limit without per-tier budget arithmetic.
sot_sanitize_component() {
    local s="$1" max="${2:-20}"
    s="$(printf '%s' "$s" | tr -c 'A-Za-z0-9._-' '-')"
    while [[ "$s" == *--* ]]; do s="${s//--/-}"; done
    s="${s#-}"; s="${s%-}"
    s="${s:0:$max}"
    s="${s%-}"
    [ -z "$s" ] && s="x"
    printf '%s' "$s"
}

# sot_registry_entry_status NAME — tagged status of the registry row for
# NAME (Codex review simplicity audit: replaces a magic sentinel string
# with tagged output, so "no row" and "row present but root unknown" can
# never be confused with each other or with an actual, if empty, root
# value):
#   "absent\t"          — NAME has no row at all
#   "present\t<root>"   — NAME has a row; <root> is "" for a legacy row
#                         that predates this feature (unknown root)
#   "error\t"           — the registry could not be read/parsed (Codex
#                         review round-3 finding 1): jq failing (malformed
#                         JSON, unreadable file, an NFS hiccup) used to
#                         print NOTHING, which read back as an empty
#                         string indistinguishable from "absent" to every
#                         caller — letting a pane-keyed legacy self-file
#                         self-heal on a basename match with the registry
#                         effectively unconsultable. Callers MUST treat
#                         "error" as NO EVIDENCE, never as "absent".
sot_registry_entry_status() {
    local out
    out="$(sot_jq -r --arg n "$1" \
        'if (.agents | has($n)) then "present\t" + (.agents[$n].root // "") else "absent\t" end' \
        "$REGISTRY" 2>/dev/null)"
    if [ $? -ne 0 ] || [ -z "$out" ]; then
        printf 'error\t\n'
        return 0
    fi
    printf '%s\n' "$out"
}

# _sot_tier_claimable MODE ROOT STATUS HELD_ROOT — true if a tier whose
# registry status is STATUS/HELD_ROOT (from sot_registry_entry_status) can
# be claimed under MODE:
#   reclaim — unclaimed, OR already held by MY OWN root (today's
#             comm-join rejoin/reclaim behavior).
#   fresh   — unclaimed ONLY. comm-spawn creates a NEW agent; an existing
#             row for the resolved name — even one sharing my root — is
#             someone/something else's from spawn's point of view and must
#             never be silently absorbed (Codex review F3: this used to
#             erase a LIVE agent's workspace_id/status fields when spawning a
#             second time against the same project root).
_sot_tier_claimable() {
    local mode="$1" root="$2" status="$3" held="$4"
    [ "$status" = "absent" ] && return 0
    [ "$mode" = "reclaim" ] && [ "$held" = "$root" ] && return 0
    return 1
}

# sot_derive_handle MODE ROOT HOST — the derived-name algorithm (ADR 0028
# addendum; see docs/adr/0028-remote-comm-autoconnect.md). MODE is
# "reclaim" (comm-join) or "fresh" (comm-spawn) — see _sot_tier_claimable.
# Liveness is deliberately NOT consulted — a stale registry row still
# holds its claim until existing cleanup paths remove it; this keeps the
# rule one-dimensional (root comparison only) and prevents handle
# flip-flop between two projects depending on who happens to be running.
#
# ROOT MUST already be canonical (sot_canonical_path) — canonicalizing
# here would mean filesystem traversal INSIDE the registry lock (this runs
# from claim_derived_handle, under with_lock), which is worse under a
# dead/slow NFS mount and was flagged as redundant (Codex review F8:
# "canonicalize once before entering the lock").
#
# Escalates through three tiers, each checked with _sot_tier_claimable —
# tier 3 is NOT an unconditional overwrite (Codex review F6: it used to be
# claimed regardless of who held it, which needs no hash collision at all
# to hit — an explicit owner of the computed hash-qualified name was
# silently overwritten). If no tier is claimable, this FAILS LOUDLY
# (nonzero return, nothing on stdout, a clear reason on stderr) rather than
# inventing a fourth tier or overwriting anything — the caller must ask
# the user for an explicit --name.
#
# Every raw piece (repo basename, parentdir, host) is sanitized+clamped
# (sot_sanitize_component) BEFORE composing any candidate (Codex review
# F4), so a derived handle can never diverge from what workspace.create
# will accept, and no raw path text reaches a shell command unsanitized.
# HOST gets an extra step (Codex review PR #148 round 2, finding 7): if
# sanitizing/clamping CHANGES it at all — a long or characters-outside-
# charset hostname got truncated/rewritten — a short digest of the RAW
# host is appended. Without this, two DIFFERENT real hosts whose names
# happen to sanitize/truncate to the IDENTICAL string would, if they ever
# shared a root (an NFS-shared repo, exactly this cluster's own shape),
# alias onto one tier-1 "reclaim" — root matches, and the (now-identical)
# host component can no longer tell them apart. An untouched host (the
# overwhelmingly common case: short, already-valid hostnames) gets no
# suffix, so today's handles are unchanged. HOST_RAW_MAX=12 leaves room
# for "-" + a 6-hex digest without the host component threatening
# sot_sanitize_component's 20-char default budget the other components
# still use (12 + 1 + 6 = 19 worst case).
#
# On success, prints THREE lines: handle, qualifier, tier1 (qualifier empty
# at tier 1, "<parentdir>" at tier 2, "<hash6>" at tier 3; tier1 is ALWAYS
# the bare "<base>-<host>" handle, win or lose, so a caller can tell whether
# this call escalated AWAY from it — comm-join.sh's stranding guard needs
# exactly that). NEWLINE-separated, not tab-separated (Codex review round-1
# finding 1): tab is one of bash's IFS-WHITESPACE characters, so `IFS=$'\t'
# read -r a b c` still COLLAPSES adjacent tabs exactly like the default
# space/tab/newline splitting does — an empty qualifier field (the tier-1
# case, `tier1<TAB><TAB>tier1`) vanished entirely instead of reading back as
# "", shifting tier1's value into qualifier and leaving CLAIMED_TIER1 empty.
# Every ordinary tier-1 spawn then misread CLAIMED_QUALIFIER as non-empty
# and comm-spawn.sh synthesized a wrong "qualified" display label. Reading
# one line per `read -r VAR` sidesteps this: with a single destination
# variable there is no splitting to collapse — the whole line, empty or
# not, becomes that variable's value verbatim. A caller that only wants the
# handle: `NAME="$(sot_derive_handle reclaim "$ROOT" "$HOST" | head -n1)"`.
sot_derive_handle() {
    local mode="$1" root="$2" raw_host="$3"
    local base parent hash6 tier1 tier2 tier3 host host_digest
    local status1 held1 status2 held2 status3 held3 shown1 shown2 shown3

    case "$mode" in
        reclaim|fresh) : ;;
        *) echo "sot_derive_handle: invalid mode '$mode' (want reclaim or fresh)" >&2; return 1 ;;
    esac

    base="$(sot_sanitize_component "$(basename "$root")")"
    host="$(sot_sanitize_component "$raw_host" 12)"
    if [ "$host" != "$raw_host" ]; then
        host_digest="$(sot_hash6 "$raw_host")" || return 1
        host="${host}-${host_digest}"
    fi

    tier1="${base}-${host}"
    IFS=$'\t' read -r status1 held1 <<< "$(sot_registry_entry_status "$tier1")"
    if _sot_tier_claimable "$mode" "$root" "$status1" "$held1"; then
        printf '%s\n%s\n%s\n' "$tier1" "" "$tier1"
        return 0
    fi
    shown1="$held1"; [ -z "$shown1" ] && shown1="an unknown project"

    parent="$(sot_sanitize_component "$(basename "$(dirname "$root")")")"
    tier2="${base}-${parent}-${host}"
    IFS=$'\t' read -r status2 held2 <<< "$(sot_registry_entry_status "$tier2")"
    if _sot_tier_claimable "$mode" "$root" "$status2" "$held2"; then
        echo "comm: '@$tier1' is already held by $shown1 — joining as '@$tier2' instead" >&2
        printf '%s\n%s\n%s\n' "$tier2" "$parent" "$tier1"
        return 0
    fi
    shown2="$held2"; [ -z "$shown2" ] && shown2="an unknown project"

    hash6="$(sot_hash6 "$root")" || return 1
    tier3="${base}-${hash6}-${host}"
    IFS=$'\t' read -r status3 held3 <<< "$(sot_registry_entry_status "$tier3")"
    if _sot_tier_claimable "$mode" "$root" "$status3" "$held3"; then
        echo "comm: '@$tier1' (held by $shown1) and '@$tier2' (held by $shown2) are both taken — joining as '@$tier3' instead" >&2
        printf '%s\n%s\n%s\n' "$tier3" "$hash6" "$tier1"
        return 0
    fi
    shown3="$held3"; [ -z "$shown3" ] && shown3="an unknown project"
    echo "comm: every derived handle for this project is already taken — '@$tier1' (held by $shown1), '@$tier2' (held by $shown2), and '@$tier3' (held by $shown3). Pass --name to pick one explicitly." >&2
    return 1
}

# --- atomic derive + claim (closes the read-then-write race) -----------
# sot_derive_handle above only DECIDES a name by reading the registry; a
# caller that derives and only LATER locks to registry_put leaves a window
# between the two where a second, concurrent derived join (a DIFFERENT
# root, same basename+host) can observe that exact same "still free" state
# and also decide on tier 1 — whichever registry_put lands second then
# silently clobbers the first. That is the aliasing bug this whole feature
# exists to close, so it must not survive as a race window. The window is
# not theoretical: comm-spawn.sh is driven programmatically for bulk
# workspace bring-up, joining many sessions back-to-back.
#
# claim_derived_handle MODE ROOT HOST OBJ_JSON — derive AND registry_put
# the result as ONE critical section under the registry lock, so no other
# claim can observe registry state in between. Sets globals CLAIMED_NAME,
# CLAIMED_QUALIFIER, and CLAIMED_TIER1 (mirrors sot_derive_handle's three
# outputs) for the caller to read after this returns; all three cleared to
# "" first, so a failure never leaves a stale value from a PREVIOUS
# successful call for a careless caller to read. CLAIMED_TIER1 is what
# comm-join.sh's stranding guard compares CLAIMED_NAME against — a mismatch
# means this call escalated away from the bare handle. Both comm-join.sh
# (MODE reclaim) and
# comm-spawn.sh (MODE fresh, the provisional row) route a derived name
# through this — one shared locked claim path, not two copies of "derive,
# then lock to write" that could each get this wrong.
#
# On failure (sot_derive_handle exhausted all three tiers, or refused to
# run at all — e.g. no hash function available), returns nonzero and
# writes NOTHING to the registry (Codex review F7): derivation failure is
# an error the caller must surface, never a claim of "".
#
# _sot_claim_derived_handle is the with_lock callee; it must NEVER be
# invoked directly, and never as `X=$(with_lock ...)`. with_lock runs its
# command directly ("$@", no subshell) specifically so a callee's global
# assignment survives past its return — capturing it via command
# substitution would fork a subshell and lose CLAIMED_NAME exactly the way
# the test harness's own next_self_file() lost its counter (see
# comm/core/tests/test-join-disambiguation.sh) — the same lesson, twice.
_sot_claim_derived_handle() {  # MODE ROOT HOST OBJ_JSON — call only via with_lock
    local mode="$1" root="$2" host="$3" obj="$4" line
    CLAIMED_NAME=""
    CLAIMED_QUALIFIER=""
    CLAIMED_TIER1=""
    line="$(sot_derive_handle "$mode" "$root" "$host")" || return 1
    # Three sequential single-var reads off the SAME herestring fd (Codex
    # review round-1 finding 1) — each `read -r VAR` consumes one line and
    # advances the shared position, and with only one destination variable
    # there is no IFS splitting to collapse an empty middle field the way
    # the old tab-delimited `read -r a b c` did. `{ …; } <<< "$line"` (not
    # `( … )`) so the reads run in THIS shell and CLAIMED_* stay set for the
    # caller.
    {
        IFS= read -r CLAIMED_NAME
        IFS= read -r CLAIMED_QUALIFIER
        IFS= read -r CLAIMED_TIER1
    } <<< "$line"
    if [ -z "$CLAIMED_NAME" ]; then
        echo "claim_derived_handle: derivation returned no name — refusing to claim an empty handle" >&2
        return 1
    fi
    registry_put "$CLAIMED_NAME" "$obj"
}
claim_derived_handle() {  # MODE ROOT HOST OBJ_JSON
    with_lock _sot_claim_derived_handle "$1" "$2" "$3" "$4"
}


# sot_json_escape STR — STR as one JSON-quoted string, surrounding quotes
# included (S19: `sot_hello_frame`'s hand-rolled `"%s"` interpolation
# produced invalid JSON for a declared value containing a quote or
# backslash — reproduced against a quoted SOT_SELF_HOST override). `jq
# -Rs .` reads STR as raw text (R), slurps the whole input into one
# string even across embedded newlines (s), and prints it back as a
# single JSON string literal — the general escaping jq's own JSON writer
# already gets right, never a hand-rolled sed/printf substitution.
sot_json_escape() {
    printf '%s' "$1" | jq -Rs .
}

# sot_host — this shell's DECLARED host name for the wire only (ADR 0046
# decision 1, manager review S1/S2): the ONE resolver matching
# sot_log::state_dir::host_name() on the Rust side exactly — `$SOT_SELF_HOST`
# verbatim if set and non-empty (a NEW variable: `SOT_HOST` already means
# the SSH target a remote frontend dials, `scripts/launch-sot.ps1`/
# `launch-sot.sh` — reusing it here would silently rename a frontend's
# declared identity to whatever it dials), else the first `.`-label of
# `hostname -s`, lowercased. Feeds ONLY `sot_hello_frame`'s wire `host`
# field and display/logs — never an address or on-disk namespace: no
# on-disk namespace changes this sprint (S1), so comm-context.sh's own
# `HOST` (the self-file key, handle derivation) does NOT call this;
# `hostname -s` there stays completely independent, exactly as on main.
# Fails loudly (S19) rather than printing empty when neither source
# resolves — a hello with an empty declared host is worse than a hello
# that never sent one at all.
sot_host() {
    if [ -n "${SOT_SELF_HOST:-}" ]; then
        printf '%s\n' "$SOT_SELF_HOST"
        return 0
    fi
    local raw
    if ! raw="$(hostname -s 2>/dev/null || hostname 2>/dev/null)"; then
        echo "sot_host: no SOT_SELF_HOST override and hostname failed -- cannot declare an identity" >&2
        return 1
    fi
    raw="${raw%%.*}"
    # Trim whitespace the same way Rust's host_name() does (`.trim()`
    # after taking the first label) — manager review round 2: the two
    # implementations must apply the SAME rule, not two independently
    # coded near-matches.
    raw="${raw#"${raw%%[![:space:]]*}"}"
    raw="${raw%"${raw##*[![:space:]]}"}"
    if [ -z "$raw" ]; then
        echo "sot_host: no SOT_SELF_HOST override and hostname returned no usable label" >&2
        return 1
    fi
    printf '%s\n' "$raw" | tr '[:upper:]' '[:lower:]'
}

# sot_hello_frame [ROLE] — the ONE hello frame every comm script sends
# before any other op (ADR 0046 decision 1: a connection declares
# `{host, role, name}` once, and the daemon binds it — never recomputed
# downstream). Replaces six pasted copies of this exact literal frame
# (comm-relay.sh, comm-despawn.sh, comm-spawn.sh, sot-fe,
# and the join-disambiguation test's own fixture) that predated `host`/
# `role`/`name` entirely and so declared nothing about the sender.
#
# ROLE overrides the default inference; no comm script passes one now.
# Every caller lets this infer "agent" ($SOT_WORKSPACE set — a session running
# inside a daemon-owned workspace) or "cli" (a bare shell invocation, the
# common case for comm-relay.sh/comm-despawn.sh/comm-spawn.sh/sot-fe).
#
# `host`: `sot_host` — works whether or not the caller ran comm-context.sh
# first (comm-despawn.sh doesn't). `name`: `$NAME` when comm-context.sh
# resolved one (empty for a not-yet-joined shell — an anonymous hello,
# exactly today's behavior).
sot_hello_frame() {
    local role="${1:-}"
    if [ -z "$role" ]; then
        if [ -n "${SOT_WORKSPACE:-}" ]; then role="agent"; else role="cli"; fi
    fi
    local tok host
    tok="${SOT_TOKEN:-$(cat "${XDG_CONFIG_HOME:-$HOME/.config}/sot/token" 2>/dev/null || true)}"
    host="$(sot_host)" || return 1
    # JSON-escape every interpolated string (S19, Codex finding S19): an
    # unescaped quote or backslash in a declared host/name/token would
    # otherwise produce invalid JSON the daemon's own parser rejects.
    #
    # The `"protocol":2` literal below is sotd's WIRE protocol
    # (`sot_protocol::PROTOCOL_VERSION`, rust/protocol/src/lib.rs) — not
    # this file's own `$PROTOCOL_VERSION` (registry.json schema version,
    # unrelated). It went stale against a live daemon when the wire
    # protocol bumped 1 -> 2 and nothing here asked the binary; bump it by
    # hand alongside every future `PROTOCOL_VERSION` change until this
    # reads `sotd --version`'s trailing `protocol <N>` instead (see that
    # function's doc comment).
    printf '{"v":1,"id":1,"kind":"req","op":"hello","payload":{"client_id":"sot-comm","last_seen_revision":0,"protocol":2,"app_version":"comm","token":%s,"host":%s,"role":%s,"name":%s}}\n' \
        "$(sot_json_escape "$tok")" "$(sot_json_escape "$host")" "$(sot_json_escape "$role")" "$(sot_json_escape "${NAME:-}")"
}

# sot_oneshot_request FRAME OP — one-shot request/response on a fresh daemon
# connection: send hello + FRAME, return (stdout) the first COMPLETE line
# whose op matches OP. Hardened after a live intermittent failure
# (2026-08-22, a peer session's targeted fe.command) and a codex review of
# the first hardening round:
#   - the WRITER lingers for the whole read window (some nc variants quit on
#     stdin EOF, racing the reply — the original bug);
#   - nc drains into a TEMP FILE we poll for the matching op line (fresh
#     connections receive ALL broadcast evt traffic — multi-MB repl frames
#     queued ahead of the res just stream past);
#   - a match is accepted only when jq parses the line (an op match can be
#     an UNTERMINATED line still being appended — op precedes payload);
#   - teardown kills the KNOWN pid only (never `kill %%`/`wait <member>`:
#     the jobspec can resolve to an unrelated background job in a caller
#     that backgrounds other work, and waiting any pipeline member waits
#     the whole job — measured as a linger-long floor per call). The
#     writer's sleep is left to die alone — bounded by the window, writes
#     nothing, holds nothing.
# Read window: SOT_SEND_TIMEOUT, else the caller's SEND_TIMEOUT (sot-fe's
# repl paths set --timeout up to minutes — the window MUST honor it), else
# 10s. Uses ENDPOINT (unix:/path, ssh:target[/host] via sot_ssh_bridge, or
# pipe:name — the last one a Windows-only named-pipe transport, see the
# pipe: arm below) from the
# caller's scope.
# _sot_oneshot_sender HELLO FRAME TIMEOUT_S PIDFILE — the write side of a
# one-shot request: hello, the frame, then `exec sleep` so the subshell's pid
# (written to PIDFILE first) is the sleep itself and one kill ends it. The
# hello is BUILT BY THE CALLER before the pipeline starts: building it here
# (hostname + four jq spawns, about a second on a Windows box) meant the
# reader had already started on an empty pipe, and PowerShell's
# Console.In.ReadLine never wakes for data that arrives after it began --
# every named-pipe one-shot timed out with no hello logged (2026-09-18).
_sot_oneshot_sender() {
    printf '%s\n' "$BASHPID" > "$4"
    printf '%s\n%s\n' "$1" "$2"
    exec sleep "$3"
}

sot_oneshot_request() {
    local frame="$1" op="$2"
    local timeout_s="${SOT_SEND_TIMEOUT:-${SEND_TIMEOUT:-10}}"
    local tmp ncpid line="" deadline hello
    hello="$(sot_hello_frame)"
    tmp="$(mktemp "${XDG_RUNTIME_DIR:-/tmp}/sot-oneshot-XXXXXX")" || return 1
    case "$ENDPOINT" in
        unix:*)
            command -v nc >/dev/null 2>&1 || {
                echo "ERROR: nc not found and endpoint is a unix socket (needs nc -U)" >&2
                rm -f "${tmp:?}"; return 1; }
            # The sender holds the write side open with a sleep (a half-close
            # via `nc -q` made stub listeners hang up early). It is `exec`'d so
            # the recorded pid IS the sleep, killed the moment the reply
            # matches, and its stderr is detached: a sender that outlived the
            # reply used to hold the CALLER's stderr for the whole timeout, so
            # any pipe or harness reading the caller waited that long
            # (2026-09-17, two boxes).
            _sot_oneshot_sender "$hello" "$frame" "$timeout_s" "$tmp.snd" 2>/dev/null \
                | timeout "$timeout_s" nc -U "${ENDPOINT#unix:}" > "$tmp" 2>/dev/null &
            ncpid=$!
            ;;
        ssh:*)
            local rest="${ENDPOINT#ssh:}" target sshhost
            case "$rest" in
                */*) target="${rest%%/*}"; sshhost="${rest#*/}" ;;
                *) target="$rest"; sshhost="" ;;
            esac
            # Own scratch file, not /dev/null (BLOCKER 1's loud-failure
            # requirement): a dying ssh child's own stderr used to vanish
            # here, so a failure and a cold-but-reachable daemon looked
            # identical. Read back below, once the wait loop ends with no
            # reply, and folded into a diagnostic on THIS function's own
            # stderr -- never into $line, which stays the reply or nothing.
            _sot_oneshot_sender "$hello" "$frame" "$timeout_s" "$tmp.snd" 2>/dev/null \
                | sot_ssh_bridge "$target" "$sshhost" "$timeout_s" > "$tmp" 2>"$tmp.err" &
            ncpid=$!
            ;;
        pipe:*)
            # ADR 0042 amendment (2026-09-07): a Windows box's LOCAL daemon
            # only listens on a named pipe, which git-bash cannot open
            # itself — comm-pipe-request.ps1 is the transport, invoked
            # exactly the way nc is above (hello + frame piped to its
            # stdin, never argv). Accepts either the full \\.\pipe\<name>
            # form sot_daemon_endpoint prints or a bare pipe:<name> — both
            # reduce to the trailing NAME (NamedPipeClientStream never
            # takes the \\.\pipe\ prefix itself).
            local pipename="${ENDPOINT#pipe:}"
            pipename="${pipename##*\\}"
            command -v powershell.exe >/dev/null 2>&1 || {
                echo "ERROR: powershell.exe not found and endpoint is a named pipe (pipe: needs PowerShell)" >&2
                rm -f "${tmp:?}"; return 1; }
            local ps1="${SCRIPT_DIR:-.}/comm-pipe-request.ps1"
            [ -f "$ps1" ] || {
                echo "ERROR: comm-pipe-request.ps1 not found next to the comm scripts (looked in ${SCRIPT_DIR:-.})" >&2
                rm -f "${tmp:?}"; return 1; }
            _sot_oneshot_sender "$hello" "$frame" "$timeout_s" "$tmp.snd" 2>/dev/null \
                | timeout "$timeout_s" powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass \
                    -File "$ps1" -PipeName "$pipename" -Mode Oneshot -Op "$op" -TimeoutSec "$timeout_s" \
                    > "$tmp" 2>/dev/null &
            ncpid=$!
            ;;
        *) rm -f "${tmp:?}"; return 1 ;;
    esac
    # Accept only a COMPLETE res line: op precedes payload on the wire, so a
    # grep hit can be a line nc is still appending. jq gates acceptance when
    # available; without jq (minimal envs) fall back to requiring that the
    # file's last byte is a newline OR more bytes follow the match.
    _sot_line_ok() {
        if command -v jq >/dev/null 2>&1; then
            printf '%s' "$1" | jq -e . >/dev/null 2>&1
        else
            case "$1" in *"}"* ) return 0 ;; * ) return 1 ;; esac
        fi
    }
    deadline=$(( $(date +%s) + timeout_s ))
    while [ "$(date +%s)" -le "$deadline" ]; do
        line="$(grep -m1 "\"op\":\"$op\"" "$tmp" 2>/dev/null || true)"
        if [ -n "$line" ] && _sot_line_ok "$line"; then
            break
        fi
        line=""
        kill -0 "$ncpid" 2>/dev/null || {
            # transport exited — one final scan for a reply that landed last
            line="$(grep -m1 "\"op\":\"$op\"" "$tmp" 2>/dev/null || true)"
            _sot_line_ok "$line" || line=""
            break; }
        sleep 0.1
    done
    kill "$ncpid" 2>/dev/null || true
    [ -r "$tmp.snd" ] && kill "$(cat "$tmp.snd" 2>/dev/null)" 2>/dev/null
    # A ssh: bridge that exited or timed out with no reply: its own stderr
    # (captured above instead of discarded) names the reason -- printed
    # here, on THIS function's stderr, never folded into $line.
    if [ -z "$line" ] && [ -s "$tmp.err" ]; then
        printf 'sot_oneshot_request: %s: %s\n' "${target:-ssh bridge}" "$(tr '\n' ' ' < "$tmp.err")" >&2
    fi
    rm -f "${tmp:?}" "${tmp:?}.snd" "${tmp:?}.err"
    [ -n "$line" ] && printf '%s\n' "$line"
}

# fmt_age SECONDS — compact relative age ("just now"/"2m ago"/"1h ago"/"3d
# ago"). Moved here from comm-list.sh (session-listing brief) so the same
# ageing rule serves every state-nav printer instead of two copies drifting:
# comm-list.sh's own agent rows and sot-fe's `version` command, which now
# prints the same "[state] summary · age" shape for a declared `fe.sessions`
# row (ADR: `status_at` is the honesty valve — an hour-old stamp prints
# "1h ago" wherever it's shown, local row or declared one alike).
fmt_age() {
    local s="$1"
    if   [ "$s" -lt 60 ];    then echo "just now"
    elif [ "$s" -lt 3600 ];  then echo "$((s / 60))m ago"
    elif [ "$s" -lt 86400 ]; then echo "$((s / 3600))h ago"
    else                          echo "$((s / 86400))d ago"
    fi
}
