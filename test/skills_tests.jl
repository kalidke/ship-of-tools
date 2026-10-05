# Tests for src/skills.jl and src/launchers.jl: skill install and orphan sweep, retired aliases, shipped copies.

@testset "skills: an unremovable ORPHAN warns and does not kill the run" begin
    # The reproduced incident: a destination entry that cannot be
    # unlinked (NFS leaves a placeholder for a file a live process still
    # holds open) made the old rm-then-cp throw and killed every later
    # skill and stage. The directory itself was writable — only the
    # removal failed — so that is what this models: shipped files land at
    # the skill root, and the sweep meets an orphan it cannot remove.
    mktempdir() do base
        srcdir = joinpath(base, "src"); root = joinpath(base, "skills")
        for n in ("aaa-skill", "zzz-skill")
            mkpath(joinpath(srcdir, n))
            write(joinpath(srcdir, n, "SKILL.md"), "NEW-$n")
        end
        locked = joinpath(root, "aaa-skill", "retired")
        mkpath(locked)
        write(joinpath(locked, "held.md"), "OLD-held")
        write(joinpath(root, "aaa-skill", "SKILL.md"), "OLD")
        chmod(locked, 0o555)
        # Runtime probe, not a platform check: CI may run as a user whom
        # mode bits do not restrain.
        restrained = try
            rm(joinpath(locked, "held.md")); false
        catch
            true
        end
        try
            if restrained
                ShipTools._install_skills(srcdir, root)
                @test read(joinpath(root, "aaa-skill", "SKILL.md"), String) == "NEW-aaa-skill"
                # A skill later in the walk order still installed.
                @test read(joinpath(root, "zzz-skill", "SKILL.md"), String) == "NEW-zzz-skill"
                # The orphan warned and stayed; it did not throw.
                @test isfile(joinpath(locked, "held.md"))
            end
        finally
            chmod(locked, 0o755)
        end
    end
end

@testset "skills: one unreplaceable skill does not strand the skills after it" begin
    mktempdir() do base
        srcdir = joinpath(base, "src"); root = joinpath(base, "skills")
        names = ["s$(lpad(i, 2, '0'))" for i in 1:6]
        stuck = names[3]
        for n in names
            mkpath(joinpath(srcdir, n))
            write(joinpath(srcdir, n, "SKILL.md"), "NEW-$n")
        end
        mkpath(joinpath(root, stuck, "SKILL.md"))
        write(joinpath(root, stuck, "SKILL.md", "marker.txt"), "keepme")
        err = try
            ShipTools._install_skills(srcdir, root); nothing
        catch e
            e
        end
        @test err isa ErrorException
        @test occursin(stuck, err.msg)
        for n in names
            n == stuck && continue
            @test read(joinpath(root, n, "SKILL.md"), String) == "NEW-$n"
        end
        @test read(joinpath(root, stuck, "SKILL.md", "marker.txt"), String) == "keepme"
    end
end

@testset "skills: the orphan sweep removes retired files and spares markers" begin
    mktempdir() do base
        srcdir = joinpath(base, "src"); root = joinpath(base, "skills")
        mkpath(joinpath(srcdir, "sk", "references"))
        write(joinpath(srcdir, "sk", "SKILL.md"), "NEW")
        write(joinpath(srcdir, "sk", "references", "keep.md"), "KEEP")
        mkpath(joinpath(root, "sk", "references"))
        mkpath(joinpath(root, "sk", "gone"))
        write(joinpath(root, "sk", "references", "retired.md"), "OLD")
        write(joinpath(root, "sk", "gone", "old.md"), "OLD")
        # Derived foreign tag: guaranteed different without naming a host.
        foreign = "x" * ShipTools._host_tag()
        marker = joinpath(root, "sk", "SKILL.md.tmp-$foreign-424242-ab12")
        write(marker, "in-flight elsewhere")
        mkpath(joinpath(root, "my-user-skill"))
        write(joinpath(root, "my-user-skill", "SKILL.md"), "MINE")
        ShipTools._install_skills(srcdir, root)
        @test read(joinpath(root, "sk", "SKILL.md"), String) == "NEW"
        @test read(joinpath(root, "sk", "references", "keep.md"), String) == "KEEP"
        @test !isfile(joinpath(root, "sk", "references", "retired.md"))
        @test !isdir(joinpath(root, "sk", "gone"))
        @test isfile(marker)
        @test read(joinpath(root, "my-user-skill", "SKILL.md"), String) == "MINE"
    end
