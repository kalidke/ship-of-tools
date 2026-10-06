# comm-lib-client.sh: the shell client of the daemon's wire: endpoints, ssh bridge, hello frame, one-shot request, pty input.
# Sourced by comm-lib.sh; defines functions and globals only.

# _sot_windows_local_pipe — the LOCAL daemon's named pipe, resolved and
# proven live (ADR 0042 amendment, decision 5, corrected 2026-09-07): asks
# the daemon binary itself for its pipe path — the SAME query
# scripts/sot-local-daemon.ps1 makes (`sotd.exe session-socket-path local`)
# — so this can never derive a different name than the one the launcher's
# own daemon binds. Then proves it through `sot_dial` (`sotd stdio-bridge
# --endpoint`) with empty input, the probe that script's Test-SotPipeOpen also makes: a pipe
# NAME can persist under \\.\pipe\ while a dead client still holds a handle
# to it, so a resolvable name alone is not evidence anything is listening,
# and the bridge connects only to a pipe this OS account serves (ADR 0049,
# User isolation). Prints the \\.\pipe\... path and returns 0 only when both
# checks pass; nothing printed, nonzero return otherwise. Windows-only —
# callers gate with _sot_is_windows first.
# _sot_windows_sotd_exe — the sotd executable on a Windows box: SOTD_BIN when
# set, else the install path. Prints it; 1 when none exists. Never a running
# process's binary: a process list names every account's sotd.exe.
_sot_windows_sotd_exe() {
    local daemon_exe="${SOTD_BIN:-}"
    if [ -z "$daemon_exe" ] || [ ! -f "$daemon_exe" ]; then
        daemon_exe="${LOCALAPPDATA:-}/sot/bin/sotd.exe"
    fi
    [ -f "$daemon_exe" ] || return 1
    printf '%s\n' "$daemon_exe"
}
_sot_windows_local_pipe() {
    local daemon_exe
    daemon_exe="$(_sot_windows_sotd_exe)" || return 1
    local raw
    raw="$("$daemon_exe" session-socket-path local 2>/dev/null | head -n1 | tr -d '\r')"
    case "$raw" in
        '\\'*'pipe'*) : ;;
        *) return 1 ;;
    esac
    sot_dial "pipe:$raw" </dev/null >/dev/null 2>&1 || return 1
    printf '%s\n' "$raw"
}

# _sot_is_plain_host_name — the shell twin of `sot_protocol`'s Rust grammar
# (`topology::endpoint::is_plain_host_name` / `ssh_bridge::SshRecipe::new`): first
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

# _sot_live_sotd_exes — the binary of each process of THIS account `pgrep -u <uid> -af sotd` lists whose
# /proc/<pid>/exe is named `sotd` or `sotd.exe`, one per line, in pgrep's order;
# nothing on Windows (no pgrep on a stock git-bash PATH) or where /proc cannot
# be read. pgrep matches any command line that mentions sotd (`tail -f
# .../sotd.log`, `gdb sotd`, `watch ...`), and both callers below would run or
# return such a process's binary: only a binary named sotd is ever listed.
# Another account's sotd is never this account's daemon (ADR 0049, User isolation).
_sot_live_sotd_exes() {
    _sot_is_windows && return 0
    local line pid exe
    while IFS= read -r line; do
        pid="${line%% *}"
        case "$pid" in ''|*[!0-9]*) continue ;; esac
        exe="$(readlink "/proc/$pid/exe" 2>/dev/null)" || continue
        [ -x "$exe" ] || continue
        case "${exe##*/}" in sotd|sotd.exe) printf '%s\n' "$exe" ;; esac
    done < <(pgrep -u "$(id -u)" -af 'sotd' 2>/dev/null || true)
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
    # LAST candidate (BLOCKER 3): a live sotd's own binary, for a source-built
    # box with no install. It is never the local daemon's socket, only a binary
    # asked for the planned relay endpoint (main's ruling, pinned at
    # join_disambiguation/pipe_endpoint.sh).
    IFS= read -r candidate < <(_sot_live_sotd_exes) || candidate=""
    [ -n "$candidate" ] && { printf '%s\n' "$candidate"; return 0; }
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
# hub's own relay unit runs (`rust/protocol/src/topology/mod.rs`) and C3
# spawns identically from Rust (`rust/protocol/src/topology/ssh_bridge.rs`) -- kept
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

