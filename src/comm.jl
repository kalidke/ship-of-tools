# comm.jl — install/update sot-comm, the session-to-session messaging system.
#
# Source of truth: comm/ in the repo. install_comm copies the CLI-agnostic core
# scripts to ~/.sot-comm/bin and each per-CLI adapter to that CLI's dir
# (~/.claude/skills, $CODEX_HOME/skills, ~/.local/bin, hooks/plugins).
# See comm/PROTOCOL.md for the wire contract.
#
# The Claude adapter additionally installs three work-state hooks
# (UserPromptSubmit → working, Notification → blocked, Stop → idle; each shells
# out to comm-status.sh) so an agent's state in the state-nav is event-driven —
# instant and automatic, no model cooperation. Wiring them touches
# ~/.claude/settings.json, but via NON-clobbering jq merges (_add_comm_hook!) that
# add one entry per event only if absent and preserve every existing hook; if jq
# is missing or the file won't parse it is left alone and the exact JSON to add by
# hand is printed.
#
# Install copies (cp force=true) and then prunes a small deprecation list — it
# does NOT discover-and-prune by diffing the dir, so a RENAMED or DELETED managed
# file would otherwise linger as an orphan on every machine (separate-HOME
# Windows FE boxes update only via pull + update_comm; there is no shared NFS
# HOME to clean centrally). RULE: the commit that renames/deletes a managed bin
# file MUST append its OLD name to `COMM_DEPRECATED_BIN` below — install_comm
# removes those, so a pull + update_comm cleans the orphan.

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

# Managed bin/ files removed or RENAMED in a past commit. install_comm deletes
# these from `$SOT_COMM_HOME/bin` so a pull + update_comm cleans the orphan on
# every machine (install is otherwise copy-only — see the RULE above). Append the
# OLD name here IN THE SAME COMMIT that renames/removes a managed bin file.
const COMM_DEPRECATED_BIN = String[]

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
    install_comm(; clis = [:claude])

Install sot-comm. Copies the core scripts to `\$SOT_COMM_HOME/bin`
(default `~/.sot-comm/bin`) and installs the adapter for each CLI in `clis`
(`:claude` skills/hooks, `:codex` skills/hooks/plugin). Idempotent — safe to
re-run to update an existing install.
"""
function install_comm(; clis = [:claude, :codex])
    bin = joinpath(comm_home(), "bin")
    mkpath(bin)
    srcscripts = joinpath(COMM_SRC, "core", "scripts")
    isdir(srcscripts) || error("comm scripts not found at $srcscripts")
    for f in readdir(srcscripts)
        dst = joinpath(bin, f)
        cp(joinpath(srcscripts, f), dst; force = true)
        endswith(f, ".sh") && chmod(dst, 0o755)
    end
    # Remove orphans left by past renames/deletions (see COMM_DEPRECATED_BIN),
    # so a pull + update_comm doesn't leave a stale binary on the machine.
    for f in COMM_DEPRECATED_BIN
        p = joinpath(bin, f)
        if isfile(p)
            rm(p; force = true)
            @info "Pruned deprecated comm script" file = p
        end
    end
    @info "Installed comm scripts" dir = bin count = length(readdir(bin))

    for cli in clis
        _install_adapter(Symbol(cli))
    end
    @info "sot-comm ready" protocol = COMM_PROTOCOL_VERSION home = comm_home()
    @info "Next: in a tmux session run  ~/.sot-comm/bin/comm-join.sh --name <handle>  (or use the /sot-comm skill)"
    return nothing
end

"""
    update_comm(; clis = [:claude, :codex])