end

@testset "installer prunes the retired session-start aliases and ccbe" begin
    # Both adapters route through _install_skills: the Codex-only path
    # that never carried resource files must not silently return.
    @test !isdefined(ShipTools, :_install_claude_skills)
    # COMM_DEPRECATED_SKILLS / COMM_DEPRECATED_LAUNCHERS: exact names
    # only. A skill or launcher the repo stopped shipping (a prior
    # install left it on disk) must be removed from BOTH the Claude and
    # Codex skills dirs, and from the shared launcher dir, on every
    # install — while an unrelated user skill/launcher in the same
    # directories survives untouched.
    mktempdir() do home
        claude_skills = joinpath(home, ".claude", "skills")
        codex_skills = joinpath(home, ".codex", "skills")
        bindir = joinpath(home, ".local", "bin")
        for skills in (claude_skills, codex_skills)
            for name in ("sot-be-session-start", "sot-fe-session-start", "my-user-skill")
                mkpath(joinpath(skills, name))
                write(joinpath(skills, name, "SKILL.md"), "---\nname: $name\n---\nstub\n")
            end
        end
        mkpath(bindir)
        write(joinpath(bindir, "ccbe"), "#!/bin/sh\necho stale\n")
        write(joinpath(bindir, "my-launcher"), "#!/bin/sh\necho keepme\n")

        in_home(home) do
            ShipTools.update_comm(clis = [:claude, :codex])
        end

        for skills in (claude_skills, codex_skills)
            @test !isdir(joinpath(skills, "sot-be-session-start"))
            @test !isdir(joinpath(skills, "sot-fe-session-start"))
            @test isdir(joinpath(skills, "my-user-skill"))
        end
        @test !isfile(joinpath(bindir, "ccbe"))
        @test isfile(joinpath(bindir, "my-launcher"))
    end
end

@testset "project-local skills match their shipped copies" begin
    # A fresh checkout runs /sot-setup (and the skills it calls) from
    # .claude/skills before anything is installed; the installer ships
    # the Claude skill folders. A skill held in both must be byte-identical.
    localdir = joinpath(dirname(@__DIR__), ".claude", "skills")
    relfiles(d) = sort([relpath(joinpath(r, f), d) for (r, _, fs) in walkdir(d) for f in fs])
    for shipped in ShipTools.CLAUDE_SKILL_SRCS, name in readdir(localdir)
        isdir(joinpath(shipped, name)) || continue
        a, b = joinpath(localdir, name), joinpath(shipped, name)
        @test relfiles(a) == relfiles(b)
        for rel in relfiles(a)
            @test read(joinpath(a, rel)) == read(joinpath(b, rel))
        end
    end
end

@testset "a CLAUDE.md in a skill or launcher folder is never installed" begin
    mktempdir() do home
        skills = joinpath(home, "src", "skills")
        mkpath(joinpath(skills, "demo", "sub"))
        write(joinpath(skills, "demo", "SKILL.md"), "---\nname: demo\n---\n")
        write(joinpath(skills, "demo", "CLAUDE.md"), "page")
        write(joinpath(skills, "demo", "sub", "CLAUDE.md"), "page")
        write(joinpath(skills, "demo", "sub", "ref.md"), "ref")
        launchers = joinpath(home, "src", "bin")
        mkpath(launchers)
        write(joinpath(launchers, "ccdemo"), "#!/bin/sh\n")
        write(joinpath(launchers, "CLAUDE.md"), "page")
        dst = joinpath(home, "dst", "skills")
        in_home(home) do
            ShipTools._install_skills(skills, dst)
            ShipTools._install_launchers(launchers)
        end
        @test isfile(joinpath(dst, "demo", "SKILL.md"))
        @test isfile(joinpath(dst, "demo", "sub", "ref.md"))
        @test !ispath(joinpath(dst, "demo", "CLAUDE.md"))
        @test !ispath(joinpath(dst, "demo", "sub", "CLAUDE.md"))
        @test isfile(joinpath(home, ".local", "bin", "ccdemo"))
        @test !ispath(joinpath(home, ".local", "bin", "CLAUDE.md"))
    end
end
