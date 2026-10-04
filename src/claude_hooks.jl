# The Claude adapter's settings.json work: account discovery, hook merges, stale-hook removal.

# The account-name rule, character for character the daemon's
# `is_account_name` (`rust/backend/src/agents/accounts.rs`), so "every account"
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
    _install_claude_hooks(claude_dir; unhooked)

Idempotently register the work-state hooks in every Claude account's
settings.json: each real file [`_claude_settings_targets`] returns for
`claude_dir` (the install's own dir, the default `~/.claude`, and each
`~/.claude-auth/<name>` that has one), links resolved, each written once.
The hook scripts themselves install with the other bin folders
(`_comm_bin_files`), whatever `clis`; this function only wires them.

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
the exact JSON to add by hand is printed.
"""
function _install_claude_hooks(claude_dir::AbstractString;
                              unhooked::Vector{String} = String[])
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