Re-sync an existing install from the repo source. Alias of [`install_comm`]
(install is idempotent); run after `git pull` on each machine to close version
skew.
"""
update_comm(; clis = [:claude, :codex]) = install_comm(; clis = clis)

function _install_adapter(cli::Symbol)
    if cli === :claude
        srcdir = joinpath(COMM_SRC, "adapters", "claude")
        skillsroot = joinpath(homedir(), ".claude", "skills")
        mkpath(skillsroot)
        installed = String[]
        for name in readdir(srcdir)
            src = joinpath(srcdir, name)
            isfile(joinpath(src, "SKILL.md")) || continue
            dst = joinpath(skillsroot, name)
            # Copy the whole skill directory (not just SKILL.md) so a skill's
            # resources/ (e.g. project-log's vendored templates) travel with
            # it. Remove any stale destination first so a renamed/removed
            # resource file doesn't linger as an orphan.
            isdir(dst) && rm(dst; recursive = true, force = true)
            cp(src, dst; force = true)
            push!(installed, "/$name")
        end
        @info "Installed Claude skills" skills = installed dir = skillsroot
        _install_launchers(joinpath(srcdir, "bin"))
        _install_claude_hooks(joinpath(srcdir, "hooks"))
    elseif cli === :codex
        # ADR 0031 — codex adapter: ccx launcher, the PermissionRequest->blocked
        # hook script, and the hooks.json plugin payload (state-nav wiring). The shared
        # state scripts (comm-status-*.sh) and codex-watch.sh ride the core
        # deploy above.
        srcdir = joinpath(COMM_SRC, "adapters", "codex")
        isdir(srcdir) || return nothing
        skills_src = joinpath(srcdir, "skills")
        if isdir(skills_src)
            skillsroot = joinpath(codex_home(), "skills")
            installed = String[]
            for name in readdir(skills_src)
                src = joinpath(skills_src, name, "SKILL.md")
                isfile(src) || continue
                dst = joinpath(skillsroot, name)
                mkpath(dst)
                cp(src, joinpath(dst, "SKILL.md"); force = true)
                push!(installed, name)
            end
            @info "Installed Codex skills" skills = installed dir = skillsroot
        end
        _install_launchers(joinpath(srcdir, "bin"))
        hookssrc = joinpath(srcdir, "hooks")
        if isdir(hookssrc)
            bin = joinpath(comm_home(), "bin")
            for f in readdir(hookssrc)
                dst = joinpath(bin, f)
                cp(joinpath(hookssrc, f), dst; force = true)
                chmod(dst, 0o755)
            end
        end
        # Global codex memory: our AGENTS.md also installs as
        # $CODEX_HOME/AGENTS.md so conventions reach codex sessions in ANY
        # project — workspaces are arbitrary repos that won't carry our file
        # (found live: the first daemon-booted codex reported "AGENTS.md
        # conventions not found" from a scratch workspace). Marker-guarded like
        # hooks.json.
        src_agents = joinpath(dirname(COMM_SRC), "AGENTS.md")
        if isfile(src_agents)
            dstdir = codex_home()
            mkpath(dstdir)
            dst = joinpath(dstdir, "AGENTS.md")
            marker = "Codex sessions in Ship of Tools"
            if !isfile(dst) || occursin(marker, read(dst, String))
                cp(src_agents, dst; force = true)
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
        # shared $HOME when CODEX_HOME is machine-local (this deployment points
        # it at /var/tmp to keep codex's flock() off NFS) — so trust is
        # per-machine. ccx additionally passes --dangerously-bypass-hook-trust
        # so a changed hash can't silently disable state reporting on
        # daemon-spawned sessions.
        #
        # The two silent-death traps this file has already fallen into (both
        # cost weeks of colourless sessions, both report Installed=0 in /hooks
        # with no error anywhere) are written up in
        # comm/adapters/codex/hooks.README.md — read it before editing
        # hooks.json. The guard below enforces the first one.
        src = joinpath(srcdir, "hooks.json")
        if isfile(src)
            pdir = joinpath(homedir(), ".agents", "plugins", "sot-comm")
            mkpath(joinpath(pdir, ".codex-plugin"))
            mkpath(joinpath(pdir, "hooks"))
            cp(joinpath(srcdir, "plugin", ".codex-plugin", "plugin.json"),
               joinpath(pdir, ".codex-plugin", "plugin.json"); force = true)
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
end

"""
    _install_claude_hooks(srchooks)

Install the comm hook script(s) from `srchooks` into `\$SOT_COMM_HOME/bin`
(next to the comm-*.sh scripts they shell out to), then idempotently register the
three work-state hooks in `~/.claude/settings.json`.

The work-state hooks make state **event-driven — instant, automatic, and free of
model cooperation**: `UserPromptSubmit → working`, `Notification → blocked`,
`Stop → idle`. They replace the pane-scraping heuristic, which could not be
instant (a poll), was fooled by an agent's own output, and could never tell
"blocked on the user" from "idle".

