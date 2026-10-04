# Atomic publish: stage a file under a private name, then rename it onto the destination.

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
    install_file(src, dst; rename = Base.Filesystem.rename, text = nothing)

Copy `src` to a temporary name beside `dst`, then rename it onto `dst`, so
`dst` is never partially written. With `text`, the temporary then holds `text()`
instead of `src`'s bytes, keeping `src`'s mode. When the rename is refused because a running
process holds `dst` open (Windows), the old file is moved aside under a
`.stale-` name rather than deleted — the process keeps its inode — the new file
lands, and the aside copy is reaped by the next successful replace of that
name. Any failure removes the temporary and throws; if the second rename fails
the old file is put back, so `dst` is never missing.
"""
function install_file(src::AbstractString, dst::AbstractString;
                      rename = Base.Filesystem.rename, text = nothing)
    tmp = _tmp_name(dst)
    aside_made = Ref{Union{Nothing,String}}(nothing)
    try
        cp(src, tmp; force = true)
        text === nothing || write(tmp, text())
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
    _install_files(srcdir, dstdir, files; executable = Returns(false), text = nothing, rename)

Install `files` (names found under `srcdir`, may include subdirectory
components) into `dstdir` one at a time via [`install_file`](@ref) —
continuing past a single failure, so ONE destination a live process still
has open (`comm-relay.sh`, observed live) cannot block updating the rest of
a directory. `executable(name)` files get `chmod(0o755)` after a successful
install. `text(name)`, when given, is what `name` installs as in place of its
source's bytes (`_comm_bin_text`); `rename` goes to `install_file`.

Raises ONE error at the end combining every file that could not be updated
(old copy kept, if one existed — stale is an acceptable outcome; MISSING is
not) with anything [`_check_installed`](@ref) still finds wrong. Every
reason lands in one joined, single-line string, because a launcher that
truncates a crash's stderr to its tail still needs the useful part to
survive.
"""
function _install_files(srcdir::AbstractString, dstdir::AbstractString, files;
                         executable = Returns(false), text = nothing,
                         rename = Base.Filesystem.rename)
    problems = String[]
    for f in files
        dst = joinpath(dstdir, f)
        mkpath(dirname(dst))
        src = joinpath(srcdir, f)
        # A destination that already holds these exact bytes is current: skip the
        # replace: on Windows a file another process holds open fails the
        # replace with EACCES even though nothing is stale.
        if isfile(dst) && read(dst) == (text === nothing ? read(src) : codeunits(text(f)))
            try
                executable(f) && chmod(dst, 0o755)
            catch
            end
            continue
        end
        try
            install_file(src, dst; rename = rename, text = text === nothing ? nothing : () -> text(f))
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
