# comm-lib-client.sh: the shell client of the daemon's wire: endpoints, ssh bridge, hello frame, one-shot request, pty input.
# Sourced by comm-lib.sh; defines functions and globals only.

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
                *comm-relay*|*comm-spawn*|*comm-despawn*|*comm-poll*|*sot-fe*|*sot-nav*)
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
