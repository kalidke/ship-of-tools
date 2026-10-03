# comm.jl — install/update sot-comm, the session-to-session messaging system.
#
# Source of truth: comm/ in the repo. install_comm copies the CLI-agnostic core
# scripts to ~/.sot-comm/bin and each per-CLI adapter to that CLI's dir
# (~/.claude/skills, $CODEX_HOME/skills, ~/.local/bin, hooks/plugins).
# Named accounts (owner ruling): a `~/.claude-auth/<name>` account folder
# gets skills and everything else the default `~/.claude` folder carries
# except the login via a SYMLINK the daemon creates at spawn
# (`rust/backend/src/accounts.rs::ensure_account_links`). The one thing the
# installer itself writes under `.claude-auth` is the comm hooks: an account
# holding a REAL settings.json of its own never sees the shared one, so the
# hooks are merged into every account's settings.json that exists, each
# real file once (`_claude_settings_targets`).
# See comm/PROTOCOL.md for the wire contract.
#
# The Claude adapter additionally installs the work-state hooks listed in
# `_COMM_STATE_HOOKS` (each shells out to a comm-status-*.sh script) so an
# agent's state in the state-nav is event-driven — instant and automatic, no
# model cooperation. Wiring them touches every Claude account's
# settings.json, but via NON-clobbering jq merges (_add_comm_hook!) that add
# one entry per event only if absent and preserve every existing hook; if jq
# is missing the exact JSON to add by hand is printed, and if a file won't
# parse it is left alone untouched (a missing file is created fresh in the
# install's own Claude dir only, since there is nothing there to preserve).
#
# Install copies each file via copy-then-rename (`install_file`, an atomic
# swap onto the destination — see below) and then prunes exact names — it never
# discovers-and-prunes by scanning the dir, so a file the installer did not
# write is never touched (separate-HOME Windows FE boxes update only via pull +
# update_comm; there is no shared NFS HOME to clean centrally). Each install
# records the names it ships in `<bin>/.sot-comm-installed`, and the next
# install removes every name in that record the release no longer ships.
# RULE: a commit that retires a managed bin script first checks every shipped
# version for a running loop that re-execs it. If one exists, the name stays
# SHIPPED as a stub that logs once and then sleeps (a shipped name is never
# pruned); only otherwise is the file deleted, and the record removes it.
# `COMM_DEPRECATED_BIN` below covers boxes installed before the record existed;
# append every retirement there while an upgrade can start from a record-less
# install (0.6.5 or older).

const COMM_PROTOCOL_VERSION = 1
const COMM_SRC = normpath(joinpath(@__DIR__, "..", "comm"))

# A set-but-EMPTY override is treated as unset, matching the repo's own shell
# convention (`${SOT_COMM_HOME:-...}`). Taking "" literally would yield a
# relative path that scatters the install into the process CWD, and mkpath("")
# throws partway through — a confusing failure for a trivially recoverable env
# slip.
_env_dir(var, default) = (v = get(ENV, var, ""); isempty(v) ? default : v)

"Resolved runtime home for sot-comm (honors `\$SOT_COMM_HOME`)."
comm_home() = _env_dir("SOT_COMM_HOME", joinpath(homedir(), ".sot-comm"))

# Codex reads its skills, AGENTS.md, config and plugin cache from $CODEX_HOME,
# NOT unconditionally from ~/.codex — and a host that points CODEX_HOME
# elsewhere (e.g. a machine-local dir, to keep per-host state off a shared NFS
# HOME) turned every hardcoded ~/.codex write into a no-op the sessions never
# read: skills and AGENTS.md installed into a directory codex had no interest
# in, while `codex plugin add` — which inherits the env — correctly wrote the
# plugin half to the real CODEX_HOME, leaving the install split across two
# dirs. Honor the env like comm_home() does.
"Resolved runtime home for codex (honors `\$CODEX_HOME`)."
codex_home() = _env_dir("CODEX_HOME", joinpath(homedir(), ".codex"))

# A login shell's profile can export CODEX_HOME to a machine-local path while
# nothing sources that profile for a daemon-spawned codex row or a plain tool
# shell (they inherit the shared login shell's non-interactive startup file
# instead) — so those see the unset-environment default above and can end up
# installing into a DIFFERENT codex home than the one an interactive login
# shell actually uses. Detect that split so install_comm can warn instead of
# silently writing skills nobody's interactive codex session will read.
#
# Detection is READ-ONLY text parsing — never sourcing or running a profile,
# which may have side effects of its own.

"Login profile files that may export CODEX_HOME, checked in this order (a later file's assignment wins, matching shell layering)."
const CODEX_HOME_PROFILES = (".profile", ".bash_profile", ".zprofile", ".zshenv")

# Expands only the handful of forms a login profile realistically uses to
# build this particular path — not a shell. Anything else left with a `\$`
# in it is reported as unresolved rather than guessed at.
function _expand_codex_home_value(raw::AbstractString, user::AbstractString, home::AbstractString)
    v = raw
    v = replace(v, r"\$\{USER:-\$\(id -un\)\}" => user)
    v = replace(v, r"\$\(id -un\)" => user)
    v = replace(v, "\${HOME}" => home)
    v = replace(v, "\$HOME" => home)
    v = replace(v, "\${USER}" => user)
    v = replace(v, "\$USER" => user)
    occursin('$', v) && return nothing
    return v
end

"""
    _parse_codex_home_export(text; user, home) -> (value, raw)

Pure, side-effect-free: scans one profile file's TEXT for the LAST
`export CODEX_HOME=...` or `CODEX_HOME=...; export CODEX_HOME` assignment
(a commented-out line is ignored) and expands it via `_expand_codex_home_value`.

Returns `(value, raw)`:
- no assignment anywhere in `text`   -> `(nothing, nothing)`
- assignment found, expands cleanly -> `(path, raw_value)`
- assignment found, can't expand it -> `(nothing, raw_value)` — callers quote
  `raw_value` in their warning rather than dropping the case silently.

Takes `user`/`home` as plain arguments — it never reads `ENV` or `homedir()`
itself — so it is testable without touching the real environment.
"""
function _parse_codex_home_export(text::AbstractString; user::AbstractString, home::AbstractString)
    raw = nothing
    for line in eachline(IOBuffer(text))
        stripped = strip(line)
        (isempty(stripped) || startswith(stripped, "#")) && continue
        m = match(r"^(?:export\s+)?CODEX_HOME=(.*)$", stripped)
        m === nothing && continue
        v = m.captures[1]
        # A trailing `; export CODEX_HOME` (the two-step form) ends the value
        # at the `;` — but the value itself may legitimately contain spaces
        # (e.g. `${USER:-$(id -un)}`), so only a `;` truncates it, not \s.
        occursin(';', v) && (v = first(split(v, ';'; limit = 2)))
        v = strip(v)
        (length(v) >= 2 && v[1] == v[end] && v[1] in ('"', '\'')) && (v = v[2:end-1])
        isempty(v) && continue
        raw = v
    end
    raw === nothing && return (nothing, nothing)
    return (_expand_codex_home_value(raw, user, home), raw)
end

"""
    _codex_home_profile_mismatch(installed_home)

Side-effecting wrapper around `_parse_codex_home_export`: reads whichever of
`CODEX_HOME_PROFILES` exist under `homedir()` and reports the last
assignment found across them, if any, and if it differs from
`installed_home`. Returns `nothing` when no profile assigns `CODEX_HOME`, or
the assignment matches what was just installed into. Otherwise returns
`(value, raw, file)` — `value` is `nothing` when the assignment couldn't be
expanded with confidence, in which case the caller warns quoting `raw`.
"""
function _codex_home_profile_mismatch(installed_home::AbstractString)
    user = Sys.username()
    home = homedir()
    found = nothing
    for name in CODEX_HOME_PROFILES
        path = joinpath(home, name)
        isfile(path) || continue
        value, raw = _parse_codex_home_export(read(path, String); user = user, home = home)
        raw === nothing && continue
        found = (value, raw, path)
    end
    found === nothing && return nothing
    value, _, _ = found
    value == installed_home && return nothing
    return found
end

"Resolved runtime home for the DEFAULT claude account (honors `\$CLAUDE_CONFIG_DIR`)."
claude_home() = _env_dir("CLAUDE_CONFIG_DIR", joinpath(homedir(), ".claude"))

# ADR 0030 §8 "Installed comm scripts": these carried no stamp before this —
# only a registry PROTOCOL_VERSION, no way to answer "what commit are the
# scripts on THIS box actually from" (the send-deaf Windows bridge incident
# had no way to state that). Best-effort, never throws: a release tarball or
# a checkout with git unavailable gets "unknown" rather than failing the
# whole install over a stamp nobody strictly needs to proceed. Codex review:
# a checkout with LOCAL EDITS installs those edited scripts, not the bare
# commit -- `-dirty`-suffixed here for the same reason the Rust build ids
# are (ADR 0030 §8 decision 31a), so the stamp never claims to describe a
# commit's files when it actually describes a commit plus local changes.
"Short git commit this checkout is at (dirty-suffixed if the working tree \
has uncommitted changes), or \"unknown\" when git is unavailable."
function _repo_commit()
    repo_root = normpath(joinpath(@__DIR__, ".."))
    try
        sha = readchomp(`git -C $repo_root rev-parse --short=9 HEAD`)
        isempty(sha) && return "unknown"
        dirty = try
            !isempty(readchomp(`git -C $repo_root status --porcelain`))
        catch
            true  # fails closed, same reasoning as the Rust build scripts
        end
        dirty ? "$sha-dirty" : sha
    catch
        "unknown"
    end
