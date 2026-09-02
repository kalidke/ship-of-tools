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
# Prints the top-level default_host value (only the last one seen, only
# above the first table header), or nothing if absent/no file.
sot_hosts_default_host() {
    local file="$1"
    [ -f "$file" ] || return 0
    awk '
        BEGIN { pro = 1 }
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
                    if (key == "default_host") { print val; exit }
                }
            }
        }
    ' "$file"
}

# sot_hosts_table <file>
# One record per [host.<name>] section, `|`-separated (NOT tab -- bash
# `read`'s IFS treats tab as "IFS whitespace" and COLLAPSES consecutive
# delimiters, silently swallowing an empty field in the middle of a record
# and shifting every field after it; `|` doesn't have that problem, and
# install.sh's own `reject_unsafe_path_chars` already forbids `|` in any
# path this project accepts, so it can't appear in remote_repo either):
#   name|ssh_alias|remote_repo|tcp_port|remote_socket
# Missing fields are empty. Tolerant, same rules as the Rust/PowerShell
# parsers: unknown keys ignored, a later duplicate key in a section wins, a
# section header with no [host.] prefix ends the current host (its keys
# belong to it, not the next [host.] section).
sot_hosts_table() {
    local file="$1"
    [ -f "$file" ] || return 0
    awk '
        function flush() {
            if (name != "") {
                printf "%s|%s|%s|%s|%s\n", name, ssh_alias, remote_repo, tcp_port, remote_socket
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

# sot_tunnel_plan <file> <default_ssh_alias> <default_port>
# One `|`-separated record per [host.<name>] entry that has an ssh_alias (a
# remote) -- same delimiter choice as sot_hosts_table, for the same reason:
#   host|ssh_alias|local_port|remote_repo|remote_socket|error
# tcp_port is required per remote UNLESS its ssh_alias matches
# <default_ssh_alias>, which falls back to <default_port> (today's
# SOT_TCP_PORT/18743 default, kept for compatibility). A missing tcp_port on
# any OTHER host comes back with an empty local_port and a non-empty error
# naming the host and the field -- the caller logs it and moves on
# (nonfatal, ADR 0042 L2b design E). `error` never itself contains `|`, so
# it's always exactly the last field. Pure text processing -- no ssh.
sot_tunnel_plan() {
    local file="$1" default_alias="$2" default_port="$3"
    sot_hosts_table "$file" | while IFS='|' read -r name ssh_alias remote_repo tcp_port remote_socket; do
        [ -n "$ssh_alias" ] || continue
        local port="$tcp_port" err=""
        if [ -z "$port" ]; then
            if [ -n "$default_alias" ] && [ "$ssh_alias" = "$default_alias" ]; then
                port="$default_port"
            else
                err="host '$name' has no tcp_port (required for a remote tunnel)"
            fi
        fi
        printf '%s|%s|%s|%s|%s|%s\n' "$name" "$ssh_alias" "$port" "$remote_repo" "$remote_socket" "$err"
    done
}
