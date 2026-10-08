# comm-lib-agent-layers.sh: the agent-layer check: which agents lie between a script and its session.
# Sourced by comm-lib.sh; defines functions and globals only.

# --- agent layers: a second agent inside a session has no identity ---
#
# A process acts as a comm handle only if at most one agent lies between it and
# its row's capsule (or the top of its process tree outside a row). A second
# agent was started inside the session; it, and anything it starts, has no comm
# identity. Agents are named, not launchers: the list is the agents comm ships
# an adapter for, and it grows in the commit that adds one.
#
# Zero agents is legitimate (a bash row, ccx before it execs codex, a CI
# runner). An npm agent is two processes, `node <script>` and its native child,
# and counts once when the child is the host's own forwarding (same arguments
# after the binary as after the script). The walk stops at the first
# sot-capsule, so a daemon that was started from an agent's shell puts nothing
# above its rows. An ancestry that cannot be read in full before the capsule or
# the top (a cap of 64, an unreadable record, a parse that did not finish) is
# refused, never trusted. A process that names a row (sot_capsule_workspace_id)
# must reach THAT row's capsule, so reparenting away from the row, or a Windows
# walk that stops early, refuses it. Still not seen, by design: for a process
# that names no row, an agent above a reparenting; an agent not in the list, a
# macOS argv[0] holding a space, on macOS an agent whose arguments `ps` cannot
# read and whose program file is not named after it, a shim that hides the agent
# behind another name (PROTOCOL.md lists them).
_SOT_AGENTS=" claude codex "

# _sot_ps_records ME — stdin: `ps -o pid= -o ppid= -o args=` lines. Prints ME's
# chain, caller first, one record per process (fields joined by US; ps cannot
# print NUL-separated arguments, so every run of white space is a field break),
# then `!end` at the top or `!truncated` where the chain stops short of it.
# Nothing when ME is not in the table. `ps` prints `(name)` (the kernel command
# name) for a process whose arguments it cannot read: that record is `name`, US,
# RS (\036) — arguments unknown, which equals nothing, as on Windows — except
# `(node)`, which cannot be told from an agent's host and is `!truncated`, ending
# the output. A readable command line that is literally `(...)` is read the same
# way, as its name with arguments unknown.
_sot_ps_records() {
    awk -v me="$1" -v us="$(printf '\037')" -v rs="$(printf '\036')" '
        { pid = $1; pp[pid] = $2; $1 = ""; $2 = ""; sub(/^ +/, "")
          if ($0 ~ /^\(.*\)$/) { c = substr($0, 2, length($0) - 2); ar[pid] = (c == "node") ? "!truncated" : (c us rs) }
          else { gsub(/[ \t]+/, us); ar[pid] = $0 } }
        END { p = me
              for (n = 0; p > 1; n++) {
                  if (n >= 64 || !(p in ar)) { if (n > 0) print "!truncated"; exit }
                  print ar[p]; if (ar[p] == "!truncated") exit; p = pp[p] }
              if (n > 0) print "!end" }'
}

# _sot_win_records — stdin: sotd.exe's `<pid>\t<exe>\t<command line>` lines, parent
# first; the pid and the exe end at the first and second TAB (neither holds one) and
# the command line is everything after, its own TABs raw. Prints one record per
# process (the exe, then the command line's arguments after argv[0], tokenised the
# way the Microsoft C runtime does: n backslashes then a quote give n/2 backslashes
# and a literal quote when n is odd, else a quote mark; a quote mark inside quotes
# followed by another quote is one literal quote; an unquoted TAB or space
# separates and a quoted one stays; \x1c and \x1b, sotd's newline and return, are
# not white space and stay inside their token). A `!` line from sotd, a line that
# does not begin with a pid (a sotd.exe older than `ancestors --from`), or a node.exe
# whose command line could not be read (a node host cannot be told from any other
# node) prints `!truncated`, ends the output and returns 1. Any other exe whose
# command line could not be read gets ONE argument, RS (\036): arguments unknown,
# which is not an empty tail and equals nothing (_sot_agent_layers). The walk
# prints its own end.
_sot_win_records() {
    awk -F '\t' -v us="$(printf '\037')" -v rs="$(printf '\036')" '
        function bs(k,   s) { s = ""; while (k-- > 0) s = s "\\"; return s }
        /^!/ || $1 !~ /^[0-9]+$/ || NF < 3 { print "!truncated"; exit 1 }
        { exe = $2; cl = substr($0, length($1) + length($2) + 3); nt = 0; cur = ""; q = 0; has = 0
          for (i = 1; i <= length(cl); i++) { c = substr(cl, i, 1)
            if (c == "\\") { n = 0; while (substr(cl, i, 1) == "\\") { n++; i++ }
              has = 1
              if (substr(cl, i, 1) != "\"") { cur = cur bs(n); i--; continue }
              cur = cur bs(int(n / 2)); if (n % 2) { cur = cur "\""; continue }
              c = "\"" }
            if (c == "\"") { if (q && substr(cl, i + 1, 1) == "\"") { cur = cur "\""; i++ } else q = !q; has = 1 }
            else if ((c == " " || c == "\t") && !q) { if (has) { tk[++nt] = cur; cur = ""; has = 0 } }
            else { cur = cur c; has = 1 } }
          if (has) tk[++nt] = cur
          if (nt == 0 && tolower(exe) == "node.exe") { print "!truncated"; exit 1 }
          rec = exe
          if (nt == 0) rec = rec us rs
          for (i = 2; i <= nt; i++) rec = rec us tk[i]
          print rec }'
}