end

# Top-level object keys of a JSON document, without taking a JSON dependency.
# Used only to guard the codex hooks payload (see the call site for why an
# unrecognized top-level key is catastrophic there). A regex over indented key
# lines was the obvious approach and is wrong: it stops matching the moment
# anyone reformats the file, and a guard that silently stops guarding is worse
# than none. This walks the text instead, so it is indentation-independent and
# is not fooled by braces, colons or escaped quotes inside string values.
function _json_toplevel_keys(txt::AbstractString)
    ks = String[]
    depth = 0
    instr = false
    esc = false
    buf = IOBuffer()
    pending = nothing   # a string that closed at depth 1: a key iff ':' follows
    for c in txt
        if instr
            if esc
                esc = false
            elseif c == '\\'
                esc = true
            elseif c == '"'
                instr = false
                s = String(take!(buf))
                pending = depth == 1 ? s : nothing
            else
                write(buf, c)
            end
        elseif c == '"'
            instr = true
        elseif c == '{' || c == '['
            depth += 1
            pending = nothing
        elseif c == '}' || c == ']'
            depth -= 1
            pending = nothing
        elseif c == ':'
            pending === nothing || push!(ks, pending)
            pending = nothing
        elseif !isspace(c)
            pending = nothing
        end
    end
    return ks
end

# Managed bin/ files retired before the install kept a record of what it wrote
# (`COMM_MANIFEST`). Seeded from
#   git log --diff-filter=D --name-only --format= -- comm/core/scripts \
#       comm/adapters/claude/hooks comm/adapters/codex/hooks
# (and --diff-filter=R --name-status, which found no rename), keeping basenames
# not shipped today. Append every retirement here while an upgrade can start
# from a record-less install (0.6.5 or older); the manifest diff covers only
# boxes that already have the record. No running loop re-execs a name on this
# list: the old relay/listen loops re-exec `comm-relay.sh bridge`, which is
# still shipped and so never pruned; `codex-watch.sh` execs `comm-wake.sh` once
# at its start, and no script here re-execs its own name. Running copies of
# these names, and the `sot-bridge` loops, are stopped by `_stop_retired_watchers`.
const COMM_DEPRECATED_BIN = ["bus.sh", "comm-listen.sh", "comm-wake.sh", "comm-watch.sh",
                             "codex-watch.sh", "comm-postcompact-reminder.sh",
                             "comm-postclear-reminder.sh", "comm-session-skill.sh"]

# One bash script reads the process table, because MSYS processes are visible
# only from inside MSYS; Linux runs the same reader. `$1` is the bin dir. Output,
# NUL-separated: the bin dir, then per process of this user pid, ppid, start
# time (field 22 of /proc/<pid>/stat, counted after the last `)` because the
# comm field may hold spaces), argc and the argv.
const _PROC_DUMP = raw"""
bin=$1
[ -r /proc/self/cmdline ] || exit 4
printf '%s\0' "$bin"
for d in /proc/[0-9]*; do
  [ -O "$d" ] || continue
  mapfile -d '' -t a < "$d/cmdline" 2>/dev/null || continue
  [ "${#a[@]}" -gt 0 ] || continue
  pp=; while read -r k v _; do [ "$k" = PPid: ] && { pp=$v; break; }; done < "$d/status" 2>/dev/null
  [ -n "$pp" ] || continue
  read -r l < "$d/stat" 2>/dev/null || continue
  f=(${l##*)})
  [ -n "${f[19]-}" ] || continue
  printf '%s\0%s\0%s\0%s\0' "${d#/proc/}" "$pp" "${f[19]}" "${#a[@]}"
  printf '%s\0' "${a[@]}"
done
"""

# Signals `pid:start` pairs, each only while its start time is unchanged: a
# process is named by (pid, start), never by pid alone, so a reused pid is never
# signalled. TERM, up to 3 s of polling, then KILL; prints `gone <pid>` or
# `left <pid>` per pair. A pair whose stat cannot be read (or is a zombie)
# counts as gone. It never signals a group.
const _STOP_IDS = raw"""
st() { local l f; read -r l < "/proc/$1/stat" 2>/dev/null || return 1; f=(${l##*)}); [ "${f[0]}" != Z ] || return 1; printf '%s' "${f[19]}"; }
same() { [ "$(st "${1%%:*}")" = "${1#*:}" ]; }
for x; do same "$x" && kill -TERM "${x%%:*}" 2>/dev/null; done
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
  any=; for x; do same "$x" && any=1; done
  [ -n "$any" ] || break
  sleep 0.2
done
for x; do same "$x" && kill -KILL "${x%%:*}" 2>/dev/null; done
for x; do if same "$x"; then echo "left ${x%%:*}"; else echo "gone ${x%%:*}"; fi; done
"""

const _ProcRow = @NamedTuple{pid::Int, ppid::Int, start::String, argv::Vector{String}}

# Runs `cmd` (wrap `ignorestatus` around the Cmd first) with stdin from devnull,
# and returns `(exitcode, stdout)`, or a String if it was still running at the
# deadline, when it is killed.
function _read_with_deadline(cmd::Base.AbstractCmd, secs::Real)
    p = open(pipeline(cmd; stdin = devnull, stderr = devnull), "r")
    timedout = Ref(false)
    t = Timer(secs) do _
        if process_running(p)
            timedout[] = true
            kill(p)
        end
    end
    try
        out = read(p, String)
        wait(p)
        timedout[] && return "timed out after $(secs) s"
        return (p.exitcode, out)
    finally
        close(t)
    end
end

# `(binposix, procs)`, or a String saying why the table cannot be read.
function _proc_table(bin::AbstractString)
    bash = Sys.which("bash")
    bash === nothing && return "no bash on PATH"
    r = _read_with_deadline(ignorestatus(`$bash -c $_PROC_DUMP sot-proc-dump $bin`), 10)
    r isa String && return r
    code, out = r
    code == 4 && return "no /proc"
    code == 0 || return "process listing failed (exit $code)"
    f = split(out, '\0')
    isempty(f) || isempty(last(f)) && pop!(f)
    bad = "unreadable process listing"
    isempty(f) && return bad
    procs = _ProcRow[]
    i = 2
    while i <= length(f)
        i + 3 <= length(f) || return bad
        pid = tryparse(Int, f[i]); ppid = tryparse(Int, f[i+1]); argc = tryparse(Int, f[i+3])
        (pid === nothing || ppid === nothing || argc === nothing || argc < 0) && return bad
        i + 3 + argc <= length(f) || return bad
        push!(procs, (pid = pid, ppid = ppid, start = String(f[i+2]), argv = String.(f[i+4:i+3+argc])))
        i += 4 + argc
    end
    return (String(f[1]), procs)
end

# `(pid, start)` of the retired processes to stop, roots first, then their
# descendants: pure over one snapshot. A root is a bash running a retired
# script of this bin dir as its script argument, or a `sot-bridge` loop
# re-running this bin dir's `comm-relay.sh`. `self` and its ancestors are never
# returned, and the descendant walk never goes through one of them.
function _retired_watchers(procs, binposix::AbstractString; self::Integer = getpid())
    retired = Set(binposix * "/" * n for n in COMM_DEPRECATED_BIN)
    parent = Dict(p.pid => p.ppid for p in procs)
    anc = Set{Int}([self])
    q = self
    while haskey(parent, q) && !(parent[q] in anc)
        q = parent[q]
        push!(anc, q)
    end
    isroot(a) = length(a) >= 2 && basename(a[1]) in ("bash", "bash.exe") &&
                (a[2] in retired ||
                 (length(a) >= 5 && a[2] == "-c" && a[4] == "sot-bridge" &&
                  a[5] == binposix * "/comm-relay.sh"))
    pids = [p.pid for p in procs if isroot(p.argv) && !(p.pid in anc)]
    seen = Set(pids)
    grew = true
    while grew
        grew = false
        for p in procs
            (p.pid in seen || p.pid in anc || !(p.ppid in seen)) && continue
            push!(seen, p.pid); push!(pids, p.pid); grew = true
        end
    end
    start = Dict(p.pid => p.start for p in procs)
    return [(x, start[x]) for x in pids]
end

# Windows: the bash-spelled (`/c/Users/...`) form of a Windows path.
_gitbash_path(p::AbstractString) =
    replace(replace(p, '\\' => '/'), r"^([A-Za-z]):" => s -> "/" * lowercase(s[1]))

# Windows: a command line split by the standard MSVC rules.
function _winargv(s::AbstractString)
    args = String[]
    cur = IOBuffer()
    cs = collect(s)
    n = length(cs)
    inq = false
    have = false
    i = 1
    while i <= n
        c = cs[i]
        if c == '\\'
            j = i
            while j <= n && cs[j] == '\\'
                j += 1
            end
            k = j - i
            have = true
            if j <= n && cs[j] == '"'
                write(cur, '\\'^(k ÷ 2))
                isodd(k) ? write(cur, '"') : (inq = !inq)
                i = j + 1
            else
                write(cur, '\\'^k)
                i = j
            end
        elseif c == '"'
            inq = !inq; have = true; i += 1
        elseif isspace(c) && !inq
            have && push!(args, String(take!(cur)))
            have = false; i += 1
        else
            write(cur, c); have = true; i += 1
        end
    end
    have && push!(args, String(take!(cur)))
    return args
