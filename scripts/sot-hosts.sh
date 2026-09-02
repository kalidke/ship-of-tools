#!/usr/bin/env bash
# sot-hosts.sh -- shared .sot/hosts.toml reader + tunnel-plan builder
# (ADR 0042 L2b). Sourced by launch-sot.sh (which SSH-ensures and opens the
# tunnels a plan names) and scripts/tests/test-tunnel-plan.sh.
#
# Same simple regex-shaped format hosts.rs / sot-hosts.ps1 parse -- see
# hosts.rs's own doc comment for the format and the "why not a TOML
# library" rationale. Plain POSIX awk only (no gawk 3-arg match(), no bash4
# associative arrays) so this runs under macOS's stock /bin/bash and awk,
# matching install.sh's own hosts.toml awk helpers.
#
# sot_tunnel_plan is PURE (no ssh, no side effects) so it's testable against
# a fixture hosts.toml without touching the network -- the actual
# ssh-ensure-and-open work stays a function in launch-sot.sh
# (sot_ensure_remote_host), which is not pure by nature.

# sot_hosts_default_host <file>
# Prints the top-level default_host value (the LAST one seen, only above
# the first table header -- matching hosts.rs/sot-hosts.ps1, which both
# keep overwriting rather than stopping at the first), or nothing if
# absent/no file. Used by launch-sot.sh to identify the primary host by
# its hosts.toml KEY (codex follow-up: identity is the key, not the ssh
# alias -- see sot_tunnel_plan below), independent of $SOT_HOST/$PORT
# which stay env-var driven for the primary tunnel's own behavior.
sot_hosts_default_host() {
    local file="$1"
    [ -f "$file" ] || return 0
    awk '
        BEGIN { pro = 1; result = "" }
        {
            s = $0
            gsub(/^[ \t]+|[ \t]+$/, "", s)
            if (s == "" || substr(s, 1, 1) == "#") next
            if (substr(s, 1, 1) == "[") { pro = 0; next }
            if (pro) {
                eq = index(s, "=")
                if (eq > 0) {
                    key = substr(s, 1, eq - 1); val = substr(s, eq + 1)
                    gsub(/^[ \t]+|[ \t]+$/, "", key); gsub(/^[ \t]+|[ \t]+$/, "", val)
                    gsub(/^"|"$/, "", val)
                    if (key == "default_host") result = val
                }
            }
        }
        END { print result }
    ' "$file"
}

# sot_hosts_table <file>
# One record per [host.<name>] section, `|`-separated (NOT tab -- bash
# `read`'s IFS treats tab as "IFS whitespace" and COLLAPSES consecutive
# delimiters, silently swallowing an empty field in the middle of a record
# and shifting every field after it; `|` doesn't have that problem in the
# GENERAL case, but see the reject-on-`|` note below for the one way it
# still could):
#   name|ssh_alias|remote_repo|tcp_port|remote_socket|error
# Missing fields are empty. Tolerant, same rules as the Rust/PowerShell
# parsers: unknown keys ignored, a later duplicate key in a section wins, a
# section header with no [host.] prefix ends the current host (its keys
# belong to it, not the next [host.] section).
#
# `error` (codex follow-up): install.sh's `reject_unsafe_path_chars`
# already forbids `|` in any path this project accepts at install time,
# but a `.sot/hosts.toml` a user edits by hand afterwards isn't gated by
# that -- a `|` inside ssh_alias/remote_repo/tcp_port/remote_socket would
# otherwise desync every field after it for this record's consumers. Checked
# here, not left to blow up downstream: a value containing `|` blanks
# ssh_alias/remote_repo/tcp_port/remote_socket for that host and sets
# `error` instead, so the host still gets a row (never silently dropped)
# naming which field was rejected.
sot_hosts_table() {
    local file="$1"
    [ -f "$file" ] || return 0
    awk '
        function has_pipe(v) { return index(v, "|") > 0 }
        function flush() {
            if (name == "") return
            err = ""
            # Spelled out, not the literal character: bash read strips a
            # TRAILING delimiter with nothing after it, so an error message
            # ending in the pipe char itself would lose it silently once
            # this record round-trips through sot_tunnel_plan (below), whose
            # own read call receives it.
            if (has_pipe(ssh_alias)) err = "ssh_alias contains a forbidden pipe character"
            else if (has_pipe(remote_repo)) err = "remote_repo contains a forbidden pipe character"
            else if (has_pipe(tcp_port)) err = "tcp_port contains a forbidden pipe character"
            else if (has_pipe(remote_socket)) err = "remote_socket contains a forbidden pipe character"
            if (err != "") {
                printf "%s|||||%s\n", name, err
            } else {
                printf "%s|%s|%s|%s|%s|\n", name, ssh_alias, remote_repo, tcp_port, remote_socket
            }
        }
        {
            s = $0
            gsub(/^[ \t]+|[ \t]+$/, "", s)
            if (s == "" || substr(s, 1, 1) == "#") next
            if (substr(s, 1, 1) == "[") {
                flush()
                name = ""
                if (substr(s, 1, 6) == "[host." && substr(s, length(s), 1) == "]") {
                    name = substr(s, 7, length(s) - 7)
                    ssh_alias = ""; remote_repo = ""; tcp_port = ""; remote_socket = ""
                }
                next
            }
            if (name == "") next
            eq = index(s, "=")
            if (eq == 0) next
            key = substr(s, 1, eq - 1)
            val = substr(s, eq + 1)
            gsub(/^[ \t]+|[ \t]+$/, "", key)
            gsub(/^[ \t]+|[ \t]+$/, "", val)
            gsub(/^"|"$/, "", val)
            if (key == "ssh_alias") ssh_alias = val
            else if (key == "remote_repo") remote_repo = val
            else if (key == "tcp_port") tcp_port = val
            else if (key == "remote_socket") remote_socket = val
        }
        END { flush() }
    ' "$file"
}