Each settings.json edit is a **non-destructive jq merge** (via [`_add_comm_hook!`]):
it adds one entry for that event only if absent and preserves every other hook
(repo-boundary-guard, tmux-send-guard, … are untouched). If settings.json is
missing or unparseable, or `jq` is unavailable, it is left alone and the exact
JSON to add by hand is printed. No-op if `srchooks` is absent.
"""
function _install_claude_hooks(srchooks::AbstractString)
    isdir(srchooks) || return nothing
    bin = joinpath(comm_home(), "bin")
    mkpath(bin)
    installed = String[]
    for f in readdir(srchooks)
        sp = joinpath(srchooks, f)
        isfile(sp) || continue
        dst = joinpath(bin, f)
        cp(sp, dst; force = true)
        chmod(dst, 0o755)
        push!(installed, f)
    end
    isempty(installed) && return nothing
    @info "Installed comm hook scripts" hooks = installed dir = bin
    # Register the work-state hooks. Together they make work-state event-driven —
    # a turn starting → working, an AskUserQuestion → blocked, a turn ending →
    # idle. Retire any stale comm wiring first (e.g. the old Notification→blocked
    # that lit agents red on plain idle) so settings ends up matching the current
    # set declaratively, then add each via a non-clobbering merge.
    _remove_stale_comm_hooks!()
    for (event, script, matcher) in _COMM_STATE_HOOKS
        _add_comm_hook!(event, script; matcher = matcher)
    end
    return nothing
end

# The comm lifecycle hooks: (Claude Code event, script in ~/.sot-comm/bin, tool
# matcher | nothing). The first four are the instant + automatic WORK-STATE
# source that replaces pane-scraping — Claude fires these on its own lifecycle,
# with zero model help. `blocked` keys off the AskUserQuestion tool (PreToolUse),
# NOT Notification: Notification also fires on plain idle, which lit agents red
# while merely waiting. A question asked in plain text has no automatic signal —
# an agent self-reports `comm-status.sh blocked "<q>"` for those (the Stop idle
# floor won't clobber it). The last TWO entries are NOT state hooks: they tell a
# context-wiped session (compaction / `/clear`) to re-run its session-start skill
# — the receive path survives both, so the re-run restores context only (below).
const _COMM_STATE_HOOKS = [
    ("UserPromptSubmit", "comm-status-working.sh", nothing),     # turn starts   → working
    ("PreToolUse", "comm-status-blocked.sh", "AskUserQuestion"), # opens question → blocked
    ("Stop", "comm-status-idle.sh", nothing),                    # turn ends     → idle
    # Long-turn heartbeat: re-stamps a WORKING row's status_at on tool
    # activity (throttled to 60s) so the nav's 10-min wilt marks real stalls,
    # not long busy turns ("a peer session reverting to white", 2026-07-03).
    ("PostToolUse", "comm-status-heartbeat.sh", nothing),
    # Post-compaction re-bootstrap (2026-07-19, Keith): SessionStart fires with
    # source=compact after a context summary; the hook (matcher-scoped to
    # `compact`) prints a stdout directive telling the session to RE-RUN its
    # session-start skill — compaction can strip the operating instructions
    # themselves (handle, verbs, work-state), so a bare reminder isn't enough.
    # The skill opens with a "Step 0" that detects survival (end-anchored pgrep
    # of the live watcher) and STOPS on a compaction, so the re-run restores the
    # instructions by being re-read but does NOT re-arm the Monitor, re-comm-poll
    # (replaying handled messages), or re-comm-join (whose row-replace would wipe
    # the live work-state). The hook's command is `comm-postcompact-reminder.sh`
    # (not `comm-status-*`), so `_remove_stale_comm_hooks!` never strips it and
    # this add is idempotent.
    ("SessionStart", "comm-postcompact-reminder.sh", "compact"),
    # Post-`/clear` re-bootstrap (2026-07-25, Keith): SessionStart also fires
    # with source=clear, which the compact hook explicitly skipped — so a
    # `/clear`ed session got NOTHING, despite `/clear` being the harsher case
    # (compaction leaves a summary; `/clear` leaves nothing, so the session no
    # longer knows its handle or verbs while its listener + Monitor keep
    # delivering). The receive path survives a `/clear` exactly as it survives a
    # compaction — the session process isn't killed, so the watcher child lives
    # (measured 2026-07-25) — hence the same Step 0 pgrep guard makes the re-run
    # safe. Both directives now name ONE skill, resolved by
    # `comm-session-skill.sh`, instead of listing three for the model to pick
    # from: the wrong pick runs the frontend bootstrap on a backend box, which
    # fails quietly.
    ("SessionStart", "comm-postclear-reminder.sh", "clear"),
]

# A hook command string. `\$HOME` (not the resolved path) so the entry is portable
# across machines with the same key but different homes.
_hook_command(script::AbstractString) = "\$HOME/.sot-comm/bin/$script"

"""
    _add_comm_hook!(event, script)

