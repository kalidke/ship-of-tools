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
# at its start, and no script here re-execs its own name.
const COMM_DEPRECATED_BIN = ["bus.sh", "comm-listen.sh", "comm-wake.sh", "comm-watch.sh",
                             "codex-watch.sh", "comm-postcompact-reminder.sh",
                             "comm-postclear-reminder.sh", "comm-session-skill.sh"]

# The names the last successful install shipped into `<bin>`, one per line.
const COMM_MANIFEST = ".sot-comm-installed"

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
