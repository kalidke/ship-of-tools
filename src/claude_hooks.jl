# The Claude adapter's settings.json work: account discovery and the retirement of the comm hooks an earlier install wrote.

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

"""
    _claude_settings_targets(home, claude_dir; unretired = String[]) -> Vector{String}

Every Claude account's existing settings.json, as the REAL files to rewrite, each once, in this order: `claude_dir`
(the install's own dir, `claude_home()`), the default `home/.claude`, then every `home/.claude-auth/<name>` folder whose
name passes [`_is_account_name`] (sorted). Links are resolved (`realpath`), so a writer renames onto the target and a
link stays a link, and two accounts linked to one file are one entry. A folder with no settings.json has nothing to
retire and is skipped silently; none is ever created. A dangling link, or a path that resolves to something other than a
regular file, is skipped with a warning, and every folder or file that could not be read is also pushed onto `unretired`
with its reason.
"""
function _claude_settings_targets(home::AbstractString, claude_dir::AbstractString;
                                  unretired::Vector{String} = String[])
    dirs = [claude_dir, joinpath(home, ".claude")]
    auth = joinpath(home, ".claude-auth")
    names = try
        isdir(auth) ? readdir(auth) : String[]  # sorted
    catch err
        @warn "could not list the Claude accounts folder — its settings files were not checked for comm hooks" dir = auth error = err
        push!(unretired, "$auth (the accounts folder could not be listed)")
        String[]
    end
    for name in names
        dir = joinpath(auth, name)
        try
            _is_account_name(name) && isdir(dir) && push!(dirs, dir)
        catch err
            @warn "could not read this Claude account folder — skipped" dir = dir error = err
            push!(unretired, "$dir (could not be read)")
        end
    end
    targets = String[]
    for dir in unique(_dirkey, dirs)
        try
            path = joinpath(dir, "settings.json")
            (islink(path) || ispath(path)) || continue
            real = try
                realpath(path)
            catch
                nothing
            end
            if real === nothing || !isfile(real)
                @warn "settings.json is a dangling link or not a regular file — skipped" file = path
                push!(unretired, "$path (a dangling link or not a regular file)")
                continue
            end
            real in targets || push!(targets, real)
        catch err
            @warn "could not read this Claude folder's settings.json — skipped" file = joinpath(dir, "settings.json") error = err
            push!(unretired, "$(joinpath(dir, "settings.json")) (could not be read)")
        end
    end
    return targets
end

"""
    _retire_claude_hooks(claude_dir; unretired)

Remove the comm hooks an earlier install wrote from every Claude account's settings.json: each real file
[`_claude_settings_targets`] returns for `claude_dir`, links resolved, each rewritten at most once. The hooks now ride the
launch recipe of a row (`rust/backend/src/agents/layer.rs`), so no settings file of a person's carries one. A file that
holds none is not rewritten, and a hook that is not a comm script's is kept. A file that cannot be rewritten is named on
`unretired`, never an error: a leftover hook does nothing outside a row. Needs jq, which `install_comm` has checked.
"""
function _retire_claude_hooks(claude_dir::AbstractString; unretired::Vector{String} = String[])
    for settings in _claude_settings_targets(homedir(), claude_dir; unretired = unretired)
        _retire_comm_hooks!(settings) || push!(unretired, "$settings (comm hooks not removed — see the warnings above)")
    end
    return nothing
end

# The head of every jq program that edits a settings.json, run with `-n`: the file is read inside jq. No value (a 0-byte
# or whitespace-only file) reads as {}, one value as itself, and more than one is an error, so a file with an appended
# block is left untouched, never rewritten without the rest.
const _JQ_READ_SETTINGS = "[inputs] | if length == 0 then {} elif length == 1 then .[0] " *
                          "else error(\"settings.json holds \\(length) JSON values, not one\") end | "

# The commands an install put under `.sot-comm/bin/` for a settings.json hook, as a jq regular expression: a path ending
# `.sot-comm/bin/comm-<script>`, quoted or not.
const _COMM_HOOK_COMMAND = "(^|/)[.]sot-comm/bin/comm-(status-|subagent-guard|post(compact|clear)-reminder)"

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
    _retire_comm_hooks!(settings)

Strip every comm command from `settings` (a resolved file): a hook command that runs a script an install put under
`.sot-comm/bin/` (`comm-status-*`, `comm-subagent-guard` and the two retired reminders `comm-postcompact-reminder` and
`comm-postclear-reminder`), whatever its event, matcher or quoting. Commands are removed one at a time, a group is
dropped only when left empty and an event only when it has no groups, so every non-comm hook is preserved, including a
user's own script whose name merely resembles a comm one (only commands under `.sot-comm/bin/` are comm commands), even
one sharing a group with a comm command. A file with none is not rewritten. Returns `false` when the file could not be
read or rewritten (a file that does not parse, or holds more than one JSON value, is left untouched), `true` otherwise.
"""
function _retire_comm_hooks!(settings::AbstractString)
    isfile(settings) || return true
    comm = "def comm: (.command // \"\") | if type == \"string\" then test(\"$_COMM_HOOK_COMMAND\") else false end;"
    # First whether any comm command is there at all: a file with none is left byte for byte, not rewritten by jq's own
    # layout.
    has = _JQ_READ_SETTINGS * comm * " [(.hooks // {})[]? | .[]? | (.hooks // [])[]? | select(comm)] | length > 0"
    prog = _JQ_READ_SETTINGS * comm * """
    if .hooks then
      .hooks |= ( to_entries
        | map(.value |= map(
            if any((.hooks // [])[]?; comm)
            then (.hooks |= map(select(comm | not))) | select(.hooks | length > 0)
            else . end))
        | map(select((.value | length) > 0))
        | from_entries )
    else . end
    """
    tmp = ""
    try
        strip(read(`jq -n -r $has $settings`, String)) == "true" || return true
        tmp = _settings_tmp(settings)
        run(pipeline(`jq -n $prog $settings`; stdout = tmp))
        # A bare rename, not `mv(...; force = true)`: that form falls back to REMOVING settings.json first when a plain
        # rename fails (e.g. the file is open/locked), which can leave it missing rather than merely un-updated —
        # install_file's docstring has the full mechanism. A bare rename either replaces it atomically or fails leaving
        # it untouched.
        Base.Filesystem.rename(tmp, settings)
        @info "Removed the comm hooks of an earlier install from settings.json" file = settings
        return true
    catch err
        @warn "could not remove the comm hooks from settings.json — leaving it untouched" file = settings error = err
        !isempty(tmp) && isfile(tmp) && rm(tmp; force = true)
        return false
    end
end