# sot_dial ENDPOINT [TIMEOUT_SECS] — stdin to the daemon at ENDPOINT, its replies to stdout (ADR 0049, User isolation):
# every `unix:` or `pipe:` connection this library opens is `sot_dial`'s. A `unix:` or `pipe:` endpoint is opened by
# `sotd stdio-bridge --endpoint`, whose connect is `connect_own`: a socket only in a folder private to this OS account,
# a pipe only when this account serves it, else exit 1 and one stderr line saying why. A bare `pipe:<name>` is written
# `pipe:\\.\pipe\<name>`. An `ssh:` endpoint is `sot_ssh_bridge`, whose far end is that box's own bridge. The bridge
# closes the connection when its input ends, so a caller keeps stdin open until it has read what it waits for. The bound
# is a parameter, as `sot_ssh_bridge`'s is: `timeout` cannot run a function.
sot_dial() {
    local ep="$1" secs="${2:-}" bin="" rest name
    case "$ep" in
        unix:*|pipe:*)
            if _sot_is_windows; then bin="$(_sot_windows_sotd_exe)" || bin=""; else bin="$(_sot_sotd_bin)" || bin=""; fi
            [ -n "$bin" ] || { echo "sot_dial: no sotd to open $ep with" >&2; return 1; }
            case "$ep" in
                pipe:*) name="${ep#pipe:}"; ep="pipe:\\\\.\\pipe\\${name##*\\}" ;;
            esac
            if [ -n "$secs" ]; then
                MSYS2_ARG_CONV_EXCL='*' timeout "$secs" "$bin" stdio-bridge --endpoint "$ep"
            else
                MSYS2_ARG_CONV_EXCL='*' "$bin" stdio-bridge --endpoint "$ep"
            fi
            ;;
        ssh:*)
            rest="${ep#ssh:}"
            case "$rest" in
                */*) sot_ssh_bridge "${rest%%/*}" "${rest#*/}" "$secs" ;;
                *) sot_ssh_bridge "$rest" "" "$secs" ;;
            esac
            ;;
        *) echo "sot_dial: not an endpoint this version dials: $ep" >&2; return 1 ;;
    esac
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
# `case_windows_relay_endpoint_is_never_the_pipe_the_shell_probed` in
# `comm/tests/join_disambiguation/pipe_endpoint.sh` -- a failed
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

    while IFS= read -r bin; do
        _try_sotd_socket_bin "$bin" && return 0
    done < <(_sot_live_sotd_exes)

    return 1
}

# --- live delivery into a workspace row (comm-bootstrap.sh, comm-probe.sh) ---
#
# sot_pty_input WORKSPACE_ID DATA_B64 — one `pty.input` request (enter:true)
# to the daemon at ENDPOINT (caller's scope); prints the response line.
# The daemon types the text into the row's capsule and appends Enter, and
# reports `enter` (sent, not_sent or unknown). This is the only live-delivery path: a message
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
# amendment) so sot-fe and comm-lib callers share the one implementation
# instead of two frame-builders drifting apart.
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

# _sot_windows_sid — the process token's user SID under git-bash, the value `own_account_id()` gives Rust on
# Windows. Three probes in turn, the first that prints a SID wins: `whoami /user` called directly (MSYS must not
# rewrite its `/user` style arguments as paths, hence MSYS2_ARG_CONV_EXCL), the same through `cmd`, and PowerShell's
# WindowsIdentity. When none does, what each printed goes to stderr, so a runner or box where the account really
# cannot be read says why.
_sot_windows_sid() {
    local probe out sid diag=""
    for probe in whoami cmd powershell; do
        case "$probe" in
            whoami) out="$(MSYS2_ARG_CONV_EXCL='*' whoami /user /fo csv /nh 2>&1)" ;;
            cmd) out="$(cmd //c "whoami /user /fo csv /nh" 2>&1)" ;;
            powershell) out="$(powershell.exe -NoProfile -NonInteractive -Command '[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value' 2>&1 </dev/null)" ;;
        esac
        out="$(printf '%s' "$out" | tr -d '\r')"
        sid="$(printf '%s\n' "$out" | grep -oE 'S-1-[0-9]+(-[0-9]+)+' | tail -n 1)"
        if [ -n "$sid" ]; then
            printf '%s\n' "$sid"
            return 0
        fi
        diag="$diag [$probe: $(printf '%s' "$out" | tr '\n' ' ' | cut -c1-120)]"
    done
    echo "_sot_windows_sid: no probe printed this process's user SID:$diag" >&2
    return 1
}

# _sot_os_user — this shell's OS account as the operating system issued it, for the hello's `os_user` (ADR 0049
# `## User isolation`; the same value `sot_log::identity::os_account::own_account_id()` gives the Rust builders):
# `uid:<euid>` on Unix, the process token's user SID on Windows (`_sot_windows_sid`). Cached in
# `_SOT_OS_USER`. Empty means unreadable: it fails, and no hello is sent. Never a name from the environment, and no
# sotd call (an older installed sotd would break every send).
_sot_os_user() {
    if [ -z "${_SOT_OS_USER:-}" ]; then
        if _sot_is_windows; then
            _SOT_OS_USER="$(_sot_windows_sid)"
        else
            _SOT_OS_USER="uid:$(id -u 2>/dev/null)"
            [ "$_SOT_OS_USER" = "uid:" ] && _SOT_OS_USER=""
        fi
    fi
    if [ -z "$_SOT_OS_USER" ]; then
        echo "_sot_os_user: this process's OS account is unreadable -- cannot declare an identity" >&2
        return 1
    fi
    printf '%s\n' "$_SOT_OS_USER"
}