end

# Windows, pure: the PowerShell scan's lines to `(pid, creation)` of this
# user's retired watchers. Lines are `me\t<owner>`, `anc\t<pid>` (the installer
# and its ancestors, never selected), `noowner\t<pid>\t<code>` (warned, never
# selected) and `<pid>\t<creation>\t<owner>\t<commandline>`. The program must be
# a bash and the script argument exactly this install's bin folder plus a
# retired name. Windows runs no bridge loops, so only watcher roots match.
function _retired_windows(lines, binposix::AbstractString, me::AbstractString; self::Integer = 0)
    retired = Set(binposix * "/" * n for n in COMM_DEPRECATED_BIN)
    skip = Set{Int}([self])
    for l in lines
        f = split(l, '\t')
        length(f) == 2 && f[1] == "anc" && (x = tryparse(Int, f[2]); x === nothing || push!(skip, x))
    end
    ids = Tuple{Int,String}[]
    for l in lines
        f = split(l, '\t'; limit = 4)
        if length(f) == 3 && f[1] == "noowner"
            @warn "could not read the owner of a candidate retired comm watcher; not touched" pid = f[2] code = f[3]
            continue
        end
        length(f) == 4 || continue
        pid = tryparse(Int, f[1])
        (pid === nothing || pid in skip || lowercase(f[3]) != lowercase(me)) && continue
        occursin(r"^\d{14}\.\d{6}$", f[2]) || continue
        a = _winargv(f[4])
        length(a) >= 2 && lowercase(basename(replace(a[1], '\\' => '/'))) in ("bash.exe", "bash") &&
            a[2] in retired && push!(ids, (pid, String(f[2])))
    end
    return ids
end

const _WIN_SCAN = raw"""
$ErrorActionPreference = 'Continue'
& {
"me`t" + $env:USERDOMAIN + '\' + $env:USERNAME
$p = @SELF@
for ($n = 0; $p -and $n -lt 64; $n++) {
    "anc`t$p"
    $x = Get-CimInstance Win32_Process -Filter "ProcessId=$p"
    if (-not $x) { break }
    $p = $x.ParentProcessId
}
Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -match '@NAMES@' } | ForEach-Object {
    $o = Invoke-CimMethod -InputObject $_ -MethodName GetOwner
    if ($o.ReturnValue -ne 0) { "noowner`t$($_.ProcessId)`t$($o.ReturnValue)" }
    else { "$($_.ProcessId)`t$($_.CreationDate.ToString('yyyyMMddHHmmss.ffffff'))`t$($o.Domain)\$($o.User)`t$($_.CommandLine)" }
}
} 2>&1
"""

const _WIN_STOP = raw"""
function Creation($i) { $x = Get-CimInstance Win32_Process -Filter "ProcessId=$i"; if ($x) { $x.CreationDate.ToString('yyyyMMddHHmmss.ffffff') } }
foreach ($a in '@PAIRS@'.Split(' ')) {
    $i, $c = $a.Split(':')
    if ((Creation $i) -eq $c) { Stop-Process -Id $i -Force -ErrorAction SilentlyContinue }
    if ((Creation $i) -eq $c) { "left $i" } else { "gone $i" }
}
"""

# Logs the outcome of a stop: `r` is `_read_with_deadline`'s answer for the
# stopper, `ids` the pairs it was given, `script(pid)` names a pid for the warning.
function _report_stopped(ids, r, script)
    if r isa String
        @warn "retired comm watchers not stopped: $r"
        return
    end
    gone = Set{Int}()
    for l in split(r[2], r"\r?\n"; keepempty = false)
        m = match(r"^gone (\d+)$", l)
        m === nothing || push!(gone, parse(Int, m.captures[1]))
    end
    left = [x for (x, _) in ids if !(x in gone)]
    @info "stopped retired comm watchers" count = length(ids) - length(left)
    for x in left
        @warn "could not stop retired comm watcher" pid = x script = script(x)
    end
end

function _stop_retired_windows(bin::AbstractString)
    names = join((replace(n, "." => "\\.") for n in COMM_DEPRECATED_BIN), "|")
    ps(script) = _read_with_deadline(ignorestatus(`powershell.exe -NoProfile -NonInteractive -Command $script`), 20)
    r = ps(replace(_WIN_SCAN, "@SELF@" => string(getpid()), "@NAMES@" => names))
    if r isa String
        @warn "retired comm watchers not checked: $r"
        return
    end
    lines = split(r[2], r"\r?\n"; keepempty = false)
    if isempty(lines) || !startswith(lines[1], "me\t")
        @warn "retired comm watchers not checked: unreadable powershell answer" output = first(r[2], 300)
        return
    end
    ids = _retired_windows(lines[2:end], _gitbash_path(bin), lines[1][4:end]; self = getpid())
    isempty(ids) && return
    pairs = join(("$p:$c" for (p, c) in ids), " ")
    _report_stopped(ids, ps(replace(_WIN_STOP, "@PAIRS@" => pairs)), _ -> "?")
    return
end

# Stops the pre-0.6.6 wake watchers and bridge loops (and their children) this
# user still runs; it never throws, since a leftover it cannot stop is a
# warning, not a failed install. Never signals a group, and signals a process
# only after re-checking that its start time is the one it was named by.
function _stop_retired_watchers(bin::AbstractString)
    try
        Sys.iswindows() && return _stop_retired_windows(bin)
        t = _proc_table(bin)
        if t isa String
            @warn "retired comm watchers not checked: $t"
            return
        end
        binposix, procs = t
        ids = _retired_watchers(procs, binposix)
        isempty(ids) && return
        argvof = Dict(p.pid => p.argv for p in procs)
        pairs = ["$p:$s" for (p, s) in ids]
        r = _read_with_deadline(ignorestatus(`$(Sys.which("bash")) -c $_STOP_IDS sot-stop $pairs`), 10)
        _report_stopped(ids, r, x -> get(argvof[x], 2, "?"))
    catch e
        @warn "retired comm watchers not checked" exception = (e, catch_backtrace())
    end
    return
end

# The names the last successful install shipped into `<bin>`, one per line.
const COMM_MANIFEST = ".sot-comm-installed"

# Skill directories retired in a past commit. Pruned from both the Claude
# skills dir and the Codex skills dir on every install — same convention as
# COMM_DEPRECATED_BIN, exact names only, never a glob. Append the OLD name
# here IN THE SAME COMMIT that stops shipping a skill directory.
const COMM_DEPRECATED_SKILLS = ["sot-be-session-start", "sot-fe-session-start"]

# Launcher scripts retired in a past commit. Pruned from `~/.local/bin` by
# `_install_launchers` on every install — same convention as
# COMM_DEPRECATED_BIN. Append the OLD name here IN THE SAME COMMIT that stops
# shipping a launcher.
const COMM_DEPRECATED_LAUNCHERS = ["ccbe"]

# Every byte-exact version of ~/.agents/plugins/marketplace.json this package
# has ever written, CURRENT FIRST — the writer emits `first()`, and a file
# matching ANY entry is ours-unmodified and safe to replace. Anything else in
# that file is content we did not put there (another marketplace, or ours with
# entries someone added), and the install must not delete it: the old guard
# overwrote on a mere `occursin("sot-local", ...)`, so a file that MENTIONED
# sot-local anywhere — including one carrying other marketplaces' plugins —
# was rewritten wholesale. Same same-commit convention as COMM_DEPRECATED_BIN:
# change the payload -> append the outgoing version here in that commit, or
# every machine that took the old payload starts warning instead of upgrading.
const CODEX_MARKETPLACE_PAYLOADS = [
    """
{
  "name": "sot-local",
  "interface": { "displayName": "Ship of Tools local" },
  "plugins": [
    {
      "name": "sot-comm",
      "source": { "source": "local", "path": "./.agents/plugins/sot-comm" },
      "policy": { "installation": "AVAILABLE" }
    }
  ]
}
""",
]

"""
    install_file(src, dst)

Copy `src` to `dst` via copy-then-rename: copy to a sibling temp path, then
`Base.Filesystem.rename` it onto `dst` — a single, non-destructive rename
syscall (the same shape `_add_comm_hook!`/`_remove_stale_comm_hooks!` already
use for settings.json).

Every plain-file install in this module goes through this instead of
`cp(src, dst; force = true)`, because that form REMOVES `dst` before copying:
a copy that then fails — source unreadable, a file in use, a transient I/O
error — leaves NOTHING at `dst`, not merely an un-updated file. This is
exactly how `comm-relay.sh` disappeared from a live `~/.sot-comm/bin` while
every other script updated normally (observed on a Windows FE box,
2026-09-04, full trace: `IOError: open(...comm-relay.sh...): permission
denied (EACCES)` from `sendfile` inside `cp(force=true)`) — a live process
still had the file open, the forced delete went pending, and the recreate
was refused until the handle closed, by which point the file was gone.

The publish step deliberately calls `Base.Filesystem.rename` directly and
NOT `mv(tmp, dst; force = true)`: `mv`'s `force=true` path retries a failed
plain rename by first REMOVING `dst` — the identical hole, one layer down
(confirmed by reading Base `file.jl`'s `_mv_replace`, and reproduced locally:
a `mv(tmp, dst; force=true)` onto a `dst` that a plain rename can't replace
silently deletes `dst`'s old content when the retry succeeds after the
delete, or destroys it with nothing to show when the retry also fails). A
bare `rename` either atomically replaces `dst` or leaves it completely
untouched — on Windows it fails outright (by design; see its docstring) if
either path is currently an open file, exactly the observed condition.

On any failure — the copy or the rename — the temp file is removed, `dst` is
left exactly as it was, and the error's first line names `dst` and the
underlying cause: a launcher that only surfaces the tail of a crash's stderr
still needs the useful part to survive truncation.
"""
# This machine's own name with every non-alphanumeric character stripped,
# read at runtime and never written into the repository.
_host_tag() = filter(c -> isletter(c) || isdigit(c), gethostname())

