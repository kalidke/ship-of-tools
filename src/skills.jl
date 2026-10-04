# Skill installation and retirement: the shared skills installer and the orphan sweep.

# Skill directories retired in a past commit. Pruned from both the Claude
# skills dir and the Codex skills dir on every install — same convention as
# COMM_DEPRECATED_BIN, exact names only, never a glob. Append the OLD name
# here IN THE SAME COMMIT that stops shipping a skill directory.
const COMM_DEPRECATED_SKILLS = ["sot-be-session-start", "sot-fe-session-start"]

"""
    _sweep_orphans!(skilldst, shipped)

Delete files under an installed skill that the source no longer ships — the
only thing the old `rm(dst; recursive = true)` ever bought: a renamed or
deleted reference must not linger and be read as current. Names carrying
`install_file`'s in-flight or aside markers are NOT ours to delete here
(another run may be mid-swap on them; reaping lives in `install_file`, which
knows their owner). Emptied directories go too. Best effort: a file a live
process holds open warns, it does not fail the skill — the skill itself is
current. Called ONLY for a skill whose install raised nothing, since sweeping
a failed skill could delete a resource while its `SKILL.md` stayed old.
"""
function _sweep_orphans!(skilldst::AbstractString, shipped)
    isdir(skilldst) || return nothing
    for (root, _, files) in walkdir(skilldst)
        for f in files
            (occursin(".tmp-", f) || occursin(".stale-", f)) && continue
            rel = relpath(joinpath(root, f), skilldst)
            rel in shipped && continue
            try
                rm(joinpath(root, f))
            catch err
                @warn "orphan left in place" file = joinpath(root, f) error = err
            end
        end
    end
    for (root, dirs, _) in walkdir(skilldst; topdown = false)
        for d in dirs
            full = joinpath(root, d)
            try
                isempty(readdir(full)) && rm(full)
            catch
            end
        end
    end
    return nothing
end

"""
    _install_skills(srcdir, skillsroot) -> Vector{String}

Install every skill directory under `srcdir` (one containing a `SKILL.md`)
into `skillsroot`, file by file through [`install_file`](@ref)'s atomic
copy-then-rename, plus one orphan sweep. Every file is either the old copy or
the new one — never missing, never partial. The destination is NEVER removed:
`rename(2)` cannot replace a non-empty directory, so a "directory swap" is
really rename-aside-then-rename-in, which re-creates the very window it
claims to close.

One path for BOTH adapters. Returns the names that landed (`"/name"`); raises
one combined error naming every skill that kept its previous copy, which the
caller records as a stage failure.
"""
function _install_skills(srcdir::AbstractString, skillsroot::AbstractString)
    mkpath(skillsroot)
    names = [n for n in readdir(srcdir) if isfile(joinpath(srcdir, n, "SKILL.md"))]
    installed = String[]
    stale = String[]
    problems = String[]
    for name in names
        skillsrc = joinpath(srcdir, name)
        rel = sort([relpath(joinpath(r, f), skillsrc)
                    for (r, _, fs) in walkdir(skillsrc) for f in fs if f != NEVER_INSTALLED])
        before = length(problems)
        _stage!(problems, "/$name") do
            _install_files(srcdir, skillsroot, [joinpath(name, r) for r in rel])
        end
        if length(problems) == before
            _sweep_orphans!(joinpath(skillsroot, name), Set(rel))
            push!(installed, "/$name")
        else
            push!(stale, "/$name")
            @warn "skill NOT updated — the destination still holds the PREVIOUS copy" skill = "/$name" dir = skillsroot reason = last(problems)
        end
    end
    _prune_deprecated_skills!(skillsroot)
    # The word Installed may never appear without its denominator and its
    # stale list in the same line: a reader who sees only this line can
    # compute the outcome without reading anything above it.
    @info "Installed skills" installed = "$(length(installed))/$(length(names))" stale = stale dir = skillsroot
    isempty(problems) || error("$(length(installed)) of $(length(names)) skills updated; " *
                               "STALE (previous copy still in place): $(join(problems, "; "))")
    return installed
end

# Remove any of COMM_DEPRECATED_SKILLS still present in `skillsroot`, exact
# names only, never a glob. Shared by both CLI adapters below — the same
# retired skill dirs must not linger under `~/.claude/skills` or
# `$CODEX_HOME/skills` once a commit stops shipping them.
function _prune_deprecated_skills!(skillsroot::AbstractString)
    for name in COMM_DEPRECATED_SKILLS
        dst = joinpath(skillsroot, name)
        isdir(dst) || continue
        rm(dst; recursive = true, force = true)
        @info "Pruned deprecated skill" dir = dst
    end
end