# sot_hello_frame — the ONE hello frame every comm script sends
# before any other op (ADR 0046 decision 1: a connection declares
# `{host, role, name}` once, and the daemon binds it — never recomputed
# downstream). Replaces six pasted copies of this exact literal frame
# (comm-relay.sh, comm-despawn.sh, comm-spawn.sh, sot-fe,
# and the join-disambiguation test's own fixture) that predated `host`/
# `role`/`name` entirely and so declared nothing about the sender.
#
# The role is inferred: "agent" ($SOT_WORKSPACE set — a session running
# inside a daemon-owned workspace) or "cli" (a bare shell invocation, the
# common case for comm-relay.sh/comm-despawn.sh/comm-spawn.sh/sot-fe).
#
# `host`: `sot_host` — works whether or not the caller ran comm-context.sh
# first (comm-despawn.sh doesn't). `name`: `$NAME` when comm-context.sh
# resolved one (empty for a not-yet-joined shell — an anonymous hello,
# exactly today's behavior).
sot_hello_frame() {
    local role
    if [ -n "${SOT_WORKSPACE:-}" ]; then role="agent"; else role="cli"; fi
    local host os_user
    host="$(sot_host)" || return 1
    _sot_os_user >/dev/null || return 1
    os_user="$_SOT_OS_USER"
    # JSON-escape every interpolated string (S19, Codex finding S19): an
    # unescaped quote or backslash in a declared host/name would
    # otherwise produce invalid JSON the daemon's own parser rejects.
    #
    # The `"protocol":3` literal below is sotd's WIRE protocol
    # (`sot_protocol::PROTOCOL_VERSION`, rust/protocol/src/lib.rs) — not
    # this file's own `$PROTOCOL_VERSION` (registry.json schema version,
    # unrelated). It is bumped by hand with every `PROTOCOL_VERSION`
    # change; sot-protocol's `comm_lib_hello_speaks_this_protocol` test
    # fails until the two match.
    printf '{"v":1,"id":1,"kind":"req","op":"hello","payload":{"client_id":"sot-comm","last_seen_revision":0,"protocol":3,"app_version":"comm","host":%s,"os_user":%s,"role":%s,"name":%s}}\n' \
        "$(sot_json_escape "$host")" "$(sot_json_escape "$os_user")" "$(sot_json_escape "$role")" "$(sot_json_escape "${NAME:-}")"
}

# sot_oneshot_request FRAME OP — one-shot request/response on a fresh daemon
# connection: send hello + FRAME, return (stdout) the first COMPLETE line
# whose op matches OP. Hardened after a live intermittent failure
# (2026-08-22, a peer session's targeted fe.command) and a codex review of
# the first hardening round:
#   - the WRITER lingers for the whole read window (the bridge closes the connection when its
#     input ends; some nc variants did too, racing the reply — the original bug);
#   - the transport drains into a TEMP FILE we poll for the matching op line (fresh
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
# 10s. Uses ENDPOINT (unix:/path, pipe:name or ssh:target[/host]) from the caller's scope,
# through sot_dial.
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

# _sot_hello_refusal FILE [any] — the daemon's message when the hello's reply in FILE (the first line whose op is hello)
# carries an error: the daemon refused this client (an older protocol, a second OS account on the host) (ADR 0049
# `## User isolation`). Nothing when the hello was accepted or has not been answered yet. With no second argument,
# nothing for a `protocol_mismatch` either: a daemon of this release closes after any refusal, but an older one refuses
# only the protocol and goes on to answer the request, so that refusal decides nothing until the connection ends with
# no reply (`any`).
_sot_hello_refusal() {
    local reply got
    reply="$(grep -m1 '"op":"hello"' "$1" 2>/dev/null || true)"
    [ -n "$reply" ] || return 0
    if command -v jq >/dev/null 2>&1; then
        got="$(printf '%s' "$reply" | jq -r 'if (.payload.error? // null) != null then ((.payload.code? // "") + "\t" + (.payload.error | tostring)) else empty end' 2>/dev/null)"
        [ -n "$got" ] || return 0
        [ -n "${2:-}" ] || [ "${got%%$'\t'*}" != protocol_mismatch ] || return 0
        printf '%s\n' "${got#*$'\t'}"
    else
        case "$reply" in
            *'"protocol_mismatch"'*) [ -z "${2:-}" ] || printf 'refused\n' ;;
            *'"error"'*) printf 'refused\n' ;;
        esac
    fi
}

