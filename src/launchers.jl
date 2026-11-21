# Launcher scripts installed into ~/.local/bin, and the retired launcher names.

# Launcher scripts retired in a past commit. Pruned from `~/.local/bin` by
# `_install_launchers` on every install — same convention as
# COMM_DEPRECATED_BIN. Append the OLD name here IN THE SAME COMMIT that stops
# shipping a launcher.
const COMM_DEPRECATED_LAUNCHERS = ["ccbe"]

# A shared command can match an earlier checkout. Each Windows shim has one shipped payload.
function _shipped_launcher(path, current, digests)
    isfile(path) && !islink(path) || return false
    old = read(path, String)
    old == current && return true
    isempty(digests) && return false
    lf = replace(old, "\r\n" => "\n")
    # Only a whole-file checkout conversion is a shipped version. Mixed line endings are an edit.
    old == lf || old == replace(lf, "\n" => "\r\n") || return false
    return bytes2hex(Base.invokelatest(Base.require(_SHA_ID).sha256, lf)) in digests
end

function _install_shared_launchers!(problems)
    known = _shipped_hashes(LAUNCHER_HASHES)
    for name in SHARED_LAUNCHERS
        _stage!(problems, name) do
            script = read(joinpath(CLAUDE_LAUNCHER_SRC, name), String)
            path = joinpath(homedir(), ".local", "bin", name)
            owned(dst, file) = dst == path ? _shipped_launcher(file, script, known[name]) :
                                            _shipped_launcher(file, _launcher_refusal(name), ())
            destinations = Sys.iswindows() ? (path, path * ".cmd") : (path,)
            for dst in destinations
                if (ispath(dst) || islink(dst)) && !owned(dst, dst)
                    error("left $dst in place: it is not a shipped launcher")
                end
            end
            _install_launchers(CLAUDE_LAUNCHER_SRC, (name,); retired = (), owned)
        end
    end
end

"""
    _install_launchers(srcbin, installed = nothing; retired = COMM_DEPRECATED_LAUNCHERS)

Install bare-command launcher scripts (e.g. `ccb`) from `srcbin` into
`~/.local/bin`, making each executable, then prune the names in `retired`
from the same dir. No-op if `srcbin` is absent. `~/.local/bin` is the
conventional user PATH dir; a warning fires if it is not on `PATH` so bare
commands won't resolve.
"""
function _install_launchers(srcbin::AbstractString, installed = nothing; retired = COMM_DEPRECATED_LAUNCHERS, owned = Returns(true))
    isdir(srcbin) || return nothing
    bindir = joinpath(homedir(), ".local", "bin")
    if installed === nothing
        installed = [f for f in readdir(srcbin) if isfile(joinpath(srcbin, f)) && f != NEVER_INSTALLED]
    end
    isempty(installed) || _install_files(srcbin, bindir, installed; executable = Returns(true), owned)
    Sys.iswindows() && _install_launcher_refusals(srcbin, bindir, installed; owned)
    for f in retired, p in (joinpath(bindir, f), joinpath(bindir, f * ".cmd"))
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

"""
    _launcher_refusal(name)

The text of `<name>.cmd`, written beside each launcher on Windows. PowerShell and cmd run it in the launcher's place
(PowerShell prefers a `.cmd` to an extensionless file in the same folder), and it says to run the launcher from Git
Bash and exits 1. PowerShell cannot run a bash script: over ssh it refuses one at once, and in a desktop session it
hands one to Windows' open-with handling, where a `sot-fe version` waited half an hour with no output. The shim
starts no bash, so no argument passes through cmd's quoting.
"""
_launcher_refusal(name::AbstractString) =
    "@echo off\r\necho $name: run it from Git Bash (Claude Code's Bash tool); PowerShell and cmd cannot run this bash script. 1>&2\r\nexit /b 1\r\n"

"""
    _install_launcher_refusals(srcbin, bindir, names)

Write `<name>.cmd` (`_launcher_refusal`) beside each launcher in `bindir`, skipping one that already holds that text.
"""
function _install_launcher_refusals(srcbin::AbstractString, bindir::AbstractString, names; owned = Returns(true))
    for name in names
        dst = joinpath(bindir, name * ".cmd")
        text = _launcher_refusal(name)
        isfile(dst) && read(dst, String) == text && continue
        install_file(joinpath(srcbin, name), dst; text = () -> text, owned)
    end
    return nothing
end
