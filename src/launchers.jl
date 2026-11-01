# Launcher scripts installed into ~/.local/bin, and the retired launcher names.

# Launcher scripts retired in a past commit. Pruned from `~/.local/bin` by
# `_install_launchers` on every install — same convention as
# COMM_DEPRECATED_BIN. Append the OLD name here IN THE SAME COMMIT that stops
# shipping a launcher.
const COMM_DEPRECATED_LAUNCHERS = ["ccbe"]

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
    installed = [f for f in readdir(srcbin) if isfile(joinpath(srcbin, f)) && f != NEVER_INSTALLED]
    isempty(installed) || _install_files(srcbin, bindir, installed; executable = Returns(true))
    Sys.iswindows() && _install_launcher_refusals(srcbin, bindir, installed)
    for f in COMM_DEPRECATED_LAUNCHERS, p in (joinpath(bindir, f), joinpath(bindir, f * ".cmd"))
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
function _install_launcher_refusals(srcbin::AbstractString, bindir::AbstractString, names)
    for name in names
        dst = joinpath(bindir, name * ".cmd")
        text = _launcher_refusal(name)
        isfile(dst) && read(dst, String) == text && continue
        install_file(joinpath(srcbin, name), dst; text = () -> text)
    end
    return nothing
end