sot_oneshot_request() {
    local frame="$1" op="$2"
    local timeout_s="${SOT_SEND_TIMEOUT:-${SEND_TIMEOUT:-10}}"
    local tmp ncpid line="" deadline hello refused=""
    tmp="$(mktemp "${XDG_RUNTIME_DIR:-/tmp}/sot-oneshot-XXXXXX")" || return 1
    # A process that cannot name its host or OS account sends nothing; the builder says why, and the caller is told.
    hello="$(sot_hello_frame 2>"$tmp.he")" || {
        printf 'sot_oneshot_request: no hello: %s\n' "$(tr '\n' ' ' < "$tmp.he" | sed 's/ $//')" >&2
        rm -f "${tmp:?}" "${tmp:?}.he"; return 1; }
    rm -f "${tmp:?}.he"
    # Every scheme goes through sot_dial (ADR 0049, User isolation). The sender
    # holds the write side open with a sleep, because the bridge closes the
    # connection when its input ends. It is `exec`'d so the recorded pid IS the
    # sleep, killed the moment the reply matches, and its stderr is detached: a
    # sender that outlived the reply used to hold the CALLER's stderr for the
    # whole timeout, so any pipe or harness reading the caller waited that long
    # (2026-09-17, two boxes). The transport's own stderr goes to its scratch
    # file (BLOCKER 1's loud-failure requirement: a refusal or a dying ssh child
    # used to vanish, so a failure and a cold-but-reachable daemon looked
    # identical), read back below once the wait loop ends with no reply.
    local target="$ENDPOINT"
    case "$ENDPOINT" in ssh:*) target="${ENDPOINT#ssh:}"; target="${target%%/*}" ;; esac
    _sot_oneshot_sender "$hello" "$frame" "$timeout_s" "$tmp.snd" 2>/dev/null \
        | sot_dial "$ENDPOINT" "$timeout_s" > "$tmp" 2>"$tmp.err" &
    ncpid=$!
    # Accept only a COMPLETE res line: op precedes payload on the wire, so a
    # grep hit can be a line the transport is still appending. jq gates acceptance when
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
        # A hello refused for anything but the protocol ends the connection: no reply is coming, so say why at once.
        refused="$(_sot_hello_refusal "$tmp")"
        [ -n "$refused" ] && break
        kill -0 "$ncpid" 2>/dev/null || {
            # transport exited — one final scan for a reply that landed last
            line="$(grep -m1 "\"op\":\"$op\"" "$tmp" 2>/dev/null || true)"
            _sot_line_ok "$line" || line=""
            break; }
        sleep 0.1
    done
    kill "$ncpid" 2>/dev/null || true
    # No reply by the end of the connection or the window: a hello refused for the protocol is named now.
    [ -n "$line" ] || [ -n "$refused" ] || refused="$(_sot_hello_refusal "$tmp" any)"
    [ -r "$tmp.snd" ] && kill "$(cat "$tmp.snd" 2>/dev/null)" 2>/dev/null
    # A transport that exited or timed out with no reply: its own stderr
    # (captured above instead of discarded) names the reason -- printed
    # here, on THIS function's stderr, never folded into $line.
    if [ -z "$line" ] && [ -s "$tmp.err" ]; then
        printf 'sot_oneshot_request: %s: %s\n' "$target" "$(tr '\n' ' ' < "$tmp.err")" >&2
    fi
    rm -f "${tmp:?}" "${tmp:?}.snd" "${tmp:?}.err"
    if [ -n "$refused" ]; then
        printf 'sot_oneshot_request: hello refused: %s\n' "$refused" >&2
        return 1
    fi
    [ -n "$line" ] && printf '%s\n' "$line"
}

# sot_daemon_path PATH — PATH in the spelling of the daemon at ENDPOINT (the
# caller's scope, as sot_oneshot_request reads it). A pipe: daemon is a native
# Windows process: it reads git-bash's /c/Users/... as a folder on the current
# drive and answers no_such_path, and MSYS converts paths only in a native
# program's argv, never inside a JSON payload, so the path goes through
# `cygpath -m` (C:/Users/...). Every other daemon reads the path as written.
# Keyed on the endpoint, never on this shell: a Windows caller reaching a Unix
# daemon over ssh: sends the path unchanged. Prints nothing and returns 1 when
# the conversion fails.
sot_daemon_path() {
    local out
    case "$ENDPOINT" in
        pipe:*)
            out="$(cygpath -m "$1" 2>/dev/null)" && [ -n "$out" ] || return 1
            printf '%s\n' "$out"
            ;;
        *) printf '%s\n' "$1" ;;
    esac
}