Idempotently add a Claude Code hook for `event` (`"UserPromptSubmit"`,
`"Notification"`, `"Stop"`, …) running `~/.sot-comm/bin/<script>` to the user's
`~/.claude/settings.json`, preserving all existing config. Uses `jq` so the merge
is structural, not a clobbering rewrite. Falls back to printing the exact JSON to
add by hand when jq is missing or the file can't be parsed — never overwrites a
file it could not safely read.
"""
function _add_comm_hook!(event::AbstractString, script::AbstractString;
                         matcher::Union{Nothing,AbstractString} = nothing)
    settings = joinpath(homedir(), ".claude", "settings.json")
    cmd = _hook_command(script)
    m = matcher === nothing ? "" : matcher
    mfield = isempty(m) ? "" : """ "matcher": "$m", """
    manual = """  "hooks": { "$event": [ {$mfield "hooks": [ { "type": "command", "command": "$cmd" } ] } ] }"""

    if Sys.which("jq") === nothing
        @warn "jq not found — add the comm $event hook to settings.json by hand" file = settings entry = manual
        return nothing
    end
    if !isfile(settings)
        @warn "no ~/.claude/settings.json — create it with the comm $event hook" file = settings entry = manual
        return nothing
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

    tmp = settings * ".tmp"
    ok = try
        run(pipeline(`jq --arg cmd $cmd --arg evt $event --arg m $m $prog $settings`; stdout = tmp))
        true
    catch err
        @warn "could not parse ~/.claude/settings.json with jq — leaving it untouched; add the comm $event hook by hand" file = settings entry = manual error = err
        isfile(tmp) && rm(tmp; force = true)
        false
    end
    ok || return nothing

    # Detect whether jq actually changed anything (already-present → no-op).
    changed = read(tmp, String) != read(settings, String)
    mv(tmp, settings; force = true)
    if changed
        @info "Added comm $event hook to settings.json" file = settings command = cmd
    else
        @info "comm $event hook already present in settings.json" file = settings
    end
    return nothing
end

"""
    _remove_stale_comm_hooks!()

Strip every comm hook (any `~/.sot-comm/bin/comm-status-*.sh` command) from
`~/.claude/settings.json`, across all events, dropping events left empty. Run
before re-adding the current set ([`_install_claude_hooks`]) so settings ends up
matching `_COMM_STATE_HOOKS` exactly — retiring wirings we no longer use (notably
the old `Notification`→blocked that lit agents red on plain idle). Every non-comm
hook is preserved. No-op if `jq` is missing or settings.json is absent/unparseable.
"""
function _remove_stale_comm_hooks!()
    settings = joinpath(homedir(), ".claude", "settings.json")
    (Sys.which("jq") === nothing || !isfile(settings)) && return nothing
    # For each event, keep only matcher-groups that do NOT run a comm-status-*.sh
    # command; then drop any event whose group list is now empty.
    prog = """
    if .hooks then
      .hooks |= ( to_entries
        | map(.value |= map(select(any((.hooks // [])[]?; (.command // "") | test("comm-status-")) | not)))
        | map(select((.value | length) > 0))
        | from_entries )
    else . end
    """
    tmp = settings * ".tmp"
    try
        run(pipeline(`jq $prog $settings`; stdout = tmp))
        changed = read(tmp, String) != read(settings, String)
        mv(tmp, settings; force = true)
        changed && @info "Retired stale comm hooks from settings.json" file = settings
    catch err
        @warn "could not prune comm hooks from settings.json — leaving it untouched" file = settings error = err
        isfile(tmp) && rm(tmp; force = true)
    end
    return nothing
end

"""
    _install_launchers(srcbin)

Install bare-command launcher scripts (e.g. `ccbe`) from `srcbin` into
`~/.local/bin`, making each executable. No-op if `srcbin` is absent.
`~/.local/bin` is the conventional user PATH dir; a warning fires if it is not
on `PATH` so bare commands won't resolve.
"""
function _install_launchers(srcbin::AbstractString)
    isdir(srcbin) || return nothing
    bindir = joinpath(homedir(), ".local", "bin")
    mkpath(bindir)
    installed = String[]
    for f in readdir(srcbin)
        sp = joinpath(srcbin, f)
        isfile(sp) || continue
        dst = joinpath(bindir, f)
        cp(sp, dst; force = true)
        chmod(dst, 0o755)
        push!(installed, f)
    end
    isempty(installed) && return nothing
    @info "Installed launchers" launchers = installed dir = bindir
    occursin(bindir, get(ENV, "PATH", "")) ||
        @warn "Launcher dir is not on PATH — add it to use bare commands" dir = bindir
    return nothing
end
