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

Install every skill directory under `srcdir` (one containing a `SKILL.md`, and `keep(name)`)
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
function _install_skills(srcdir::AbstractString, skillsroot::AbstractString; keep = Returns(true), prune::Bool = true)
    mkpath(skillsroot)
    names = [n for n in readdir(srcdir) if isfile(joinpath(srcdir, n, "SKILL.md")) && keep(n)]
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
    prune && _prune_deprecated_skills!(skillsroot)
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

"""
    _prune_layer_skills!(layerskills, srcdirs)

The layer is the installer's alone, so a skill directory in it that no source folder in `srcdirs` ships any more, or that is an onboarding skill
([`CLAUDE_GLOBAL_SKILLS`], which live in the global folder), goes:
a retired skill must not stay loadable in a row. Called only after every skill installed.
"""
function _prune_layer_skills!(layerskills::AbstractString, srcdirs)
    isdir(layerskills) || return nothing
    shipped = Set(n for src in srcdirs for n in readdir(src) if isfile(joinpath(src, n, "SKILL.md")) && !(n in CLAUDE_GLOBAL_SKILLS))
    for name in readdir(layerskills)
        name in shipped && continue
        rm(joinpath(layerskills, name); recursive = true, force = true)
        @info "Pruned a skill the release no longer ships from the Claude layer" skill = name
    end
    return nothing
end

"""
    _legacy_skill_hashes() -> Dict{String,Set{String}}

The skills an earlier installer placed straight into a Claude skills folder: each name with the hash of every version the
repo ever shipped (`comm/legacy-skills.txt`, made by `scripts/legacy-skill-hashes.sh`; frozen history).
"""
function _legacy_skill_hashes()
    known = Dict{String,Set{String}}()
    for line in eachline(LEGACY_SKILL_HASHES)
        name, hash = split(line, ' ')
        push!(get!(known, String(name), Set{String}()), String(hash))
    end
    return known
end

# SHA is a standard library the package does not list as a dependency (a stale manifest would then need resolving), so
# it is found by its identity at the first use.
const _SHA_ID = Base.PkgId(Base.UUID("ea8e919c-243c-51af-8825-aaa63cd721ce"), "SHA")

"""
    _skill_folder_hash(dir) -> Union{Nothing,String}

The hash `scripts/legacy-skill-hashes.sh` gives a skill folder: sha256 of its sorted manifest, one line
`<path>\t<git blob sha1>` per file. `nothing` for a folder with no file or with any link in it: that is not a copy an
installer made.
"""
function _skill_folder_hash(dir::AbstractString)
    SHA = Base.require(_SHA_ID)
    lines = String[]
    for (root, dirs, files) in walkdir(dir)
        any(d -> islink(joinpath(root, d)), dirs) && return nothing
        for f in files
            path = joinpath(root, f)
            islink(path) && return nothing
            f == NEVER_INSTALLED && continue
            data = read(path)
            blob = Base.invokelatest(SHA.sha1, vcat(Vector{UInt8}("blob $(length(data))\0"), data))
            push!(lines, replace(relpath(path, dir), '\\' => '/') * "\t" * bytes2hex(blob))
        end
    end
    isempty(lines) && return nothing
    return bytes2hex(Base.invokelatest(SHA.sha256, join(sort(lines), "\n")))
end

"""
    _retire_placed_skills!(skillsroot; known = _legacy_skill_hashes())

Remove from a Claude skills folder the skills an earlier install put there, and nothing else. A folder goes only when its
contents hash to a version the repo shipped of the skill of that name; any other folder of such a name is a person's own
and stays, with a warning. A skills folder that is itself a link is never acted through (its target is somebody's
dotfiles), and a link inside it is never removed. The rule holds on every run, so it keeps no marker.
"""
function _retire_placed_skills!(skillsroot::AbstractString; known = _legacy_skill_hashes())
    if islink(skillsroot)
        @warn "this Claude skills folder is a symbolic link: skills an earlier install placed in it were left where they are" dir = skillsroot
        return nothing
    end
    isdir(skillsroot) || return nothing
    for name in sort!(collect(keys(known)))
        name in CLAUDE_GLOBAL_SKILLS && continue   # the onboarding skills live here
        dst = joinpath(skillsroot, name)
        (isdir(dst) && !islink(dst)) || continue
        if _skill_folder_hash(dst) in known[name]
            rm(dst; recursive = true)
            @info "Removed a skill an earlier install placed here; a row loads it from the Claude layer" dir = dst
        else
            @warn "a skill of this name is not a copy of one this installer shipped: left in place" dir = dst
        end
    end
    return nothing
end