# No two LIVE copies of the installer may share a staging path — a fixed name
# lets one run's copy interleave with another's rename and publish a torn file
# under a name that then passes every existence check. Host tag + pid also
# make an ABANDONED staging file decidable: pid liveness is only meaningful
# against files written on this host. Returns a SIBLING of `dst`, which is
# load-bearing: the publish is a rename, atomic only within one filesystem.
_tmp_name(dst::AbstractString) =
    string(dst, ".tmp-", _host_tag(), "-", getpid(), "-",
           string(rand(UInt16); base = 16))

# Zero-signal liveness probe. Non-unix returns `true` (never reap): the probe
# is unix-only, so a Windows host killed mid-install leaves one inert staging
# file that nothing reaps — a named residual, not an age threshold, which
# would delete a slow run's LIVE file.
function _pid_alive(pid::Integer)
    Sys.isunix() || return true
    return try
        ccall(:kill, Cint, (Cint, Cint), pid, 0) == 0
    catch
        true
    end
end

"""
    _reap_markers(dst)

Remove this destination's own leftover markers, run only AFTER a successful
rename onto `dst`. A directory-wide PRE-pass can delete another run's aside
during the window between its two renames — the one moment that aside is the
only copy of the old file; done after our own rename succeeds, the worst case
is that the other run's restore fails and `dst` keeps our valid new content,
never missing. An aside goes unconditionally (best effort). A staging file
goes ONLY when its host field is this host AND its pid is not alive: a live
pid means a run is writing it right now, and another host's pid number says
nothing about liveness here, so both exclusions are absolute.
"""
function _reap_markers(dst::AbstractString; keep::Union{Nothing,AbstractString} = nothing)
    dir = dirname(dst)
    base = basename(dst)
    isdir(dir) || return nothing
    tag = _host_tag()
    for name in readdir(dir)
        (startswith(name, base) && name != base) || continue
        full = joinpath(dir, name)
        # The aside this call just created belongs to the NEXT replace, not
        # this one: it is the only copy of the old file a live holder still
        # has open.
        (keep !== nothing && full == keep) && continue
        if occursin(".stale-", name)
            try
                rm(full; force = true)
            catch
            end
            continue
        end
        r = findfirst(".tmp-", name)
        r === nothing && continue
        fields = split(name[(last(r) + 1):end], '-')
        length(fields) >= 2 || continue
        fields[1] == tag || continue
        pid = tryparse(Int, fields[2])
        (pid === nothing || _pid_alive(pid)) && continue
        try
            rm(full; force = true)
        catch
        end
    end
    return nothing
end

"""
    install_file(src, dst; rename = Base.Filesystem.rename)

Copy `src` to a temporary name beside `dst`, then rename it onto `dst`, so
`dst` is never partially written. When the rename is refused because a running
process holds `dst` open (Windows), the old file is moved aside under a
`.stale-` name rather than deleted — the process keeps its inode — the new file
lands, and the aside copy is reaped by the next successful replace of that
name. Any failure removes the temporary and throws; if the second rename fails
the old file is put back, so `dst` is never missing.
"""
function install_file(src::AbstractString, dst::AbstractString;
                      rename = Base.Filesystem.rename)
    tmp = _tmp_name(dst)
    aside_made = Ref{Union{Nothing,String}}(nothing)
    try
        cp(src, tmp; force = true)
        try
            rename(tmp, dst)
        catch first_err
            # Windows refuses to rename over a file another process holds
            # open (field report 2026-09-11). The old file is moved
            # ASIDE, never deleted: the running process keeps its inode, the
            # name is freed, the new file lands, and the aside copy is pruned
            # by the next successful replace of this name (`_reap_markers`). Only a FILE is moved
            # aside; anything else at dst (a directory) stays an error, as
            # before, and if the second rename fails too the old file is put
            # back so dst is never missing.
            isfile(dst) || rethrow(first_err)
            aside = dst * ".stale-" * string(rand(UInt32); base = 16)
            rename(dst, aside)
            aside_made[] = aside
            try
                rename(tmp, dst)
            catch second_err
                try
                    rename(aside, dst)
                catch
                end
                rethrow(second_err)
            end
        end
        _reap_markers(dst; keep = aside_made[])
    catch err
        isfile(tmp) && rm(tmp; force = true)
        error("install_file: $dst: $(sprint(showerror, err))")
    end
    return nothing
end

"""
    _check_installed(dstdir, files; executable = Returns(false)) -> Vector{String}

Check that every name in `files` exists in `dstdir`, and — for names where
`executable(name)` is true — that the destination copy is actually
executable. Returns one description per problem found (empty on success);
never throws, so callers ([`_install_files`](@ref)) can fold it into a single
combined report alongside their own failures.

`install_file` already guarantees a single failed copy can't silently destroy
a working file, but nothing so far confirms every source file actually
landed — a copy that returns without error yet produces nothing is a
different failure mode (unproven on this platform, not ruled out on others).
This is that confirmation.
"""
function _check_installed(dstdir::AbstractString, files; executable = Returns(false))
    problems = String[]
    for f in files
        dst = joinpath(dstdir, f)
        if !isfile(dst)
            push!(problems, "$f is missing from $dstdir")
        elseif executable(f) && !Sys.isexecutable(dst)
            push!(problems, "$f in $dstdir is not executable")
        end
    end
    return problems
end

"""
    _install_files(srcdir, dstdir, files; executable = Returns(false))

Install `files` (names found under `srcdir`, may include subdirectory
components) into `dstdir` one at a time via [`install_file`](@ref) —
continuing past a single failure, so ONE destination a live process still
has open (`comm-relay.sh`, observed live) cannot block updating the rest of
a directory. `executable(name)` files get `chmod(0o755)` after a successful
install.

Raises ONE error at the end combining every file that could not be updated
(old copy kept, if one existed — stale is an acceptable outcome; MISSING is
not) with anything [`_check_installed`](@ref) still finds wrong. Every
reason lands in one joined, single-line string, because a launcher that
truncates a crash's stderr to its tail still needs the useful part to
survive.
"""
function _install_files(srcdir::AbstractString, dstdir::AbstractString, files;
                         executable = Returns(false))
    problems = String[]
    for f in files
        dst = joinpath(dstdir, f)
        mkpath(dirname(dst))
        src = joinpath(srcdir, f)
        # A destination that already holds these exact bytes is current: skip the
        # replace: on Windows a file another process holds open fails the
        # replace with EACCES even though nothing is stale.
        if isfile(dst) && read(dst) == read(src)
            try
                executable(f) && chmod(dst, 0o755)
            catch
            end
            continue
        end
        try
            install_file(src, dst)
            executable(f) && chmod(dst, 0o755)
        catch err
            push!(problems, "$f could not be updated, kept the previous copy ($(sprint(showerror, err)))")
        end
    end
    append!(problems, _check_installed(dstdir, files; executable = executable))
    isempty(problems) ||
        error("sot-comm install incomplete in $dstdir: $(join(problems, "; "))")
    return nothing
end

# Every name this release installs directly into `<bin>`: the core scripts and
# both adapters' hook scripts, from all three dirs whatever CLIs this run
# installs, so a name only another CLI's run installs is never pruned.
function _comm_bin_shipped()
    dirs = [joinpath(COMM_SRC, "core", "scripts"),
            joinpath(COMM_SRC, "adapters", "claude", "hooks"),
            joinpath(COMM_SRC, "adapters", "codex", "hooks")]
    return sort!(unique(reduce(vcat, [isdir(d) ? readdir(d) : String[] for d in dirs])))
end

# A bare file name: the only shape a prune will act on.
_plain_name(n::AbstractString) =
    !isempty(n) && n != "." && n != ".." && !any(c -> c in ('/', '\\', '\0', '\n'), n)

# The previous install's names, or `nothing` when the record is missing, empty
# or has a line that is not a bare name — then only COMM_DEPRECATED_BIN prunes.
function _read_comm_manifest(bin::AbstractString)
    path = joinpath(bin, COMM_MANIFEST)
    isfile(path) || return nothing
    names = try
        split(read(path, String), '\n'; keepempty = false)
    catch
        String[]
    end
    if isempty(names) || !all(_plain_name, names)
        @info "Previous comm install record is empty or unparseable; pruning from the frozen list only" file = path
        return nothing
    end
    return String.(names)
end