# The /proc a walk reads: Linux's, or on Windows Cygwin's own (the MSYS runtime Git
# for Windows ships). A plain assignment, never read from the environment; a test's
# private copy of this file points it at a stand-in.
_SOT_PROC=/proc

# _sot_proc_ppid PID — sets the caller's pp to the field after PID's command name and
# state in $_SOT_PROC/PID/stat (its parent's pid); 1 when that file cannot be read.
# The caller checks that pp is a number.
_sot_proc_ppid() {
    local line
    IFS= read -r line 2>/dev/null < "$_SOT_PROC/$1/stat" || return 1
    line="${line##*) }"; line="${line#* }"; pp="${line%% *}"
}

# _sot_ancestor_chain — this process and its ancestors, one record per process,
# the caller first, at most 64: the argv, fields joined by US (a space inside
# an argument survives, except from ps; a newline is \x1c from /proc, and sotd.exe
# prints a newline and a return as \x1c and \x1b and a TAB raw). The output ends
# with `!end` (the top was reached) or `!truncated` (the walk stopped short of it).
# 1 when nothing could be read, or sotd.exe failed (its last line says why). The
# walk starts at $$, this script's own process, never at a $(...) fork of it.
# On Windows it reads Cygwin's /proc while the parent is an MSYS process, and above
# the first MSYS process whose parent is native (ppid 1) runs `sotd.exe ancestors
# --from <its winpid>`, never _sot_windows_sotd_exe, which spawns powershell. An
# MSYS program that execs another MSYS program is a new Windows process whose
# Windows parent has exited, so sotd's walk can stop at an MSYS process short of
# the agents; the walk then goes on from that process's Cygwin parent.
_sot_ancestor_chain() {
    local us=$'\037' p=$$ pp n=0 a rec w sotd out rc line buf wt wb f c scanned=""
    if [ -r "$_SOT_PROC/$$/stat" ]; then
        while :; do
            if [ "$n" -ge 64 ]; then echo '!truncated'; break; fi
            _sot_proc_ppid "$p" || { echo '!truncated'; break; }
            rec=""
            # A newline would end the record: it becomes \x1c, never a space (an argument
            # that differs only by one must not look equal to the host's).
            while IFS= read -r -d '' a; do rec="$rec${a//$'\n'/$'\034'}$us"; done 2>/dev/null < "$_SOT_PROC/$p/cmdline"
            if [ -z "$rec" ]; then echo '!truncated'; break; fi
            printf '%s\n' "${rec%"$us"}"
            n=$((n + 1))
            case "$pp" in ''|*[!0-9]*) echo '!truncated'; break ;; esac
            if [ "$pp" -gt 1 ]; then p="$pp"; continue; fi
            _sot_is_windows || { echo '!end'; break; }
            # Windows: p's parent is native, so sotd.exe walks on from p's Windows process.
            IFS= read -r w 2>/dev/null < "$_SOT_PROC/$p/winpid" || { echo '!truncated'; break; }
            if [ -z "$scanned" ]; then
                # Cygwin's processes by winpid, read once and before any sotd snapshot: a
                # process sotd prints is an ancestor, so it was created before this scan and
                # is alive at the snapshot, and the pid it holds there names it here too.
                local -A cyg; scanned=1
                for f in "$_SOT_PROC"/[0-9]*/winpid; do
                    IFS= read -r a 2>/dev/null < "$f" && [ -n "$a" ] || continue
                    f="${f%/winpid}"; cyg[$a]="${f##*/}"
                done
            fi
            sotd="${SOTD_BIN:-${LOCALAPPDATA:-}/sot/bin/sotd.exe}"
            # Its exit status is kept: 0 is a whole walk, 3 a truncated one, anything
            # else a sotd that lacks `ancestors --from` or failed.
            out="$("$sotd" ancestors --from "$w" 2>/dev/null)"; rc=$?
            case "$rc" in 0|3) ;; *) echo "sotd ancestors failed (rc $rc): update sotd"; return 1 ;; esac
            # Its lines up to the walk's 64th record; wt is the last one's pid, wb the pid
            # of the one before it (p's own winpid when there is one line).
            buf=""; wt=""; wb="$w"
            while IFS= read -r line; do
                line="${line%$'\r'}"
                [ -n "$line" ] || continue
                case "$line" in '!'*) buf="$buf$line"$'\n'; break ;; esac
                if [ "$n" -ge 64 ]; then buf="$buf!truncated"$'\n'; break; fi
                buf="$buf$line"$'\n'; wb="${wt:-$w}"; wt="${line%%$'\t'*}"; n=$((n + 1))
            done <<< "$out"
            # On 3 sotd's own last line is `!truncated`; the filter stops at the first.
            [ "$rc" -ne 3 ] || buf="$buf!truncated"$'\n'
            # A `!` line, a line with no pid or an unreadable node.exe: it printed `!truncated`.
            printf '%s' "$buf" | _sot_win_records || break
            [ -n "$wt" ] || { echo '!end'; break; }
            # sotd stopped at its last line. The walk goes on from the Cygwin parent of the
            # process that line is, or else of the one whose current image is the line before
            # (the last line is then that process's stub, left by its exec of a native program).
            c="${cyg[$wt]:-${cyg[$wb]:-}}"
            [ -n "$c" ] || { echo '!end'; break; }
            _sot_proc_ppid "$c" || { echo '!truncated'; break; }
            case "$pp" in ''|*[!0-9]*) echo '!truncated'; break ;; esac
            [ "$pp" -gt 1 ] || { echo '!end'; break; }
            p="$pp"
        done
        [ "$n" -gt 0 ] || return 1
    elif _sot_is_windows; then
        return 1
    else
        # Separate -o: BSD ps reads "pid=,..." as one header.
        out="$(ps -ww -A -o pid= -o ppid= -o args= 2>/dev/null | _sot_ps_records "$$")" || return 1
        [ -n "$out" ] || return 1
        printf '%s\n' "$out"
    fi
}

