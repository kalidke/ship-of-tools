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