# Remove every name in the previous record or COMM_DEPRECATED_BIN that this
# release does not ship. Plain files directly in `bin` only: a symlink, a
# directory or anything else is left alone, and nothing is matched by pattern.
function _prune_comm_bin(bin::AbstractString, shipped, prev)
    for n in sort!(unique(vcat(COMM_DEPRECATED_BIN, something(prev, String[]))))
        _plain_name(n) || continue
        p = joinpath(bin, n)
        if n in shipped
            ispath(p) && @info "Kept comm script: still shipped by this release" file = p
            continue
        end
        st = try lstat(p) catch; continue end
        isfile(st) || continue
        try
            rm(p)
            @info "Pruned retired comm script" file = p
        catch err
            @warn "Could not prune retired comm script" file = p exception = err
        end
    end
    return nothing
end

# Write-then-rename, like every other file the install writes (a plain
# `Base.Filesystem.rename`, never `mv(; force = true)` — see `install_file`).
function _write_comm_manifest(bin::AbstractString, shipped)
    path = joinpath(bin, COMM_MANIFEST)
    tmp = _tmp_name(path)
    write(tmp, join(shipped, "\n") * "\n")
    Base.Filesystem.rename(tmp, path)
    return nothing
end

"""
    install_comm(; clis = [:claude])

Install sot-comm. Copies the core scripts to `\$SOT_COMM_HOME/bin`
(default `~/.sot-comm/bin`) and installs the adapter for each CLI in `clis`
(`:claude` skills/hooks, `:codex` skills/hooks/plugin). Idempotent — safe to
re-run to update an existing install.
"""
function install_comm(; clis = [:claude, :codex])
    bin = joinpath(comm_home(), "bin")
    mkpath(bin)
    # ADR 0030 §8 "Installed comm scripts", Codex review (should-fix): a
    # stamp that survives a FAILED install claims a commit that may not
    # describe what actually landed -- invalidate it FIRST, before any
    # copy, so a mid-install throw below leaves NO stamp (read as
    # "unknown" by `sot-fe version`) rather than a stale, misleading one.
    # The real stamp is written only once, at the very end, past every
    # copy that could still throw.
    version_file = joinpath(comm_home(), "VERSION")
    rm(version_file; force = true)

    srcscripts = joinpath(COMM_SRC, "core", "scripts")
    isdir(srcscripts) || error("comm scripts not found at $srcscripts")
    srcfiles = readdir(srcscripts)
    # Every stage runs; failures are collected and raised together at the
    # end, so one refused file (field report
    # 2026-09-11) no longer leaves the skills and hooks un-updated.
    problems = String[]
    _stage!(problems, "comm scripts") do
        _install_files(srcscripts, bin, srcfiles; executable = endswith(".sh"))
    end
    prev_manifest = _read_comm_manifest(bin)
    @info "Installed comm scripts" dir = bin count = length(readdir(bin))

    unhooked = String[]
    for cli in clis
        _stage!(problems, "adapter $cli") do
            _install_adapter(Symbol(cli); unhooked = unhooked)
        end
    end
    # Never silent about an account the hooks did not reach (each line was
    # also warned where it happened; this is the one place they are listed).
    isempty(unhooked) || @warn "The comm hooks are NOT in these Claude settings — a session there does not report its work state:\n  " *
                               join(unhooked, "\n  ")
    isempty(problems) ||
        error("sot-comm install INCOMPLETE — " * join(problems, "; ") *
              "; no version stamp written, `sot-fe version` reports this install as unknown")

    # Only now, with every file of this install copied, prune what the release
    # no longer ships and record what it does; a failed install got here never.
    shipped = _comm_bin_shipped()
    _prune_comm_bin(bin, shipped, prev_manifest)
    _write_comm_manifest(bin, shipped)
    _stop_retired_watchers(bin)

    # Published LAST, only once every copy above has actually succeeded —
    # invariant "the scripts on this box came from commit X" (dirty-
    # suffixed when the source checkout has local edits), the fact a
    # send-deaf Windows bridge incident had no way to state.
    write(version_file, _repo_commit())
    @info "sot-comm ready" protocol = COMM_PROTOCOL_VERSION home = comm_home()
    @info "Next: in your session run  ~/.sot-comm/bin/comm-join.sh --name <handle>  (or use the /sot-comm skill)"
    return nothing
end

"""
    update_comm(; clis = [:claude, :codex])

Re-sync an existing install from the repo source. Alias of [`install_comm`]
(install is idempotent); run after `git pull` on each machine to close version
skew.
"""
update_comm(; clis = [:claude, :codex]) = install_comm(; clis = clis)

"""
    _stage!([f], problems, label)

Run `f`, and on a throw record ONE line on `problems` naming `label` and the
cause, then return. A failure is recorded and named, never propagated: no
file strands its skill, no skill strands the skills after it, no stage
strands the next one, no adapter strands the next one. The run raises one
combined error at the end instead.
"""
function _stage!(f::Function, problems::Vector{String}, label::AbstractString)
    try
        f()
    catch err
        push!(problems, "$label ($(sprint(showerror, err)))")
    end
    return nothing
end
_stage!(problems::Vector{String}, label::AbstractString, f::Function) =
    _stage!(f, problems, label)

"""
    _sweep_orphans!(skilldst, shipped)

Delete files under an installed skill that the source no longer ships — the
only thing the old `rm(dst; recursive = true)` ever bought: a renamed or
deleted reference must not linger and be read as current. Names carrying
`install_file`'s in-flight or aside markers are NOT ours to delete here
(another run may be mid-swap on them; reaping lives in `install_file`, which
knows their owner). Emptied directories go too. Best effort: a file a live
process holds open warns, it does not fail the skill — the skill itself is
current. Called ONLY for a skill whose install raised nothing, since sweeping
a failed skill could delete a resource while its `SKILL.md` stayed old.
"""
function _sweep_orphans!(skilldst::AbstractString, shipped)
    isdir(skilldst) || return nothing
    for (root, _, files) in walkdir(skilldst)
        for f in files
            (occursin(".tmp-", f) || occursin(".stale-", f)) && continue
            rel = relpath(joinpath(root, f), skilldst)
            rel in shipped && continue
            try
                rm(joinpath(root, f))
            catch err
                @warn "orphan left in place" file = joinpath(root, f) error = err
            end
        end
    end
    for (root, dirs, _) in walkdir(skilldst; topdown = false)
        for d in dirs
            full = joinpath(root, d)
            try
                isempty(readdir(full)) && rm(full)
            catch
            end
        end
    end
    return nothing
end

"""
    _install_skills(srcdir, skillsroot) -> Vector{String}

Install every skill directory under `srcdir` (one containing a `SKILL.md`)
into `skillsroot`, file by file through [`install_file`](@ref)'s atomic
copy-then-rename, plus one orphan sweep. Every file is either the old copy or
the new one — never missing, never partial. The destination is NEVER removed:
`rename(2)` cannot replace a non-empty directory, so a "directory swap" is
really rename-aside-then-rename-in, which re-creates the very window it
claims to close.

One path for BOTH adapters. Returns the names that landed (`"/name"`); raises
one combined error naming every skill that kept its previous copy, which the
caller records as a stage failure.
"""
function _install_skills(srcdir::AbstractString, skillsroot::AbstractString)
    mkpath(skillsroot)
    names = [n for n in readdir(srcdir) if isfile(joinpath(srcdir, n, "SKILL.md"))]
    installed = String[]
    stale = String[]
    problems = String[]
    for name in names
        skillsrc = joinpath(srcdir, name)
        rel = sort([relpath(joinpath(r, f), skillsrc)
                    for (r, _, fs) in walkdir(skillsrc) for f in fs])
        before = length(problems)
        _stage!(problems, "/$name") do
            _install_files(srcdir, skillsroot, [joinpath(name, r) for r in rel])
        end
        if length(problems) == before
            _sweep_orphans!(joinpath(skillsroot, name), Set(rel))
            push!(installed, "/$name")
        else
            push!(stale, "/$name")
            @warn "skill NOT updated — the destination still holds the PREVIOUS copy" skill = "/$name" dir = skillsroot reason = last(problems)
        end
    end
    _prune_deprecated_skills!(skillsroot)
    # The word Installed may never appear without its denominator and its
    # stale list in the same line: a reader who sees only this line can
    # compute the outcome without reading anything above it.
    @info "Installed skills" installed = "$(length(installed))/$(length(names))" stale = stale dir = skillsroot
    isempty(problems) || error("$(length(installed)) of $(length(names)) skills updated; " *
                               "STALE (previous copy still in place): $(join(problems, "; "))")
    return installed
end

# Remove any of COMM_DEPRECATED_SKILLS still present in `skillsroot`, exact
# names only, never a glob. Shared by both CLI adapters below — the same
# retired skill dirs must not linger under `~/.claude/skills` or
# `$CODEX_HOME/skills` once a commit stops shipping them.
function _prune_deprecated_skills!(skillsroot::AbstractString)
    for name in COMM_DEPRECATED_SKILLS
        dst = joinpath(skillsroot, name)
        isdir(dst) || continue
        rm(dst; recursive = true, force = true)
        @info "Pruned deprecated skill" dir = dst
    end
end

