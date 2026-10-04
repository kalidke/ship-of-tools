# Codex-side install checks: the CODEX_HOME profile check, JSON key guard, marketplace payloads.

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