# _sot_agent_layers [ROW] — stdin: a chain from _sot_ancestor_chain. Prints one agent
# name per layer, nearest first, stopping after the capsule's record, then `!ok`
# when the chain was read to the capsule or the top; `!tree` and nothing more when
# it ends short of both. Output without a closing `!ok`, `!norow` or `!elsewhere` is a filter that did not finish.
# With a ROW id the end must be that row's capsule: the top prints `!norow`, and a
# capsule none of whose arguments before `--` (with `\` read as `/`) ends in
# /workspaces/ROW or holds /workspaces/ROW/voyages/ prints `!elsewhere`, both in
# place of `!ok`. With no ROW the top and any capsule print `!ok`.
# A record is an agent layer when argv[0] names one, or when argv[0] is node and
# ANY argument names one (its basename minus .js/.mjs/.cjs) or lies in the npm
# package of one. A node host and its native child count once when the child is
# the host's direct child and its arguments equal the host's after its script;
# a native agent whose arguments are unknown (a lone RS argument, from a Windows
# command line that could not be read) never counts once with its host, though a
# standalone one still counts as one layer.
_sot_agent_layers() {
    _SOT_ROW="${1:-}" awk -F "$(printf '\037')" -v agents="$_SOT_AGENTS" -v unk="$(printf '\036')" '
        BEGIN { row = ENVIRON["_SOT_ROW"] }
        function nm(w) { sub(/^.*[\/\\]/, "", w); sub(/^-/, "", w); w = tolower(w); sub(/\.exe$/, "", w); return w }
        function agentof(a,   p, b) {
            p = a; gsub(/\\/, "/", p)
            b = tolower(p); sub(/^.*\//, "", b); sub(/\.(js|mjs|cjs)$/, "", b)
            if (index(agents, " " b " ") > 0) return b
            if (index(p, "@anthropic-ai/claude-code") > 0) return "claude"
            if (index(p, "@openai/codex") > 0) return "codex"
            return "" }
        function ofrow(   i, a, k) {
            k = "/workspaces/" row
            for (i = 2; i <= NF; i++) {
                if ($i == "--") break
                a = $i; gsub(/\\/, "/", a)
                if (length(a) >= length(k) && substr(a, length(a) - length(k) + 1) == k) return 1
                if (index(a, k "/voyages/") > 0) return 1 }
            return 0 }
        /^!end$/ { fin = 1; print (row == "" ? "!ok" : "!norow"); exit }
        /^!/ { fin = 1; print "!tree"; exit }
        { w0 = nm($1); n = ""; kind = "native"; from = 2
          if (w0 == "node") {
              kind = "node"
              for (i = 2; i <= NF; i++) { n = agentof($i); if (n != "") { from = i + 1; break } }
          } else n = w0
          agent = (n != "" && index(agents, " " n " ") > 0)
          after = ""; for (i = from; i <= NF; i++) after = after FS $i
          unknown = (kind == "native" && $2 == unk)
          if (agent && !(kind == "node" && pkind == "native" && pagent && !punk && pn == n && pafter == after)) print n
          pkind = kind; pn = n; pagent = agent; pafter = after; punk = unknown
          if (w0 == "sot-capsule") { fin = 1; print (row == "" || ofrow() ? "!ok" : "!elsewhere"); exit } }
        END { if (!fin) print "!tree" }'
}

# sot_require_agent — 0 when this process may act as its row's handle. Otherwise
# prints ONE reason on stdout, the way sot_require_routable_identity does, and
# returns 1 for a child (a second agent) or 2 for an ancestry that cannot be read
# in full (it cannot be shown to be the session's own agent) or for a process that
# names a row it is not shown to run inside (its walk ends at the top or at another
# capsule). Layers are judged first: a child is 1 whatever the row verdict.
sot_require_agent() {
    local chain layers l n=0 ok=0 inner="" outer="" row="" away=0
    chain="$(_sot_ancestor_chain)" || { _sot_agent_unreadable "$chain"; return 2; }
    row="$(sot_capsule_workspace_id)" || row=""
    layers="$(printf '%s\n' "$chain" | _sot_agent_layers "$row")"
    while IFS= read -r l; do
        [ -n "$l" ] || continue
        [ "$l" != "!tree" ] || { _sot_agent_unreadable; return 2; }
        [ "$l" != "!ok" ] || { ok=1; continue; }
        case "$l" in '!norow'|'!elsewhere') ok=1; away=1; continue ;; esac
        n=$((n + 1)); [ -n "$inner" ] || inner="$l"; outer="$l"
    done <<< "$layers"
    [ "$ok" = 1 ] || { _sot_agent_unreadable; return 2; }
    if [ "$n" -gt 1 ]; then
        echo "this process runs under $inner, started inside $outer's session, so it has no comm identity — nothing was read, sent or stamped; do not retry or join: an agent that needs its own handle is started as its own row"
        return 1
    fi
    if [ "$away" = 1 ]; then
        echo "this process names row $row but is not shown to run inside that row's capsule, so it has no comm identity; nothing was read, sent or stamped; start it inside the row, or as its own row"
        return 2
    fi
    return 0
}
# _sot_agent_unreadable [CHAIN_OUTPUT] — the reason; a sotd.exe failure (the output's last line) is named.
_sot_agent_unreadable() {
    local why="" last="${1:-}"
    last="${last##*$'\n'}"
    case "$last" in "sotd ancestors failed"*) why=" ($last)" ;; esac
    echo "cannot read this process's ancestry in full (/proc or ps; sotd.exe ancestors on Windows), so it cannot be shown to be its session's own agent${why} — nothing was read, sent or stamped"
}