function _install_adapter(cli::Symbol; unhooked::Vector{String} = String[])
    problems = String[]
    if cli === :claude
        srcdir = joinpath(COMM_SRC, "adapters", "claude")
        _stage!(problems, "claude skills") do
            _install_skills(srcdir, joinpath(claude_home(), "skills"))
        end
        _stage!(problems, "claude launchers") do
            _install_launchers(joinpath(srcdir, "bin"))
        end
        _stage!(problems, "claude hooks") do
            _install_claude_hooks(joinpath(srcdir, "hooks"), claude_home(); unhooked = unhooked)
        end
        # Named accounts get skills via the shared-folder symlink the
        # daemon creates at spawn (`rust/backend/src/accounts.rs::
        # ensure_account_links`); the hooks are merged into every
        # account's own settings.json by `_install_claude_hooks` itself,
        # since an account with a real settings.json never sees the
        # shared one.
    elseif cli === :codex
        # ADR 0031 — codex adapter: ccx launcher, the PermissionRequest->blocked
        # hook script, and the hooks.json plugin payload (state-nav wiring). The shared
        # state scripts (comm-status-*.sh) ride the core
        # deploy above.
        home = codex_home()
        @info "Installed into codex home" dir = home
        mismatch = _codex_home_profile_mismatch(home)
        if mismatch !== nothing
            value, raw, path = mismatch
            if value === nothing
                @warn """
                    a login profile ($path) exports CODEX_HOME=$raw, which this installer \
                    could not resolve with confidence — it may name a different codex home \
                    than the one just installed into ($home). Resolve the value and re-run \
                    under it: CODEX_HOME=<resolved value> julia --project=. -e 'using ShipTools; ShipTools.update_comm()'""" profile = path raw = raw installed = home
            else
                @warn """
                    a login profile exports a different CODEX_HOME than this install used. \
                    Login profile ($path) resolves to $value; installed into $home instead — \
                    a daemon-spawned or non-interactive shell doesn't source that profile, so \
                    it sees the unset-environment default while an interactive login shell \
                    sees the profile's value. Re-run under that environment: \
                    CODEX_HOME=$value julia --project=. -e 'using ShipTools; ShipTools.update_comm()'""" profile = path resolved = value installed = home
            end
        end
        srcdir = joinpath(COMM_SRC, "adapters", "codex")
        isdir(srcdir) || return nothing
        skills_src = joinpath(srcdir, "skills")
        # Same function as the Claude adapter: a Codex skill's resource files
        # travel too, which the old one-entry-per-skill list never carried.
        isdir(skills_src) && _stage!(problems, "codex skills") do
            _install_skills(skills_src, joinpath(codex_home(), "skills"))
        end
        _stage!(problems, "codex launchers") do
            _install_launchers(joinpath(srcdir, "bin"))
        end
        hookssrc = joinpath(srcdir, "hooks")
        isdir(hookssrc) && _stage!(problems, "codex hooks") do
            bin = joinpath(comm_home(), "bin")
            _install_files(hookssrc, bin, readdir(hookssrc); executable = Returns(true))
        end
        # Global codex memory: our AGENTS.md also installs as
        # $CODEX_HOME/AGENTS.md so conventions reach codex sessions in ANY
        # project — workspaces are arbitrary repos that won't carry our file
        # (found live: the first daemon-booted codex reported "AGENTS.md
        # conventions not found" from a scratch workspace). Marker-guarded like
        # hooks.json.
        src_agents = joinpath(dirname(COMM_SRC), "AGENTS.md")
        isfile(src_agents) && _stage!(problems, "codex AGENTS.md") do
            dstdir = codex_home()
            mkpath(dstdir)
            dst = joinpath(dstdir, "AGENTS.md")
            marker = "Codex sessions in Ship of Tools"
            if !isfile(dst) || occursin(marker, read(dst, String))
                install_file(src_agents, dst)
                @info "Installed global codex AGENTS.md" file = dst
            else
                @warn "codex AGENTS.md exists and is not ours — merge manually" file = dst
            end
        end
        # Hooks deploy as a LOCAL PLUGIN (found the hard way, 2026-07-06:
        # codex 0.142 loads lifecycle hooks from config and ENABLED PLUGINS —
        # a standalone hooks.json in the codex home is silently ignored, which
        # was the "session coloring isn't working" bug). Layout: the implicit
        # local marketplace ~/.agents/plugins/marketplace.json + a sot-comm
        # plugin dir (manifest + hooks/hooks.json), then `codex plugin add`
        # (which re-copies into $CODEX_HOME/plugins/cache on every run, so a
        # repo edit does propagate). Paths in marketplace.json resolve from TWO
        # levels above the file ($HOME) — the marketplace stays HOME-relative
        # even when CODEX_HOME points elsewhere, which is why only skills and
        # AGENTS.md above needed codex_home().
        #
        # Hook TRUST: persisted per hook HASH in $CODEX_HOME/config.toml under
        # [hooks.state], granted once via the /hooks TUI. NOT covered by a
        # shared $HOME when a host points CODEX_HOME at a machine-local path —
        # then trust is per-machine. Unset (the default, ~/.codex) it follows
        # the shared $HOME like everything else. ccx additionally passes
        # --dangerously-bypass-hook-trust so a changed hash can't silently
        # disable state reporting on daemon-spawned sessions.
        #
        # The two silent-death traps this file has already fallen into (both
        # cost weeks of colourless sessions, both report Installed=0 in /hooks
        # with no error anywhere) are written up in
        # comm/adapters/codex/hooks.README.md — read it before editing
        # hooks.json. The guard below enforces the first one.
        src = joinpath(srcdir, "hooks.json")
        isfile(src) && _stage!(problems, "codex plugin") do
            pdir = joinpath(homedir(), ".agents", "plugins", "sot-comm")
            mkpath(joinpath(pdir, ".codex-plugin"))
            mkpath(joinpath(pdir, "hooks"))
            install_file(joinpath(srcdir, "plugin", ".codex-plugin", "plugin.json"),
                         joinpath(pdir, ".codex-plugin", "plugin.json"))
            txt = replace(read(src, String), "\$HOME" => homedir())
            # codex REJECTS the whole hooks file — every event, silently — if it
            # carries ANY unrecognized top-level key (the config struct is
            # deny_unknown_fields). A "_comment" key documenting the file did
            # exactly that, for weeks. Fail loudly here instead of shipping a
            # file that installs nothing.
            #
            # The scanner does NOT validate JSON syntax, and on malformed input
            # (unbalanced braces) a stray key can sit at depth <= 0 and be
            # missed — measured, not hypothetical. So require the "hooks" key to
            # be PRESENT as well: that fails closed on truncated, array-rooted
            # or empty files, and on any future scanner bug. test/runtests.jl
            # additionally parses the file for real.
            ks = _json_toplevel_keys(txt)
            "hooks" in ks || error("""
                codex hooks.json has no top-level "hooks" key — the file is truncated,
                mangled, or not the shape codex expects. Nothing would install and no
                session would ever show its work-state.""")
            stray = filter(!=("hooks"), ks)
            isempty(stray) || error("""
                codex hooks.json has unrecognized top-level key(s): $(join(stray, ", ")).
                codex silently rejects the ENTIRE file when one is present — every hook
                would report Installed=0 and no session would ever show its work-state.
                Only "hooks" may appear at the top level; put prose in
                comm/adapters/codex/hooks.README.md instead.""")
            write(joinpath(pdir, "hooks", "hooks.json"), txt)
            mp = joinpath(homedir(), ".agents", "plugins", "marketplace.json")
            # Replace the file only when it is absent or byte-identical to a
            # version we wrote (see CODEX_MARKETPLACE_PAYLOADS). Fail closed on
            # anything else — a modified file may hold entries other tools or
            # the user added, and uncertainty is not permission to delete them.
            if !isfile(mp) || read(mp, String) in CODEX_MARKETPLACE_PAYLOADS
                write(mp, first(CODEX_MARKETPLACE_PAYLOADS))
            else
                @warn """
                    ~/.agents/plugins/marketplace.json exists with content this installer
                    did not write, so it was left alone. Ensure it carries the sot-comm
                    plugin entry (source path ./.agents/plugins/sot-comm) — or delete the
                    file and re-run update_comm() to regenerate it.""" file = mp
            end
            if isnothing(Sys.which("codex"))
                # Codex is OPTIONAL (maintainer, 2026-07-11): a machine
                # without the codex CLI is a normal configuration, not a
                # problem — @info, never @warn, so launchers/installers that
                # surface warnings don't read as complaining on every sync.
                @info "codex CLI not installed (optional) — plugin files staged; if you later install codex, run `codex plugin add sot-comm@sot-local`" plugin = pdir
            else
                ok = success(pipeline(`codex plugin add sot-comm@sot-local`; stdout = devnull, stderr = devnull))
                # Name the trust step on the SUCCESS path too: a hook whose
                # definition changed is untrusted again, and an untrusted hook
                # is silently inert in any session not launched by ccx (which
                # passes --dangerously-bypass-hook-trust). Trust is per
                # $CODEX_HOME, so a machine-local CODEX_HOME means per-machine.
                @info """Installed codex hooks plugin — verify in codex with /hooks: \
                    the four events should read Installed=1, and Active=1 once trusted \
                    (press `t` there; needed once per machine)""" plugin = pdir added = ok codex_home = codex_home()
                ok || @warn "codex plugin add failed or already installed — check `codex plugin list` / trust via /hooks"
            end
        end
    else
        @warn "No adapter for this CLI yet — add comm/adapters/$(cli)/ and a case here" cli
    end
    isempty(problems) || error(join(problems, "; "))
    return nothing
