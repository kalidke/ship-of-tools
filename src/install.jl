# The installer's entry points: protocol version, commit stamp, install_comm and update_comm.

const COMM_PROTOCOL_VERSION = 1

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
    repo_root = REPO_ROOT
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

"""
    install_comm(; clis = [:claude])

Install sot-comm. Copies the files of every folder listed in
comm/bin-folders.txt to `\$SOT_COMM_HOME/bin` (default `~/.sot-comm/bin`),
whatever `clis`, and installs the adapter for each CLI in `clis`
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

    files = _comm_bin_files()
    # Every stage runs; failures are collected and raised together at the
    # end, so one refused file (field report
    # 2026-09-11) no longer leaves the skills and hooks un-updated.
    problems = String[]
    for folder in unique(first.(files))
        names = [n for (f, n) in files if f == folder]
        _stage!(problems, "comm scripts ($(relpath(folder, REPO_ROOT)))") do
            _install_files(folder, bin, names; executable = endswith(".sh"))
        end
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
    shipped = _comm_bin_shipped(files)
    _prune_comm_bin(bin, shipped, prev_manifest)
    _write_comm_manifest(bin, shipped)

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
