#!/usr/bin/env bash
# sot-hosts.sh -- shared `sotd topology plan` reader (topology plan, lane
# D). Sourced by launch-sot.sh (which SSH-ensures and opens the tunnels a
# plan names) and scripts/tests/test-tunnel-plan.sh.
#
# The old hosts.toml TOML parsing (sot_hosts_default_host / sot_hosts_table
# / sot_tunnel_plan) is DELETED -- `sot_protocol::topology` (Rust) is the
# one parser for that file now, and `sotd topology plan [--self <host>]`
# is the one way anything reads it. This file is a reader of THAT
# command's plain-line stdout, nothing else -- no TOML, no `.sot/hosts.toml`
# path knowledge here at all. Plain POSIX awk only (no gawk 3-arg match(),
# no bash4 associative arrays) so this runs under macOS's stock /bin/bash
# and awk, matching install.sh's own awk helpers.
#
# sot_topology_plan is PURE apart from the one `sotd` invocation it shells
# out to (no ssh, no other side effects) -- easy to fake in a test by
# pointing it at a stub script that prints fixed lines.
#
# Contract (rust/protocol/src/topology.rs's `plan` doc comment -- read it
# there before changing this; a reviewer may still adjust the format, so
# this parses it in ONE function and nowhere else): one fact per line,
# first word a keyword, then either one value (self/hub/relay-endpoint) or
# a host name followed by a value (dial/tunnel). The value can itself
# contain a space (a Windows pipe path carries the username verbatim), so
# every split below is a TWO-WAY split on the FIRST remaining space only
# (awk's `index()` + `substr()`, never a fixed-field split) -- the
# remainder is never re-split. A line whose first word isn't recognised is
# ignored, not an error, so a newer sotd can add facts without breaking an
# older launcher.
#
#   self <host>
#   hub <host>
#   relay-endpoint <endpoint>
#   dial <host> <endpoint>      # one per dialable host (daemon, not frontend --
#                                # D8: a frontend box's daemon is never dialled)
#   tunnel <host> <port>        # one per dialable host except self
#
# sot_topology_plan <sotd-bin> [self-host]
# Runs `<sotd-bin> topology plan [--self <self-host>]` and re-emits its
# plain-line stdout as normalized, `|`-separated records -- every OTHER
# function/caller in this file (and launch-sot.sh) reads THESE records,
# never raw `sotd` output again, so the topology grammar is interpreted in
# exactly this one place:
#   SELF|<host>
#   HUB|<host>
#   RELAY|<endpoint>
#   DIAL|<host>|<endpoint>
#   TUNNEL|<host>|<port>
# Prints nothing and returns nonzero when <sotd-bin> is empty, not
# executable, or the command itself fails (no plan yet -- e.g. a fresh
# checkout with no sotd built) -- the caller's job is to treat that as
# "no declared hosts", same as an absent/empty hosts.toml always was.
sot_topology_plan() {
    local sotd="$1" self="${2:-}"
    [ -n "$sotd" ] && [ -x "$sotd" ] || return 1
    local out
    if [ -n "$self" ]; then
        out="$("$sotd" topology plan --self "$self" 2>/dev/null)" || return 1
    else
        out="$("$sotd" topology plan 2>/dev/null)" || return 1
    fi
    printf '%s\n' "$out" | awk '
        {
            line = $0
            sp = index(line, " ")
            if (sp == 0) { kw = line; rest = "" } else { kw = substr(line, 1, sp - 1); rest = substr(line, sp + 1) }
            if (kw == "self") { print "SELF|" rest }
            else if (kw == "hub") { print "HUB|" rest }
            else if (kw == "relay-endpoint") { print "RELAY|" rest }
            else if (kw == "dial") {
                sp2 = index(rest, " ")
                if (sp2 > 0) print "DIAL|" substr(rest, 1, sp2 - 1) "|" substr(rest, sp2 + 1)
            }
            else if (kw == "tunnel") {
                sp2 = index(rest, " ")
                if (sp2 > 0) print "TUNNEL|" substr(rest, 1, sp2 - 1) "|" substr(rest, sp2 + 1)
            }
            # else: unknown first word -- ignored, not an error (forward-compat).
        }
    '
}

# sot_topology_field <plan-text> <TAG>
# The single value for a scalar record (SELF/HUB/RELAY) out of
# sot_topology_plan's normalized output -- the LAST matching line wins
# (mirrors the old parsers' "last one seen" rule), empty if absent.
sot_topology_field() {
    local plan="$1" tag="$2"
    printf '%s\n' "$plan" | awk -F'|' -v tag="$tag" '$1==tag{ v=$0; sub(/^[^|]*\|/,"",v) } END{ print v }'
}