end

# The account-name rule, character for character the daemon's
# `is_account_name` (`rust/backend/src/accounts.rs`), so "every account"
# here is exactly the set the daemon can spawn a session as. `\z`, not `$`:
# PCRE's `$` also matches before a trailing newline.
_is_account_name(name::AbstractString) = occursin(r"\A[a-z0-9][a-z0-9_-]*\z", name)

# A dir compared by where it really is: absolute, links resolved when it
# exists (a probe that cannot resolve falls back to the absolute path), no
# trailing separator.
function _dirkey(d::AbstractString)
    a = abspath(d)
    r = try
        ispath(a) ? realpath(a) : a
    catch
        a
    end
    return rstrip(normpath(r), ['/', '\\'])
end

# `dir` is `root` or lies below it, by the path as written OR by where it
# really is (`_dirkey`). The rule this guards FORBIDS, so either reading is
# enough: a linked account folder resolves outside `root`, and a not-yet-
# created dir under a linked HOME matches only as written.
function _is_under(dir::AbstractString, root::AbstractString)
    within(p, r) = length(p) >= length(r) && p[1:length(r)] == r
    written(d) = splitpath(rstrip(normpath(abspath(d)), ['/', '\\']))
    return within(written(dir), written(root)) ||
           within(splitpath(_dirkey(dir)), splitpath(_dirkey(root)))
end

"""
    _claude_settings_targets(home, claude_dir; unhooked = String[]) -> Vector{String}

Every Claude account's settings.json the comm hooks go into, as the REAL
files to write, each once, in this order: `claude_dir` (the install's own
dir, `claude_home()`), the default `home/.claude`, then every
`home/.claude-auth/<name>` folder whose name passes [`_is_account_name`]
(sorted). Links are resolved (`realpath`), so a writer renames onto the
target and a link stays a link, and two accounts linked to one file are one
entry.

A MISSING settings.json is returned (unresolved) for exactly one dir, the
create dir: `claude_dir`, unless that is `home/.claude-auth` or lies in it
(compared by real location, so a relative or linked `CLAUDE_CONFIG_DIR` is
caught), in which case `home/.claude` (the backend's link then carries that file into the
account), where `_add_comm_hook!` creates it fresh. No file is ever created
under `.claude-auth`: a real file there would permanently shadow the shared
settings.json the daemon links in at spawn (`ensure_account_links` leaves
any existing entry alone). Any other dir without one is skipped. A dangling link, or a path that resolves to
something other than a regular file, is skipped with a warning and never
created through. Every skipped folder or file is also pushed onto `unhooked` with its reason.
"""
function _claude_settings_targets(home::AbstractString, claude_dir::AbstractString;
                                  unhooked::Vector{String} = String[])
    dirs = [claude_dir, joinpath(home, ".claude")]
    auth = joinpath(home, ".claude-auth")
    create_dir = _is_under(claude_dir, auth) ? joinpath(home, ".claude") : claude_dir
    names = try
        isdir(auth) ? readdir(auth) : String[]  # sorted
    catch err
        @warn "could not list the Claude accounts folder — no account settings merged; add the comm hooks to each by hand" dir = auth error = err
        push!(unhooked, "$auth (the accounts folder could not be listed)")
        String[]
    end
    for name in names
        dir = joinpath(auth, name)
        try
            _is_account_name(name) && isdir(dir) && push!(dirs, dir)
        catch err
            @warn "could not read this Claude account folder — skipped; add the comm hooks to it by hand" dir = dir error = err
            push!(unhooked, "$dir (could not be read)")
        end
    end
    dirs = unique(_dirkey, dirs)
    targets = String[]
    for dir in dirs
        try
            path = joinpath(dir, "settings.json")
            if !islink(path) && !ispath(path)
                if _dirkey(dir) == _dirkey(create_dir)
                    push!(targets, path)  # the one dir where a missing file is created fresh
                elseif _is_under(dir, auth)
                    @info "no settings.json in this Claude account folder — skipped, none created (the daemon links the shared one in at the account's first spawn)" dir = dir
                    push!(unhooked, "$dir (no settings.json; none created — the backend links the shared one in at the account's first spawn)")
                elseif isdir(dir)
                    push!(unhooked, "$dir (no settings.json; none created)")
                end
                continue
            end
            real = try
                realpath(path)
            catch
                nothing
            end
            if real === nothing || !isfile(real)
                @warn "settings.json is a dangling link or not a regular file — skipped; add the comm hooks to it by hand" file = path
                push!(unhooked, "$path (a dangling link or not a regular file)")
                continue
            end
            real in targets || push!(targets, real)
        catch err
            @warn "could not read this Claude folder's settings.json — skipped; add the comm hooks to it by hand" file = joinpath(dir, "settings.json") error = err
            push!(unhooked, "$(joinpath(dir, "settings.json")) (could not be read)")
        end
    end
    return targets
end

"""
    _install_claude_hooks(srchooks, claude_dir)

Install the comm hook script(s) from `srchooks` into `\$SOT_COMM_HOME/bin`
(next to the comm-*.sh scripts they shell out to), then idempotently
register the work-state hooks in every Claude account's
settings.json: each real file [`_claude_settings_targets`] returns for
`claude_dir` (the install's own dir, the default `~/.claude`, and each
`~/.claude-auth/<name>` that has one), links resolved, each written once.
The scripts are installed once, not per account.

The work-state hooks make state **event-driven — instant, automatic, and free of
model cooperation**: `UserPromptSubmit → working`, `PreToolUse` on `AskUserQuestion` → blocked, `Stop → idle`, and a `PostToolUse`
heartbeat. They replace the pane-scraping heuristic, which could not be
instant (a poll), was fooled by an agent's own output, and could never tell
"blocked on the user" from "idle".

Each settings.json edit is a **non-destructive jq merge** (via [`_add_comm_hook!`]):
it adds one entry for that event only if absent and preserves every other hook
(repo-boundary-guard, tmux-send-guard, … are untouched). A missing settings.json is created fresh in one dir only, the create dir of [`_claude_settings_targets`], and
never under `~/.claude-auth`; an
unparseable one, or any file when `jq` is unavailable, is left alone and
the exact JSON to add by hand is printed. No-op if `srchooks` is absent.
"""
function _install_claude_hooks(srchooks::AbstractString, claude_dir::AbstractString;
                              unhooked::Vector{String} = String[])
    isdir(srchooks) || return nothing
    bin = joinpath(comm_home(), "bin")
    installed = [f for f in readdir(srchooks) if isfile(joinpath(srchooks, f))]
    isempty(installed) && return nothing
    _install_files(srchooks, bin, installed; executable = Returns(true))
    @info "Installed comm hook scripts" hooks = installed dir = bin
    # Register the work-state hooks. Together they write the FACTS a row is
    # reduced from (ADR 0044 amendment): a turn starting sets `floor`, an
    # AskUserQuestion sets `question` then clears `floor` (a real turn end),
    # a turn ending sets `done` when nothing else is pending and clears
    # `floor`. Retire any comm wiring that is no longer current first (e.g. the old
    # Notification→blocked that lit agents red on plain idle) so settings
    # ends up matching the current set declaratively, then add each via a
    # non-clobbering merge.
    failed = String[]
    for settings in _claude_settings_targets(homedir(), claude_dir; unhooked = unhooked)
        try
            ok = _remove_stale_comm_hooks!(settings)
            for (event, script, matcher) in _COMM_STATE_HOOKS
                ok = _add_comm_hook!(event, script, settings; matcher = matcher) && ok
            end
            ok || push!(unhooked, "$settings (not merged — see the warnings above)")
        catch err
            @warn "could not merge the comm hooks into this settings.json — skipped; add them by hand" file = settings error = err
            push!(failed, settings)
            push!(unhooked, "$settings (merge failed: $(sprint(showerror, err)))")
        end
    end
    isempty(failed) || error("could not merge the comm hooks into: " * join(failed, ", "))
    return nothing
end

# The comm lifecycle hooks: (Claude Code event, script in ~/.sot-comm/bin, tool
# matcher | nothing). The first four are the instant + automatic WORK-STATE
# source that replaces pane-scraping — Claude fires these on its own lifecycle,
# with zero model help. A row is a set of FACTS reduced to one display state
# by comm-status.sh (ADR 0044 amendment, 2026-09-19): `blocked` keys off the
# AskUserQuestion tool (PreToolUse), NOT Notification: Notification also fires
# on plain idle, which lit agents red while merely waiting. A question asked
# in plain text has no automatic signal — an agent self-reports
# `comm-status.sh blocked "<q>"` for those.
const _COMM_STATE_HOOKS = [
    ("UserPromptSubmit", "comm-status-working.sh", nothing),     # turn starts    → prompt (sets floor)
    ("PreToolUse", "comm-status-blocked.sh", "AskUserQuestion"), # opens question → blocked, then stop (a real turn end)
    ("Stop", "comm-status-idle.sh", nothing),                    # turn ends      → stop (done iff floor=user, nothing pending)
    # Long-turn heartbeat: re-stamps a WORKING row's status_at on tool
    # activity (throttled to 60s) so the nav's 10-min wilt marks real stalls,
    # not long busy turns ("a peer session reverting to white", 2026-07-03).
    # Writes no fact of its own. The SAME tool call's answer, when tool_name
    # is AskUserQuestion, is instead the owner replying: sends prompt origin
    # user (a fresh turn start), not a heartbeat refresh.
    ("PostToolUse", "comm-status-heartbeat.sh", nothing),
]