# sot_tunnel_plan <file> <default_host> <default_port>
# One `|`-separated record per [host.<name>] entry that has an ssh_alias (a
# remote) -- same delimiter choice as sot_hosts_table, for the same reason:
#   host|ssh_alias|local_port|remote_repo|remote_socket|error
#
# `local` is EXCLUDED unconditionally (codex follow-up) -- it is
# socket-only, so an ssh_alias accidentally left on a [host.local] section
# must never turn it into a tunnel target.
#
# <default_host> is the hosts.toml KEY of the primary host (codex
# follow-up: identity is the key, NOT its ssh_alias -- the alias is only
# the SSH destination, and nothing stops two different hosts.toml entries
# from sharing one). tcp_port is required per remote UNLESS its NAME
# matches <default_host>, which falls back to <default_port> (today's
# SOT_TCP_PORT/18743 default, kept for compatibility).
#
# A row with a table-level error from sot_hosts_table (a forbidden `|` in
# one of its fields) is passed straight through as a plan error, bypassing
# the ssh_alias/tcp_port checks entirely -- it has no trustworthy ssh_alias
# to check.
#
# Two hosts sharing one tcp_port is NOT checked here (owner ruling, codex
# follow-up round 2): detected once, in rust/frontend/src/hosts.rs's
# resolve_connections. A second `ssh -fN -L` on an already-bound local port
# fails to bind on its own and is already nonfatal (sot_ensure_remote_host
# treats it the same as any other unreachable host) -- which is all the
# launcher itself needs; a second detector here would just be the same
# check twice.
#
# A missing or malformed tcp_port comes back with an empty local_port and
# a non-empty error naming the host and the reason -- the caller logs it
# and moves on (nonfatal, ADR 0042 L2b design E). `error` never itself
# contains `|`, so it's always exactly the last field. Pure text
# processing -- no ssh.
sot_tunnel_plan() {
    local file="$1" default_host="$2" default_port="$3"
    sot_hosts_table "$file" | while IFS='|' read -r name ssh_alias remote_repo tcp_port remote_socket tblerr; do
        [ -n "$name" ] || continue
        if [ -n "$tblerr" ]; then
            printf '%s|||||%s\n' "$name" "$tblerr"
            continue
        fi
        [ "$name" = "local" ] && continue
        [ -n "$ssh_alias" ] || continue
        local port="$tcp_port" err=""
        if [ -z "$port" ]; then
            if [ -n "$default_host" ] && [ "$name" = "$default_host" ]; then
                port="$default_port"
            else
                err="host '$name' has no tcp_port (required for a remote tunnel)"
            fi
        fi
        printf '%s|%s|%s|%s|%s|%s\n' "$name" "$ssh_alias" "$port" "$remote_repo" "$remote_socket" "$err"
    done
}