# A hook command string. `\$HOME` (not the resolved path) so the entry is portable
# across machines with the same key but different homes.
_hook_command(script::AbstractString) = "\$HOME/.sot-comm/bin/$script"

"""
    _add_comm_hook!(event, script, settings)

Idempotently add a Claude Code hook for `event` (`"UserPromptSubmit"`,
`"Notification"`, `"Stop"`, …) running `~/.sot-comm/bin/<script>` to
`settings`, preserving all existing config. Uses `jq` so the
merge is structural, not a clobbering rewrite. Falls back to printing the
exact JSON to add by hand when jq is missing or the file can't be parsed —
never overwrites a file it could not safely read. Returns `false` when it
warned and left the file as it was, `true` otherwise (already present, added).

A MISSING `settings.json` (e.g. a fresh `~/.claude` that has never been run
yet) gets a fresh `{}` created first, so the install still gets the hook
registration.

`settings` must be the RESOLVED file ([`_claude_settings_targets`]), never a
symlink: the publish is a rename, which replaces a link at the destination
with a plain file instead of writing through it.
"""
function _add_comm_hook!(event::AbstractString, script::AbstractString, settings::AbstractString;
                         matcher::Union{Nothing,AbstractString} = nothing)
    cmd = _hook_command(script)
    m = matcher === nothing ? "" : matcher
    mfield = isempty(m) ? "" : """ "matcher": "$m", """
    manual = """  "hooks": { "$event": [ {$mfield "hooks": [ { "type": "command", "command": "$cmd" } ] } ] }"""

    if Sys.which("jq") === nothing
        @warn "jq not found — add the comm $event hook to settings.json by hand" file = settings entry = manual
        return false
    end
    if !isfile(settings)
        mkpath(dirname(settings))
        write(settings, "{}")
        @info "Created settings.json for the comm hooks" file = settings
    end

    # jq: add our entry for `event` only if no existing hook for that event
    # already runs this command. .hooks[$evt] is an array of matcher-groups, each
    # with a `hooks` array of {type,command}. We append one group carrying our
    # single command — with a `matcher` when $m is non-empty (PreToolUse needs a
    # tool matcher), without one otherwise. Existing groups are left exactly
    # as-is. `$evt` is a dynamic object key.
    prog = """
    (.hooks // {}) as \$h
    | (\$h[\$evt] // []) as \$cur
    | (any(\$cur[]?; (.hooks // [])[]?.command == \$cmd)) as \$present
    | ({type: "command", command: \$cmd}) as \$h1
    | (if \$m == "" then {hooks: [\$h1]} else {matcher: \$m, hooks: [\$h1]} end) as \$grp
    | if \$present then .
      else .hooks = (\$h + {(\$evt): (\$cur + [\$grp])})
      end
    """

    tmp = ""
    ok = try
        tmp = _settings_tmp(settings)
        run(pipeline(`jq --arg cmd $cmd --arg evt $event --arg m $m $prog $settings`; stdout = tmp))
        true
    catch err
        @warn "could not parse settings.json with jq — leaving it untouched; add the comm $event hook by hand" file = settings entry = manual error = err
        !isempty(tmp) && isfile(tmp) && rm(tmp; force = true)
        false
    end
    ok || return false

    # Detect whether jq actually changed anything (already-present → no-op).
    changed = read(tmp, String) != read(settings, String)
    if !changed
        rm(tmp; force = true)
        @info "comm $event hook already present in settings.json" file = settings
        return true
    end
    # A bare rename, not `mv(...; force = true)`: that form falls back to
    # REMOVING settings.json first when a plain rename fails (e.g. the file
    # is open/locked), which can leave it missing rather than merely
    # un-updated — install_file's docstring has the full mechanism. A bare
    # rename either replaces it atomically or fails leaving it untouched.
    try
        Base.Filesystem.rename(tmp, settings)
    catch err
        isfile(tmp) && rm(tmp; force = true)
        @warn "could not update settings.json (in use?) — leaving the previous copy in place; add the comm $event hook by hand" file = settings entry = manual error = err
        return false
    end
    @info "Added comm $event hook to settings.json" file = settings command = cmd
    return true
end

# The current comm wiring as jq data: one [event, command, matcher-or-""] per hook.
_comm_keep_json() = "[" * join(("[\"$e\",\"$(_hook_command(s))\",\"$(something(m, ""))\"]"
                                for (e, s, m) in _COMM_STATE_HOOKS), ",") * "]"

# A staging file for `settings` that has the settings file's mode BEFORE any
# content is written into it (POSIX), so the contents are never readable more
# widely than the file they replace. On Windows `filemode` is synthesized and
# `chmod` rewrites the ACL, so no mode is set there.
function _settings_tmp(settings::AbstractString)
    tmp = _tmp_name(settings)
    touch(tmp)
    Sys.iswindows() || chmod(tmp, filemode(settings) & 0o7777)
    return tmp
end

"""
    _remove_stale_comm_hooks!(settings)

Strip every comm command that is not part of the current wiring
(`_COMM_STATE_HOOKS`, matched by event, command and matcher) from `settings` (a
resolved file, see [`_add_comm_hook!`]) — retired scripts (any other
`~/.sot-comm/bin/comm-status-*.sh`, the retired `comm-postcompact-reminder.sh` /
`comm-postclear-reminder.sh`) and a current script wired to an old event or
matcher (notably the old `Notification`→blocked that lit agents red on plain
idle). Commands are removed one at a time, a group is dropped only when left
empty and an event only when it has no groups, so every non-comm hook is
preserved, including a user's own script whose name merely resembles a comm one
(only commands under `.sot-comm/bin/` are comm commands), even one sharing a group with a comm command. A current hook is left
in place, so a re-run rewrites nothing. No-op if `jq` is missing or settings.json
is absent/unparseable (a fresh `~/.claude` has nothing to prune yet either way).
Returns `false` when the file could not be pruned (no `jq`, or an error), `true` otherwise.
"""
function _remove_stale_comm_hooks!(settings::AbstractString)
    Sys.which("jq") === nothing && return false
    isfile(settings) || return true  # nothing to prune; the add creates it
    # Remove each comm command that is not current (see the docstring), one
    # by one; a group is dropped only when that leaves it empty, then any
    # event whose group list is now empty.
    keep = _comm_keep_json()
    prog = """
    def comm: (.command // "") | test("(^|/)[.]sot-comm/bin/comm-(status-|post(compact|clear)-reminder)");
    def current(\$e; \$m): (.command // "") as \$c | any(\$keep[]; . == [\$e, \$c, \$m]);
    if .hooks then
      .hooks |= ( to_entries
        | map(.key as \$e | .value |= map(
            (.matcher // "") as \$m
            | if any((.hooks // [])[]?; comm and (current(\$e; \$m) | not))
              then (.hooks |= map(select((comm and (current(\$e; \$m) | not)) | not))) | select(.hooks | length > 0)
              else . end))
        | map(select((.value | length) > 0))
        | from_entries )
    else . end
    """
    tmp = ""
    try
        tmp = _settings_tmp(settings)
        run(pipeline(`jq --argjson keep $keep $prog $settings`; stdout = tmp))
        changed = read(tmp, String) != read(settings, String)
        if !changed
            rm(tmp; force = true)
            return true
        end
        # Bare rename, not `mv(...; force = true)` — see _add_comm_hook!'s
        # comment on the same line for why.
        Base.Filesystem.rename(tmp, settings)
        @info "Retired stale comm hooks from settings.json" file = settings
        return true
    catch err
        @warn "could not prune comm hooks from settings.json — leaving it untouched" file = settings error = err
        !isempty(tmp) && isfile(tmp) && rm(tmp; force = true)
        return false
    end
end

"""
    _install_launchers(srcbin)

Install bare-command launcher scripts (e.g. `ccb`) from `srcbin` into
`~/.local/bin`, making each executable, then prune COMM_DEPRECATED_LAUNCHERS
from the same dir. No-op if `srcbin` is absent. `~/.local/bin` is the
conventional user PATH dir; a warning fires if it is not on `PATH` so bare
commands won't resolve.
"""
function _install_launchers(srcbin::AbstractString)
    isdir(srcbin) || return nothing
    bindir = joinpath(homedir(), ".local", "bin")
    installed = [f for f in readdir(srcbin) if isfile(joinpath(srcbin, f))]
    isempty(installed) || _install_files(srcbin, bindir, installed; executable = Returns(true))
    for f in COMM_DEPRECATED_LAUNCHERS
        p = joinpath(bindir, f)
        if isfile(p)
            rm(p; force = true)
            @info "Pruned deprecated launcher" file = p
        end
    end
    isempty(installed) && return nothing
    @info "Installed launchers" launchers = installed dir = bindir
    occursin(bindir, get(ENV, "PATH", "")) ||
        @warn "Launcher dir is not on PATH — add it to use bare commands" dir = bindir
    return nothing
end
